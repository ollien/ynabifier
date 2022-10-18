//! The inbox module holds implementations to allow for the monitoring of a real inbox

use self::idle::{SessionCell, SessionState};
use super::{login::SessionGenerator, SequenceNumber};
use crate::{
    email::inbox::reconnect::Delay,
    task::{self, ResolveOrStop, Spawn, SpawnError, StopResolution},
    CloseableStream, IMAPSession, IMAPTransportStream,
};
use async_imap::{
    error::{Error as IMAPError, Result as IMAPResult},
    extensions::idle::Handle as IMAPIdleHandle,
    imap_proto::{MailboxDatum, Response},
};
use async_trait::async_trait;
use futures::{
    channel::mpsc::{self, UnboundedReceiver, UnboundedSender},
    future::Shared,
    FutureExt, SinkExt, Stream,
};
use std::{
    fmt::Debug,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use stop_token::{StopSource, StopToken};
use thiserror::Error;

mod idle;
mod reconnect;

/// An error that occurs during the setup process of a stream from [`watch`]
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

/// Watches the inbox for new emails, yielding their sequence numbers numbers.
///
/// # Errors
/// [`WatchError`] is returned if the stream could not be set up. Note that individual watch
/// failures are simply logged, rather than bubbled up.
pub async fn watch<S, G>(
    spawner: &S,
    session_generator: Arc<G>,
) -> Result<impl CloseableStream<Item = SequenceNumber>, WatchError>
where
    S: Spawn + Sync,
    S::Handle: Unpin,
    G: SessionGenerator + Sync + Send + 'static,
{
    let (tx, rx) = mpsc::unbounded();

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
    let task_handle = spawner
        .spawn(watch_future)
        .map_err(WatchError::SpawnError)?;

    let stream = SequenceNumberStream::new(rx, stop_src, task_handle);

    Ok(stream)
}

struct SequenceNumberStream<H: Unpin + Send> {
    output_stream: UnboundedReceiver<SequenceNumber>,
    stop_src: StopSource,
    handle: H,
}

impl<H: Unpin + Send> SequenceNumberStream<H> {
    pub fn new(
        output_stream: UnboundedReceiver<SequenceNumber>,
        stop_src: StopSource,
        handle: H,
    ) -> Self {
        Self {
            output_stream,
            stop_src,
            handle,
        }
    }
}

impl<H: Unpin + Send> Stream for SequenceNumberStream<H> {
    type Item = SequenceNumber;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let output_stream = &mut self.get_mut().output_stream;

        Pin::new(output_stream).poll_next(cx)
    }
}

#[async_trait]
impl<H: Unpin + task::Handle + Send> CloseableStream for SequenceNumberStream<H> {
    async fn close(self) {
        let Self {
            handle,
            stop_src,
            output_stream: _,
        } = self;
        drop(stop_src);
        // If we fail to join, the resources are as cleaned up as we're going to get them. Bubbling this up
        // is kinda tricky, and honestly, is only going to happen if the tasks got killed for some reason.
        if let Err(err) = handle.join().await {
            error!("Failed to join with join task: {err:?}");
        }
    }
}

