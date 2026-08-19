use crate::error::Result;
use apollo_opentelemetry::default_instrumentation_scope;
use apollo_opentelemetry::metrics::{GaugeExt, PollGuard};
use notify::{Config as NotifyConfig, Event, EventKind, PollWatcher, RecursiveMode, Watcher};
use opentelemetry::KeyValue;
use opentelemetry::global::meter_with_scope;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tracing::error;

/// How often the `subgraph_mock.cache.size` gauge samples the internal caches' entry counts.
const CACHE_SIZE_POLL_INTERVAL: Duration = Duration::from_secs(30);

mod config;
mod rng;
mod schema;

pub use config::Config;
pub use config::TelemetryConfig;
pub use config::default_port;
pub use rng::RngSource;
pub use schema::{FederatedSchema, SchemaError};

use schema::update_schema;

pub struct State {
    pub config: Arc<RwLock<Config>>,
    pub schema: Arc<RwLock<FederatedSchema>>,
    pub rng: RngSource,
    /// Time spent generating a response body
    pub response_generation_duration: Histogram<f64>,
    /// Count of `into_response_bytes_and_status_code` cache lookups
    pub response_cache_lookups: Counter<u64>,
    /// Keeps the [GaugeExt::poll] background task that samples cache entry counts into
    /// `subgraph_mock.cache.size` alive for as long as this `State` is
    _cache_size_poll_guard: Option<PollGuard>,
    /// Handle to the pollwatcher that updates the schema for this config, so that it only drops out of scope when this state does
    _schema_watcher: PollWatcher,
}

impl State {
    pub fn new(config: Config, schema_path: PathBuf) -> Result<Self> {
        let schema = FederatedSchema::parse(&schema_path)?;
        let schema = Arc::new(RwLock::new(schema));

        let lock = schema.clone();
        // We have to use a PollWatcher because Docker on MacOS doesn't support filesystem events:
        // https://docs.rs/notify/8.2.0/notify/index.html#docker-with-linux-on-macos-m1
        let mut schema_watcher = PollWatcher::new(
            move |res: std::result::Result<Event, _>| match res {
                Ok(event) => {
                    if let EventKind::Modify(_) = event.kind
                        && let Some(path) = event.paths.first()
                        && let Err(err) = update_schema(path, lock.clone())
                    {
                        error!("Failed to reload schema: {}", err);
                    }
                }
                Err(errors) => {
                    error!("Error watching schema file: {:?}", errors)
                }
            },
            NotifyConfig::default()
                .with_poll_interval(Duration::from_secs(1))
                .with_compare_contents(true),
        )
        .map_err(SchemaError::from)?;
        schema_watcher
            .watch(&schema_path, RecursiveMode::NonRecursive)
            .map_err(SchemaError::from)?;

        Ok(Self {
            config: Arc::new(RwLock::new(config)),
            schema,
            rng: RngSource::default(),
            response_generation_duration: response_generation_duration_histogram(),
            response_cache_lookups: response_cache_lookups_counter(),
            _cache_size_poll_guard: None,
            _schema_watcher: schema_watcher,
        })
    }

    pub fn default(schema_path: PathBuf) -> Result<Self> {
        Self::new(Config::default(), schema_path)
    }

    pub fn with_rng(mut self, rng: RngSource) -> Self {
        self.rng = rng;
        self
    }

    /// Rebuilds [response_generation_duration](State::response_generation_duration) from
    /// whichever [MeterProvider](opentelemetry::metrics::MeterProvider) is globally installed at
    /// the time this is called.
    pub fn with_response_generation_duration(mut self) -> Self {
        self.response_generation_duration = response_generation_duration_histogram();

        self
    }

