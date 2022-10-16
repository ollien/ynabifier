use futures::{
    channel::mpsc::{self, Receiver},
    Sink, SinkExt, Stream, StreamExt,
};
use mailparse::{MailParseError, ParsedMail};
use std::{
    error::Error,
    fmt::{Debug, Display, Formatter},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use stop_token::{StopSource, StopToken};
use thiserror::Error;

use crate::{
    task::{
        self,
        multi::{self, Runner, Task},
        CancelOnDrop, ResolveOrStop, Spawn, SpawnError,
    },
    CloseableStream, CHANNEL_SIZE,
};

use async_trait::async_trait;

pub mod inbox;
pub mod login;
pub mod message;

/// The ID of a single message stored in the inbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Hash))]
pub struct SequenceNumber(u32);

/// Represents a single message fetched from the mail server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    raw: Vec<u8>,
}

impl Message {
    #[must_use]
    pub fn new(raw: Vec<u8>) -> Self {
        Self { raw }
    }

    /// Get the raw bytes of this message
    #[must_use]
    pub fn raw(&self) -> &[u8] {
        self.raw.as_ref()
    }

    /// Parse this message as an email.
    ///
    /// # Errors
    /// An error is returned if this message not is not parsable as an email.
    /// See [`mailparse::parse_mail`] for more details.
    pub fn parsed(&self) -> Result<ParsedMail<'_>, MailParseError> {
        mailparse::parse_mail(self.raw.as_ref())
    }
}

/// `MessageFetcher` will fetch the contents of an email. Implementations should not really perform post-processing on
/// the emails, so as to prevent blocking other async tasks.
#[async_trait]
pub trait MessageFetcher {
    type Error: Error;

    /// Fetch an individual email based on its sequence number. Unfortunately, because the format cannot be guaranteed
    ///  to be UTF-8, each message returned must return an owned copy of the raw bytes for further post-processing.
    async fn fetch_message(&self, sequence_number: SequenceNumber) -> Result<Message, Self::Error>;
}

/// `StreamSetupError` will be returned if there was an error in setting up the message stream.
#[derive(Debug, Error)]
pub enum StreamSetupError {
    /// Spawning the background stream task failed
    #[error("failed to spawn stream task: {0}")]
    SpawnFailed(SpawnError),
}

/// `MessageStream` is a stream of any messages returned from `stream_incoming_messages`.
pub struct MessageStream<H> {
    output_stream: Receiver<Message>,
    sink_stop_source: StopSource,
    stream_task_handle: H,
    // TODO: This should join too
    cancel_guard: CancelOnDrop<multi::CancelHandle>,
}

impl SequenceNumber {
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

impl<H: Unpin> Stream for MessageStream<H> {
    type Item = Message;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let output_stream = &mut self.get_mut().output_stream;

        Pin::new(output_stream).poll_next(cx)
    }
}

#[async_trait]
impl<H: task::Handle + Send + Unpin> CloseableStream for MessageStream<H> {
    async fn close(self) {
        let Self {
            output_stream: _,
            sink_stop_source,
            stream_task_handle,
            cancel_guard,
        } = self;

        drop(cancel_guard);
        drop(sink_stop_source);
        // If we fail to join, the resources are as cleaned up as we're going to get them. Bubbling this up
        // is kinda tricky, and honestly, is only going to happen if the tasks got killed for some reason.
        if let Err(err) = stream_task_handle.join().await {
            error!("Failed to join with sequence number stream: {:?}", err);
        }
    }
}

#[derive(Error, Debug)]
enum StreamError<F: MessageFetcher, O: Sink<T>, T>
where
    O::Error: Debug,
{
    #[error("{0:?}")]
    FetchFailed(F::Error),

    #[error("{0:?}")]
    SinkFailed(O::Error),
}

