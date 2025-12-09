#![allow(dead_code)]
use anyhow::anyhow;
use apollo_compiler::{Schema, validation::Valid};
use apollo_parser::Parser;
use apollo_smith::{Document, DocumentBuilder};
use arbitrary::Unstructured;
use cached::proc_macro::cached;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Bytes};
use rand::{RngCore, SeedableRng, rngs::StdRng};
use serde_json_bytes::{Value, serde_json};
use std::{borrow::Borrow, collections::HashMap, path::PathBuf};
use subgraph_mock::{
    Args,
    handle::{ByteResponse, graphql::GraphQLRequest, handle_request},
};
use tokio::time::{self, Duration, Instant};
use tracing::debug;
use tracing_subscriber::{
    filter::{EnvFilter, LevelFilter},
    fmt,
    prelude::*,
};

mod response;

pub use response::*;

/// Initializes the global state of the mock server based on the optional config file name that maps to
/// a YAML config located in `tests/data/config`. **Because these values are static, this function can only be
/// invoked once per integration test suite.**
///
/// If no config file name is provided, the default will be used.
///
/// Returns the port number that the server would have been mapped to, since that value is not actually
/// contained within the state of the app and cannot be otherwise tested as such.
pub fn initialize(config_file_name: Option<&str>) -> anyhow::Result<u16> {
    tracing_subscriber::registry()
        .with(fmt::layer().compact())
        .with(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::OFF.into())
                .from_env_lossy(),
        )
        .try_init()
        .expect("unable to set a global tracing subscriber");

    let pkg_root = env!("CARGO_MANIFEST_DIR");
    let args = Args {
        config: config_file_name
            .map(|name| PathBuf::from(format!("{pkg_root}/tests/data/config/{name}"))),
        schema: PathBuf::from(format!("{pkg_root}/tests/data/schema.graphql")),
    };
    args.init()
}

/// Cached supergraph document that is used as the basis for generating requests
#[cached(result = true)]
fn generate_test_doc() -> anyhow::Result<Document> {
    let supergraph = include_str!("../data/schema.graphql");
    let parser = Parser::new(supergraph);

    let tree = parser.parse();
    if tree.errors().next().is_some() {
        return Err(anyhow!("cannot parse the graphql file"));
    }

    // Convert `apollo_parser::Document` into `apollo_smith::Document`.
    Document::try_from(tree.document()).map_err(|err| err.into())
}

/// Cached supergraph schema for response validation
#[cached(result = true)]
fn generate_schema() -> anyhow::Result<Valid<Schema>> {
    let supergraph = include_str!("../data/schema.graphql");
    Schema::parse_and_validate(supergraph, "schema.graphql")
        .map_err(|err| anyhow!(err.errors.to_string()))
}

