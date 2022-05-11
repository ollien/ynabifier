use std::fs::File;
use tokio::{runtime::Runtime, sync::broadcast};
use ynabifier::{Config, SequenceNumber, Watcher};

fn main() {
    let config_file = File::open("config.yml").expect("failed to open config file");
    let config =
        serde_yaml::from_reader::<_, Config>(config_file).expect("failed to parse config file");

    let runtime = Runtime::new().expect("failed to create runtime");
    let (tx, mut rx) = broadcast::channel::<SequenceNumber>(16);

    let cloned_config = config.clone();
    runtime.spawn(async move {
        let mut session = ynabifier::setup_session(&cloned_config)
            .await
            .expect("failed to setup socket");

        loop {
            let rx_res = rx.recv().await;
            if let Err(err) = rx_res {
                eprintln!("failed to recv: {}", err);
                continue;
            }

            session.examine("INBOX").await.expect("couldn't examine");

            let sequence_number = rx_res.unwrap();
            println!("fetching email {}", sequence_number.value());

            match ynabifier::fetch_email(&mut session, sequence_number).await {
                Ok(email) => println!("{}", email),
                Err(err) => eprintln!("failed to fetch email: {}", err),
            }
        }
    });

    runtime.block_on(async {
        let session = ynabifier::setup_session(&config)
            .await
            .expect("failed to setup socket");

        let watcher = Watcher::new(tx);
        let mut current_session = session;
        current_session
            .examine("INBOX")
            .await
            .expect("couldn't examine");
        loop {
            println!("Beginning idle loop");
            current_session = watcher.watch_for_email(current_session).await.unwrap();
        }
    });
}
