#[macro_use]
extern crate log;

use futures::{stream::StreamExt, Future};
use log::LevelFilter;
use std::{fs::File, sync::Arc};
use tokio::runtime::Runtime;
use ynabifier::parse::{Transaction, TransactionEmailParser};
use ynabifier::Message;
use ynabifier::{
    parse::{CitiEmailParser, TDEmailParser},
    task::{Cancel, Spawn, SpawnError},
    Config,
};

fn main() {
    setup_logger().expect("failed to seutp logger");
    let config_file = File::open("config.yml").expect("failed to open config file");
    let config =
        serde_yaml::from_reader::<_, Config>(config_file).expect("failed to parse config file");

    let runtime = Runtime::new().expect("failed to create runtime");
    let parsers = vec![
        (
            "citi",
            Box::new(CitiEmailParser) as Box<dyn TransactionEmailParser>,
        ),
        (
            "td",
            Box::new(TDEmailParser) as Box<dyn TransactionEmailParser>,
        ),
    ];

    runtime.block_on(async move {
        let mut stream =
            ynabifier::stream_new_messages(Arc::new(TokioSpawner), config.imap().clone())
                .await
                .expect("failed to setup stream");
        while let Some(msg) = stream.next().await {
            if let Some(transaction) = try_parse_email(parsers.iter(), &msg) {
                info!(
                    "got transaction from {} for {}",
                    transaction.payee(),
                    transaction.amount()
                )
            }
        }
    });
}

fn setup_logger() -> Result<(), fern::InitError> {
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
        .level(LevelFilter::Info)
        .level_for("ynabifier", LevelFilter::Debug)
        .chain(std::io::stderr())
        .apply()?;

    Ok(())
}

fn try_parse_email<'a, I>(parser_iter: I, msg: &Message) -> Option<Transaction>
where
    I: Iterator<Item = &'a (&'a str, Box<dyn TransactionEmailParser>)>,
{
    for (parser_name, parser) in parser_iter {
        match parser.parse_transaction_email(msg) {
            Ok(transaction) => return Some(transaction),
            Err(err) => debug!(
                "failed to parse message with parser '{}': {:?}",
                parser_name, err
            ),
        }
    }

    None
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
