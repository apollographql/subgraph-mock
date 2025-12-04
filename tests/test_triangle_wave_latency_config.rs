use harness::assert_is_triangle;
use tokio::time::Duration;

mod harness;

/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
#[tokio::test(start_paused = true)]
async fn triangle_wave() -> anyhow::Result<()> {
    harness::initialize(Some("triangle_wave.yaml"))?;
    let rng_seed = 20;

    // The configured latency generator is a triangle wave with a base value of 0 ms, an amplitude of 10ms,
    // and a period of 10 seconds.
    assert_is_triangle(rng_seed, 0, 10, Duration::from_secs(10), None).await
}
