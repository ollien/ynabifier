#![warn(clippy::all, clippy::pedantic)]

#[macro_use]
extern crate log;

use async_native_tls::TlsStream;
use async_std::net::TcpStream;
pub use config::{Config, IMAP as IMAPConfig};

pub use email::inbox::Watcher;
pub use email::login::{ConfigSessionGenerator, SessionGenerator};
pub use email::SequenceNumberStreamer;

mod config;
mod email;
pub mod fetch;
pub mod task;

type IMAPTransportStream = TlsStream<TcpStream>;
type IMAPClient = async_imap::Client<IMAPTransportStream>;
// TODO: Probably doesn't need to be pub?
pub type IMAPSession = async_imap::Session<IMAPTransportStream>;