/// Watch for new emails in the given [`IMAPSession`]. All sequence numbers will be sent to the given `sender`.
/// When the [`IMAPSession`] times out, a new session will be generate from the given `SessionGenerator`.
async fn watch_for_new_emails<G: SessionGenerator + Sync>(
    session: IMAPSession,
    session_generator: Arc<G>,
    stop: StopToken,
    sender: &mut UnboundedSender<SequenceNumber>,
) {
    // Should always be Some, we just need a container we can move out of.
    let mut current_session = Some(session);
    let shared_stop = stop.shared();
    info!("Beginning inbox watch...");
    loop {
        // This is always a bug. The only reason that we have to use an optional for current_session
        // is because it is moved out of every loop.
        let session = current_session
            .take()
            .expect("Lost track of the current session, can't continue");

        let session_res =
            watch_session_for_new_emails_until_fail(session, sender, shared_stop.clone()).await;

        match session_res {
            Ok(session) => {
                current_session = Some(session);
                // the StopToken will have a value in the future once a stop has ocurred, so we can just stop.
                if shared_stop.peek().is_some() {
                    break;
                }
            }
            Err(err) => {
                error!("Failed to watch for new emails: {err}");

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

/// Watch for the arrival of as many emails as possible before a failure occurs. While watching, the caller must
/// relinquish ownership of the `Session`, but it will be returned to it upon successful completion
async fn watch_session_for_new_emails_until_fail(
    session: IMAPSession,
    sender: &mut UnboundedSender<SequenceNumber>,
    stop: Shared<StopToken>,
) -> Result<IMAPSession, IdleWatchError> {
    let mut session_cell = SessionCell::new(session);
    watch_session_cell_for_new_emails_until_fail(&mut session_cell, sender, stop).await?;

    debug!("Done watching, reclaiming session...");
    match session_cell.into_state() {
        // If we've only initialized the cell, we don't need to actually do anything...
        SessionState::Initialized(session) => Ok(session),
        // If we've begun idling however, then we need to finish up and return the reclaimed session
        SessionState::IdleReady(idle_cell) => {
            let idle_handle = idle_cell.into_inner();
            debug!("Marking idle handle done...");
            idle_handle.done().await.map_err(IdleWatchError::IMAPError)
        }
    }
}

/// Watch for the arrival of as many emails as possible before a failure occurs.
async fn watch_session_cell_for_new_emails_until_fail(
    session_cell: &mut SessionCell<IMAPSession, IMAPIdleHandle<IMAPTransportStream>>,
    sender: &mut UnboundedSender<SequenceNumber>,
    stop: Shared<StopToken>,
) -> Result<(), IdleWatchError> {
    loop {
        let idler_cell = session_cell.get_idler_cell();
        let maybe_idle_handle = idler_cell
            .prepare()
            .resolve_or_stop(stop.clone())
            .await
            .transpose_result()
            .map_err(IdleWatchError::IMAPError)?;

        if maybe_idle_handle.was_stopped() {
            return Ok(());
        }

        let idle_handle = maybe_idle_handle.unwrap();
        debug!("Idling for new emails...");
        let maybe_sequence_number_res = idle_for_email(idle_handle)
            .resolve_or_stop(stop.clone())
            .await;

        match maybe_sequence_number_res {
            StopResolution::Stopped => return Ok(()),
            StopResolution::Resolved(Err(err)) => return Err(err),
            StopResolution::Resolved(Ok(sequence_number)) => {
                debug!("Idle received email with sequence number {sequence_number}, sending to stream...");
                let send_res = sender.send(sequence_number).await;
                if let Err(err) = send_res {
                    error!(
                        "Successfully fetched, but failed to dispatch email with sequence number {sequence_number}: {err}",
                    );
                } else {
                    debug!("Message sent");
                }
            }
        }
    }
}

/// Idle for a single email, returning that message's sequence number.
///
/// # Errors
/// This can fail in one of two ways:
/// - A general IMAP failure that occurred during the idle process
/// - A timeout/disconnect, which happens roughly every 29 minutes per RFC 2177.
async fn idle_for_email(
    idle_handle: &mut IMAPIdleHandle<IMAPTransportStream>,
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
                        debug!("Re-waiting for IDLE after getting non-EXISTS response from IDLE command: {response:?}");
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
async fn get_session_with_retry<G: SessionGenerator + Sync>(
    session_generator: &G,
    stop: Shared<StopToken>,
) -> Option<IMAPSession> {
    let mut attempts = 1;
    let mut delay = Delay::new();

    loop {
        debug!("Generating a new session...");
        let session_res = get_session(session_generator)
            .resolve_or_stop(stop.clone())
            .await
            .into_option()?;

        match session_res {
            Ok(new_session) => return Some(new_session),
            Err(err) => {
                error!("failed to get new session on attempt {attempts}: {err}");
                attempts += 1;
                debug!(
                    "Waiting for {} seconds before trying again...",
                    delay.peek_delay().as_secs()
                );

                delay
                    .backoff_wait()
                    .resolve_or_stop(stop.clone())
                    .await
                    .into_option()?;
            }
        }
    }
}

async fn get_session<G: SessionGenerator + Sync>(session_generator: &G) -> IMAPResult<IMAPSession> {
    let mut session = session_generator.new_session().await?;
    session.examine("INBOX").await?;

    Ok(session)
}
