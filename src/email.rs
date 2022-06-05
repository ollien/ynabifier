use futures::{
    channel::mpsc::{self, Receiver, Sender},
    lock::Mutex,
    Sink, SinkExt, Stream, StreamExt,
};
use std::{
    error::Error,
    fmt::{Debug, Display, Formatter},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use thiserror::Error;

use crate::{
    task::{Spawn, SpawnError},
    CHANNEL_SIZE,
};

use async_trait::async_trait;

pub mod inbox;
pub mod login;
pub mod message;

/// The ID of a single message stored in the inbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Hash))]
pub struct SequenceNumber(u32);

impl SequenceNumber {
    pub fn new(seq: u32) -> Self {
        Self(seq)
    }

    /// Get the integral value of this sequence number
    pub fn value(self) -> u32 {
        self.0
    }
}

impl Display for SequenceNumber {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.value())
    }
}

/// `MessageFetcher` will fetch the contents of an email. Implementations should not really perform post-processing on
/// the emails, so as to prevent blocking other async tasks.
#[async_trait]
pub trait MessageFetcher {
    type Error: Error;

    /// Fetch an individual email based on its sequence number. Unfortunately, because the format cannot be guaranteed
    ///  to be UTF-8, each message returned must return an owned copy of the raw bytes for further post-processing.
    async fn fetch_message(&self, sequence_number: SequenceNumber) -> Result<Vec<u8>, Self::Error>;
}

/// `StreamSetupError` will be returned if there was an error in setting up the message stream.
#[derive(Debug, Error)]
pub enum StreamSetupError {
    /// Spawning the background stream task failed
    #[error("failed to spawn stream task: {0}")]
    SpawnFailed(SpawnError),
}

/// `MessageStream` is a stream of any messages returned from `stream_incoming_messages`.
pub struct MessageStream {
    output_stream: Receiver<Vec<u8>>,
}

/// Get a stream of any messages that come into the given [`SequenceNumberStraemer`]. These messsages will be fetched
/// by sequence number using the given [`MessageFetcher`]. This requires spawning several background tasks, so a
/// [`Spawn`] is also required.
///
/// # Errors
/// If the stream cannot be set up (i.e. because the underlying task could not be set up for any reason), then it is
/// returned.
// TODO: Does this need to use StreamSetupError? It's an enum with one variant...
pub async fn stream_incoming_messages<M, F, S>(
    spawn: Arc<S>,
    sequence_number_stream: M,
    fetcher: F,
) -> Result<MessageStream, StreamSetupError>
where
    M: Stream<Item = SequenceNumber> + Send + Unpin + 'static,
    F: MessageFetcher + Send + Sync + 'static,
    F::Error: Sync + Send,
    S: Spawn + Send + Sync + 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_SIZE);

    let spawn_clone = spawn.clone();
    // TODO: We should use this
    let canceler = spawn
        .spawn(async move {
            stream_incoming_messages_to_sink(spawn_clone, sequence_number_stream, fetcher, tx)
                .await;
        })
        .map_err(StreamSetupError::SpawnFailed)?;

    Ok(MessageStream { output_stream: rx })
}

/// Fetch any messages from the incoming stream of sequence numbers, and send them to the output sink.
///
/// # Errors
/// Returns an error if there was a problem in setting up the stream. Individual message failures
/// will be logged.
async fn stream_incoming_messages_to_sink<S, N, F, O>(
    spawner: Arc<S>,
    mut sequence_number_stream: N,
    fetcher: F,
    output_sink: O,
) where
    S: Spawn,
    N: Stream<Item = SequenceNumber> + Unpin,
    F: MessageFetcher + Send + Sync + 'static,
    F::Error: Send + Sync,
    O: Sink<Vec<u8>> + Send + Unpin + Clone + 'static,
    O::Error: Debug,
{
    let fetcher_arc = Arc::new(fetcher);
    while let Some(sequence_number) = sequence_number_stream.next().await {
        let fetcher_arc_clone = fetcher_arc.clone();
        let output_sink_clone = output_sink.clone();
        // TODO: We should use this for cancellation
        let spawn_res = spawner.spawn(async move {
            let mut fetch_task = FetchTask {
                fetcher: fetcher_arc_clone,
                output_sink: output_sink_clone,
            };

            fetch_task.fetch_and_sink_message(sequence_number).await;
        });
        if let Err(spawn_err) = spawn_res {
            error!(
                "failed to spawn task to fetch sequence number {sequence_number}: {spawn_err:?}",
            );
        }
    }
}

struct FetchTask<F, O> {
    fetcher: Arc<F>,
    output_sink: O,
}

impl<F, O> FetchTask<F, O>
where
    F: MessageFetcher,
    O: Sink<Vec<u8>> + Send + Unpin + Clone + 'static,
    O::Error: Debug,
{
    /// Fetch a message with the given sequence number, and send its output to this Task's
    /// output sink. Errors for this are logged, as this is intended to be run in a forked off task.
    async fn fetch_and_sink_message(&mut self, sequence_number: SequenceNumber) {
        let fetch_result = self.fetcher.fetch_message(sequence_number).await;
        if let Err(fetch_err) = fetch_result {
            error!("failed to fetch message: {:?}", fetch_err);
            return;
        }

        let msg = fetch_result.unwrap();
        let send_res = self.output_sink.send(msg).await;
        if let Err(send_err) = send_res {
            error!("failed to send fetched message to output stream {send_err:?}");
        }
    }
}

impl Stream for MessageStream {
    type Item = Vec<u8>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let output_stream = &mut self.get_mut().output_stream;

        Pin::new(output_stream).poll_next(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fmt::{Display, Formatter},
        time::Duration,
    };

    use crate::testutil::TokioSpawner;

    use super::*;
    use futures::{select, StreamExt};
    use futures::{stream, FutureExt};
    use futures_timer::Delay;
    use thiserror::Error;

    #[derive(Error, Debug)]
    struct StringError(String);

    impl Display for StringError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    #[derive(Debug, Default)]
    struct MockMessageFetcher {
        messages: HashMap<SequenceNumber, Vec<u8>>,
    }

    impl MockMessageFetcher {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn stage_message(&mut self, seq: SequenceNumber, message: &str) {
            self.messages.insert(seq, message.bytes().collect());
        }
    }

    #[async_trait]
    impl MessageFetcher for MockMessageFetcher {
        type Error = StringError;

        async fn fetch_message(
            &self,
            sequence_number: SequenceNumber,
        ) -> Result<Vec<u8>, Self::Error> {
            self.messages.get(&sequence_number).cloned().ok_or_else(|| {
                StringError(format!("sequence number {:?} not found", sequence_number))
            })
        }
    }

    #[tokio::test]
    async fn test_messages_from_streamer_go_to_sink() {
        let sequence_number_stream = stream::iter(vec![SequenceNumber(123)].into_iter());
        let mut mock_fetcher = MockMessageFetcher::new();

        mock_fetcher.stage_message(SequenceNumber(123), "hello, world!");

        let mut broker_handle =
            stream_incoming_messages(Arc::new(TokioSpawner), sequence_number_stream, mock_fetcher)
                .await
                .expect("failed to setup stream");

        select! {
            msg = broker_handle.next().fuse() => assert_eq!(
                "hello, world!".bytes().collect::<Vec<u8>>(),
                msg.expect("empty channel"),
            ),
            _ = Delay::new(Duration::from_secs(5)).fuse() => panic!("test timed out"),
        };
    }
}
