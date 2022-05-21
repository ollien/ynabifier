use futures::{lock::Mutex, Sink, SinkExt, Stream, StreamExt};
use std::{
    error::Error,
    fmt::{Debug, Display, Formatter},
    sync::Arc,
};

use crate::task::Spawn;

use async_trait::async_trait;

pub mod inbox;
pub mod login;
pub mod message;

/// The ID of a single message stored in the inbox.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Hash))]
pub struct SequenceNumber(u32);

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

/// `SequenceNumberStreamer` will stream new emails, and then return their sequence numbers to the stream.
#[async_trait]
pub trait SequenceNumberStreamer {
    type Stream: Stream<Item = SequenceNumber>;
    type Error: Error;

    /// Watch for any new messages comming in, and emit them on the given stream.
    async fn watch_for_new_messages(&mut self) -> Result<Self::Stream, Self::Error>;

    /// Stop all Stream senders from giving future values.
    fn stop(&mut self);
}

/// `MessageFetcher` will fetch the contents of an email
#[async_trait]
pub trait MessageFetcher {
    type Error: Error;

    async fn fetch_message(&self, sequence_number: SequenceNumber) -> Result<String, Self::Error>;
}

struct FetchTask<F: MessageFetcher, O: Sink<String> + Clone> {
    fetcher: Arc<Mutex<F>>,
    output_sink: O,
}

pub struct Broker<W, F, S, O>
where
    W: SequenceNumberStreamer,
    F: MessageFetcher,
    S: Spawn,
    O: Sink<String> + Clone,
{
    watcher: W,
    task_data: FetchTask<F, O>,
    spawner: S,
}

impl<W, F, S, O> Broker<W, F, S, O>
where
    W: SequenceNumberStreamer,
    W::Stream: Unpin,
    F: MessageFetcher + Send + Sync + 'static,
    F::Error: Send,
    S: Spawn,
    O: Sink<String> + Unpin + Clone + Send + Sync + 'static,
    O::Error: Debug,
{
    pub fn new(watcher: W, fetcher: F, spawner: S, output_sink: O) -> Self {
        Self {
            watcher,
            task_data: FetchTask::new(fetcher, output_sink),
            spawner,
        }
    }

    /// Stream any incoming mesages from the [`SequenceNumberStreamer`] to the output sink.
    ///
    /// # Errors
    /// Returns an error if there was a problem in setting up the stream. Individual message failures
    /// will be logged.
    pub async fn stream_incoming_messages_to_sink(&mut self) -> Result<(), W::Error> {
        let mut stream = self.watcher.watch_for_new_messages().await?;

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

        Ok(())
    }
}

impl<F, O> FetchTask<F, O>
where
    F: MessageFetcher,
    O: Sink<String> + Clone + Send + Sync + Unpin,
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

impl<F: MessageFetcher, O: Sink<String> + Clone> Clone for FetchTask<F, O> {
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
        sync::Arc,
        time::Duration,
    };

    use crate::task::{Cancel, SpawnError};

    use super::*;
    use futures::{
        channel::mpsc::{self, UnboundedReceiver, UnboundedSender},
        lock::Mutex,
        Future, FutureExt, SinkExt,
    };
    use futures::{select, StreamExt};
    use futures_timer::Delay;
    use thiserror::Error;

    #[derive(Error, Debug)]
    struct StringError(String);

    impl Display for StringError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    struct MockSequenceNumberStreamer {
        sender: Arc<Mutex<UnboundedSender<SequenceNumber>>>,
        receiver: Arc<Mutex<UnboundedReceiver<SequenceNumber>>>,
        stopped: bool,
    }

    impl MockSequenceNumberStreamer {
        pub fn new() -> Self {
            let (tx, rx) = mpsc::unbounded();
            Self {
                sender: Arc::new(Mutex::new(tx)),
                receiver: Arc::new(Mutex::new(rx)),
                stopped: false,
            }
        }
    }

    #[async_trait]
    impl SequenceNumberStreamer for MockSequenceNumberStreamer {
        type Stream = UnboundedReceiver<SequenceNumber>;
        type Error = StringError;

        async fn watch_for_new_messages(&mut self) -> Result<Self::Stream, Self::Error> {
            let (mut tx, rx) = mpsc::unbounded();

            // normally I'd use a spawner for this but this is a mock so it's ok...
            let receiver_mutex = self.receiver.clone();
            tokio::spawn(async move {
                let mut receiver = receiver_mutex.lock().await;
                while let Some(msg) = receiver.next().await {
                    tx.send(msg).await.expect("send failed");
                }
            });

            Ok(rx)
        }

        fn stop(&mut self) {
            unimplemented!();
        }
    }

    #[derive(Default)]
    struct MockMessageFetcher {
        messages: HashMap<SequenceNumber, String>,
    }

    impl MockMessageFetcher {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn stage_message(&mut self, seq: SequenceNumber, message: &str) {
            self.messages.insert(seq, message.to_string());
        }
    }

    #[async_trait]
    impl MessageFetcher for MockMessageFetcher {
        type Error = StringError;

        async fn fetch_message(
            &self,
            sequence_number: SequenceNumber,
        ) -> Result<String, Self::Error> {
            self.messages.get(&sequence_number).cloned().ok_or_else(|| {
                StringError(format!("sequence number {:?} not found", sequence_number))
            })
        }
    }

    struct TokioSpawner;
    impl Spawn for TokioSpawner {
        type Cancel = CancelFnOnce;

        fn spawn<F: Future + Send + 'static>(&self, future: F) -> Result<Self::Cancel, SpawnError>
        where
            <F as Future>::Output: Send,
        {
            let handle = tokio::spawn(future);
            let canceler = CancelFnOnce {
                cancel_func: Box::new(move || handle.abort()),
            };

            Ok(canceler)
        }
    }

    struct CancelFnOnce {
        cancel_func: Box<dyn FnOnce() + Send + Sync>,
    }

    impl Cancel for CancelFnOnce {
        fn cancel(self) {
            (self.cancel_func)();
        }
    }

    #[tokio::test]
    async fn test_messages_from_streamer_go_to_sink() {
        let mock_watcher = MockSequenceNumberStreamer::new();
        let mut mock_fetcher = MockMessageFetcher::new();
        let (message_tx, mut message_rx) = mpsc::unbounded::<String>();

        let watcher_sender_mutex = mock_watcher.sender.clone();
        let mut watcher_sender = watcher_sender_mutex.lock().await;

        mock_fetcher.stage_message(SequenceNumber(123), "hello, world!");

        let mut broker = Broker::new(mock_watcher, mock_fetcher, TokioSpawner, message_tx);
        let join_handle = tokio::spawn(async move {
            broker
                .stream_incoming_messages_to_sink()
                .await
                .expect("stream failed");
        });

        watcher_sender
            .send(SequenceNumber(123))
            .await
            .expect("send failed");

        select! {
            msg = message_rx.next() => assert_eq!("hello, world!", msg.expect("empty channel")),
            _ = join_handle.fuse() => panic!("broker returned, but did not receive message"),
            _ = Delay::new(Duration::from_secs(5)).fuse() => panic!("test timed out"),
        };
    }
}
