//! The inbox module holds implementations to allow for the monitoring of a real inbox

use self::idle::{Idler, IdlerCell, SessionCell, SessionState};
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
use futures::{channel::mpsc, future::Shared, select, FutureExt, SinkExt, Stream};
use std::{
    fmt::Debug,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use stop_token::{StopSource, StopToken};
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

#[derive(Error, Debug)]
enum IdleWatchError {
    #[error("IMAP failure while idling: {0}")]
    IMAPError(IMAPError),
    #[error("Disconnected from IMAP server")]
    Disconnected,
}

pub async fn watch_for_new_messages<S, G>(
    spawner: &S,
    session_generator: Arc<G>,
) -> Result<impl Stream<Item = SequenceNumber>, WatchError>
where
    S: Spawn + Sync,
    S::Handle: Unpin,
    G: SessionGenerator + Sync + Send + 'static,
{
    let (tx, rx) = mpsc::channel(CHANNEL_SIZE);

    let mut session = session_generator
        .new_session()
        .await
        .map_err(WatchError::IMAPSetupError)?;

    session
        .examine("INBOX")
        .await
        .map_err(WatchError::IMAPSetupError)?;

    let stop_src = StopSource::new();
    let stop_token = stop_src.token();
    let watch_future = async move {
        let mut tx = tx;
        watch_for_new_emails(session, session_generator, stop_token, &mut tx).await;
    };

    // NOTE: We deliberately *DO NOT* want a canceler here. We want the ability to clean up our sessions, so we use
    // a StopToken. If we must stop, the executor will kill us from the top level.
    spawner
        .spawn(watch_future)
        .map_err(WatchError::SpawnError)?;

    let stream = SequenceNumberStream::new(rx, stop_src);

    Ok(stream)
}

struct SequenceNumberStream {
    output_stream: mpsc::Receiver<SequenceNumber>,
    _stop_src: StopSource,
}

impl SequenceNumberStream {
    pub fn new(output_stream: mpsc::Receiver<SequenceNumber>, stop_src: StopSource) -> Self {
        Self {
            output_stream,
            _stop_src: stop_src,
        }
    }
}

impl Stream for SequenceNumberStream {
    type Item = SequenceNumber;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let output_stream = &mut self.get_mut().output_stream;

        Pin::new(output_stream).poll_next(cx)
    }
}

async fn watch_for_new_emails<G: SessionGenerator>(
    session: IMAPSession,
    session_generator: Arc<G>,
    stop: StopToken,
    sender: &mut mpsc::Sender<SequenceNumber>,
) {
    // Should always be Some, we just need a container we can move out of.
    let mut current_session = Some(session);
    let shared_stop = stop.shared();
    loop {
        // This is always a bug. The only reason that we have to use an optional for current_session
        // is because it is moved out of every loop.
        let session = current_session
            .take()
            .expect("Lost track of the current session, can't continue");

        let session_res =
            watch_for_new_emails_until_fail(session, sender, shared_stop.clone()).await;

        match session_res {
            Ok(session) => {
                current_session = Some(session);
                // the StopToken will have a value in the future once a stop has ocurred, so we can just stop.
                if shared_stop.peek().is_some() {
                    break;
                }
            }
            Err(err) => {
                error!("failed to watch for new emails: {}", err);
                let maybe_session =
                    get_session_with_retry(session_generator.as_ref(), shared_stop.clone()).await;
                match maybe_session {
                    Some(new_session) => current_session = Some(new_session),
                    // `get_session_with_retry` will only return None if the current watcher is stopped
                    None => break,
                }
            }
        }
    }

    if let Some(mut session) = current_session {
        debug!("Logging session out before shutting down");
        let logout_res = session.logout().await;
        if let Err(err) = logout_res {
            error!("Failed to log session out: {}", err);
        }
    }

    debug!("Watch finished...");
}

