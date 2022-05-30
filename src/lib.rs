#![warn(clippy::all, clippy::pedantic)]

#[macro_use]
extern crate log;

use async_native_tls::TlsStream;
use async_std::net::TcpStream;
pub use config::{Config, IMAP as IMAPConfig};

pub use email::{
    inbox::Watcher,
    // TODO: most of these shouldn't be public
    login::{ConfigSessionGenerator, SessionGenerator},
    message::RawFetcher,
    MessageFetcher,
    SequenceNumber,
    SequenceNumberStreamer,
    StreamSetupError,
};
use futures::Stream;
use task::Spawn;

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
) -> Result<
    impl Stream<Item = Vec<u8>>,
    StreamSetupError<<Watcher<ConfigSessionGenerator, S> as SequenceNumberStreamer>::Error>,
> {
    // TODO: Instead of making multiple session generators, maybe these types could take an Arc
    let make_session_generator = || ConfigSessionGenerator::new(imap_config.clone());
    let watcher = Watcher::new(make_session_generator(), spawner.clone());
    let fetcher = RawFetcher::new(make_session_generator());

    let spawner_clone = spawner.clone();
    let fetch_stream = email::stream_incoming_messages(spawner_clone, watcher, fetcher).await?;

    Ok(fetch_stream)
}
