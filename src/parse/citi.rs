use itertools::Itertools;
use scraper::{Html, Selector};

use crate::Message;

use super::{NaiveDate, Transaction, TransactionEmailParser};

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
        let payee = find_payee_from_table_text(td_text_iter.clone())?;
        let date = find_date_from_table_text(td_text_iter)?;

        let trans = Transaction {
            amount: amount.to_string(),
            payee: payee.to_string(),
            date,
        };

        Ok(trans)
    }
}

fn find_payee_from_table_text<'a, I>(table_text_iter: I) -> Result<&'a str, super::Error>
where
    I: Iterator<Item = &'a str>,
{
    find_table_value_with_label(table_text_iter, |label| label == "Merchant")
        .ok_or_else(|| super::Error("failed to find merchant in html body".to_string()))
}

fn find_date_from_table_text<'a, I>(table_text_iter: I) -> Result<NaiveDate, super::Error>
where
    I: Iterator<Item = &'a str>,
{
    let date_text = find_table_value_with_label(table_text_iter, |label| label == "Date")
        .ok_or_else(|| super::Error("failed to find date in html body".to_string()))?;

    NaiveDate::parse_from_str(dbg!(date_text), "%m/%d/%Y")
        .map_err(|err| super::Error(format!("failed to parse date from html body: {:?}", err)))
}

fn find_table_value_with_label<'a, I, F>(table_text_iter: I, mut find_func: F) -> Option<&'a str>
where
    I: Iterator<Item = &'a str>,
    F: FnMut(&'a str) -> bool,
{
    // The Citi emails have two parallel tables, with a heading on the left side and a value on the right.
    // In our list, this ends up as something like [..., "Merchant", "The Store", ...]
    // So we iterate in pairs until we find what we want.
    table_text_iter
        .tuples()
        .find(|&(label, _)| find_func(label))
        .map(|(_, value)| value.trim())
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
                payee: "STOP & SHOP".to_string(),
                date: NaiveDate::from_ymd(2022, 9, 13),
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
