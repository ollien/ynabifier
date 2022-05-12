use std::error::Error;

use crate::inbox::SequenceNumber;
use crate::IMAPSession;
use futures::StreamExt;

/// `fetch_email` will consume the current session and put it into an `Idle` state, until a new email is received
///
/// # Errors
/// If, for any reason, the email fails to be fetched, one of `async_imap` error's will be returned.
#[allow(clippy::module_name_repetitions)]
pub async fn fetch_email(
    session: &mut IMAPSession,
    sequence_number: SequenceNumber,
) -> Result<String, Box<dyn Error>> {
    let message = session
        .fetch(format!("{}", sequence_number.value()), "RFC822")
        .await?
        // god please don't do this just to get a single message
        .next()
        .await
        .unwrap()?;

    let parsed = mailparse::parse_mail(message.body().unwrap())?;
    let subparts = parsed.subparts;
    // TODO: Don't just concat these parts...
    let res = subparts
        .into_iter()
        .map(|x| x.get_body())
        .collect::<Result<String, _>>()?;

    Ok(res)
}
