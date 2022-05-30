#![warn(clippy::all, clippy::pedantic)]

#[macro_use]
extern crate log;

use std::{
    pin::Pin,
    task::{Context, Poll},
};

use async_native_tls::TlsStream;
use async_std::net::TcpStream;
pub use config::{Config, IMAP as IMAPConfig};
use thiserror::Error;

pub use email::{
    inbox::Watcher,
    // TODO: most of these shouldn't be public
    login::{ConfigSessionGenerator, SessionGenerator},
    message::RawFetcher,
    Broker,
    MessageFetcher,
    SequenceNumber,
    SequenceNumberStreamer,
};
use futures::{
    channel::mpsc::{self, Receiver}, Stream,
};
use task::{Spawn, SpawnError};

const CHANNEL_SIZE: usize = 16;

mod config;
mod email;
pub mod task;

type IMAPTransportStream = TlsStream<TcpStream>;
type IMAPClient = async_imap::Client<IMAPTransportStream>;
// TODO: Probably doesn't need to be pub?
pub type IMAPSession = async_imap::Session<IMAPTransportStream>;

/// `StreamSetupError` will be returned if there was an error in setting up the stream in [`stream_new_messages`]
#[derive(Debug, Error)]
pub enum StreamSetupError {
    /// Spawning the background stream task failed
    #[error("failed to spawn stream task: {0}")]
    SpawnFailed(SpawnError),
}

/// Stream any new messages that come into the inbox provided by the given [`IMAPConfig`].
///
/// This iterator must take ownership of the configuration, due to async implementation details, but callers
/// are encouraged to clone their configuration if they feel they cannot part with it.
///
/// # Errors
/// Will return a [`StreamSetupError`] if there was any failure in constructing the stream. See its docs
/// for more details.
pub fn stream_new_messages<S: Spawn + Send + Clone + 'static>(
    spawner: &S,
    imap_config: IMAPConfig,
) -> Result<MessageStream, StreamSetupError> {
    // TODO: Instead of making multiple session generators, maybe these types could take an Arc
    let make_session_generator = || ConfigSessionGenerator::new(imap_config.clone());
    let watcher = Watcher::new(make_session_generator(), spawner.clone());
    let fetcher = RawFetcher::new(make_session_generator());
    let (mut tx, rx) = mpsc::channel(CHANNEL_SIZE);

    let spawner_clone = spawner.clone();
    let mut broker = Broker::new(watcher, fetcher, spawner_clone, tx.clone());
    spawner
        .spawn(async move {
            let stream_start_res = broker.stream_incoming_messages_to_sink().await;
            if let Err(err) = stream_start_res {
                error!("failed to start streaming: {}", err);
                // is this the right close?
                tx.close_channel();
            }
        })
        .map_err(StreamSetupError::SpawnFailed)?;

    let stream = MessageStream {
        config: imap_config,
        message_rx: rx,
    };

    Ok(stream)
}

pub struct MessageStream {
    config: IMAPConfig,
    message_rx: Receiver<Vec<u8>>,
}

impl Stream for MessageStream {
    type Item = Vec<u8>;

    /// Streams all messages from the underlying message watcher
    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        let rx = &mut self.get_mut().message_rx;

        Pin::new(rx).poll_next(cx)
    }
}
