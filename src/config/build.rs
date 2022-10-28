use super::{Config, Parser, YNABAccount, IMAP, YNAB};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("An IMAP configuration is already present")]
    AlreadyHaveIMAP,
    #[error("A YNAB configuration is already present")]
    AlreadyHaveYNAB,
    #[error("No IMAP configuration was given")]
    NoIMAP,
    #[error("No YNAB configuration was given")]
    NoYNAB,
    #[error("No YNAB accounts were given")]
    NoAccounts,
}

#[derive(Default, Debug)]
pub struct Builder {
    imap: Option<IMAP>,
    ynab: Option<YNAB>,
    ynab_accounts: Vec<YNABAccount>,
}

impl Builder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an IMAP configuration to the configuration. Uses the default port of 993. To use a
    /// different port, see [`Self::imap_port_port`].
    ///
    /// # Errors
    /// Returns [`Error::AlreadyHaveIMAP`] if IMAP has already been set on this structure.
    pub fn imap<S: Into<String>>(self, domain: S, username: S, password: S) -> Result<Self, Error> {
        self.imap_with_port(domain, username, password, super::defaults::port())
    }

    /// Add an IMAP configuration to the configuration.
    ///
    /// # Errors
    /// Returns [`Error::AlreadyHaveIMAP`] if IMAP has already been set.
    pub fn imap_with_port<S: Into<String>>(
        self,
        domain: S,
        username: S,
        password: S,
        port: u16,
    ) -> Result<Self, Error> {
        if self.imap.is_some() {
            return Err(Error::AlreadyHaveIMAP);
        }

        let imap_config = IMAP {
            domain: domain.into(),
            username: username.into(),
            password: password.into(),
            port,
        };

        Ok(Self {
            imap: Some(imap_config),
            ..self
        })
    }

    /// Add an You Need A Budget authentication and budget information to the configuration
    ///
    /// # Errors
    /// Returns [`Error::AlreadyHaveYNAB`] if YNAB configurations has already been set.
    pub fn ynab<S: Into<String>>(
        self,
        personal_access_token: S,
        budget_id: S,
    ) -> Result<Self, Error> {
        if self.ynab.is_some() {
            return Err(Error::AlreadyHaveYNAB);
        }

        let ynab_config = YNAB {
            personal_access_token: personal_access_token.into(),
            budget_id: budget_id.into(),
            accounts: Vec::new(),
        };

        Ok(Self {
            ynab: Some(ynab_config),
            ..self
        })
    }

    /// Add a YNAB account to the configuration.
    #[must_use]
    pub fn ynab_account<S: Into<String>>(mut self, account_id: S, parser: Parser) -> Self {
        let account = YNABAccount {
            id: account_id.into(),
            parser,
        };

        self.ynab_accounts.push(account);

        self
    }

    /// Build the final configuration
    ///
    /// # Errors
    /// Returns [`Error::NoIMAP`] if an IMAP configuration was not specified
    #[allow(clippy::missing_panics_doc)]
    pub fn build(self) -> Result<Config, Error> {
        self.validate()?;

        let ynab_config = YNAB {
            accounts: self.ynab_accounts,
            ..self.ynab.unwrap()
        };

        Ok(Config {
            imap: self.imap.unwrap(),
            ynab: ynab_config,
            log_level: super::defaults::log_level(),
        })
    }

    fn validate(&self) -> Result<(), Error> {
        if self.imap.is_none() {
            return Err(Error::NoIMAP);
        } else if self.ynab.is_none() {
            return Err(Error::NoYNAB);
        } else if self.ynab_accounts.is_empty() {
            return Err(Error::NoAccounts);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::config::LogLevel;

    use super::*;

    #[test]
    fn test_builds_configuration_with_default_imap_port() {
        let config = (|| -> Result<Config, Error> {
            Builder::new()
                .imap("imap.gmail.com", "nick@ollien.com", "hunter2")?
                .ynab(
                    "super-secret-dont-leak-this",
                    "11f70ff5-ce55-4ada-abed-1ac70bac1111",
                )?
                .ynab_account("1eb157e5-cabe-4fee-a1d5-c011ec7ab1e5", Parser::TD)
                .ynab_account("c10711de-5ea7-40e0-be1d-e1a571c51ded", Parser::Citi)
                .build()
        })()
        .expect("failed to build config");

        let expected = Config {
            log_level: LogLevel::Info,
            ynab: YNAB {
                personal_access_token: "super-secret-dont-leak-this".to_string(),
                budget_id: "11f70ff5-ce55-4ada-abed-1ac70bac1111".to_string(),
                accounts: vec![
                    YNABAccount {
                        id: "1eb157e5-cabe-4fee-a1d5-c011ec7ab1e5".to_string(),
                        parser: Parser::TD,
                    },
                    YNABAccount {
                        id: "c10711de-5ea7-40e0-be1d-e1a571c51ded".to_string(),
                        parser: Parser::Citi,
                    },
                ],
            },
            imap: IMAP {
                domain: "imap.gmail.com".to_string(),
                username: "nick@ollien.com".to_string(),
                password: "hunter2".to_string(),
                port: 993,
            },
        };

        assert_eq!(config, expected);
    }

    #[test]
    fn test_builds_configuration_with_custom_imap_port() {
        let config = (|| {
            Builder::new()
                .imap_with_port("imap.gmail.com", "nick@ollien.com", "hunter2", 1337)?
                .ynab(
                    "super-secret-dont-leak-this",
                    "11f70ff5-ce55-4ada-abed-1ac70bac1111",
                )?
                .ynab_account("1eb157e5-cabe-4fee-a1d5-c011ec7ab1e5", Parser::TD)
                .ynab_account("c10711de-5ea7-40e0-be1d-e1a571c51ded", Parser::Citi)
                .build()
        })()
        .expect("failed to build config");

        let expected = Config {
            log_level: LogLevel::Info,
            ynab: YNAB {
                personal_access_token: "super-secret-dont-leak-this".to_string(),
                budget_id: "11f70ff5-ce55-4ada-abed-1ac70bac1111".to_string(),
                accounts: vec![
                    YNABAccount {
                        id: "1eb157e5-cabe-4fee-a1d5-c011ec7ab1e5".to_string(),
                        parser: Parser::TD,
                    },
                    YNABAccount {
                        id: "c10711de-5ea7-40e0-be1d-e1a571c51ded".to_string(),
                        parser: Parser::Citi,
                    },
                ],
            },
            imap: IMAP {
                domain: "imap.gmail.com".to_string(),
                username: "nick@ollien.com".to_string(),
                password: "hunter2".to_string(),
                port: 1337,
            },
        };

        assert_eq!(config, expected);
    }

    #[test]
    fn test_cannot_build_config_without_imap() {
        let err = (|| {
            Builder::new()
                .imap("imap.gmail.com", "nick@ollien.com", "hunter2")?
                .build()
        })()
        .expect_err("should not have been able to build without YNAB config");

        assert!(matches!(err, Error::NoYNAB));
    }

    #[test]
    fn test_cannot_build_config_without_ynab_accounts() {
        let err = (|| {
            Builder::new()
                .imap("imap.gmail.com", "nick@ollien.com", "hunter2")?
                .ynab(
                    "super-secret-dont-leak-this",
                    "11f70ff5-ce55-4ada-abed-1ac70bac1111",
                )?
                .build()
        })()
        .expect_err("should not have been able to build configuration without accounts");

        assert!(matches!(err, Error::NoAccounts));
    }

    #[test]
    fn test_cannot_build_config_without_ynab() {
        let err = (|| {
            Builder::new()
                .ynab(
                    "super-secret-dont-leak-this",
                    "11f70ff5-ce55-4ada-abed-1ac70bac1111",
                )?
                .build()
        })()
        .expect_err("should not have been able to build without IMAP config");

        assert!(matches!(err, Error::NoIMAP));
    }

    #[test]
    fn test_cannot_call_imap_twice() {
        let builder = Builder::new()
            .imap("imap.gmail.com", "nick@ollien.com", "hunter2")
            .expect("failed to add imap");

        let err = builder
            .imap(
                "someotherdomain.com",
                "me@someotherdomain.com",
                "correcthorsebatterystaple",
            )
            .expect_err("should not have been able to add imap config twice");

        assert!(matches!(err, Error::AlreadyHaveIMAP));
    }

    #[test]
    fn test_cannot_call_ynab_twice() {
        let builder = Builder::new()
            .ynab(
                "super-secret-dont-leak-this",
                "11f70ff5-ce55-4ada-abed-1ac70bac1111",
            )
            .expect("failed to add ynab config");

        let err = builder
            .ynab(
                "super-secret-dont-leak-this",
                "11f70ff5-ce55-4ada-abed-1ac70bac1111",
            )
            .expect_err("should not have been able to add imap config twice");

        assert!(matches!(err, Error::AlreadyHaveYNAB));
    }

    #[test]
    fn test_can_add_accounts_before_ynab() {
        let config = (|| -> Result<Config, Error> {
            Builder::new()
                .imap("imap.gmail.com", "nick@ollien.com", "hunter2")?
                .ynab_account("1eb157e5-cabe-4fee-a1d5-c011ec7ab1e5", Parser::TD)
                .ynab_account("c10711de-5ea7-40e0-be1d-e1a571c51ded", Parser::Citi)
                .ynab(
                    "super-secret-dont-leak-this",
                    "11f70ff5-ce55-4ada-abed-1ac70bac1111",
                )?
                .build()
        })()
        .expect("failed to build config");

        let expected = Config {
            log_level: LogLevel::Info,
            ynab: YNAB {
                personal_access_token: "super-secret-dont-leak-this".to_string(),
                budget_id: "11f70ff5-ce55-4ada-abed-1ac70bac1111".to_string(),
                accounts: vec![
                    YNABAccount {
                        id: "1eb157e5-cabe-4fee-a1d5-c011ec7ab1e5".to_string(),
                        parser: Parser::TD,
                    },
                    YNABAccount {
                        id: "c10711de-5ea7-40e0-be1d-e1a571c51ded".to_string(),
                        parser: Parser::Citi,
                    },
                ],
            },
            imap: IMAP {
                domain: "imap.gmail.com".to_string(),
                username: "nick@ollien.com".to_string(),
                password: "hunter2".to_string(),
                port: 993,
            },
        };

        assert_eq!(config, expected);
    }
}