/// Get a stream of any messages that come into the given [`SequenceNumberStreamer`]. These messages will be fetched
/// by sequence number using the given [`MessageFetcher`]. This requires spawning several background tasks, so a
/// [`Spawn`] is also required. When the given `stop_source` is triggered, all fetching and streaming will stop.
///
/// # Errors
/// If the stream cannot be set up (i.e. because the underlying task could not be set up for any reason), then it is
/// returned.
// TODO: Does this need to use StreamSetupError? It's an enum with one variant...
pub async fn stream_incoming_messages<M, F, S>(
    spawn: Arc<S>,
    sequence_number_stream: M,
    fetcher: F,
    stop_source: StopSource,
) -> Result<MessageStream<S::Handle>, StreamSetupError>
where
    M: CloseableStream<Item = SequenceNumber> + Send + Unpin + 'static,
    F: MessageFetcher + Send + Sync + 'static,
    F::Error: Sync + Send,
    S: Spawn + Send + Sync + 'static,
    S::Handle: 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_SIZE);

    let spawn_clone = spawn.clone();
    let (runner, runner_cancel_handle) =
        multi::listen_for_tasks(spawn).map_err(StreamSetupError::SpawnFailed)?;
    let stop_token = stop_source.token();
    let stream_handle = spawn_clone
        .spawn(async move {
            let mut runner = runner;

            stream_incoming_messages_to_sink(
                &mut runner,
                sequence_number_stream,
                fetcher,
                tx,
                stop_token,
            )
            .await;
        })
        .map_err(StreamSetupError::SpawnFailed)?;

    Ok(MessageStream {
        output_stream: rx,
        sink_stop_source: stop_source,
        stream_task_handle: stream_handle,
        cancel_guard: CancelOnDrop::new(runner_cancel_handle),
    })
}
/// Fetch any messages from the incoming stream of sequence numbers, and send them to the output sink.
///
/// This is meant to be forked into the background, so individual message failures will be logged.
async fn stream_incoming_messages_to_sink<N, F, O>(
    runner: &mut Runner,
    mut sequence_number_stream: N,
    fetcher: F,
    output_sink: O,
    mut stop_token: StopToken,
) where
    N: CloseableStream<Item = SequenceNumber> + Send + Unpin,
    F: MessageFetcher + Send + Sync + 'static,
    F::Error: Send + Sync,
    O: Sink<Message> + Send + Unpin + Clone + 'static,
    O::Error: Debug,
{
    let fetcher_arc = Arc::new(fetcher);
    while let Some(sequence_number) = sequence_number_stream
        .next()
        .resolve_or_stop(&mut stop_token)
        .await
        .flatten()
    {
        let fetcher_arc_clone = fetcher_arc.clone();
        let output_sink_clone = output_sink.clone();

        let fetch_future = async move {
            let fetcher = fetcher_arc_clone;
            let mut output_sink_clone = output_sink_clone;

            let fetch_res =
                fetch_and_sink_message(fetcher.as_ref(), &mut output_sink_clone, sequence_number)
                    .await;
            match fetch_res {
                Ok(_) => {}
                Err(StreamError::FetchFailed(err)) => error!("failed to fetch message: {:?}", err),
                Err(StreamError::SinkFailed(err)) => {
                    error!("failed to send fetched message to output sink: {:?}", err);
                }
            }
        };

        let fetch_task = Task::new(
            format!("Fetch sequence number {}", sequence_number),
            Box::new(fetch_future),
        );
        debug!("submitting task for sequence number {}", sequence_number);
        let spawn_res = runner.submit_new_task(fetch_task).await;

        if let Err(spawn_err) = spawn_res {
            error!(
                "failed to spawn task to fetch sequence number {sequence_number}: {spawn_err:?}"
            );
        }
    }

    sequence_number_stream.close().await;
}

