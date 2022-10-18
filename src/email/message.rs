use crate::{task::ResolveOrStop, IMAPSession, Message, SessionGenerator};
use std::sync::Arc;

use super::{MessageFetcher, SequenceNumber};
use async_imap::{error::Error as IMAPError, types::Fetch};
use async_trait::async_trait;
use futures::StreamExt;
use stop_token::StopToken;
use thiserror::Error;

/// Indicates an error that occured during the fetch process
#[derive(Error, Debug)]
pub enum FetchError {
    #[error("failed to perform IMAP setup: {0}")]
    IMAPSetupFailed(IMAPError),
    #[error("perform message fetch: {0}")]
    MessageFetchFailed(IMAPError),
    #[error("the given sequence number ({0}) yielded no results")]
    MessageNotFound(SequenceNumber),
    #[error("a message was fetched for sequence number {0}, but it had no body")]
    NoBody(SequenceNumber),
    #[error("Fetcher received stop signal")]
    Stopped,
}

/// Fetches a message in its unadorned from the mail server
pub struct RawFetcher<G> {
    session_generator: Arc<G>,
}

impl<G> RawFetcher<G> {
    pub fn new(session_generator: Arc<G>) -> Self {
        Self { session_generator }
    }
}

#[async_trait]
impl<G> MessageFetcher for RawFetcher<G>
where
    // These bounds are necessary to the + Send bound `async_trait` provides on the return type.
    G: SessionGenerator + Send + Sync,
{
    type Error = FetchError;

    async fn fetch_message(
        &self,
        sequence_number: SequenceNumber,
        stop_token: &mut StopToken,
    ) -> Result<Message, Self::Error> {
        let session_gen = self.session_generator.as_ref();
        let mut session = generate_fetchable_session(session_gen)
            .resolve_or_stop(&mut *stop_token)
            .await
            .ok_or(FetchError::Stopped)?
            .map_err(FetchError::IMAPSetupFailed)?;

        let body_res = {
            let msg_fetch_result = get_message_from_session(sequence_number, &mut session)
                .resolve_or_stop(&mut *stop_token)
                .await;

            match msg_fetch_result {
                None => Err(FetchError::Stopped),
                Some(Err(err)) => Err(err),
                Some(Ok(message)) => message
                    .body()
                    .ok_or(FetchError::NoBody(sequence_number))
                    .map(<[u8]>::to_vec),
            }
        };

        best_effort_logout(&mut session).await;
        body_res.map(|body| Message { raw: body })
    }
}

async fn get_message_from_session(
    sequence_number: SequenceNumber,
    session: &mut IMAPSession,
) -> Result<Fetch, FetchError> {
    let message_iter_res = session
        .fetch(format!("{}", sequence_number.value()), "RFC822")
        .await;

    let mut message_iter = message_iter_res.map_err(FetchError::MessageFetchFailed)?;

    message_iter
        .next()
        .await
        .ok_or(FetchError::MessageNotFound(sequence_number))?
        .map_err(FetchError::MessageFetchFailed)
}

async fn generate_fetchable_session<G: SessionGenerator>(
    session_generator: &G,
) -> Result<IMAPSession, IMAPError> {
    let mut session = session_generator.new_session().await?;
    let examine_res = session.examine("INBOX").await;

    match examine_res {
        Ok(_) => Ok(session),
        Err(err) => {
            best_effort_logout(&mut session).await;
            Err(err)
        }
    }
}

async fn best_effort_logout(session: &mut IMAPSession) {
    debug!("Logging out after fetch");

    let logout_res = session.logout().await;
    if let Err(err) = logout_res {
        error!("Failed to best-effort tear down session: {err}");
    }
}
