#![warn(clippy::all, clippy::pedantic)]
// TODO: Remove the need for these. I'm experimenting right now.
#![allow(clippy::missing_errors_doc)]

#[macro_use]
extern crate log;

use std::fmt::Debug;

use async_imap::{self, Client, Session};
use async_native_tls::TlsConnector;
pub use config::{Config, IMAP as IMAPConfig};
use futures::{AsyncRead, AsyncWrite};

mod config;
mod fetch;
mod inbox;

// TODO: Remove
pub use fetch::fetch_email;
pub use inbox::{SequenceNumber, Watcher};

pub async fn setup_session(
    cfg: &Config,
) -> async_imap::error::Result<Session<impl AsyncRead + AsyncWrite + Unpin + Debug + Send>> {
    let imap_cfg = cfg.imap();
    println!("Logging in...");
    let client = build_imap_client(imap_cfg).await?;

    client
        .login(imap_cfg.username(), imap_cfg.password())
        .await
        .map_err(|err| err.0)
}

async fn build_imap_client(
    imap_cfg: &IMAPConfig,
) -> async_imap::error::Result<Client<impl AsyncRead + AsyncWrite + Unpin + Debug>> {
    let tls_connector = TlsConnector::new();
    async_imap::connect(
        (imap_cfg.domain(), imap_cfg.port()),
        imap_cfg.domain(),
        tls_connector,
    )
    .await
}