/// Watch for the arrival of a single email. While the `Watcher` is watching, the caller must relinquish ownership
/// of the `Session`, but it will be returned to it upon succesful completion
async fn watch_for_new_emails_until_fail(
    session: IMAPSession,
    sender: &mut mpsc::Sender<SequenceNumber>,
    stop: Shared<StopToken>,
) -> Result<IMAPSession, IdleWatchError> {
    let mut session_cell = SessionCell::new(session);

    loop {
        let idle_cell = session_cell.get_idler_cell();
        let maybe_idle_handle = prepare_idler_if_unstopped(idle_cell, stop.clone())
            .await
            .map_err(IdleWatchError::IMAPError)?;

        if maybe_idle_handle.is_none() {
            break;
        }

        let idle_handle = maybe_idle_handle.unwrap();
        debug!("Idling for new emails...");
        select! {
            _ = stop.clone().fuse()  => break,
            sequence_number_res = idle_for_email(idle_handle).fuse() => {
                let sequence_number = sequence_number_res?;
                debug!("Idle received email with sequence number {}, sending to stream...", sequence_number);
                let send_res = sender.send(sequence_number).await;
                if let Err(err) = send_res {
                    error!(
                        "Successfully fetched, but failed to dispatch email with sequence number {}: {}",
                        sequence_number, err
                    );
                } else {
                    debug!("Message sent");
                }
            }
        }
    }

    debug!("Done watching, reclaiming session...");

    match session_cell.into_state() {
        // If we've only initialized the cell, we don't need to actually do anything...
        SessionState::Initialized(session) => Ok(session),
        // If we've begun idling however, then we need to finish up and return the reclaimed session
        SessionState::IdleReady(idle_cell) => {
            let idle_handle = idle_cell.into_inner();
            debug!("marking done...");
            idle_handle.done().await.map_err(IdleWatchError::IMAPError)
        }
    }
}

async fn prepare_idler_if_unstopped<I: Idler + Unpin>(
    idler_cell: &mut IdlerCell<I>,
    stop: Shared<StopToken>,
) -> IMAPResult<Option<&mut I>> {
    select! {
        // TODO: an Option is a bit semantically weird but it works for this purpose...
        _ = stop.fuse() => Ok(None),
        idle_handle_res = idler_cell.prepare().fuse() => idle_handle_res.map(Some)
    }
}

async fn idle_for_email(
    idle_handle: &mut Handle<IMAPTransportStream>,
) -> Result<SequenceNumber, IdleWatchError> {
    loop {
        let idle_res = idle::wait_for_data(idle_handle).await;
        match idle_res {
            Ok(data) => {
                let response = data.response();
                let maybe_sequence_number = get_sequence_number_from_response(response);

                match maybe_sequence_number {
                    Some(sequence_number) => return Ok(sequence_number),
                    None => {
                        debug!("re-waiting for IDLE after getting non-EXISTS response from IDLE command: {:?}", response);
                    }
                }
            }
            Err(idle::Error::AsyncIMAPError(err)) => return Err(IdleWatchError::IMAPError(err)),
            Err(idle::Error::NeedReconnect) => return Err(IdleWatchError::Disconnected),
        }
    }
}

fn get_sequence_number_from_response(response: &Response) -> Option<SequenceNumber> {
    match response {
        Response::MailboxData(MailboxDatum::Exists(seq)) => Some(SequenceNumber(*seq)),
        _ => None,
    }
}

/// continues to try and get a new session. If the Watcher is currently stopped, returns None.
async fn get_session_with_retry<G: SessionGenerator>(
    session_generator: &G,
    stop: Shared<StopToken>,
) -> Option<IMAPSession> {
    let mut attempts = 1;
    let mut fused_stop = stop.fuse();
    loop {
        info!("generating a new session...");
        let session_future = get_session(session_generator);
        select! {
            _ = fused_stop => break,
            new_session_res = session_future.fuse() => {
                match new_session_res {
                    Ok(new_session) => return Some(new_session),
                    Err(err) => {
                        error!("failed to get new session on attempt {}: {}", attempts, err);
                        attempts += 1;
                    }
                }
            },
        }
    }

    None
}

async fn get_session<G: SessionGenerator>(session_generator: &G) -> IMAPResult<IMAPSession> {
    let mut session = session_generator.new_session().await?;
    session.examine("INBOX").await?;

    Ok(session)
}
