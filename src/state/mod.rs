use notify::{Config as NotifyConfig, Event, EventKind, PollWatcher, RecursiveMode, Watcher};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tracing::error;

mod config;
mod schema;

pub use config::Config;
pub use config::default_port;
pub use schema::FederatedSchema;

use schema::update_schema;

pub struct State {
    pub config: Arc<RwLock<Config>>,
    pub schema: Arc<RwLock<FederatedSchema>>,
    /// Handle to the pollwatcher that updates the schema for this config, so that it only drops out of scope when this state does
    _schema_watcher: PollWatcher,
}

impl State {
    pub fn new(config: Config, schema_path: PathBuf) -> anyhow::Result<Self> {
        let schema = FederatedSchema::parse(&schema_path)?;
        let schema = Arc::new(RwLock::new(schema));

        let lock = schema.clone();
        // We have to use a PollWatcher because Docker on MacOS doesn't support filesystem events:
        // https://docs.rs/notify/8.2.0/notify/index.html#docker-with-linux-on-macos-m1
        let mut schema_watcher = PollWatcher::new(
            move |res: Result<Event, _>| match res {
                Ok(event) => {
                    if let EventKind::Modify(_) = event.kind
                        && let Some(path) = event.paths.first()
                        && let Err(err) = update_schema(path, lock.clone())
                    {
                        error!("Failed to reload schema: {}", err);
                    }
                }
                Err(errors) => {
                    error!("Error watching schema file: {:?}", errors)
                }
            },
            NotifyConfig::default()
                .with_poll_interval(Duration::from_secs(1))
                .with_compare_contents(true),
        )?;
        schema_watcher.watch(&schema_path, RecursiveMode::NonRecursive)?;

        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            schema,
            _schema_watcher: schema_watcher,
        })
    }

    pub fn default(schema_path: PathBuf) -> anyhow::Result<Self> {
        Self::new(Config::default(), schema_path)
    }
}
