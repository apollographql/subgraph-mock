use crate::state::State;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Body, Bytes},
};
use std::sync::Arc;
use tokio::time::{Instant, sleep};
use tower::BoxError;
use tracing::{trace, warn};

pub mod error;
pub mod graphql;

pub type ByteResponse = Response<BoxBody<Bytes, hyper::Error>>;

/// Top level handler function that is called for every incoming request from Hyper.
pub async fn handle_request<B>(req: Request<B>, state: Arc<State>) -> Result<ByteResponse, B::Error>
where
    B: Body,
    B::Error: Into<BoxError>,
{
    let (parts, body) = req.into_parts();
    let (method, path) = (parts.method, parts.uri.path());
    let include_ftv1 = parts
        .headers
        .get("apollo-federation-include-trace")
        .is_some_and(|value| value.as_bytes() == b"ftv1");
    let body_bytes = body.collect().await?.to_bytes().to_vec();

    let config = state.config.read().await;

    let (res, generator_override) = match (&method, path) {
        // matches routes in the form of `/{subgraph_name}`
        // all further path elements will be ignored for the sake of not spending too much
        // compute time on this condition
        (&Method::POST, route) if route.len() > 1 && route.starts_with('/') => {
            let subgraph_name = route
                .split('/')
                .nth(1)
                .expect("split will yield at least 2 elements based on the match condition");

            apollo_opentelemetry::span_attr!("subgraph.name" = subgraph_name);

            let rgen_cfg = config
                .subgraph_overrides
                .response_generation
                .get(subgraph_name)
                .unwrap_or(&config.response_generation);
            let should_emit_ftv1 = rgen_cfg.ftv1.unwrap_or(include_ftv1);

            (
                graphql::handle(
                    body_bytes,
                    Some(subgraph_name),
                    state.clone(),
                    should_emit_ftv1,
                )
                .await,
                config
                    .subgraph_overrides
                    .latency_generator
                    .get(subgraph_name),
            )
        }
        (&Method::POST, "/") => {
            let should_emit_ftv1 = config.response_generation.ftv1.unwrap_or(include_ftv1);

            (
                graphql::handle(body_bytes, None, state.clone(), should_emit_ftv1).await,
                None,
            )
        }

        // default to 404
        (method, path) => {
            warn!(%method, %path, "received unexpected request");
            let mut resp = Response::new(
                Full::new("Not found\n".into())
                    .map_err(|never| match never {})
                    .boxed(),
            );
            *resp.status_mut() = StatusCode::NOT_FOUND;

            (resp, None)
        }
    };

    let latency = generator_override
        .unwrap_or_else(|| &config.latency_generator)
        .generate(Instant::now());
    trace!(latency_ms = latency.as_millis(), "injecting latency");
    sleep(latency).await;

    Ok(res)
}
