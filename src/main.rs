use futures::{stream::StreamExt, Future};
use log::LevelFilter;
use simplelog::{Config as LogConfig, SimpleLogger};
use std::fs::File;
use tokio::runtime::Runtime;
use ynabifier::{
    task::{Cancel, Spawn, SpawnError},
    Config, ConfigSessionGenerator, MessageFetcher, RawFetcher, SequenceNumberStreamer, Watcher,
};

// TODO: This function is incredibly messy but it's mostly been used for prototyping so I'll allow it... for now
fn main() {
    let _ = SimpleLogger::init(LevelFilter::Debug, LogConfig::default()).expect("setup failed");
    let config_file = File::open("config.yml").expect("failed to open config file");
    let config =
        serde_yaml::from_reader::<_, Config>(config_file).expect("failed to parse config file");

    let runtime = Runtime::new().expect("failed to create runtime");

    runtime.block_on(async {
        let session_generator = ConfigSessionGenerator::new(config.imap().clone());
        let tokio_spawner = TokioSpawner {};
        let mut watcher = Watcher::new(session_generator, tokio_spawner);
        let mut stream = watcher
            .watch_for_new_messages()
            .await
            .expect("failed to get stream");

        let session_generator = ConfigSessionGenerator::new(config.imap().clone());
        let message_fetcher = RawFetcher::new(session_generator);
        while let Some(seq) = stream.next().await {
            let email = message_fetcher
                .fetch_message(seq)
                .await
                .expect("failed to get email");
            println!(
                "{}",
                String::from_utf8(email).expect("message was not utf-8")
            );
            watcher.stop();
        }
    });
}

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
}
