use harness::{assert_is_saw, assert_is_square, assert_is_triangle};
use tokio::time::Duration;

mod harness;

/// For details on how paused time works, see
/// https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
#[tokio::test(start_paused = true)]
async fn saw_wave() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("saw_wave.yaml"))?;
    let rng_seed = 12;

    // The configured latency generator is a saw wave with a base value of 10 ms, an amplitude of 20ms,
    // and a period of 10 seconds.
    assert_is_saw(10, 20, Duration::from_secs(10), rng_seed, state, None).await
}

#[tokio::test(start_paused = true)]
async fn square_wave() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("square_wave.yaml"))?;
    let rng_seed = 12;

    // The configured latency generator is a square wave with a base value of 10 ms, an amplitude of 5ms,
    // and a period of 20 seconds.
    assert_is_square(10, 5, Duration::from_secs(20), rng_seed, state, None).await
}

#[tokio::test(start_paused = true)]
async fn triangle_wave() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("triangle_wave.yaml"))?;
    let rng_seed = 20;

    // The configured latency generator is a triangle wave with a base value of 0 ms, an amplitude of 10ms,
    // and a period of 10 seconds.
    assert_is_triangle(0, 10, Duration::from_secs(10), rng_seed, state, None).await
}