    /// Rebuilds [response_cache_lookups](State::response_cache_lookups) and (re)starts the
    /// `subgraph_mock.cache.size` poller against whichever meter provider is currently installed
    pub fn with_cache_metrics(mut self) -> Self {
        self.response_cache_lookups = response_cache_lookups_counter();

        let gauge = meter()
            .i64_gauge("subgraph_mock.cache.size")
            .with_description(
                "Current entry count of subgraph-mock's internal caches, tagged by `cache` name. \
                 None of them evict, so steady growth over a run's lifetime signals unbounded \
                 memory use rather than expected cache reuse.",
            )
            .build();
        self._cache_size_poll_guard = Some(gauge.poll(CACHE_SIZE_POLL_INTERVAL, |observer| {
            for (cache, size) in crate::handle::graphql::cache_sizes() {
                observer.observe(size, &[KeyValue::new("cache", cache)]);
            }
        }));

        self
    }
}

fn meter() -> Meter {
    meter_with_scope(default_instrumentation_scope!().clone())
}

/// Builds the `subgraph_mock.response_generation.duration` histogram from the currently
/// installed global meter provider.
fn response_generation_duration_histogram() -> Histogram<f64> {
    meter()
        .f64_histogram("subgraph_mock.response_generation.duration")
        .with_description(
            "Time spent generating a subgraph mock response body: parsing, validating, and \
             building it. Excludes cache hits and injected latency.",
        )
        .with_unit("s")
        .build()
}

/// Builds the `subgraph_mock.response_cache.lookups` counter from the currently installed
/// global meter provider.
fn response_cache_lookups_counter() -> Counter<u64> {
    meter()
        .u64_counter("subgraph_mock.response_cache.lookups")
        .with_description(
            "Count of response-cache lookups (hit vs. miss) for `cache_responses`-enabled \
             subgraphs. A miss means a lookup actually paid the response-generation cost \
             recorded by subgraph_mock.response_generation.duration; a hit means it didn't.",
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::global;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    /// Proves the exact hazard [State::with_response_generation_duration]'s docs warn about: a
    /// histogram obtained from the global meter provider stays bound to whichever provider was
    /// active *at that moment*, so building it before the real provider is installed leaves it a
    /// permanent no-op — refreshing it after the fact is what actually fixes that, not merely
    /// installing the provider sooner.
    ///
    /// This test mutates the process-global meter provider. That's safe today because no other
    /// test in this binary (`subgraph_mock`'s unit tests) touches it, but it would race with one
    /// that did, since `cargo test` runs unit tests in parallel within one process.
    #[test]
    fn refreshing_after_provider_install_is_required_to_record() {
        // Mirrors `State::new` running before `main.rs` calls `Telemetry::builder(...).build()`:
        // only the SDK's no-op provider is installed when this histogram is built.
        let stale = response_generation_duration_histogram();

        // Mirrors `Telemetry::builder(...).build()` installing the real provider afterward.
        let exporter = InMemoryMetricExporter::default();
        let provider = SdkMeterProvider::builder()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        global::set_meter_provider(provider.clone());

        // The already-built histogram doesn't pick up the new provider...
        stale.record(1.0, &[]);
        // ...only a histogram built via `with_response_generation_duration` (i.e. built again,
        // now that the real provider is installed) does.
        let fresh = response_generation_duration_histogram();
        fresh.record(2.0, &[]);

        provider.force_flush().expect("flush should succeed");
        let finished = exporter
            .get_finished_metrics()
            .expect("export should succeed");

        let total_count: u64 = finished
            .iter()
            .flat_map(|rm| rm.scope_metrics())
            .flat_map(|sm| sm.metrics())
            .filter(|m| m.name() == "subgraph_mock.response_generation.duration")
            .filter_map(|m| match m.data() {
                AggregatedMetrics::F64(MetricData::Histogram(histogram)) => Some(histogram),
                _ => None,
            })
            .flat_map(|histogram| histogram.data_points())
            .map(|data_point| data_point.count())
            .sum();

        assert_eq!(
            total_count, 1,
            "only the histogram built after the provider was installed should have recorded"
        );
    }
}
