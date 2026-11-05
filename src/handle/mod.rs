use crate::{
    failure_schedule::Failure,
    state::{Config, State},
};
use http_body_util::{BodyExt, Empty, Full, combinators::BoxBody};
use hyper::{
    Method, Request, Response, StatusCode,
    body::{Body, Bytes},
};
use std::sync::Arc;
use tokio::time::{Instant, sleep};
use tower::{BoxError, Service};
use tracing::{trace, warn};

pub mod error;
pub mod graphql;

pub type ByteResponse = Response<BoxBody<Bytes, hyper::Error>>;

/// Top level handler function that is called for every incoming request from Hyper.
pub async fn handle_request<B>(req: Request<B>, state: Arc<State>) -> Result<ByteResponse, B::Error>
where
    B: Body + Send + 'static,
    B::Error: Into<BoxError>,
{
    let (parts, body) = req.into_parts();

    if parts.method == Method::GET {
        let response = state
            .health_service
            .clone()
            .call(Request::from_parts(parts, body))
            .await
            .unwrap_or_else(|never| match never {});
        let (parts, body) = response.into_parts();
        return Ok(Response::from_parts(
            parts,
            body.map_err(|never| match never {}).boxed(),
        ));
    }

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
                handle_graphql(
                    body_bytes,
                    Some(subgraph_name),
                    &state,
                    &config,
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
                handle_graphql(body_bytes, None, &state, &config, should_emit_ftv1).await,
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

/// Applies the failure schedule covering `subgraph_name`, then hands the request to
/// [graphql::handle] unless the schedule fails it outright.
///
/// An `http_status` failure returns before the body is parsed, the RNG is drawn or the response
/// cache is consulted, so a cached response can't slip through a failure window.
async fn handle_graphql(
    body_bytes: Vec<u8>,
    subgraph_name: Option<&str>,
    state: &Arc<State>,
    config: &Config,
    should_emit_ftv1: bool,
) -> ByteResponse {
    // Every GraphQL request reads the clock, even for an unscheduled subgraph, because the first
    // one starts it for all schedules.
    let elapsed = state.failure_schedule_clock.elapsed();
    match config
        .failure_schedule_for(subgraph_name)
        .and_then(|schedule| schedule.failure_at(elapsed))
    {
        Some(Failure::HttpStatus(status)) => {
            let mut resp = Response::new(Empty::new().map_err(|never| match never {}).boxed());
            *resp.status_mut() = StatusCode::from_u16(status)
                .expect("config validation limits http_status to 400..=599");
            return resp;
        }
        Some(Failure::Delay(delay)) => sleep(*delay).await,
        None => {}
    }

    graphql::handle(body_bytes, subgraph_name, state.clone(), should_emit_ftv1).await
}
