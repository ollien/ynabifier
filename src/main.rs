#[macro_use]
extern crate log;

use futures::{stream::StreamExt, Future};
use log::LevelFilter;
use std::{fs::File, sync::Arc};
use tokio::runtime::Runtime;
use ynabifier::parse::Transaction;
use ynabifier::{
    config::{Config, YNABAccount},
    task::{Cancel, Spawn, SpawnError},
    ynab::Client as YNABClient,
    Message,
};

fn main() {
    let config_file = File::open("config.yml").expect("failed to open config file");
    let config =
        serde_yaml::from_reader::<_, Config>(config_file).expect("failed to parse config file");

    setup_logger(config.log_level()).expect("failed to seutp logger");

    let runtime = Runtime::new().expect("failed to create runtime");
    runtime.block_on(async move {
        let ynab_client = YNABClient::new(config.ynab().personal_access_token().to_string());
        let mut stream =
            ynabifier::stream_new_messages(Arc::new(TokioSpawner), config.imap().clone())
                .await
                .expect("failed to setup stream");

        let accounts = config.ynab().accounts();
        while let Some(msg) = stream.next().await {
            if let Some((account, transaction)) = try_parse_email(accounts.iter(), &msg) {
                info!(
                    "Parsed transaction for {} to {} with parser {}",
                    transaction.amount(),
                    transaction.payee(),
                    account.parser_name(),
                );
                submit_transaction(
                    &ynab_client,
                    &transaction,
                    config.ynab().budeget_id(),
                    account.id(),
                )
                .await;
            }
        }
    });
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
            ))
        })
        .level(LevelFilter::Error)
        .level_for("ynabifier", log_level)
        .chain(std::io::stderr())
        .apply()?;

    Ok(())
}

fn try_parse_email<'a, I>(ynab_accounts: I, msg: &Message) -> Option<(&'a YNABAccount, Transaction)>
where
    I: Iterator<Item = &'a YNABAccount>,
{
    for account in ynab_accounts {
        match account.parser().parse_transaction_email(msg) {
            Ok(transaction) => return Some((account, transaction)),
            Err(err) => debug!(
                "failed to parse message with parser '{}': {:?}",
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
        error!("Failed to submit transaction: {}", err);
    }
}

#[derive(Clone)]
struct TokioSpawner;

impl Spawn for TokioSpawner {
    type Cancel = CancelFnOnce;

    fn spawn<F: Future + Send + 'static>(&self, future: F) -> Result<Self::Cancel, SpawnError>
    where
        <F as Future>::Output: Send,
    {
        let handle = tokio::spawn(future);
        let canceler = CancelFnOnce {
            cancel_func: Box::new(move || handle.abort()),
        };
        Ok(canceler)
    }
}

struct CancelFnOnce {
    cancel_func: Box<dyn FnOnce() + Send + Sync>,
}

impl Cancel for CancelFnOnce {
    fn cancel(self) {
        (self.cancel_func)()
    }

    fn cancel_boxed(self: Box<Self>) {
        self.cancel()
    }
}
