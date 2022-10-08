//! Provides utilities to parse emails for transactions.

pub use chrono::naive::NaiveDate;
pub use citi::EmailParser as CitiEmailParser;
pub use td::EmailParser as TDEmailParser;

use crate::email::Message;
use mailparse::{MailHeader, MailHeaderMap, ParsedMail};
use std::collections::VecDeque;
use thiserror::Error;

mod citi;
mod td;

#[derive(Error, Debug)]
#[error("failed to parse email: {0}")]
pub struct Error(String);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Transaction {
    payee: String,
    amount: String,
    date: NaiveDate,
}

/// A `TransactionEmailParser` will parse a given email for transaction details
pub trait TransactionEmailParser {
    /// Parses a message in hopes of finding a transaction.
    ///
    /// # Errors
    /// If no transaction data is found, an Error is returned. This error will likely not be something
    /// that is matchable, as it is freeform based on the specific parser.
    fn parse_transaction_email(&self, msg: &Message) -> Result<Transaction, Error>;
}

impl Transaction {
    #[must_use]
    pub fn payee(&self) -> &str {
        &self.payee
    }

    #[must_use]
    pub fn amount(&self) -> &str {
        &self.amount
    }

    #[must_use]
    pub fn date(&self) -> &NaiveDate {
        &self.date
    }
}

/// perform a BFS to find a text/html element; this is likely where the actual fun parts of the email are.
/// This might be a bit brittle if any banks ever add more than one of these, but it works with their current
/// emails (Making an iterator for this is actually a bit challenging without GATs, so I'm punting).
fn find_html_subpart<'a, 'inner>(
    parsed_mail: &'a ParsedMail<'inner>,
) -> Result<&'a ParsedMail<'inner>, Error> {
    let mut to_visit = [parsed_mail].into_iter().collect::<VecDeque<_>>();
    while let Some(visiting) = to_visit.pop_front() {
        if has_matching_header(visiting, "Content-Type", |header| {
            header.get_value().starts_with("text/html;")
        }) {
            return Ok(visiting);
        }

        visiting
            .subparts
            .iter()
            .for_each(|subpart| to_visit.push_back(subpart));
    }

    Err(Error(
        "no subpart matched the appropriate content type".to_string(),
    ))
}

fn has_matching_header<'a, F: FnOnce(&'a MailHeader<'_>) -> bool>(
    parsed_mail: &'a ParsedMail<'_>,
    header_name: &str,
    matcher: F,
) -> bool {
    if let Some(header) = parsed_mail.headers.get_first_header(header_name) {
        if matcher(header) {
            return true;
        }
    }

    false
}

fn has_correct_sender(parsed_mail: &ParsedMail<'_>, expected_sender: &str) -> bool {
    has_matching_header(parsed_mail, "From", |header| {
        header.get_value() == expected_sender
    })
}
