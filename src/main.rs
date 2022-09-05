use futures::{stream::StreamExt, Future};
use log::LevelFilter;
use simplelog::{Config as LogConfig, SimpleLogger};
use std::thread;
use std::{fs::File, sync::Arc};
use tokio::runtime::Runtime;
use ynabifier::{
    task::{Cancel, Spawn, SpawnError},
    Config,
};

fn main() {
    SimpleLogger::init(LevelFilter::Debug, LogConfig::default()).expect("setup failed");
    let config_file = File::open("config.yml").expect("failed to open config file");
    let config =
        serde_yaml::from_reader::<_, Config>(config_file).expect("failed to parse config file");

    let runtime = Runtime::new().expect("failed to create runtime");

    runtime.block_on(async move {
        {
            let mut stream =
                ynabifier::stream_new_messages(Arc::new(TokioSpawner), config.imap().clone())
                    .await
                    .expect("failed to setup stream");
            if let Some(msg) = stream.next().await {
                // This is perhaps not a sound assumption, but is fine for testing
                dbg!(String::from_utf8(msg));
            }
            drop(stream);
        }
    });

    thread::sleep_ms(10000);
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
