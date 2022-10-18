#![warn(clippy::all, clippy::pedantic)]

#[macro_use]
extern crate log;

use std::sync::Arc;

pub use async_imap::error::Error as IMAPError;
pub use email::{inbox, Message};

use async_native_tls::TlsStream;
use async_std::net::TcpStream;
use async_trait::async_trait;
use thiserror::Error;

use email::{
    inbox::WatchError,
    login::{ConfigSessionGenerator, SessionGenerator},
    message::RawFetcher,
};
use futures::Stream;
use task::{Join, Spawn, SpawnError};

pub mod config;
mod email;
pub mod parse;
pub mod task;
#[cfg(test)]
mod testutil;
pub mod ynab;

type IMAPTransportStream = TlsStream<TcpStream>;
type IMAPClient = async_imap::Client<IMAPTransportStream>;
type IMAPSession = async_imap::Session<IMAPTransportStream>;

#[derive(Error, Debug)]
pub enum StreamSetupError {
    #[error("failed to spawn stream task: {0}")]
    SpawnFailed(SpawnError),
    #[error("failed to setup IMAP for inbox watch: {0}")]
    WatchFailed(IMAPError),
}

impl From<email::StreamSetupError> for StreamSetupError {
    fn from(err: email::StreamSetupError) -> Self {
        match err {
            email::StreamSetupError::SpawnFailed(spawn_err) => Self::SpawnFailed(spawn_err),
        }
    }
}

/// `CloseableStream` is a stream that be cleaned up manually. The implementation is free to (and likely should) clean
/// up resources on drop, but given the async nature of this trait, this should not block, for risk of blocking the
/// event loop.
#[async_trait]
pub trait CloseableStream: Stream {
    // close will clean up all resources associated with the stream. This returned `Future` will resolve
    // when the resources have been cleaned up.
    async fn close(self);
}

/// Stream any new messages that come into the inbox provided by the given [`config::IMAP`].
///
/// This iterator must take ownership of the configuration, due to async implementation details, but callers
/// are encouraged to clone their configuration if they feel they cannot part with it.
///
/// # Errors
/// Will return a [`StreamSetupError`] if there was any failure in constructing the stream. See its docs
/// for more details.
pub async fn stream_new_messages<S>(
    spawner: Arc<S>,
    imap_config: config::IMAP,
) -> Result<impl CloseableStream<Item = Message> + Send, StreamSetupError>
where
    S: Spawn + Send + Sync + Unpin + 'static,
    S::Handle: Unpin + 'static,
    <<S as Spawn>::Handle as Join>::Error: Send,
{
    let session_generator_arc = Arc::new(ConfigSessionGenerator::new(imap_config.clone()));
    let sequence_number_stream = inbox::watch(spawner.as_ref(), session_generator_arc.clone())
        .await
        .map_err(|err| match err {
            WatchError::SpawnError(err) => StreamSetupError::SpawnFailed(err),
            WatchError::IMAPSetupError(err) => StreamSetupError::WatchFailed(err),
        })?;

    let fetcher = RawFetcher::new(session_generator_arc);
    let fetch_stream = email::stream_incoming_messages(spawner, sequence_number_stream, fetcher)?;

    Ok(fetch_stream)
}
