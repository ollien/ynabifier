use futures::{
    channel::{
        mpsc::{self, UnboundedReceiver},
        oneshot,
    },
    lock::Mutex,
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
    task::{self, Join, Registry, ResolveOrStop, Spawn, SpawnError},
    CloseableStream,
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
    /// to be UTF-8, each message returned must return an owned copy of the raw bytes for further post-processing.
    /// When the given StopToken resolves, the fetcher implementation is expected to clean up all of its resources.
    /// If it is possible to return a message, the fetcher is free to do so, but otherwise it may wish to return
    /// some kind of indicative error.
    async fn fetch_message(
        &self,
        sequence_number: SequenceNumber,
        stop_token: &mut StopToken,
    ) -> Result<Message, Self::Error>;
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
    output_stream: UnboundedReceiver<Message>,
    sink_stop_source: StopSource,
    stream_task_handle: H,
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
        } = self;

        drop(sink_stop_source);
        // If we fail to join, the resources are as cleaned up as we're going to get them. Bubbling this up
        // is kinda tricky, and honestly, is only going to happen if the tasks got killed for some reason.
        if let Err(err) = stream_task_handle.join().await {
            error!("Failed to join with sequence number stream: {err:?}");
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
pub fn stream_incoming_messages<M, F, S>(
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
    <<S as Spawn>::Handle as Join>::Error: Send,
{
    let (tx, rx) = mpsc::unbounded();

    let spawn_clone = spawn.clone();
    let stop_token = stop_source.token();
    let stream_handle = spawn_clone
        .spawn(async move {
            let mut stop_token = stop_token;
            stream_incoming_messages_to_sink(
                spawn,
                sequence_number_stream,
                fetcher,
                tx,
                &mut stop_token,
            )
            .await;
        })
        .map_err(StreamSetupError::SpawnFailed)?;

    Ok(MessageStream {
        output_stream: rx,
        sink_stop_source: stop_source,
        stream_task_handle: stream_handle,
    })
}
/// Fetch any messages from the incoming stream of sequence numbers, and send them to the output sink.
///
/// This is meant to be forked into the background, so individual message failures will be logged.
async fn stream_incoming_messages_to_sink<S, N, F, O>(
    spawn: Arc<S>,
    mut sequence_number_stream: N,
    fetcher: F,
    output_sink: O,
    stop_token: &mut StopToken,
) where
    S: Spawn + Send + Sync + 'static,
    S::Handle: 'static,
    <<S as Spawn>::Handle as Join>::Error: Send,
    N: CloseableStream<Item = SequenceNumber> + Send + Unpin,
    F: MessageFetcher + Send + Sync + 'static,
    F::Error: Send + Sync,
    O: Sink<Message> + Send + Unpin + Clone + 'static,
    O::Error: Debug,
{
    let fetcher_arc = Arc::new(fetcher);
    let task_registry = Arc::new(Mutex::new(Registry::new()));
    while let Some(sequence_number) = sequence_number_stream
        .next()
        .resolve_or_stop(stop_token)
        .await
        .flatten()
    {
        let fetcher_arc_clone = fetcher_arc.clone();
        let output_sink_clone = output_sink.clone();

        let stop_token_clone = stop_token.clone();
        let fetch_future = async move {
            let fetcher = fetcher_arc_clone;
            let mut output_sink_clone = output_sink_clone;
            let mut stop_token_clone = stop_token_clone;

            let fetch_res = fetch_and_sink_message(
                fetcher.as_ref(),
                &mut output_sink_clone,
                sequence_number,
                &mut stop_token_clone,
            )
            .await;

            match fetch_res {
                Ok(_) => {}
                Err(StreamError::FetchFailed(err)) => error!("Failed to fetch message: {err}"),
                Err(StreamError::SinkFailed(err)) => {
                    error!("Failed to send fetched message to output sink: {err:?}");
                }
            }
        };

        debug!(
            "Starting fetch task for sequence number {}",
            sequence_number
        );

        let (token_tx, token_rx) = oneshot::channel();
        let registry_weak = Arc::downgrade(&task_registry);
        let spawn_res = spawn.spawn(async move {
            let task_registry = registry_weak;
            // This cannot happen, as the tx channel cannot have been dropped.
            let task_token = token_rx.await.expect("failed to get task token");
            fetch_future.await;

            // Unregister ourselves fro the registry.
            // We don't really care if the registry has been dropped, as all we're trying to signal is that we're done.
            if let Some(registry) = task_registry.upgrade() {
                registry.lock().await.unregister_handle(task_token);
            }
        });

        match spawn_res {
            Ok(task_handle) => {
                let token = task_registry.lock().await.register_handle(task_handle);
                // This cannot happen, as the task cannot have been dropped.
                token_tx.send(token).expect("failed to send task token");
            }
            Err(spawn_err) => {
                error!(
                    "Failed to spawn task to fetch sequence number {sequence_number}: {spawn_err:?}"
                );
            }
        }
    }

    sequence_number_stream.close().await;
    debug!("Joining all remaining fetch tasks...");
    if let Err(err) = task_registry.lock().await.join_all().await {
        error!("Failed to join all fetch tasks: {err:?}");
    };
}

/// Fetch a message with the given sequence number, and send its output to this Task's
/// output sink. Errors for this are logged, as this is intended to be run in a forked off task.
async fn fetch_and_sink_message<F, O>(
    fetcher: &F,
    output_sink: &mut O,
    sequence_number: SequenceNumber,
    stop_token: &mut StopToken,
) -> Result<(), StreamError<F, O, Message>>
where
    F: MessageFetcher,
    O: Sink<Message> + Send + Unpin + Clone + 'static,
    O::Error: Debug,
{
    let msg = fetcher
        .fetch_message(sequence_number, stop_token)
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
    use futures::stream;
    use futures::{
        channel::oneshot::{self, Sender},
        StreamExt,
    };
    use thiserror::Error;
    use tokio::time;

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
            _stop_token: &mut StopToken,
        ) -> Result<Message, Self::Error> {
            self.messages
                .get(&sequence_number)
                .cloned()
                .ok_or_else(|| StringError(format!("sequence number {sequence_number} not found")))
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
        .expect("failed to setup stream");

        let msg = time::timeout(Duration::from_secs(5), broker_handle.next())
            .await
            .expect("test timed out");
        assert_eq!(
            "hello, world!".bytes().collect::<Vec<u8>>(),
            msg.expect("empty channel").raw(),
        );
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
        .expect("failed to setup stream");

        message_stream.close().await;

        let rx_res = time::timeout(Duration::from_secs(5), rx)
            .await
            .expect("test timed out");

        rx_res.expect("did not get close signal");
    }
}
