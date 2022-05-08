use std::fs::File;
use tokio::runtime::Runtime;
use ynabifier::Config;

fn main() {
    let config_file = File::open("config.yml").expect("failed to open config file");
    let config =
        serde_yaml::from_reader::<_, Config>(config_file).expect("failed to parse config file");

    let runtime = Runtime::new().expect("failed to create runtime");
    runtime.block_on(async {
        let session = ynabifier::setup_session(&config)
            .await
            .expect("failed to setup socket");
        ynabifier::fetch_emails(session)
            .await
            .expect("failed to fetch emails");
    });
}
