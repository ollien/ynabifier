#![warn(clippy::all, clippy::pedantic)]

#[macro_use]
extern crate log;

use async_trait::async_trait;
use clap::Parser;
use futures::{select, FutureExt};
use futures::{stream::StreamExt, Future};
use log::LevelFilter;
use tokio::task::{JoinError, JoinHandle};
use ynabifier::task::{Handle, Join};

use std::{fs::File, process, sync::Arc};
use tokio::runtime::Runtime;
use ynabifier::{
    config::{Config, YNABAccount},
    parse::Transaction,
    task::{Cancel, Spawn, SpawnError},
    ynab::Client as YNABClient,
    CloseableStream, Message,
};

#[derive(Parser, Debug)]
struct Args {
    #[arg(short, default_value = "config.yml")]
    config_file: String,
}

fn main() {
    let args = Args::parse();

    let config_res = load_config(&args.config_file);
    if let Err(err) = config_res {
        eprintln!("Failed to load configuration: {err}");
        process::exit(1);
    }

    let config = config_res.unwrap();
    if let Err(err) = setup_logger(config.log_level()) {
        eprintln!("Failed to setup logger: {err}");
        process::exit(1);
    }

    if let Err(err) = listen_for_transactions(&config) {
        error!("Failed to listen for transaction emails: {err}");
        process::exit(2);
    }
}

fn load_config(filename: &str) -> Result<Config, anyhow::Error> {
    let config_file = File::open(filename)?;
    let config = serde_yaml::from_reader(config_file)?;
    Ok(config)
}

fn setup_logger(log_level: LevelFilter) -> Result<(), fern::InitError> {
    fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] [{}] {}",
                chrono::Local::now().format("%Y-%m-%dT%H:%M:%S"),
                record.level(),
                record.target(),
                message
            ));
        })
        .level(LevelFilter::Error)
        .level_for("ynabifier", log_level)
        .chain(std::io::stderr())
        .apply()?;

    Ok(())
}

fn listen_for_transactions(config: &Config) -> Result<(), anyhow::Error> {
    let runtime = Runtime::new()?;
    runtime.block_on(async move {
        let ynab_client = YNABClient::new(config.ynab().personal_access_token().to_string());
        let mut stream =
            ynabifier::stream_new_messages(Arc::new(TokioSpawner), config.imap().clone()).await?;

        let accounts = config.ynab().accounts();
        loop {
            select! {
                _ = tokio::signal::ctrl_c().fuse() => break,
                maybe_msg = stream.next().fuse() => {
                    if maybe_msg.is_none() {
                        break;
                    }
                    let msg = maybe_msg.unwrap();
                    if let Some((account, transaction)) = try_parse_email(accounts.iter(), &msg) {
                        let amount = transaction.amount();
                        let payee = transaction.payee();
                        let parser_name = account.parser_name();
                        info!("Parsed transaction for {amount} to {payee} with parser {parser_name}");
                        submit_transaction(
                            &ynab_client,
                            &transaction,
                            config.ynab().budeget_id(),
                            account.id(),
                        )
                        .await;
                    }
                }
            }
        }

        // Wait for the user to either ctrl-c a second time to forcibly shut off the app, or wait a "reasonable"
        // amount of time for cleanup
        select! {
            _ = tokio::signal::ctrl_c().fuse() => (),
            _ = stream.close().fuse() => (),
        }

        Ok(())
    })
}

fn try_parse_email<'a, I>(ynab_accounts: I, msg: &Message) -> Option<(&'a YNABAccount, Transaction)>
where
    I: Iterator<Item = &'a YNABAccount>,
{
    for account in ynab_accounts {
        match account.parser().parse_transaction_email(msg) {
            Ok(transaction) => return Some((account, transaction)),
            Err(err) => debug!(
                "Failed to parse message with parser '{}': {:?}",
                account.parser_name(),
                err
            ),
        }
    }

    None
}

async fn submit_transaction(
    client: &YNABClient,
    transaction: &Transaction,
    budget_id: &str,
    account_id: &str,
) {
    if let Err(err) = client
        .submit_transaction(transaction, budget_id, account_id)
        .await
    {
        error!("Failed to submit transaction: {err}");
    }
}

#[derive(Clone)]
struct TokioSpawner;
struct TokioJoinHandle(JoinHandle<()>);

impl Spawn for TokioSpawner {
    type Handle = TokioJoinHandle;

    fn spawn<F: Future + Send + 'static>(&self, future: F) -> Result<Self::Handle, SpawnError>
    where
        <F as Future>::Output: Send,
    {
        let handle = tokio::spawn(async move {
            future.await;
        });

        Ok(TokioJoinHandle(handle))
    }
}

impl Cancel for TokioJoinHandle {
    fn cancel(self) {
        self.0.abort();
    }
}

#[async_trait]
impl Join for TokioJoinHandle {
    type Error = JoinError;

    async fn join(self) -> Result<(), Self::Error> {
        match self.0.await {
            Err(err) => Err(err),
            Ok(_) => Ok(()),
        }
    }
}

impl Handle for TokioJoinHandle {}