/// Fetch a message with the given sequence number, and send its output to this Task's
/// output sink. Errors for this are logged, as this is intended to be run in a forked off task.
async fn fetch_and_sink_message<F, O>(
    fetcher: &F,
    output_sink: &mut O,
    sequence_number: SequenceNumber,
) -> Result<(), StreamError<F, O, Message>>
where
    F: MessageFetcher,
    O: Sink<Message> + Send + Unpin + Clone + 'static,
    O::Error: Debug,
{
    let msg = fetcher
        .fetch_message(sequence_number)
        .await
        .map_err(StreamError::FetchFailed)?;

    output_sink
        .send(msg)
        .await
        .map_err(StreamError::SinkFailed)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        fmt::{Display, Formatter},
        iter,
        time::Duration,
    };

    use crate::testutil::TokioSpawner;

    use super::*;
    use futures::{
        channel::oneshot::{self, Sender},
        select, StreamExt,
    };
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
        messages: HashMap<SequenceNumber, Message>,
    }

    impl MockMessageFetcher {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn stage_message(&mut self, seq: SequenceNumber, message: &str) {
            self.messages.insert(
                seq,
                Message {
                    raw: message.bytes().collect(),
                },
            );
        }
    }

    #[async_trait]
    impl MessageFetcher for MockMessageFetcher {
        type Error = StringError;

        async fn fetch_message(
            &self,
            sequence_number: SequenceNumber,
        ) -> Result<Message, Self::Error> {
            self.messages.get(&sequence_number).cloned().ok_or_else(|| {
                StringError(format!("sequence number {:?} not found", sequence_number))
            })
        }
    }

    struct TrackedCloseableSteam<S> {
        stream: S,
        tx: Sender<()>,
    }

    impl<S> TrackedCloseableSteam<S> {
        pub fn new(stream: S, oneshot_sender: Sender<()>) -> Self {
            Self {
                stream,
                tx: oneshot_sender,
            }
        }
    }

    impl<S: Stream + Unpin> Stream for TrackedCloseableSteam<S> {
        type Item = S::Item;
        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let stream = &mut self.get_mut().stream;

            Pin::new(stream).poll_next(cx)
        }
    }

    #[async_trait]
    impl<S: Stream + Send + Unpin> CloseableStream for TrackedCloseableSteam<S> {
        async fn close(self) {
            self.tx.send(()).expect("failed to signal finished");
        }
    }

    #[tokio::test]
    async fn test_messages_from_streamer_go_to_sink() {
        let (tx, _rx) = oneshot::channel::<()>();
        let sequence_number_stream =
            TrackedCloseableSteam::new(stream::iter(vec![SequenceNumber(123)].into_iter()), tx);
        let mut mock_fetcher = MockMessageFetcher::new();

        mock_fetcher.stage_message(SequenceNumber(123), "hello, world!");

        let mut broker_handle = stream_incoming_messages(
            Arc::new(TokioSpawner),
            sequence_number_stream,
            mock_fetcher,
            StopSource::new(),
        )
        .await
        .expect("failed to setup stream");

        select! {
            msg = broker_handle.next().fuse() => assert_eq!(
                "hello, world!".bytes().collect::<Vec<u8>>(),
                msg.expect("empty channel").raw(),
            ),
            _ = Delay::new(Duration::from_secs(5)).fuse() => panic!("test timed out"),
        };
    }

    #[tokio::test]
    async fn test_closing_stream_closes_underlying_stream() {
        let (tx, rx) = oneshot::channel::<()>();
        let sequence_number_stream =
            TrackedCloseableSteam::new(stream::iter(iter::empty::<SequenceNumber>()), tx);

        let message_stream = stream_incoming_messages(
            Arc::new(TokioSpawner),
            sequence_number_stream,
            MockMessageFetcher::new(),
            StopSource::new(),
        )
        .await
        .expect("failed to setup stream");

        message_stream.close().await;

        select! {
            res = rx.fuse() => assert!(res.is_ok(), "did not get close signal"),
            _ = Delay::new(Duration::from_secs(5)).fuse() => panic!("test timed out"),
        };
    }
}
