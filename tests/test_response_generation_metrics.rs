//! Verifies that handling a request actually records a
//! `subgraph_mock.response_generation.duration` data point, not just that the histogram is wired
//! up and never touched.
use anyhow::ensure;
use harness::make_request;
use opentelemetry::global;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

mod harness;

/// Installing the provider *before* [harness::initialize] mirrors `main.rs`, which calls
/// `Telemetry::builder(...).build()` (installing the real global meter provider) before
/// `State::with_response_generation_duration` rebuilds the histogram against it. See that
/// method's docs for why a histogram built against whatever provider is active at construction
/// time silently stops recording if the global provider changes afterward.
#[tokio::test]
async fn generate_body_records_response_generation_duration() -> anyhow::Result<()> {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    global::set_meter_provider(provider.clone());

    let (_port, state) = harness::initialize(None, None)?;
    make_request(0, state, None).await?;

    provider.force_flush()?;
    let finished = exporter.get_finished_metrics()?;

    let metric = finished
        .iter()
        .flat_map(|rm| rm.scope_metrics())
        .flat_map(|sm| sm.metrics())
        .find(|m| m.name() == "subgraph_mock.response_generation.duration");

    let Some(metric) = metric else {
        anyhow::bail!(
            "expected a subgraph_mock.response_generation.duration metric, found: {:#?}",
            finished
        );
    };

    let AggregatedMetrics::F64(MetricData::Histogram(histogram)) = metric.data() else {
        anyhow::bail!("expected an f64 histogram, got: {:#?}", metric.data());
    };

    let total_count: u64 = histogram.data_points().map(|dp| dp.count()).sum();
    ensure!(
        total_count >= 1,
        "expected at least one recorded data point, got: {:#?}",
        histogram
    );

    Ok(())
}
