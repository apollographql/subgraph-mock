use http_body_util::{BodyExt, Empty};
use hyper::{Request, StatusCode, body::Bytes};
use serde::Deserialize;
use serde_json_bytes::serde_json;
use subgraph_mock::handle::handle_request;

mod harness;

#[derive(Deserialize)]
struct ProbeResponse {
    status: String,
}

fn get(path: &str) -> Request<Empty<Bytes>> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Empty::new())
        .expect("building a bodyless GET request should never fail")
}

async fn probe_status(response: subgraph_mock::handle::ByteResponse) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let probe: ProbeResponse =
        serde_json::from_slice(&bytes).expect("probe response body should be JSON");
    probe.status
}

#[tokio::test]
async fn default_health_path_reports_healthy() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(None, None)?;

    let response = handle_request(get("/health"), state).await?;
    assert_eq!(StatusCode::OK, response.status());
    assert_eq!("healthy", probe_status(response).await);

    Ok(())
}

#[tokio::test]
async fn unrecognized_get_path_still_404s() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(None, None)?;

    let response = handle_request(get("/not-a-probe-path"), state.clone()).await?;
    assert_eq!(StatusCode::NOT_FOUND, response.status());

    Ok(())
}

#[tokio::test]
async fn custom_health_path_is_honored() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("custom_health_path.yaml"), None)?;

    let response = handle_request(get("/healthz"), state.clone()).await?;
    assert_eq!(StatusCode::OK, response.status());
    assert_eq!("healthy", probe_status(response).await);

    // The default path no longer resolves once the config overrides it.
    let response = handle_request(get("/health"), state).await?;
    assert_eq!(StatusCode::NOT_FOUND, response.status());

    Ok(())
}
