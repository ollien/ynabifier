use itertools::Itertools;
use scraper::{Html, Selector};

use crate::Message;

use super::{Transaction, TransactionEmailParser};

pub struct EmailParser;

impl EmailParser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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

        if !super::has_correct_sender(&parsed_mail, "\"Citi Alerts\" <alerts@info6.citi.com>") {
            return Err(super::Error("message has incorrect sender".to_string()));
        }

        let html_part = super::find_html_subpart(&parsed_mail)?;
        let html_contents = html_part
            .get_body()
            .map_err(|err| super::Error(format!("failed to find html body: {:?}", err)))?;

        // If these are malformed it's programmer error
        let table_selector = Selector::parse("table[role=\"presentation\"]")
            .expect("failed to create selector to parse email");
        // The Citi emails contain many nested tables, and parsing them all would be cumbersome.
        // If, instead, we get all <tds> and iterate over tehir text contents, then we can extrapolate enough info
        let td_selector = &Selector::parse("td").expect("failed to create selector to parse email");

        let html_document = Html::parse_document(&html_contents);
        let table = html_document.select(&table_selector).next();
        let td_text_iter = table
            .unwrap()
            .select(td_selector)
            .flat_map(|element| element.text());

        let amount = find_amount_from_table_text(td_text_iter.clone())?;
        let payee = find_payee_from_table_text(td_text_iter)?;

        let trans = Transaction {
            amount: amount.to_string(),
            payee: payee.to_string(),
        };

        Ok(trans)
    }
}

fn find_payee_from_table_text<'a, I>(table_text_iter: I) -> Result<&'a str, super::Error>
where
    I: Iterator<Item = &'a str>,
{
    // The Citi emails have two parallel tables, with a heading on the left side and a value on the right.
    // In our list, this ends up as something like [..., "Merchant", "The Store", ...]
    // So we iterate in pairs until we find what we want.
    let maybe_merchant = table_text_iter
        .tuples()
        .find(|&(label, _)| label == "Merchant")
        .map(|(_, value)| value.trim());

    maybe_merchant.ok_or_else(|| super::Error("failed to find merchant in html body".to_string()))
}

fn find_amount_from_table_text<'a, I>(mut table_text_iter: I) -> Result<&'a str, super::Error>
where
    I: Iterator<Item = &'a str>,
{
    let maybe_amount = table_text_iter
        .find(|item| item.starts_with("Amount: "))
        .map(|item| item.trim_start_matches("Amount: "));

    maybe_amount
        .ok_or_else(|| super::Error("failed to find transaction amount in html body".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extracts_email_contents() {
        let raw_msg = include_bytes!("./testdata/citi/citi.eml");
        let msg = Message::new(raw_msg.to_vec());
        let parser = EmailParser::new();
        let transaction = parser
            .parse_transaction_email(&msg)
            .expect("failed to parse email");
        assert_eq!(
            Transaction {
                amount: "$3.28".to_string(),
                payee: "STOP & SHOP".to_string()
            },
            transaction
        );
    }

    #[test]
    fn test_email_mut_be_from_citi() {
        let raw_msg = include_bytes!("./testdata/citi/wrong_sender.eml");
        let msg = Message::new(raw_msg.to_vec());
        let parser = EmailParser::new();
        parser
            .parse_transaction_email(&msg)
            .expect_err("email should not have parsed");
    }
}
