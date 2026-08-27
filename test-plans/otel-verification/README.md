# otel-verification

Verifies subgraph-mock's OTel telemetry actually gets recorded correctly - propagation, span
nesting, `subgraph.name` enrichment, the HTTP-layer body-size metrics, and subgraph-mock's own
response-generation/cache metrics - without a live OTel Collector, and fully locally (will not run
via the orchestrator).

subgraph-mock's `telemetry.otel` is configured with the `console` exporter, so telemetry data is
written directly to the container's stdout instead of exported over the network. The scenario
(`scripts/verify-otel.sh`) drives two requests, stops subgraph-mock's container to force its
telemetry to flush, reads its captured log back with `docker logs`, and greps it - all in one
script, no separate pull-and-verify step required.

## Running

```bash
docker build -t local-subgraph-mock .
rtf run test-plans/otel-verification/test-plan.yaml
```

The captured log lands in this run's output directory (`output/subgraph-mock.log` by default) if you
want to inspect it yourself afterward.

## Why the scenario stops subgraph-mock itself, mid-run

`telemetry.otel` here deliberately uses realistic settings - a `batch` span processor and the
default 60s metrics interval, not shortened for the test's convenience. Waiting on either to fire
naturally, or on the whole environment's teardown afterward, would make this test slow and racy.
Instead, `verify-otel.sh` calls `docker stop subgraph-mock` itself right after sending its two
requests, forcing an immediate flush via subgraph-mock's graceful shutdown handling (SIGTERM is
caught, `mock_server_loop` drains in-flight connections and returns normally instead of the process
being killed, so `Telemetry`'s `Drop` - which flushes buffered spans/metrics - actually runs), then
reads the log right away. Deterministic, and it doubles as a live check that graceful shutdown
itself keeps working, not just this test's own assertions about spans and metrics.
