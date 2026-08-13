use apollo_configuration::{ParseYamlOptions, configuration, expansion::EnvVariables};
use apollo_healthcheck::{HealthEndpoints, HealthService, config::HealthEndpointsConfig};
use http_body_util::Full;
use hyper::{Request, Response, StatusCode, body::Bytes, header};
use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tower::Service;

#[configuration]
pub struct HealthConfig {
    /// How long the health check stays unhealthy after a failed schema hot-reload before
    /// auto-recovering. Accepts human-readable durations: "30s", "5m", "1h30m".
    #[config(default = "30s".parse().unwrap())]
    pub recovery_ttl: apollo_configuration::types::Duration,
    /// URL path at which the combined health endpoint is served.
    #[config(default = default_health_path())]
    pub path: String,
}

fn default_health_path() -> String {
    "/health".to_string()
}

impl HealthConfig {
    /// Bridges to apollo-healthcheck's own config type, which can only be built by parsing
    /// YAML (its fields are private to that crate). `recovery_ttl` is the only part of it
    /// subgraph-mock actually exposes; `paths` is left at the crate's own defaults, since
    /// [SingleHealthEndpoint] never exposes those individual paths externally.
    pub(super) fn to_health_endpoints_config(&self) -> HealthEndpointsConfig {
        apollo_configuration::parse_yaml(
            &format!("recovery_ttl: {}\n", self.recovery_ttl),
            &ParseYamlOptions::default().variables(EnvVariables),
        )
        .expect("a Duration re-serialized via Display always parses back as one")
    }
}

/// Tower [Service] that serves one combined health endpoint at [HealthConfig::path], backed
/// by apollo-healthcheck's separate liveness/readiness/startup probes. Returns `404` for any
/// other path, `200` with `{"status":"healthy"}` if every underlying probe is healthy, or
/// `503` with `{"status":"unhealthy"}` otherwise.
#[derive(Clone)]
pub struct SingleHealthEndpoint {
    path: String,
    probe_paths: [String; 3],
    inner: HealthService,
}

impl SingleHealthEndpoint {
    /// Builds the combined endpoint from an already-configured [HealthEndpoints] - call this
    /// instead of [HealthEndpoints::into_service] to serve one path rather than three.
    pub fn new(path: String, endpoints: HealthEndpoints) -> Self {
        // Captured before `into_service` consumes `endpoints` - its own docs require this.
        let probe_paths = [
            endpoints.liveness_path().to_string(),
            endpoints.readiness_path().to_string(),
            endpoints.startup_path().to_string(),
        ];
        Self {
            path,
            probe_paths,
            inner: endpoints.into_service(),
        }
    }
}

impl<B> Service<Request<B>> for SingleHealthEndpoint
where
    B: Send + 'static,
{
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        if req.uri().path() != self.path {
            return Box::pin(async {
                Ok(Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::new()))
                    .expect("building a bodyless 404 response should never fail"))
            });
        }

        let mut inner = self.inner.clone();
        let probe_paths = self.probe_paths.clone();
        Box::pin(async move {
            // The request body is never read by any of the three probes, so a fresh
            // bodyless request per probe is enough - no need to preserve the original body.
            let mut healthy = true;
            for probe_path in &probe_paths {
                let probe_req = Request::builder()
                    .uri(probe_path.as_str())
                    .body(())
                    .expect("a request to a known-valid configured path always builds");
                let response = inner
                    .call(probe_req)
                    .await
                    .unwrap_or_else(|never| match never {});
                healthy &= response.status() == StatusCode::OK;
            }

            let (status, body) = if healthy {
                (StatusCode::OK, r#"{"status":"healthy"}"#)
            } else {
                (StatusCode::SERVICE_UNAVAILABLE, r#"{"status":"unhealthy"}"#)
            };
            Ok(Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Full::new(Bytes::from_static(body.as_bytes())))
                .expect("building this fixed-shape response should never fail"))
        })
    }
}
