# YNABifier


Loads transactions into [You Need a Budget](https://www.youneedabudget.com/) (YNAB) in real time!*

\* for varying definitions of real time

<hr>

YNAB has built in functionality to import transactions from your credit cards, but banks often don't make this data available very quickly; usually it takes a day or two. However, many banks have the ability to send emails to you when you have a transaction over a certain amount (which can be $0, if you like!). YNABifier scans these emails for transactions, and automatically imports them to YNAB.

This project takes heavy inspiration from [buzzlawless/live-import-for-ynab](https://github.com/buzzlawless/live-import-for-ynab), which does effectively the same thing, but makes use of AWS Simple Email Service, if self-hosting isn't your game.

## Note
YNABifier, while functional, is still a bit of a work in progress. There are some edges that need to be cleaned up, but it works for the most part.

Planned improvements:
 - [ ] Streamline the configuration process to not require fetching YNAB IDs by hand.
 - [x] IMAP sessions need to be more properly cleaned up.
 - [x] Various QoL features, such as allowing alternate configuration paths

## Setup

YNABifier requires a configuration file named `config.yml` placed in your present working directory. It does require some fields that you can fetch from the YNAB API about your account, as well as a [personal access token](https://api.youneedabudget.com/).

```yml
log_level: info # optional
imap:
  username: email address
  password: IMAP Password
  domain: Your IMAP Server
  port: 993 # optional
ynab:
  personal_access_token: Your YNAB Personal Access Token
  budget_id: The YNAB ID for the budget that YNABifier should insert into
  accounts:
    - account_id: The YNAB ID for account that this should parse
      parser: The parser to use (see below)
```

Once this is in place, you can use `cargo run --release`.

## Supported transaction providers
- TD Bank (`td` in the configuration)
- Citi Bank (`citi` in the configuration)
