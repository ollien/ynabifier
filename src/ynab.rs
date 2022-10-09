//! Provides a client to submit transactions to YNAB

use regex::Regex;
use reqwest::{Response, Url};
use serde::Serialize;
use thiserror::Error;

use crate::parse::Transaction;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Failed to parse amount '{0}': {1}")]
    AmountParseFailure(String, String),
    #[error("Failed to build URL: {0}")]
    URLBuildFailed(url::ParseError),
    #[error("Failed to build YNAB request: {0}")]
    RequestBuildFailed(reqwest::Error),
    #[error("Failed to send YNAB request: {0}")]
    RequestFailure(reqwest::Error),
}

#[derive(Debug, Serialize)]
struct YNABTransactionRequestData<'a> {
    transaction: YNABTransaction<'a>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(PartialEq, Eq))]
struct YNABTransaction<'a> {
    account_id: &'a str,
    payee_name: &'a str,
    date: String,
    amount: i32,
}

impl<'a> YNABTransaction<'a> {
    fn from_transaction_for_account(
        transaction: &'a Transaction,
        account_id: &'a str,
    ) -> Result<Self, Error> {
        let ynab_amount = convert_amount_to_ynab_form(transaction.amount())?;
        let ynab_transaction = YNABTransaction {
            account_id,
            payee_name: transaction.payee(),
            date: transaction.date().format("%F").to_string(),
            amount: ynab_amount,
        };

        Ok(ynab_transaction)
    }
}

/// Client is a YNAB client that authenticates with a personal access token.
pub struct Client {
    access_token: String,
    http_client: reqwest::Client,
}

impl Client {
    ///  Make a new `Client` that will authenticate with the given personal access token.
    #[must_use]
    pub fn new(access_token: String) -> Self {
        Self {
            access_token,
            http_client: reqwest::Client::new(),
        }
    }

    /// Submit a transaction to YNAB for the given account ID.
    ///
    /// # Errors
    /// An Error is returned under the following circumstances
    /// - The transaction cannot be serialized to a response properly ([`Error::AmountParseFailure`]).
    /// - A request cannot be successfully built ([`Error::URLBuildFailure`] or [`Error::RequestBuildFailure`])
    /// - There is a general HTTP failure ([`Error::RequestFailure`])
    pub async fn submit_transaction(
        &self,
        transaction: &Transaction,
        budget_id: &str,
        account_id: &str,
    ) -> Result<(), Error> {
        let ynab_transaction =
            YNABTransaction::from_transaction_for_account(transaction, account_id)?;

        let request_data = YNABTransactionRequestData {
            transaction: ynab_transaction,
        };

        let transaction_url = Self::build_transaction_url(budget_id)?;
        debug!(
            "Sending YNAB request to {} for budget id {} and account id {}",
            transaction_url, budget_id, account_id
        );

        let request = self
            .http_client
            .post(transaction_url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .json(&request_data)
            .build()
            .map_err(Error::RequestBuildFailed)?;

        let response = self
            .http_client
            .execute(request)
            .await
            .map_err(Error::RequestFailure)?;

        Self::log_and_error_for_status(response)
            .await
            .map_err(Error::RequestFailure)?;

        info!(
            "Submitted transaction for {} to {} to YNAB",
            transaction.amount(),
            transaction.payee()
        );

        Ok(())
    }

    fn build_transaction_url(budget_id: &str) -> Result<Url, Error> {
        const BASE_URL: &str = "https://api.youneedabudget.com/v1/budgets/";
        // const BASE_URL: &str = "http://localhost:8080/v1/budgets/";
        Url::parse(BASE_URL)
            .expect("parsing ynab base url failed")
            .join(format!("{}/", budget_id).as_ref())
            .and_then(|url| url.join("transactions"))
            .map_err(Error::URLBuildFailed)
    }

    async fn log_and_error_for_status(res: Response) -> Result<(), reqwest::Error> {
        let status_code = res.status();
        match res.error_for_status_ref() {
            Ok(_) => Ok(()),
            Err(err) => {
                let body = res.text().await?;
                error!("Got status code {}; response: {}", status_code, body);
                Err(err)
            }
        }
    }
}

fn convert_amount_to_ynab_form(amount: &str) -> Result<i32, Error> {
    let pattern = Regex::new(r"^\$?(\d+)\.(\d\d)$").unwrap();
    let captures = pattern.captures(amount).ok_or_else(|| {
        Error::AmountParseFailure(amount.to_string(), "Amount is malformed".to_string())
    })?;

    let get_i32_capture_group = |n| captures.get(n).unwrap().as_str().parse::<i32>();
    let dollars = get_i32_capture_group(1).map_err(|err| {
        Error::AmountParseFailure(
            amount.to_string(),
            format!("could not parse dollars - {:?}", err),
        )
    })?;
    let cents = get_i32_capture_group(2).map_err(|err| {
        Error::AmountParseFailure(
            amount.to_string(),
            format!("could not parse cents - {:?}", err),
        )
    })?;

    let converted = dollars
        .checked_mul(100)
        .and_then(|dollar_cents| dollar_cents.checked_add(cents))
        .and_then(|total_cents| total_cents.checked_mul(10))
        .and_then(i32::checked_neg)
        .ok_or_else(|| {
            Error::AmountParseFailure(
                amount.to_string(),
                "Overflow occurred during parsing of the transaction amount".to_string(),
            )
        })?;

    Ok(converted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use test_case::test_case;

    #[test_case("$4.50", -4500)]
    #[test_case("$1.00", -1000)]
    #[test_case("4.50", -4500; "no dollar sign")]
    fn test_converts_dollar_amount_to_ynab_form(raw: &str, expected: i32) {
        let converted = convert_amount_to_ynab_form(raw).expect("failed to convert");
        assert_eq!(converted, expected);
    }

    #[test_case("some garbage")]
    #[test_case("$100000000.50"; "overflows during conversion")]
    fn test_convert_amount_failure(raw: &str) {
        convert_amount_to_ynab_form(raw).expect_err("should not have succeeded");
    }

    #[test]
    fn test_convert_transaction_for_request() {
        let transaction = Transaction::new(
            "Ferris, LLC".to_string(),
            "$10.00".to_string(),
            NaiveDate::from_ymd(2022, 10, 8),
        );
        let account_id = "b1a7701d-1eee-4cf1-b101-5011d1f1ab1e";
        let ynab_transaction =
            YNABTransaction::from_transaction_for_account(&transaction, account_id)
                .expect("failed to create ynab transaction");

        let expected = YNABTransaction {
            payee_name: "Ferris, LLC",
            account_id,
            date: "2022-10-08".to_string(),
            amount: -10000,
        };

        assert_eq!(ynab_transaction, expected);
    }
}
