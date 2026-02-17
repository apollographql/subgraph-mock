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
            let response = make_request(1122833, state.clone(), None).await?;
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

    assert_eq!("0.5", format!("{:.1}", header_count as f64 / 1000.0));
    assert_eq!("0.8", format!("{:.1}", non_null_count as f64 / 1000.0));

    Ok(())
}
