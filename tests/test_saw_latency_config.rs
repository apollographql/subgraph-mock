use harness::assert_is_saw;
use tokio::time::Duration;

mod harness;

/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
#[tokio::test(start_paused = true)]
async fn saw_wave() -> anyhow::Result<()> {
    harness::initialize(Some("saw_wave.yaml"))?;
    let rng_seed = 12;

    // The configured latency generator is a saw wave with a base value of 10 ms, an amplitude of 20ms,
    // and a period of 10 seconds.
    assert_is_saw(rng_seed, 10, 20, Duration::from_secs(10), None).await
}
