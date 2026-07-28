use anyhow::ensure;
use futures::{
    StreamExt,
    stream::{self, FuturesUnordered},
};
use harness::{make_request, parse_response};

mod harness;

#[tokio::test(flavor = "multi_thread")]
async fn custom_ratios() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("custom_ratios.yaml"), None)?;

    let mut responses = Vec::with_capacity(1000);
    let mut requests: FuturesUnordered<_> = (0..1000)
        .map(|_| async {
            let response = make_request(54167, state.clone(), None).await?;
            ensure!(200 == response.status());
            Ok(response)
        })
        .collect();

    while let Some(response) = requests.next().await {
        responses.push(response?);
    }

    let header_count = responses
        .iter()
        .filter_map(|response| response.headers().get("sometimes-present"))
        .count();

    let non_null_count = stream::iter(responses)
        .filter_map(async |response| {
            parse_response(response)
                .await
                .ok()
                .and_then(|query| query.user)
        })
        .count()
        .await;

    // the default header is 1/2, outcome seeded for determinism by the harness
    assert_eq!(539, header_count);
    // the default null ratio is 1/5, outcome seeded for determinism by the harness
    assert_eq!(815, non_null_count);

    Ok(())
}
