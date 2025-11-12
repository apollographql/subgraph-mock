use std::time::Duration;
use tokio::time::{self, Instant};

use crate::harness::make_request;

mod harness;

/// This test must be the first test in the file because it manipulates the latency generator
/// and the passage of time. To do this correctly, it must own the process of initializing the
/// test harness and thus must be the first test to run.
///
/// The default latency generator is a sine wave with a base value of 5 ms, an amplitude of 2,
/// and a period of 10 seconds.
#[tokio::test(start_paused = true)]
async fn test_default_latency_and_port() -> anyhow::Result<()> {
    let port = harness::initialize(None).unwrap();
    assert_eq!(port, 8080);

    // At t=0 seconds, our sine wave is halfway up its amplitude
    let elapsed = test_latency(6).await?;

    // Advancing half a period should put us at the same latency on the other side of the wave
    time::advance(Duration::from_secs(5) - elapsed).await;
    let elapsed = test_latency(6).await?;

    // Advancing a quarter period should put us at the bottom
    time::advance(Duration::from_millis(2500) - elapsed).await;
    let elapsed = test_latency(5).await?;

    // Advancing a half period should put us at the top
    time::advance(Duration::from_secs(5) - elapsed).await;
    test_latency(7).await?;

    Ok(())
}

async fn test_latency(expected: u64) -> anyhow::Result<Duration> {
    let start = Instant::now();
    let response = make_request(0, None).await?;
    assert_eq!(200, response.status());
    let elapsed = start.elapsed();
    assert_eq!(Duration::from_millis(expected), elapsed);

    Ok(elapsed)
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
