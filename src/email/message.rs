use crate::{IMAPSession, SessionGenerator};
use std::iter;

use super::{inbox::SequenceNumber, MessageFetcher};
use async_imap::{error::Error as IMAPError, types::Fetch};
use async_trait::async_trait;
use futures::StreamExt;
use mailparse::MailParseError;
use thiserror::Error;

/// Indicates an error that occured during the fetch process
#[derive(Error, Debug)]
pub enum FetchError {
    #[error("failed to perform IMAP setup: {0}")]
    IMAPSetupFailed(IMAPError),
    #[error("perform message fetch: {0}")]
    MessageFetchFailed(IMAPError),
    // SequenceNumber does not implement Display
    #[error("the given sequence number ({0:?}) yielded no results")]
    MessageNotFound(SequenceNumber),
    #[error("failed to parse email: {0}")]
    ParseFailed(MailParseError),
    #[error("failed to tear down session: {0}")]
    TeardownFailed(IMAPError),
}

/// Fetches a message, and concatenates all sub-parts
pub struct ConcatenatedFetcher<G: SessionGenerator> {
    session_generator: G,
}

impl<G: SessionGenerator> ConcatenatedFetcher<G> {
    pub fn new(session_generator: G) -> Self {
        Self { session_generator }
    }
}

#[async_trait]
impl<G> MessageFetcher for ConcatenatedFetcher<G>
where
    G: SessionGenerator + Send + Sync + 'static,
{
    type Error = FetchError;

    async fn fetch_message(&self, sequence_number: SequenceNumber) -> Result<String, Self::Error> {
        // TODO: This could maybe be longer lived?
        let mut session = generate_fetchable_session(&self.session_generator)
            .await
            .map_err(FetchError::IMAPSetupFailed)?;

        let message = get_message_from_session(sequence_number, &mut session).await?;
        let concatted_message = {
            let message_part_iter = get_message_parts(&message).map_err(FetchError::ParseFailed)?;
            message_part_iter
                .collect::<Result<String, _>>()
                .map_err(FetchError::ParseFailed)?
        };

        // TODO: this could use RAII (has the same problem as `inbox` does)
        session.logout().await.map_err(FetchError::TeardownFailed)?;

        Ok(concatted_message)
    }
}

async fn get_message_from_session(
    sequence_number: SequenceNumber,
    session: &mut IMAPSession,
) -> Result<Fetch, FetchError> {
    let mut message_iter = session
        .fetch(format!("{}", sequence_number.value()), "RFC822")
        .await
        .map_err(FetchError::MessageFetchFailed)?;

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

fn get_message_parts<'a>(
    message: &'a Fetch,
) -> Result<Box<dyn Iterator<Item = Result<String, MailParseError>> + 'a>, MailParseError> {
    let maybe_body = message.body();
    if maybe_body.is_none() {
        // no body means no message parts!
        return Ok(Box::new(iter::empty()));
    }

    let parsed = mailparse::parse_mail(maybe_body.unwrap())?;
    let part_iter = parsed.subparts.into_iter().map(|x| x.get_body());

    Ok(Box::new(part_iter))
}

async fn best_effort_logout(session: &mut IMAPSession) {
    let logout_res = session.logout().await;
    if let Err(err) = logout_res {
        error!("failed to best-effort tear down session: {}", err);
    }
}
