//! The inbox module holds implementations to allow for the monitoring of a real inbox

use self::idle::{SessionCell, SessionState};
use super::{login::SessionGenerator, SequenceNumber};
use crate::{
    task::{Spawn, SpawnError},
    IMAPSession, IMAPTransportStream,
};
use async_imap::{
    error::{Error as IMAPError, Result as IMAPResult},
    extensions::idle::Handle,
    imap_proto::{MailboxDatum, Response},
};
use futures::{
    channel::mpsc::{self, Sender},
    SinkExt, Stream,
};
use std::{
    fmt::Debug,
    sync::atomic::{AtomicBool, Ordering},
};
use thiserror::Error;

mod idle;

const CHANNEL_SIZE: usize = 16;

/// An error that occurs during the setup process of a [`Watcher`] stream
#[derive(Error, Debug)]
pub enum WatchError {
    #[error("failed to setup IMAP environment to begin watching: {0}")]
    IMAPSetupError(IMAPError),
    #[error("failed to spawn watch task: {0}")]
    SpawnError(SpawnError),
}

pub async fn watch_for_new_messages<S, G, E>(
    spawner: &S,
    session_generator: E,
) -> Result<impl Stream<Item = SequenceNumber>, WatchError>
where
    S: Spawn + Sync,
    G: SessionGenerator + Sync + Send,
    E: AsRef<G> + Send + Sync + 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_SIZE);
    // TODO: this session never does a log-out. We need some kind of async RAII for that

    let mut session = session_generator
        .as_ref()
        .new_session()
        .await
        .map_err(WatchError::IMAPSetupError)?;

    session
        .examine("INBOX")
        .await
        .map_err(WatchError::IMAPSetupError)?;

    let watch_future = async move {
        let task = WatchTask {
            session_generator: session_generator.as_ref(),
            stopped: AtomicBool::new(false),
        };

        let mut tx = tx;
        task.watch_for_new_emails(session, &mut tx).await;
    };

    // TODO: use this
    let canceler = spawner
        .spawn(watch_future)
        .map_err(WatchError::SpawnError)?;

    Ok(rx)
}

/// Holds any data that watch tasks may be accessed during a `Watcher`'s stream.
struct WatchTask<'a, G> {
    session_generator: &'a G,
    stopped: AtomicBool,
}

impl<'a, G> WatchTask<'a, G>
where
    G: SessionGenerator + Sync + 'a,
{
    async fn watch_for_new_emails(
        &self,
        session: IMAPSession,
        sender: &mut Sender<SequenceNumber>,
    ) {
        // TODO: should this return errors beyond logging?
        let mut current_session = session;
        while !self.stopped.load(Ordering::Acquire) {
            let sink_res = self
                .watch_for_new_emails_until_fail(current_session, sender)
                .await;

            match sink_res {
                Ok(next_session) => {
                    current_session = next_session;
                    continue;
                }
                Err(err) => {
                    error!("failed to watch for new emails: {}", err);
                    let maybe_session = self.get_session_with_retry().await;
                    match maybe_session {
                        Some(new_session) => current_session = new_session,
                        // `get_session_with_retry` will only return None if the current watcher is stopped
                        None => break,
                    }
                }
            }
        }

        info!("stopped");
    }

    /// Watch for the arrival of a single email. While the `Watcher` is watching, the caller must relinquish ownership
    /// of the `Session`, but it will be returned to it upon succesful completion
    async fn watch_for_new_emails_until_fail(
        &self,
        session: IMAPSession,
        sender: &mut Sender<SequenceNumber>,
    ) -> Result<IMAPSession, IMAPError> {
        let mut session_cell = SessionCell::new(session);

        info!("beginning watch");
        while !self.stopped.load(Ordering::Acquire) {
            let idle_cell = session_cell.get_idler_cell();
            let idle_handle = idle_cell.prepare().await?;

            debug!("idling");
            let sequence_number = Self::idle_for_email(idle_handle).await?;
            debug!("got email");
            // TODO: handle these failures
            sender.send(sequence_number).await.expect("failed to send");
            debug!("sent");
        }
        debug!("done");

        match session_cell.into_state() {
            // If we've only initialized the cell, we don't need to actually do anything...
            SessionState::Initialized(session) => Ok(session),
            // If we've begun idling however, then we need to finish up and return the reclaimed session
            SessionState::IdleReady(idle_cell) => {
                let idle_handle = idle_cell.into_inner();
                let reclaimed_session = idle_handle.done().await?;
                Ok(reclaimed_session)
            }
        }
    }

    async fn idle_for_email(
        idle_handle: &mut Handle<IMAPTransportStream>,
    ) -> IMAPResult<SequenceNumber> {
        loop {
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

    async fn get_sequence_number_from_response<'b>(
        response: &Response<'b>,
    ) -> Option<SequenceNumber> {
        match response {
            Response::MailboxData(MailboxDatum::Exists(seq)) => Some(SequenceNumber(*seq)),
            _ => None,
        }
    }

    /// continues to try and get a new session. If the Watcher is currently stopped, returns None.
    async fn get_session_with_retry(&self) -> Option<IMAPSession> {
        let mut attempts = 1;
        while !self.stopped.load(Ordering::Acquire) {
            info!("generating a new session...");
            let new_session_res = self.get_session().await;
            match new_session_res {
                Ok(new_session) => return Some(new_session),
                Err(err) => {
                    error!("failed to get new session on attempt {}: {}", attempts, err);
                    attempts += 1;
                }
            }
        }

        None
    }

    async fn get_session(&self) -> IMAPResult<IMAPSession> {
        let mut session = self.session_generator.new_session().await?;
        session.examine("INBOX").await?;

        Ok(session)
    }
}