/// Runs a single request to the mock server through the handler method. Uses seeded RNG for test-case
/// reproducibility. Validates that the response was valid based on the generated query.
///
/// If `subgraph_name` is [Some], this request will be sent to the mock as if it were a request to that specific
/// subgraph.
///
/// Run your test case with `RUST_LOG=debug` to see the query generated for a given RNG seed. This will allow you
/// to then make assumptions about the structure of your responses in the test.
///
/// Borrows heavily from the example in the apollo-smith docs.
pub async fn make_request<T>(rng_seed: u64, subgraph_name: T) -> anyhow::Result<ByteResponse>
where
    T: Borrow<Option<String>>,
{
    let apollo_smith_doc = generate_test_doc()?;

    let mut rng = StdRng::seed_from_u64(rng_seed);
    let mut bytes = vec![0u8; 1024];
    rng.fill_bytes(&mut bytes);

    let mut u = Unstructured::new(&bytes);

    // Create a `DocumentBuilder` given an existing document to match a schema.
    let mut gql_doc = DocumentBuilder::with_document(&mut u, apollo_smith_doc)?;
    let operation_def: String = gql_doc.operation_definition()?.unwrap().into();

    let uri = match subgraph_name.borrow() {
        Some(name) => format!("/{name}"),
        None => "/".to_owned(),
    };

    let body = serde_json::to_vec(&GraphQLRequest {
        query: operation_def.clone(),
        operation_name: None,
        variables: HashMap::new(),
    })?;

    debug!("Query for seed {rng_seed}:\n{operation_def}");

    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .body(Full::<Bytes>::from(body))?;

    // Rip the body out, validate it, then repackage it to return
    let (parts, body) = handle_request(req).await?.into_parts();
    let bytes = body.collect().await?.to_bytes();

    debug!(
        "Response for seed {rng_seed}:\n{}",
        String::from_utf8_lossy(&bytes)
    );

    let raw: Value = serde_json::from_slice(&bytes)?;
    validate_response(
        &generate_schema()?,
        &operation_def,
        raw.as_object()
            .ok_or(anyhow!("response should be a JSON object"))?
            .get("data")
            .expect("response should have data"),
    )
    .map_err(|validation_errors| {
        anyhow!(
            validation_errors
                .iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;

    let boxed_body = Full::new(bytes)
        .map_err(|infallible| match infallible {})
        .boxed();

    Ok(Response::from_parts(parts, boxed_body))
}

/// Run a single request with a timed lifecycle and assert that the generated latency for it matches
/// `expected`. Returns the generated latency as a convenience for advancing time correctly as needed.
async fn test_latency<T>(expected: u64, rng_seed: u64, subgraph_name: T) -> anyhow::Result<Duration>
where
    T: Borrow<Option<String>>,
{
    let start = Instant::now();
    let response = make_request(rng_seed, subgraph_name).await?;
    assert_eq!(200, response.status());
    let elapsed = start.elapsed();
    assert_eq!(Duration::from_millis(expected), elapsed);

    Ok(elapsed)
}

/// Asserts that the request latency function is sine
///
/// This must be called in a test that has paused time before initializing the mock server in order to make
/// consistent assertions about the wave state.
///
/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
pub async fn assert_is_sine<T>(
    rng_seed: u64,
    base: u64,
    amplitude: u64,
    period: Duration,
    subgraph_name: T,
) -> anyhow::Result<()>
where
    T: Borrow<Option<String>>,
{
    // At t=0 seconds, our sine wave is halfway up its amplitude
    let elapsed = test_latency(base + (amplitude / 2), rng_seed, subgraph_name.borrow()).await?;

    // Advancing half a period should put us at the same latency on the other side of the wave
    time::advance(period.div_f64(2.0) - elapsed).await;
    let elapsed = test_latency(base + (amplitude / 2), rng_seed, subgraph_name.borrow()).await?;

    // Advancing a quarter period should put us at the bottom
    time::advance(period.div_f64(4.0) - elapsed).await;
    let elapsed = test_latency(base, rng_seed, subgraph_name.borrow()).await?;

    // Advancing a half period should put us at the top
    time::advance(period.div_f64(2.0) - elapsed).await;
    test_latency(base + amplitude, rng_seed, subgraph_name.borrow()).await?;

    Ok(())
}

/// Asserts that the request latency function is square
///
/// This must be called in a test that has paused time before initializing the mock server in order to make
/// consistent assertions about the wave state.
///
/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
pub async fn assert_is_square<T>(
    rng_seed: u64,
    base: u64,
    amplitude: u64,
    period: Duration,
    subgraph_name: T,
) -> anyhow::Result<()>
where
    T: Borrow<Option<String>>,
{
    // At t=0 seconds, our square wave is at the top of the wave
    let elapsed = test_latency(base + amplitude, rng_seed, subgraph_name.borrow()).await?;

    // Advancing a quarter period should still have the same value
    time::advance(period.div_f64(4.0) - elapsed).await;
    let elapsed = test_latency(base + amplitude, rng_seed, subgraph_name.borrow()).await?;

    // Advancing another quarter period should drop to the bottom of the wave
    time::advance(period.div_f64(4.0) - elapsed).await;
    let elapsed = test_latency(base, rng_seed, subgraph_name.borrow()).await?;

    // Advancing another quarter period should stay at the bottom of the wave
    time::advance(period.div_f64(4.0) - elapsed).await;
    let elapsed = test_latency(base, rng_seed, subgraph_name.borrow()).await?;

    // Advancing another quarter period should go back to the top
    time::advance(period.div_f64(4.0) - elapsed).await;
    test_latency(base + amplitude, rng_seed, subgraph_name.borrow()).await?;

    Ok(())
}

/// Asserts that the request latency function is saw
///
/// This must be called in a test that has paused time before initializing the mock server in order to make
/// consistent assertions about the wave state.
///
/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
pub async fn assert_is_saw<T>(
    rng_seed: u64,
    base: u64,
    amplitude: u64,
    period: Duration,
    subgraph_name: T,
) -> anyhow::Result<()>
where
    T: Borrow<Option<String>>,
{
    // At t=0 seconds, our saw wave is at the bottom of the wave
    let elapsed = test_latency(base, rng_seed, subgraph_name.borrow()).await?;

    // Advancing a half period should move us halfway up the slope
    time::advance(period.div_f64(2.0) - elapsed).await;
    let elapsed = test_latency(base + amplitude / 2, rng_seed, subgraph_name.borrow()).await?;

    // Advancing another half period minus 1ms should hit the 'top'. By the nature of a saw wave, the drop from the
    // top and 0 are a straight line (effectively simultaneous). So our function will never actually hit amplitude
    // because it resets to 0 in that same tick of time.
    time::advance(period.div_f64(2.0) - elapsed - Duration::from_millis(1)).await;
    test_latency(base + amplitude - 1, rng_seed, subgraph_name.borrow()).await?;

    // Advancing 1ms should put us back at the bottom of the wave
    time::advance(Duration::from_millis(1)).await;
    test_latency(base, rng_seed, subgraph_name.borrow()).await?;

    Ok(())
}

/// Asserts that the request latency function is triangle
///
/// This must be called in a test that has paused time before initializing the mock server in order to make
/// consistent assertions about the wave state.
///
/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
pub async fn assert_is_triangle<T>(
    rng_seed: u64,
    base: u64,
    amplitude: u64,
    period: Duration,
    subgraph_name: T,
) -> anyhow::Result<()>
where
    T: Borrow<Option<String>>,
{
    // At t=0 seconds, our triangle wave is at the bottom of the wave
    let elapsed = test_latency(base, rng_seed, subgraph_name.borrow()).await?;

    // Advancing a quarter period should move us halfway up the slope
    time::advance(period.div_f64(4.0) - elapsed).await;
    let elapsed = test_latency(base + amplitude / 2, rng_seed, subgraph_name.borrow()).await?;

    // Advancing another quarter period should put us at the top
    time::advance(period.div_f64(4.0) - elapsed).await;
    let elapsed = test_latency(base + amplitude, rng_seed, subgraph_name.borrow()).await?;

    // Advancing another quarter period should put us halfway down the slope
    time::advance(period.div_f64(4.0) - elapsed).await;
    let elapsed = test_latency(base + amplitude / 2, rng_seed, subgraph_name.borrow()).await?;

    // Advancing another quarter period should put us at the bottom of the slope
    time::advance(period.div_f64(4.0) - elapsed).await;
    test_latency(base, rng_seed, subgraph_name.borrow()).await?;

    Ok(())
}
