use std::fmt::Debug;

use async_imap::{
    error::{Error as IMAPError, Result as IMAPResult},
    extensions::idle::Handle,
    imap_proto::{MailboxDatum, Response},
};
use futures::{AsyncRead, AsyncWrite};
use thiserror::Error;
use tokio::sync::broadcast::{self, error::SendError};

use crate::{IMAPClient, IMAPSession, IMAPTransportStream};

mod idle;

#[derive(Clone, Copy, Debug)]
// `SequenceNumber` represents the ID of a single message stored in the inbox.
pub struct SequenceNumber(u32);

/// An error that occured during the watching for an email.
#[derive(Error, Debug)]
pub enum WatchError {
    /// `AsyncImapFailure` is a general failure from `async-imap`
    #[error("imap failure: {0}")]
    AsyncIMAPFailure(IMAPError),
    #[error("send failure: {0}")]
    SendFailure(SendError<SequenceNumber>),
}

impl From<IMAPError> for WatchError {
    fn from(err: IMAPError) -> Self {
        Self::AsyncIMAPFailure(err)
    }
}

impl From<SendError<SequenceNumber>> for WatchError {
    fn from(err: SendError<SequenceNumber>) -> Self {
        Self::SendFailure(err)
    }
}

/// Monitors an inbox for new messages, and sends them to other listeners.
pub struct Watcher {
    sender: broadcast::Sender<SequenceNumber>,
}

impl SequenceNumber {
    /// Get the integral value of this sequence number
    pub fn value(self) -> u32 {
        self.0
    }
}

impl Watcher {
    /// `new` constructs a new `Watcher` that will send updates to the given Sender
    pub fn new(sender: broadcast::Sender<SequenceNumber>) -> Self {
        Self { sender }
    }

    /// Watch for the arrival of a single email. While the `Watcher` is watching, the caller must relinquish ownership
    /// of the `Session`, but it will be returned to it upon succesful completion of the
    pub async fn watch_for_email(&self, session: IMAPSession) -> Result<IMAPSession, WatchError> {
        let mut idle_handle = session.idle();
        let sequence_number = Self::idle_for_email(&mut idle_handle).await?;
        self.sender.send(sequence_number)?;

        let reclaimed_session = idle_handle.done().await?;
        Ok(reclaimed_session)
    }

    async fn idle_for_email(
        idle_handle: &mut Handle<IMAPTransportStream>,
    ) -> IMAPResult<SequenceNumber> {
        loop {
            idle_handle.init().await?;
            let idle_res = idle::wait_for_data(idle_handle).await;
            match idle_res {
                Ok(data) => {
                    let response = data.response();
                    let maybe_sequence_number =
                        Self::get_sequence_number_from_response(response).await;

                    match maybe_sequence_number {
                        Some(sequence_number) => return Ok(sequence_number),
                        None => {
                            debug!("re-issuing IDLE after getting non-EXISTS response from IDLE command: {:?}", response);
                        }
                    }
                }
                Err(idle::Error::AsyncIMAPError(err)) => return Err(err),
                Err(idle::Error::Timeout) => {
                    debug!("re-issuing IDLE after timeout");
                }
            }
        }
    }

    async fn get_sequence_number_from_response<'a>(
        response: &Response<'a>,
    ) -> Option<SequenceNumber> {
        match response {
            Response::MailboxData(MailboxDatum::Exists(seq)) => Some(SequenceNumber(*seq)),
            _ => None,
        }
    }
}
