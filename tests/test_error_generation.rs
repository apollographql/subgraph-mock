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
        .map(|_| async { make_request(72, state.clone(), None).await })
        .collect();

    while let Some(response) = requests.next().await {
        responses.push(response?);
    }

    let (successes, failures): (Vec<_>, Vec<_>) = responses
        .into_iter()
        .partition(|response| response.status().is_success());

    // 50% of our requests should have HTTP errors
    assert_eq!("0.5", format!("{:.1}", failures.len() as f64 / 4000.0));

    let graphql_responses: Vec<Response> = stream::iter(successes.into_iter())
        .filter_map(async |response| parse_response_with_errors(response).await.ok())
        .collect()
        .await;

    let (no_response_errors, response_errors): (Vec<_>, Vec<_>) = graphql_responses
        .into_iter()
        .partition(|response| response.data.is_some());

    // 50% of our remaining responses should have GraphQL response errors
    assert_eq!(
        "0.5",
        format!("{:.1}", response_errors.len() as f64 / 2000.0)
    );

    // 50% of the requests with no response errors should have field-level errors
    assert_eq!(
        "0.5",
        format!(
            "{:.1}",
            no_response_errors
                .into_iter()
                .filter(|response| !response.errors.is_empty())
                .count() as f64
                / 1000.0
        )
    );

    Ok(())
}
