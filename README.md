# YNABifier


Loads transactions into [You Need a Budget](https://www.youneedabudget.com/)
(YNAB) in real time!*

\* for varying definitions of real time

<hr>

YNAB has built in functionality to import transactions from your credit cards,
but banks often don't make this data available very quickly; usually it takes a
day or two. However, many banks have the ability to send emails to you when you
have a transaction over a certain amount (which can be $0, if you like!).
YNABifier scans these emails for transactions, and automatically imports them to
YNAB.

This project takes heavy inspiration from
[buzzlawless/live-import-for-ynab](https://github.com/buzzlawless/live-import-for-ynab),
which does effectively the same thing, but makes use of AWS Simple Email
Service, if self-hosting isn't your game.

## Setup

YNABifier requires a configuration file named `config.yml` placed in your
present working directory. It does require some fields that you can fetch from
the YNAB API about your account, as well as a [personal access
token](https://api.youneedabudget.com/).

As a convenience, a setup utility is provided to generate the configuration. You
can access this by running `cargo run --bin setup`, and then following the
on-screen prompts.

### Config Schema
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

Once this is in place, you can use `cargo run --release` to run YNABifier, or
`cargo build --release`, and use the built binary.

## Supported transaction providers
- TD Bank (`td` in the configuration)
- Citi Bank (`citi` in the configuration)

## A meta note
This project was my first foray into async Rust, and there's definitely some
remnants of it being my first time. An explicit goal for the lib crate was to
make it async runtime agnostic, which was a harder challenge than expected, but
is why there are wrappers around things like task cancellation and management.
All in all, while this project was fun, I don't know that I would have chosen
async Rust for it if I were to do it all over; I might have used something like
Elixir. If I did use async Rust again, I might reach for an actor framework like
Actix.
