#!/bin/sh
# Drives traffic against subgraph-mock, stops it to force a telemetry flush, then reads its
# captured stdout back to verify what it actually recorded -- see scenario.yaml/README.md.
set -eu

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

# This script's only reliable link to subgraph-mock is the docker socket (also used below for
# `docker stop`/`docker logs`) -- it has no guaranteed network-namespace relationship to
# wherever subgraph-mock's ports actually land (under CI, `rtf` itself runs inside a wrapper
# container that shares the docker socket with the real host but not its network). So HTTP
# traffic goes through `curl-sidecar` (data/docker-compose.yaml), a container that's already on
# subgraph-mock's own compose network, via `docker exec` instead of a direct curl.
request() {
  docker exec curl-sidecar curl "$@"
}

echo "sending plain request (expect a fresh root span, no parent)"
plain_status=$(request -s -o /dev/null -w '%{http_code}' -X POST "$GRAPHQL_URL" \
  -H 'Content-Type: application/json' \
  -d '{"query":"{ __typename }"}')
[ "$plain_status" = "200" ] || fail "plain request failed: expected 200, got $plain_status"

echo "sending request with a fixed traceparent (expect a parented span)"
traced_status=$(request -s -o /dev/null -w '%{http_code}' -X POST "$GRAPHQL_URL" \
  -H 'Content-Type: application/json' \
  -H "traceparent: $TRACEPARENT" \
  -d '{"query":"{ __typename }"}')
[ "$traced_status" = "200" ] || fail "traceparent request failed: expected 200, got $traced_status"

# Stops subgraph-mock now (sends SIGTERM) instead of waiting for RTF's own environment teardown
# to do it after this script exits. Two reasons: it forces subgraph-mock's buffered telemetry
# (this test plan deliberately uses realistic `batch`/default-interval settings, not a
# workaround-shortened one) to flush via graceful shutdown, and it makes the log read below
# deterministic instead of racing a batch/interval timer against this script's own execution.
echo "stopping $SUBGRAPH_CONTAINER to force its telemetry to flush"
docker stop "$SUBGRAPH_CONTAINER" >/dev/null

log_file="$OUTDIR/subgraph-mock.log"
echo "saving subgraph-mock's captured log to $log_file"
docker logs "$SUBGRAPH_CONTAINER" >"$log_file" 2>&1

echo "verifying captured log"
[ -s "$log_file" ] || fail "log file is empty: $log_file"

grep -q '"subgraph.name"' "$log_file" \
  || fail "no span carries a subgraph.name attribute at all"
grep -q '"otel-verification"' "$log_file" \
  || fail "no span's subgraph.name matches the route this scenario hit (otel-verification)"

grep -q '4bf92f3577b34da6a3ce929d0e0e4736' "$log_file" \
  || fail "no span carries the fixed traceparent's trace ID -- propagation is likely broken"
grep -q '"parentSpanId":"00f067aa0ba902b7"' "$log_file" \
  || fail "no span parents under the fixed traceparent's span ID -- check propagation/telemetry layer order in mock_server_loop"

grep -q '"traceId":"[0-9a-f]\{32\}","spanId":"[0-9a-f]\{16\}","traceState":"","parentSpanId":""' "$log_file" \
  || fail "no root span (empty parentSpanId) found -- the plain request should have produced one"

grep -q 'http.server.request.duration' "$log_file" \
  || fail "no http.server.request.duration metric found"
grep -q 'http.server.active_requests' "$log_file" \
  || fail "no http.server.active_requests metric found"
grep -q 'http.server.request.body.size' "$log_file" \
  || fail "no http.server.request.body.size metric found -- check telemetry.http defaults"
grep -q 'http.server.response.body.size' "$log_file" \
  || fail "no http.server.response.body.size metric found -- check telemetry.http defaults"

echo "OK: subgraph-mock's captured log contains the expected spans and metrics"
