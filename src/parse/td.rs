use regex::Regex;
use scraper::{Html, Selector};

use crate::Message;

use super::{NaiveDate, Transaction, TransactionEmailParser};

pub struct EmailParser;

// UndatedTransaction is similar to a transaction, but it does not have a date.
struct UndatedTransaction {
    payee: String,
    amount: String,
}

impl EmailParser {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for EmailParser {
    fn default() -> Self {
        Self
    }
}

impl TransactionEmailParser for EmailParser {
    fn parse_transaction_email(&self, msg: &Message) -> Result<Transaction, super::Error> {
        let parsed_mail = msg
            .parsed()
            .map_err(|err| super::Error(format!("message could not be parsed {:?}", err)))?;

        if !super::has_correct_sender(&parsed_mail, "alerts@td.com") {
            return Err(super::Error("message has incorrect sender".to_string()));
        }

        let html_part = super::find_html_subpart(&parsed_mail)?;
        let html_contents = html_part
            .get_body()
            .map_err(|err| super::Error(format!("failed to find html body: {:?}", err)))?;

        let html_document = Html::parse_document(&html_contents);
        let date = extract_date(&html_document)?;
        let undated_info = extract_undated_info(&html_document)?;

        let trans = Transaction {
            payee: undated_info.payee,
            amount: undated_info.amount,
            date,
        };

        Ok(trans)
    }
}

fn extract_date(email_html: &Html) -> Result<NaiveDate, super::Error> {
    // If these are malformed it's programmer error
    let td_selector = Selector::parse("td").expect("failed to create selector to parse email");
    let date_regexp = Regex::new(r"\d{4}-\d{2}-\d{2}").expect("failed to create regex for date");

    // Somewhere in the email, there will be a div with the text "A purchase was made on yyyy-mm-dd, which is
    // greater than...". We can find this line and parse it out; there isn't enough specificity in the email
    // to select it manually.
    let date_line = email_html
        .select(&td_selector)
        .flat_map(|div| div.text())
        .find(|item| item.starts_with("A purchase was made on "))
        .ok_or_else(|| {
            super::Error("failed to find element containing date in email".to_string())
        })?;

    // We cannot pass any kind of "ignore text after" to `NaiveDate::parse_from_str`, and while we could mess around
    // with splitting and such, it will be less brittle to just use a regular expression and then re-parse.
    let date_text_match = date_regexp
        .find(date_line)
        .ok_or_else(|| super::Error("failed to extract date from email text".to_string()))?;
    let date_text = &date_line[date_text_match.start()..date_text_match.end()];

    NaiveDate::parse_from_str(date_text, "%F")
        .map_err(|err| super::Error(format!("failed to parse date from email: {:?}", err)))
}

fn extract_undated_info(email_html: &Html) -> Result<UndatedTransaction, super::Error> {
    // If these are mmalformed it's a programmer rror
    let ul_selector = Selector::parse("ul").expect("failed to create selector to parse email");
    let li_selector = Selector::parse("ul > li").expect("failed to create selector to parse email");

    let lis = email_html
        .select(&ul_selector)
        .flat_map(|ul| ul.select(&li_selector))
        .flat_map(|li| li.text())
        .collect::<Vec<_>>();

    if lis.len() != 2 {
        return Err(super::Error(
            "email did not follow expected HTML structure; could not find list of details"
                .to_string(),
        ));
    }

    let undated_trans = UndatedTransaction {
        payee: lis[0].trim_start_matches("Merchant Name: ").to_string(),
        amount: format!("${}", lis[1].trim_start_matches("Amount: ")),
    };

    Ok(undated_trans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extracts_email_contents() {
        let raw_msg = include_bytes!("./testdata/td/td.eml");
        let msg = Message::new(raw_msg.to_vec());
        let parser = EmailParser::new();
        let transaction = parser
            .parse_transaction_email(&msg)
            .expect("failed to parse email");
        assert_eq!(
            Transaction {
                amount: "$0.60".to_string(),
                payee: "PAYPAL *MARKETPLACE".to_string(),
                date: NaiveDate::from_ymd(2022, 9, 17)
            },
            transaction
        );
    }

    #[test]
    fn test_email_mut_be_from_td() {
        let raw_msg = include_bytes!("./testdata/td/wrong_sender.eml");
        let msg = Message::new(raw_msg.to_vec());
        let parser = EmailParser::new();
        parser
            .parse_transaction_email(&msg)
            .expect_err("email should not have parsed");
    }
}
