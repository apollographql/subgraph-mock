use clap::Parser;
use std::panic::set_hook;
use subgraph_mock::{Args, mock_server_loop};
use tracing::error;
use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    fmt,
    prelude::*,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer().compact().with_target(false))
        .with(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .try_init()
        .expect("unable to set a global tracing subscriber");

    set_hook(Box::new(|panic| {
        if let Some(loc) = panic.location() {
            error!(
                message=%panic,
                panic.file=loc.file(),
                panic.line=loc.line(),
                panic.column=loc.column()
            );
        } else {
            error!(message=%panic);
        }
    }));

    let (port, state) = Args::parse().init()?;
    mock_server_loop(port, state).await
}
