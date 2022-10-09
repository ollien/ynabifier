use log::LevelFilter;
use serde::Deserialize;

use crate::parse::{CitiEmailParser, TDEmailParser, TransactionEmailParser};

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq, Eq))]
enum LogLevel {
    #[serde(rename = "debug")]
    Debug,
    #[serde(rename = "info")]
    Info,
    #[serde(rename = "warning")]
    Warning,
    #[serde(rename = "error")]
    Error,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq, Eq))]
enum Parser {
    #[serde(rename = "td")]
    TD,
    #[serde(rename = "citi")]
    Citi,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct Config {
    #[serde(default = "defaults::log_level")]
    log_level: LogLevel,
    imap: IMAP,
    ynab: YNAB,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct IMAP {
    domain: String,
    #[serde(default = "defaults::port")]
    port: u16,
    username: String,
    password: String,
    // unfortunately, we can't configure tls because async_imap requires it
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct YNAB {
    personal_access_token: String,
    budget_id: String,
    accounts: Vec<YNABAccount>,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub struct YNABAccount {
    #[serde(rename = "account_id")]
    id: String,
    parser: Parser,
}

impl Config {
    #[must_use]
    pub fn log_level(&self) -> LevelFilter {
        match &self.log_level {
            LogLevel::Debug => LevelFilter::Debug,
            LogLevel::Info => LevelFilter::Info,
            LogLevel::Warning => LevelFilter::Warn,
            LogLevel::Error => LevelFilter::Error,
        }
    }

    #[must_use]
    pub fn imap(&self) -> &IMAP {
        &self.imap
    }

    #[must_use]
    pub fn ynab(&self) -> &YNAB {
        &self.ynab
    }
}

impl IMAP {
    #[must_use]
    pub fn domain(&self) -> &str {
        &self.domain
    }

    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }
}

impl YNAB {
    #[must_use]
    pub fn personal_access_token(&self) -> &str {
        &self.personal_access_token
    }

    #[must_use]
    pub fn budeget_id(&self) -> &str {
        &self.budget_id
    }

    #[must_use]
    pub fn accounts(&self) -> &[YNABAccount] {
        &self.accounts
    }
}

impl YNABAccount {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub fn parser(&self) -> Box<dyn TransactionEmailParser> {
        match self.parser {
            Parser::Citi => Box::new(CitiEmailParser::new()),
            Parser::TD => Box::new(TDEmailParser::new()),
        }
    }

    #[must_use]
    pub fn parser_name(&self) -> &str {
        match self.parser {
            Parser::Citi => "Citi",
            Parser::TD => "TD",
        }
    }
}

mod defaults {
    pub(super) fn log_level() -> super::LogLevel {
        super::LogLevel::Info
    }

    pub(super) fn port() -> u16 {
        993
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_config() {
        let data = textwrap::dedent(
            r#"
            log_level: debug
            imap:
                domain: imap.google.com
                port: 993
                username: nick@ollien.com
                password: hunter2
            ynab:
                personal_access_token: 7hi5_15_5up3r_s3cr3t!!
                budget_id: c11ff1e7-de51-4b1e-bb15-c01ac0b105e5
                accounts:
                    - account_id: 48b16c37-8fdc-4072-977c-e6084c924032
                      parser: citi
        "#,
        );

        let expected = Config {
            log_level: LogLevel::Debug,
            ynab: YNAB {
                personal_access_token: "7hi5_15_5up3r_s3cr3t!!".to_string(),
                budget_id: "c11ff1e7-de51-4b1e-bb15-c01ac0b105e5".to_string(),
                accounts: vec![YNABAccount {
                    id: "48b16c37-8fdc-4072-977c-e6084c924032".to_string(),
                    parser: Parser::Citi,
                }],
            },
            imap: IMAP {
                domain: "imap.google.com".to_string(),
                port: 993,
                username: "nick@ollien.com".to_string(),
                password: "hunter2".to_string(),
            },
        };

        let parsed_config = serde_yaml::from_str::<Config>(&data).expect("failed to parse config");

        assert_eq!(expected, parsed_config);
    }
}
