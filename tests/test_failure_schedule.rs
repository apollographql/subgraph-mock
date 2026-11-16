//! Verifies `failure_schedule` against paused time: when windows open and close, which requests
//! they fail, and that nothing (the response cache included) lets a request through a window.
//!
//! For details on how paused time works, see
//! https://tokio.rs/tokio/topics/testing#pausing-and-resuming-time-in-tests
use harness::send_request;
use http_body_util::BodyExt;
use std::sync::Arc;
use subgraph_mock::state::State;
use tokio::time::{self, Duration, Instant};

mod harness;

const QUERY: &str = "{ posts { title } }";
const SPECIAL: &str = "special_subgraph";

/// Sends [QUERY] and returns the response status.
async fn status(state: &Arc<State>, subgraph: Option<&str>) -> anyhow::Result<u16> {
    let response = send_request(
        QUERY.to_owned(),
        None,
        state.clone(),
        subgraph.map(str::to_owned),
        true,
    )
    .await?;
    Ok(response.status().as_u16())
}

/// Advances paused time to `offset` after `clock_start`.
async fn advance_to(clock_start: Instant, offset: Duration) {
    time::advance((clock_start + offset).saturating_duration_since(Instant::now())).await;
}

fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

#[tokio::test(start_paused = true)]
async fn window_opens_at_start_closes_after_duration_and_repeats() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("failure_schedule.yaml"), None)?;

    let clock_start = Instant::now();
    assert_eq!(status(&state, Some(SPECIAL)).await?, 200);

    advance_to(clock_start, secs(59)).await;
    assert_eq!(status(&state, Some(SPECIAL)).await?, 200);

    advance_to(clock_start, secs(60)).await;
    let response = send_request(
        QUERY.to_owned(),
        None,
        state.clone(),
        Some(SPECIAL.to_owned()),
        true,
    )
    .await?;
    assert_eq!(response.status(), 503);
    assert!(response.into_body().collect().await?.to_bytes().is_empty());

    advance_to(clock_start, secs(90)).await;
    assert_eq!(status(&state, Some(SPECIAL)).await?, 200);

    advance_to(clock_start, secs(150)).await;
    assert_eq!(status(&state, Some(SPECIAL)).await?, 503);

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn other_subgraphs_keep_succeeding_during_a_window() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("failure_schedule.yaml"), None)?;

    let clock_start = Instant::now();
    assert_eq!(status(&state, Some(SPECIAL)).await?, 200);

    advance_to(clock_start, secs(70)).await;
    assert_eq!(status(&state, Some(SPECIAL)).await?, 503);
    assert_eq!(status(&state, Some("other_subgraph")).await?, 200);
    assert_eq!(status(&state, None).await?, 200);

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn clock_starts_at_the_first_request_not_at_startup() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("failure_schedule.yaml"), None)?;

    // Idle time before the first request doesn't count towards `start`.
    time::advance(secs(120)).await;

    // The first request to any subgraph starts the clock for every schedule.
    let clock_start = Instant::now();
    assert_eq!(status(&state, Some("other_subgraph")).await?, 200);

    advance_to(clock_start, secs(59)).await;
    assert_eq!(status(&state, Some(SPECIAL)).await?, 200);

    advance_to(clock_start, secs(60)).await;
    assert_eq!(status(&state, Some(SPECIAL)).await?, 503);

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn delay_holds_the_response_then_succeeds_with_latency_on_top() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("failure_schedule.yaml"), None)?;

    // `slow_subgraph` delays every request by 2s for the first 10s, and the fixture's base
    // latency of 5ms still applies afterwards.
    let clock_start = Instant::now();
    assert_eq!(status(&state, Some("slow_subgraph")).await?, 200);
    assert_eq!(clock_start.elapsed(), Duration::from_millis(2005));

    advance_to(clock_start, secs(10)).await;
    let before = Instant::now();
    assert_eq!(status(&state, Some("slow_subgraph")).await?, 200);
    assert_eq!(before.elapsed(), Duration::from_millis(5));

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn http_status_failure_still_waits_for_configured_latency() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("failure_schedule.yaml"), None)?;

    let clock_start = Instant::now();
    status(&state, Some(SPECIAL)).await?;
    advance_to(clock_start, secs(60)).await;

    let before = Instant::now();
    assert_eq!(status(&state, Some(SPECIAL)).await?, 503);
    assert_eq!(before.elapsed(), Duration::from_millis(5));

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn null_override_switches_off_the_top_level_schedule() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("failure_schedule_disabled_override.yaml"), None)?;

    assert_eq!(status(&state, None).await?, 503);
    assert_eq!(status(&state, Some("any_subgraph")).await?, 503);
    assert_eq!(status(&state, Some("healthy_subgraph")).await?, 200);
    assert_eq!(state.config.read().await.failure_schedules().len(), 1);

    Ok(())
}

#[tokio::test(start_paused = true)]
async fn override_without_the_key_inherits_without_adding_a_schedule() -> anyhow::Result<()> {
    let (_, state) = harness::initialize(Some("failure_schedule_inherited_override.yaml"), None)?;

    assert_eq!(status(&state, Some("headers_only_subgraph")).await?, 503);

    // Only the top-level schedule is listed, so the gauge and window logs report it once.
    let schedules = state.config.read().await.failure_schedules();
    assert_eq!(schedules.len(), 1);
    assert_eq!(schedules[0].0, None);

    Ok(())
}
