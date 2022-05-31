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

/// Get a stream of any messages that come into the given [`SequenceNumberStraemer`]. These messsages will be fetched
/// by sequence number using the given [`MessageFetcher`]. This requires spawning several background tasks, so a
/// [`Spawn`] is also required.
///
/// # Errors
/// If the stream cannot be set up (i.e. because the underlying task could not be set up for any reason), then it is
/// returned.
// TODO: Does this really need to take ownership?
// TODO: Does this need to use StreamSetupError? It's an enum with one variant...
pub async fn stream_incoming_messages<M, F, S>(
    spawn: S,
    sequence_number_stream: M,
    fetcher: F,
) -> Result<MessageStream<S, F>, StreamSetupError>
where
    M: Stream<Item = SequenceNumber> + Send + Unpin + 'static,
    F: MessageFetcher + Send + Sync + 'static,
    F::Error: Send,
    S: Spawn + Send + Sync + Clone + 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_SIZE);
    let broker = Arc::new(Broker::new(spawn.clone(), fetcher, tx));
    let broker_arc_clone = broker.clone();
    spawn
        .spawn(async move {
            let mut message_stream = sequence_number_stream;
            broker_arc_clone
                .stream_incoming_messages_to_sink(&mut message_stream)
                .await;
        })
        .map_err(StreamSetupError::SpawnFailed)?;

    Ok(MessageStream {
        _broker: broker,
        output_stream: rx,
    })
}

/// `MessageStream` is a stream of any messages returned from `stream_incoming_messages`.
// TODO: add some kind of `stop` method to this.
pub struct MessageStream<S, F> {
    _broker: Arc<Broker<S, F, Sender<Vec<u8>>>>,
    output_stream: Receiver<Vec<u8>>,
}

struct FetchTask<F, O> {
    fetcher: Arc<Mutex<F>>,
    output_sink: O,
}

/// Broker acts as an intermediary that will take incoming sequence numbers, fetch these messages on background tasks,
/// and return the results to a given output sink.
struct Broker<S, F, O> {
    spawner: S,
    task_data: FetchTask<F, O>,
}

impl<S, F, O> Broker<S, F, O>
where
    F: MessageFetcher + Send + Sync + 'static,
    F::Error: Send,
    S: Spawn,
    O: Sink<Vec<u8>> + Send + Unpin + Clone + 'static,
    O::Error: Debug,
{
    fn new(spawner: S, fetcher: F, output_sink: O) -> Self {
        Self {
            task_data: FetchTask::new(fetcher, output_sink),
            spawner,
        }
    }

    /// Fetch any messages from the incoming stream of sequence numbers, and send them to the output sink.
    ///
    /// # Errors
    /// Returns an error if there was a problem in setting up the stream. Individual message failures
    /// will be logged.
    async fn stream_incoming_messages_to_sink<N: Stream<Item = SequenceNumber> + Unpin>(
        &self,
        stream: &mut N,
    ) {
        while let Some(sequence_number) = stream.next().await {
            let mut task_data = self.task_data.clone();
            let spawn_res = self.spawner.spawn(async move {
                task_data.fetch_and_sink_message(sequence_number).await;
            });
            if let Err(spawn_err) = spawn_res {
                error!(
                    "failed to spawn task to fetch sequence number {sequence_number}: {spawn_err:?}",
                );
            }
        }
    }
}

impl<F, O> FetchTask<F, O>
where
    F: MessageFetcher,
    O: Sink<Vec<u8>> + Clone + Send + Unpin,
    O::Error: Debug,
{
    pub fn new(fetcher: F, output_sink: O) -> Self {
        Self {
            fetcher: Arc::new(Mutex::new(fetcher)),
            output_sink,
        }
    }

    /// Fetch a message with the given sequence number, and send its output to this Task's
    /// output sink. Errors for this are logged, as this is intended to be run in a forked off task.
    pub async fn fetch_and_sink_message(&mut self, sequence_number: SequenceNumber) {
        let fetcher = self.fetcher.lock().await;

        let fetch_result = fetcher.fetch_message(sequence_number).await;
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

impl<S, F> Stream for MessageStream<S, F> {
    type Item = Vec<u8>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let output_stream = &mut self.get_mut().output_stream;

        Pin::new(output_stream).poll_next(cx)
    }
}

impl<F: MessageFetcher, O: Sink<Vec<u8>> + Clone> Clone for FetchTask<F, O> {
    fn clone(&self) -> Self {
        Self {
            // because `fetcher` is wrapped in an Arc, we must derive Clone manually.
            fetcher: self.fetcher.clone(),
            output_sink: self.output_sink.clone(),
        }
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
            stream_incoming_messages(TokioSpawner, sequence_number_stream, mock_fetcher)
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
