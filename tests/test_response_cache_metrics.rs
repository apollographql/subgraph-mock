//! Verifies that a `cache_responses`-enabled request records a
//! `subgraph_mock.response_cache.lookups` data point per outcome (hit vs. miss), not just that
//! the counter is wired up and never touched.
use anyhow::ensure;
use harness::send_request;
use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

mod harness;

const QUERY: &str = "{ posts { title } }";

/// Installing the provider before `harness::initialize` mirrors `main.rs` -- see
/// `tests/test_response_generation_metrics.rs` for why that ordering matters.
#[tokio::test]
async fn repeat_request_records_a_miss_then_a_hit() -> anyhow::Result<()> {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    global::set_meter_provider(provider.clone());

    let (_port, state) = harness::initialize(None, None)?;

    // Same query against the same state -> same `cache_hash` both times, so the first lookup is
    // a miss (nothing cached yet) and the second is a hit (`cache_responses` defaults to true).
    send_request(QUERY.to_owned(), None, state.clone(), None, true).await?;
    send_request(QUERY.to_owned(), None, state, None, true).await?;

    provider.force_flush()?;
    let finished = exporter.get_finished_metrics()?;

    let count_for_result = |result: &str| -> u64 {
        finished
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .filter(|m| m.name() == "subgraph_mock.response_cache.lookups")
            .filter_map(|m| match m.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => Some(sum),
                _ => None,
            })
            .flat_map(|sum| sum.data_points())
            .filter(|dp| {
                dp.attributes()
                    .any(|kv| kv.key.as_str() == "cache.result" && kv.value.as_str() == result)
            })
            .map(|dp| dp.value())
            .sum()
    };

    let (misses, hits) = (count_for_result("miss"), count_for_result("hit"));
    ensure!(
        misses >= 1 && hits >= 1,
        "expected at least one miss and one hit, got {misses} miss(es) and {hits} hit(s): {finished:#?}"
    );

    Ok(())
}
