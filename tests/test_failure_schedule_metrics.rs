//! Verifies that `subgraph_mock.failure_schedule.active` follows the failure window, and that a
//! request inside the window never reaches the response cache. Kept in its own binary because it
//! installs a global meter provider, which other tests' requests would otherwise record into.
use harness::send_request;
use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use std::sync::Arc;
use subgraph_mock::state::State;
use tokio::time::{self, Duration, Instant};

mod harness;

const QUERY: &str = "{ posts { title } }";
const SPECIAL: &str = "special_subgraph";

async fn status(state: &Arc<State>, subgraph: &str) -> anyhow::Result<u16> {
    let response = send_request(
        QUERY.to_owned(),
        None,
        state.clone(),
        Some(subgraph.to_owned()),
        true,
    )
    .await?;
    Ok(response.status().as_u16())
}

fn secs(secs: u64) -> Duration {
    Duration::from_secs(secs)
}

/// Installing the provider before initializing mirrors `main.rs`; see
/// `tests/test_response_generation_metrics.rs` for why that ordering matters.
#[tokio::test(start_paused = true)]
async fn gauge_tracks_the_window_and_the_cache_is_never_consulted_inside_it() -> anyhow::Result<()>
{
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    global::set_meter_provider(provider.clone());

    let (_, state) = harness::initialize_owned(Some("failure_schedule_cached.yaml"), None)?;
    let state = Arc::new(state.with_failure_schedule_metrics());

    let gauge = |subgraph: &str| -> anyhow::Result<Option<u64>> {
        provider.force_flush()?;
        let finished = exporter.get_finished_metrics()?;
        Ok(finished
            .last()
            .into_iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .filter(|m| m.name() == "subgraph_mock.failure_schedule.active")
            .filter_map(|m| match m.data() {
                AggregatedMetrics::U64(MetricData::Gauge(gauge)) => Some(gauge),
                _ => None,
            })
            .flat_map(|gauge| gauge.data_points())
            .find(|dp| {
                dp.attributes()
                    .any(|kv| kv.key.as_str() == "subgraph.name" && kv.value.as_str() == subgraph)
            })
            .map(|dp| dp.value()))
    };
    let cache_lookups = || -> anyhow::Result<u64> {
        provider.force_flush()?;
        let finished = exporter.get_finished_metrics()?;
        Ok(finished
            .last()
            .into_iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .filter(|m| m.name() == "subgraph_mock.response_cache.lookups")
            .filter_map(|m| match m.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => Some(sum),
                _ => None,
            })
            .flat_map(|sum| sum.data_points())
            .map(|dp| dp.value())
            .sum())
    };
    // Lets the once-a-second gauge poller sample the current state.
    let sample = || time::sleep(secs(1));

    // Caches the response for QUERY before the window opens.
    let clock_start = Instant::now();
    assert_eq!(status(&state, SPECIAL).await?, 200);
    sample().await;
    assert_eq!(gauge(SPECIAL)?, Some(0));
    let lookups_before_window = cache_lookups()?;

    time::advance(clock_start + secs(60) - Instant::now()).await;
    sample().await;
    assert_eq!(gauge(SPECIAL)?, Some(1));
    assert_eq!(status(&state, SPECIAL).await?, 503);
    assert_eq!(cache_lookups()?, lookups_before_window);

    time::advance(clock_start + secs(90) - Instant::now()).await;
    sample().await;
    assert_eq!(gauge(SPECIAL)?, Some(0));

    Ok(())
}
