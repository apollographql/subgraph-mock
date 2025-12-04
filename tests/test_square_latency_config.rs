use harness::assert_is_square;
use tokio::time::Duration;

mod harness;

/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
#[tokio::test(start_paused = true)]
async fn square_wave() -> anyhow::Result<()> {
    harness::initialize(Some("square_wave.yaml"))?;
    let rng_seed = 12;

    // The configured latency generator is a square wave with a base value of 10 ms, an amplitude of 5ms,
    // and a period of 20 seconds.
    assert_is_square(rng_seed, 10, 5, Duration::from_secs(20), None).await
}
