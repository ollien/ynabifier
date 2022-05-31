#![warn(clippy::all, clippy::pedantic)]

#[macro_use]
extern crate log;

use async_native_tls::TlsStream;
use async_std::net::TcpStream;
use thiserror::Error;

pub use config::{Config, IMAP as IMAPConfig};
pub use email::inbox::WatchError;

use email::{
    login::{ConfigSessionGenerator, SessionGenerator},
    message::RawFetcher,
};
use futures::Stream;
use task::{Spawn, SpawnError};

const CHANNEL_SIZE: usize = 16;

mod config;
mod email;
pub mod task;
#[cfg(test)]
mod testutil;

type IMAPTransportStream = TlsStream<TcpStream>;
type IMAPClient = async_imap::Client<IMAPTransportStream>;
// TODO: Probably doesn't need to be pub?
pub type IMAPSession = async_imap::Session<IMAPTransportStream>;

#[derive(Error, Debug)]
pub enum StreamSetupError {
    #[error("failed to spawn stream task: {0}")]
    SpawnFailed(SpawnError),
    #[error("failed to initiate inbox watch: {0}")]
    WatchFailed(WatchError),
}

impl From<email::StreamSetupError> for StreamSetupError {
    fn from(err: email::StreamSetupError) -> Self {
        match err {
            email::StreamSetupError::SpawnFailed(spawn_err) => Self::SpawnFailed(spawn_err),
        }
    }
}

/// Stream any new messages that come into the inbox provided by the given [`IMAPConfig`].
///
/// This iterator must take ownership of the configuration, due to async implementation details, but callers
/// are encouraged to clone their configuration if they feel they cannot part with it.
///
/// # Errors
/// Will return a [`StreamSetupError`] if there was any failure in constructing the stream. See its docs
/// for more details.
pub async fn stream_new_messages<S: Spawn + Send + Sync + Clone + 'static>(
    spawner: &S,
    imap_config: IMAPConfig,
) -> Result<impl Stream<Item = Vec<u8>>, StreamSetupError> {
    // TODO: Instead of making multiple session generators, maybe these types could take an Arc
    let make_session_generator = || ConfigSessionGenerator::new(imap_config.clone());
    let watcher = email::inbox::watch_for_new_messages(spawner, Box::new(make_session_generator()))
        .await
        .map_err(StreamSetupError::WatchFailed)?;

    let fetcher = RawFetcher::new(make_session_generator());

    let spawner_clone = spawner.clone();
    let fetch_stream = email::stream_incoming_messages(spawner_clone, watcher, fetcher).await?;

    Ok(fetch_stream)
}
