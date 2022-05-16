use futures::Stream;
use std::error::Error;

use self::inbox::SequenceNumber;
use async_trait::async_trait;

pub mod inbox;
pub mod login;

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

pub struct Broker<S: SequenceNumberStreamer> {
    watcher: S,
}
