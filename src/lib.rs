use apollo_http_server_telemetry::{HttpServerTelemetryConfig, ServiceBuilderExt as _};
use apollo_opentelemetry::tower::ServiceBuilderExt as _;
use error::{Result, ServerError};
use handle::handle_request;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::{conn::auto::Builder, graceful::GracefulShutdown},
    service::TowerToHyperService,
};
use serde_yaml::{Mapping, Value};
use state::{Config, RngSource, State, TelemetryConfig};
#[cfg(not(unix))]
use std::future;
use std::{future::Future, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{net::TcpListener, signal};
use tower::{ServiceBuilder, service_fn};
use tracing::{error, info, warn};

/// How long [mock_server_loop] waits for in-flight connections to finish after a shutdown
/// signal before giving up and letting them be dropped mid-request.
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(10);

pub mod error;
pub mod ftv1;
pub mod handle;
pub mod latency;
pub mod state;

/// A general purpose subgraph mock.
#[derive(Debug, clap::Parser)]
#[clap(about, name = "subgraph-mock", long_about = None)]
pub struct Args {
    /// Path to the config file that should be used to configure the server
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Path to the supergraph SDL that the server should mock
    #[arg(short, long)]
    pub schema: PathBuf,
}

impl Args {
    /// Load and initialise the configuration based on command line args
    pub fn init(self) -> Result<(u16, State, TelemetryConfig)> {
        let (port, seed, config, telemetry_config) = match self.config {
            Some(path) => {
                info!(path=%path.display(), "loading and parsing config file");
                Config::from_file(&path)?
            }
            None => {
                info!("using default config");
                Config::parse_yaml(Value::Mapping(Mapping::default()))?
            }
        };

        let rng = seed.map(RngSource::seeded).unwrap_or_default();
        Ok((
            port,
            State::new(config, self.schema)?.with_rng(rng),
            telemetry_config,
        ))
    }
}

/// Resolves once a shutdown signal (Ctrl+C, or `SIGTERM` on Unix — the signal `docker stop`/
/// Kubernetes send on normal container teardown) is received.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}

/// Run the server loop with the provided [State].
pub async fn mock_server_loop(
    port: u16,
    state: State,
    http_telemetry_config: HttpServerTelemetryConfig,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port)))
        .await
        .map_err(|source| ServerError::Bind { source })?;
    info!(%port, "subgraph mock server now listening");

    let state = Arc::new(state);
    let telemetry_stack = ServiceBuilder::new()
        .http_server_propagation()
        .http_server_telemetry(http_telemetry_config);
    let graceful = GracefulShutdown::new();

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|source| ServerError::Accept { source })?;
                let io = TokioIo::new(stream);

                let conn_state = state.clone();
                let svc = telemetry_stack
                    .clone()
                    .service(service_fn(move |req| handle_request(req, conn_state.clone())));

                // `watcher()` (an owned handle, unlike `graceful` itself) is what makes this
                // sendable into the spawned task below — `Builder::serve_connection` borrows
                // the `Builder`, so both have to be constructed inside the same `async move`
                // block that owns them, rather than built out here and moved in already-watched.
                let watcher = graceful.watcher();
                tokio::spawn(async move {
                    let builder = Builder::new(TokioExecutor::new());
                    let conn =
                        watcher.watch(builder.serve_connection(io, TowerToHyperService::new(svc)));
                    if let Err(err) = conn.await {
                        error!(%err, "server error");
                    }
                });
            }
            () = &mut shutdown => {
                info!("shutdown signal received, no longer accepting new connections");
                break;
            }
        }
    }

    tokio::select! {
        () = graceful.shutdown() => {
            info!("all in-flight connections closed gracefully");
        }
        () = tokio::time::sleep(SHUTDOWN_GRACE_PERIOD) => {
            warn!(
                grace_period_secs = SHUTDOWN_GRACE_PERIOD.as_secs(),
                "timed out waiting for in-flight connections to close; some requests may have been interrupted"
            );
        }
    }

    Ok(())
}
