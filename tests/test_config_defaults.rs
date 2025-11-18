use std::time::Duration;

use crate::harness::{assert_is_sine, make_request};

mod harness;

/// This test must be the first test in the file because it manipulates the latency generator
/// and the passage of time. To do this correctly, it must own the process of initializing the
/// test harness and thus must be the first test to run.
///
/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
#[tokio::test(start_paused = true)]
async fn test_default_latency_and_port() -> anyhow::Result<()> {
    let port = harness::initialize(None)?;
    let rng_seed = 0;
    let subgraph_name = None;
    assert_eq!(port, 8080);

    // The default latency generator is a sine wave with a base value of 5 ms, an amplitude of 2,
    // and a period of 10 seconds.
    assert_is_sine(rng_seed, 5, 2, Duration::from_secs(10), subgraph_name).await
}

#[tokio::test]
async fn test_default_headers() -> anyhow::Result<()> {
    let response = make_request(42, None).await?;
    let headers = response.headers();

    assert_eq!(200, response.status());
    assert_eq!(1, headers.len());

    assert!(headers.contains_key("content-type"));
    Ok(())
}
