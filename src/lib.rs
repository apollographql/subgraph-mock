use handle::handle_request;
use hyper::service::service_fn;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
};
use state::{Config, RngSource, State, default_port};
use std::{fs, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tracing::{error, info};

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
    pub fn init(self) -> anyhow::Result<(u16, State)> {
        let (port, seed, config) = match self.config {
            Some(path) => {
                info!(path=%path.display(), "loading and parsing config file");
                Config::parse_yaml(serde_yaml::from_slice(&fs::read(path)?)?)?
            }
            None => {
                info!("using default config");
                (default_port(), None, Config::default())
            }
        };

        let rng = seed.map(RngSource::seeded).unwrap_or_default();
        Ok((port, State::new(config, self.schema)?.with_rng(rng)))
    }
}

/// Run the server loop with the provided [State]
pub async fn mock_server_loop(port: u16, state: State) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    info!(%port, "subgraph mock server now listening");

    let state = Arc::new(state);
    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = Builder::new(TokioExecutor::new())
                .serve_connection(io, service_fn(|req| handle_request(req, state.clone())))
                .await
            {
                error!(%err, "server error");
            }
        });
    }
}
