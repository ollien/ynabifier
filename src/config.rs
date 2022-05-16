use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct Config {
    imap: IMAP,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(PartialEq))]
pub struct IMAP {
    domain: String,
    #[serde(default = "defaults::port")]
    port: u16,
    username: String,
    password: String,
    // unfortunately, we can't configure tls because async_imap requires it
}

impl Config {
    #[must_use]
    pub fn imap(&self) -> &IMAP {
        &self.imap
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

mod defaults {
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
            imap:
                domain: imap.google.com
                port: 993
                username: nick@ollien.com
                password: hunter2
        "#,
        );

        let expected = Config {
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
