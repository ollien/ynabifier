//! The login module abstracts the generation of IMAP sessions for use when fetching emails

use crate::config::IMAP as IMAPConfig;
use crate::{IMAPClient, IMAPSession};
use async_imap::error::Result as IMAPResult;
use async_native_tls::TlsConnector;
use async_trait::async_trait;

#[async_trait]
pub trait SessionGenerator {
    async fn new_session(&self) -> IMAPResult<IMAPSession>;
}

pub struct ConfigSessionGenerator {
    config: IMAPConfig,
}

#[async_trait]
impl SessionGenerator for ConfigSessionGenerator {
    async fn new_session(&self) -> IMAPResult<IMAPSession> {
        let client = self.new_client().await?;

        client
            .login(self.config.username(), self.config.password())
            .await
            .map_err(|err| err.0)
    }
}

impl ConfigSessionGenerator {
    pub fn new(config: IMAPConfig) -> Self {
        Self { config }
    }

    async fn new_client(&self) -> IMAPResult<IMAPClient> {
        let tls_connector = TlsConnector::new();
        async_imap::connect(
            (self.config.domain(), self.config.port()),
            self.config.domain(),
            tls_connector,
        )
        .await
    }
}
