#!/bin/sh
# Sends a single GraphQL request to the router, then asserts via mock-studio's
# /request-stats endpoint that the router received and correctly decoded the subgraph's
# FTV1 trace, with no anomalies. See subgraph-mock's SPEC_ftv1.md (emission) and
# mock-studio's SPEC_mock-studio-ftv1.md (the counters this checks).
#
# Checks POST /v1/traces (OTLP), not /studio: otlp_tracing_sampler is on in this test
# plan's router config, so the raw FTV1 trace routes there instead of the legacy Report -
# see mock-studio's README, "/studio vs /v1/traces are mutually exclusive for FTV1".
#
# This is a wiring/correctness check, not a load test: field_level_instrumentation_sampler
# is set to always_on in this test plan's router config, so a single request is enough to
# make FTV1 arrival deterministic instead of sampled - n_ftv1_reports=0 is a failure here,
# not something to skip past.
set -eu

command -v jq > /dev/null || { echo >&2 "jq is required to run this script"; exit 1; }

# Spans both subgraphs (posts + users): `bio` is owned by users but `@requires` posts.content,
# which is owned by posts, forcing a multi-fetch query plan (sequence/flatten, not just a bare
# fetch) - see mock-studio's SPEC_mock-studio-ftv1.md on why that recursion needs exercising.
QUERY_BODY='{"query":"{ users { id name email bio address { city country } posts { id title views } } }"}'

echo "resetting mock-studio request-stats before the call"
curl -fsS -X DELETE "$MOCK_STUDIO_URL/request-stats" > /dev/null

echo "sending GraphQL request to the router at $GRAPHQL_URL"
response="$(curl -fsS -X POST "$GRAPHQL_URL" \
  -H 'Content-Type: application/json' \
  --retry 5 --retry-delay 2 --retry-connrefused \
  -d "$QUERY_BODY")"

if echo "$response" | jq -e '.errors' > /dev/null 2>&1; then
  echo >&2 "FAIL: router returned GraphQL errors: $(echo "$response" | jq -c '.errors')"
  exit 1
fi

# telemetry.apollo.tracing.batch_processor.scheduled_delay is 1s in this test plan's router
# config, but leave a safety margin for the export round-trip to mock-studio.
echo "waiting for the router's OTLP batch span processor to flush"
sleep 10

echo "querying mock-studio request-stats"
stats_json="$(curl -fsS "$MOCK_STUDIO_URL/request-stats")"
echo "$stats_json"

if [ -n "${OUTDIR:-}" ] && mkdir -p "$OUTDIR/results" 2> /dev/null; then
  echo "$stats_json" > "$OUTDIR/results/mock-studio-request-stats.json"
fi

n_otlp_calls="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_calls // 0')"
n_root_spans="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_root_spans // 0')"
n_ftv1_reports="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_ftv1_reports // 0')"
n_ftv1_trace_parsing_failed="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_ftv1_trace_parsing_failed // 0')"
n_ftv1_nodes="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_ftv1_nodes // 0')"
n_ftv1_bad_type_nodes="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_ftv1_bad_type_nodes // 0')"
n_ftv1_timing_inversions="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_ftv1_timing_inversions // 0')"
n_ftv1_index_nodes="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_ftv1_index_nodes // 0')"
n_ftv1_errors="$(echo "$stats_json" | jq -r '.["POST /v1/traces"].n_ftv1_errors // 0')"

echo "POST /v1/traces n_calls=$n_otlp_calls n_root_spans=$n_root_spans n_ftv1_reports=$n_ftv1_reports n_ftv1_trace_parsing_failed=$n_ftv1_trace_parsing_failed n_ftv1_nodes=$n_ftv1_nodes n_ftv1_bad_type_nodes=$n_ftv1_bad_type_nodes n_ftv1_timing_inversions=$n_ftv1_timing_inversions n_ftv1_index_nodes=$n_ftv1_index_nodes n_ftv1_errors=$n_ftv1_errors"

failures=0

