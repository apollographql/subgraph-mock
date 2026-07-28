use futures::{
    StreamExt,
    stream::{self, FuturesUnordered},
};
use harness::{Response, make_request, parse_response_with_errors};

mod harness;

#[tokio::test(flavor = "multi_thread")]
async fn error_ratios() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("error_ratios.yaml"), None)?;

    let mut responses = Vec::with_capacity(4000);
    let mut requests: FuturesUnordered<_> = (0..4000)
        .map(|_| async { make_request(54167, state.clone(), None).await })
        .collect();

    while let Some(response) = requests.next().await {
        responses.push(response?);
    }

    let (successes, failures): (Vec<_>, Vec<_>) = responses
        .into_iter()
        .partition(|response| response.status().is_success());

    // 1/2 of 4000, seeded for determinism by the harness
    assert_eq!(2046, failures.len());

    let graphql_responses: Vec<Response> = stream::iter(successes.into_iter())
        .filter_map(async |response| parse_response_with_errors(response).await.ok())
        .collect()
        .await;

    let (no_response_errors, response_errors): (Vec<_>, Vec<_>) = graphql_responses
        .into_iter()
        .partition(|response| response.data.is_some());

    let field_errors_len = no_response_errors
        .into_iter()
        .filter(|response| !response.errors.is_empty())
        .count();

    // 1/2 of 2000, seeded for determinism by the harness
    assert_eq!(1015, response_errors.len());
    // 1/2 of 1000, seeded for determinism by the harness
    assert_eq!(484, field_errors_len);

    Ok(())
}
