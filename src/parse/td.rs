use scraper::{Html, Selector};

use crate::Message;

use super::{Transaction, TransactionEmailParser};

pub struct EmailParser;

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
        // If this is malformed it's programmer error
        let ul_selector = Selector::parse("ul").expect("failed to create selector to parse email");
        let li_selector =
            Selector::parse("ul > li").expect("failed to create selector to parse email");
        let html_document = Html::parse_document(&html_contents);
        let lis = html_document
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
        let trans = Transaction {
            payee: lis[0].trim_start_matches("Merchant Name: ").to_string(),
            amount: format!("${}", lis[1].trim_start_matches("Amount: ")),
        };

        Ok(trans)
    }
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
                payee: "PAYPAL *MARKETPLACE".to_string()
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