# Every field RequestStats can ever serialize (mock-studio's src/stats.rs) - asserted present,
# numeric, and non-negative. This catches mock-studio's own bugs (a crashed decode, a broken
# serde rename, a missing match arm) independent of anything the router/subgraph sent - the
# `// 0` defaults above would silently paper over a missing field, so this checks the raw entry
# instead of the already-defaulted shell variables.
entry="$(echo "$stats_json" | jq -c '.["POST /v1/traces"] // empty')"
if [ -z "$entry" ]; then
  echo >&2 "FAIL: mock-studio's /request-stats has no entry at all for \"POST /v1/traces\""
  failures=$((failures + 1))
else
  for field in n_calls n_bytes n_spans n_root_spans n_ftv1_reports n_ftv1_trace_parsing_failed \
    n_ftv1_nodes n_ftv1_bad_type_nodes n_ftv1_timing_inversions n_ftv1_index_nodes n_ftv1_errors; do
    ok="$(echo "$entry" | jq --arg f "$field" '(has($f)) and (.[$f] | type == "number") and (.[$f] >= 0)')"
    if [ "$ok" != "true" ]; then
      echo >&2 "FAIL: \"POST /v1/traces\".$field is missing, non-numeric, or negative in mock-studio's stats"
      failures=$((failures + 1))
    fi
  done
fi

if [ "$n_root_spans" -le 0 ]; then
  echo >&2 "FAIL: expected the router to send OTLP trace spans to mock-studio's /v1/traces, got n_root_spans=$n_root_spans"
  echo >&2 "  Check telemetry.apollo.otlp_tracing_sampler / experimental_otlp_endpoint / experimental_otlp_tracing_protocol in the router config."
  failures=$((failures + 1))
fi

if [ "$n_ftv1_reports" -gt 0 ]; then
  # Every decoded report has at least a root node, so nodes can never be fewer than reports.
  # A violation here means mock-studio's own FTV1 walk is under-counting, not that the subgraph
  # sent something unusual.
  if [ "$n_ftv1_nodes" -lt "$n_ftv1_reports" ]; then
    echo >&2 "FAIL: mock-studio recorded $n_ftv1_reports FTV1 report(s) but only $n_ftv1_nodes total node(s) - every report has at least a root node"
    failures=$((failures + 1))
  fi

  if [ "$n_ftv1_trace_parsing_failed" -ne 0 ]; then
    echo >&2 "FAIL: the router failed to parse a subgraph's FTV1 trace, n_ftv1_trace_parsing_failed=$n_ftv1_trace_parsing_failed"
    failures=$((failures + 1))
  fi

  if [ "$n_ftv1_bad_type_nodes" -ne 0 ]; then
    echo >&2 "FAIL: found FTV1 node(s) with an empty type or parent_type, n_ftv1_bad_type_nodes=$n_ftv1_bad_type_nodes"
    echo >&2 "  Check mock-studio's server-side logs for the offending service_name/response_name."
    failures=$((failures + 1))
  fi

  if [ "$n_ftv1_timing_inversions" -ne 0 ]; then
    echo >&2 "FAIL: found FTV1 node(s) with an inverted or out-of-bounds timing span, n_ftv1_timing_inversions=$n_ftv1_timing_inversions"
    echo >&2 "  Check mock-studio's server-side logs for the offending service_name/response_name."
    failures=$((failures + 1))
  fi

  if [ "$n_ftv1_index_nodes" -ne 0 ]; then
    echo >&2 "FAIL: found FTV1 node(s) using the index oneof arm instead of response_name, n_ftv1_index_nodes=$n_ftv1_index_nodes"
    failures=$((failures + 1))
  fi

  if [ "$n_ftv1_errors" -ne 0 ]; then
    echo >&2 "FAIL: found FTV1 Node.error entries, n_ftv1_errors=$n_ftv1_errors, but no errors were injected for this query"
    failures=$((failures + 1))
  fi
else
  echo >&2 "FAIL: expected at least one subgraph FTV1 trace to be decoded, got n_ftv1_reports=0"
  echo >&2 "  Check field_level_instrumentation_sampler in the router config and subgraph-mock's ftv1 header handling."
  failures=$((failures + 1))
fi

if [ "$failures" -gt 0 ]; then
  exit 1
fi

echo "PASS: router forwarded $n_ftv1_reports FTV1 trace(s) to mock-studio's /v1/traces with no anomalies"
