use apollo_opentelemetry::Telemetry;
use clap::Parser;
use opentelemetry::KeyValue;
use opentelemetry_sdk::Resource;
use std::panic::set_hook;
use subgraph_mock::{Args, error::Error, mock_server_loop, shutdown_signal};
use tracing::error;
use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    fmt,
    prelude::*,
};

#[tokio::main]
async fn main() {
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

    let (port, state, telemetry_config) = match Args::parse().init() {
        Ok(ok) => ok,
        Err(err) => report_and_exit(err),
    };

    let _telemetry = match Telemetry::builder(telemetry_config.otel)
        .with_global_tracer_provider()
        .with_global_meter_provider()
        .with_global_propagator()
        .with_resource_builder(
            Resource::builder()
                .with_service_name(env!("CARGO_PKG_NAME"))
                .with_attributes([KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))]),
        )
        .build()
    {
        Ok(telemetry) => telemetry,
        Err(err) => report_and_exit(Error::from(err)),
    };

    // On a signal, this returns normally instead of the process being killed out from under it,
    // so `_telemetry` above actually gets dropped (and flushed) before the process exits.
    if let Err(err) = mock_server_loop(port, state, telemetry_config.http, shutdown_signal()).await
    {
        report_and_exit(err);
    }
}

fn report_and_exit(err: Error) -> ! {
    eprintln!("{:?}", miette::Report::new(err));

    std::process::exit(1);
}
