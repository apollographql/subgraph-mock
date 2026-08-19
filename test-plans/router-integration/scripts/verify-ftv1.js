import http from 'k6/http';
import { check, sleep } from 'k6';
import { Counter } from 'k6/metrics';

// Runs volume traffic against the router using this test plan's own canned GraphQL
// operations (data/canned-ops.json - authored by hand against subgraph-mock's synthetic
// users/posts schema, not pulled from a real GraphOS graph, since no real graph shares this
// schema), then asserts via mock-studio's /request-stats endpoint that every subgraph FTV1
// trace arrived and decoded with no anomalies. See subgraph-mock's SPEC_ftv1.md (emission)
// and mock-studio's SPEC_mock-studio-ftv1.md (the counters this checks).
//
// This is the orchestrator-k6-graphql scenario's docker.command override, invoked via
// scripts/verify-ftv1.sh - k6 has no filesystem-write API without a custom xk6 build, so the
// actual HTTP calls and JSON assertions happen here in k6's own http/JSON runtime rather
// than shelling out to curl+jq: the grafana/k6 image runs as a non-root user with no apk
// access and has no jq at all, only busybox wget - not enough to do field-level JSON
// assertions cleanly. See verify-ftv1.sh for the full explanation.
//
// field_level_instrumentation_sampler is always_on in this test plan's router config, so
// every request should produce a decodable FTV1 report - n_ftv1_reports=0 is a failure
// here, not something to skip past.
//
// otlp_tracing_sampler is deliberately always_on too: OTLP tracing and the legacy Report's
// raw-trace embedding are mutually exclusive, so FTV1 is checked via mock-studio's
// /v1/traces (OTLP) decode, not /studio - see this test plan's README.

const MOCK_STUDIO_URL = __ENV.MOCK_STUDIO_URL;
if (!MOCK_STUDIO_URL) {
  throw new Error('MOCK_STUDIO_URL environment variable is required');
}

const GRAPHQL_URL = __ENV.GRAPHQL_URL || 'http://localhost:4000';

const configPath = __ENV.K6_CONFIG_FILE;
if (!configPath) {
  throw new Error('K6_CONFIG_FILE environment variable is required');
}
const configData = JSON.parse(open(configPath));

const opsPath = __ENV.CANNED_OPS_FILE;
if (!opsPath) {
  throw new Error('CANNED_OPS_FILE environment variable is required');
}
const operations = open(opsPath)
  .split('\n')
  .filter(line => line.trim())
  .map(line => JSON.parse(line));

if (operations.length === 0) {
  throw new Error('No operations found in canned ops file');
}

const graphqlErrors = new Counter('graphql_query_errors');

// RPS represents total requests/sec, but each iteration executes ALL operations - mirrors
// lib/scenario-config/custom-providers/k6-graphql's graphql-test.js in rtf-morgue, which
// this replaces so a custom mock-studio verification step can run in the same k6 process.
function adjustScenarioRates(scenarios, opCount) {
  const adjusted = JSON.parse(JSON.stringify(scenarios));
  for (const scenario of Object.values(adjusted)) {
    if (scenario.rate) scenario.rate = Math.ceil(scenario.rate / opCount);
    if (scenario.stages) {
      scenario.stages = scenario.stages.map(s => ({ ...s, target: Math.ceil(s.target / opCount) }));
    }
    if (scenario.startRate !== undefined) scenario.startRate = Math.ceil(scenario.startRate / opCount);
  }
  return adjusted;
}

export const options = {
  scenarios: adjustScenarioRates(configData.scenarios, operations.length),
};

export function setup() {
  console.log(`k6 GraphQL FTV1 volume test starting - ${operations.length} operation(s) per iteration`);
  console.log('resetting mock-studio request-stats before load generation');
  http.del(`${MOCK_STUDIO_URL}/request-stats`);
}

export default function () {
  for (const op of operations) {
    const res = http.post(GRAPHQL_URL, JSON.stringify({ query: op.query, variables: op.variables }), {
      headers: { 'Content-Type': 'application/json' },
    });
    const ok = check(res, { 'HTTP status is 200': r => r.status === 200 });
    if (ok && res.body.indexOf('"errors"') !== -1) {
      graphqlErrors.add(1);
    }
  }
}

// Every field RequestStats can ever serialize (mock-studio's src/stats.rs) - asserted
// present, numeric, and non-negative. Catches mock-studio's own bugs (a crashed decode, a
// broken serde rename, a missing match arm) independent of anything the router/subgraph sent.
const STATS_FIELDS = [
  'n_calls', 'n_bytes', 'n_spans', 'n_root_spans', 'n_ftv1_reports',
  'n_ftv1_trace_parsing_failed', 'n_ftv1_nodes', 'n_ftv1_bad_type_nodes',
  'n_ftv1_timing_inversions', 'n_ftv1_index_nodes', 'n_ftv1_errors',
];

// Content-dependent anomaly counters: hard-failed here (unlike rtf-morgue's
// mock-studio-validation, which only logs them) because this test plan fully controls the
// canned ops' content - unlike a real GraphOS graph's traffic, an anomaly here always means
// a subgraph-mock or router regression, never "someone else's query had a bad day".
const ANOMALY_FIELDS = [
  'n_ftv1_trace_parsing_failed', 'n_ftv1_bad_type_nodes',
  'n_ftv1_timing_inversions', 'n_ftv1_index_nodes', 'n_ftv1_errors',
];

export function teardown() {
  // telemetry.apollo.tracing.batch_processor.scheduled_delay is 1s in this test plan's
  // router config, but leave a safety margin for the export round-trip to mock-studio.
  console.log("waiting for the router's OTLP batch span processor to flush");
  sleep(10);

  console.log('querying mock-studio request-stats');
  const res = http.get(`${MOCK_STUDIO_URL}/request-stats`);
  if (res.status !== 200 || !res.body) {
    // Confirmed by testing against an unreachable host: res.json() on a failed request
    // throws an opaque native GoError ("the body is null so we can't transform it to
    // JSON") instead of a clear message, so fail explicitly before that point.
    throw new Error(
      `GET ${MOCK_STUDIO_URL}/request-stats failed: status=${res.status} error=${res.error || 'none'}`
    );
  }
  const stats = res.json();
  console.log(JSON.stringify(stats));

  const failures = [];
  const entry = stats['POST /v1/traces'];

  if (!entry) {
    failures.push('mock-studio\'s /request-stats has no entry at all for "POST /v1/traces"');
  } else {
    for (const field of STATS_FIELDS) {
      const val = entry[field];
      if (typeof val !== 'number' || val < 0) {
        failures.push(`"POST /v1/traces".${field} is missing, non-numeric, or negative in mock-studio's stats`);
      }
    }
  }

  const nRootSpans = (entry && entry.n_root_spans) || 0;
  const nFtv1Reports = (entry && entry.n_ftv1_reports) || 0;
  const nFtv1Nodes = (entry && entry.n_ftv1_nodes) || 0;

  console.log(
    `POST /v1/traces n_calls=${(entry && entry.n_calls) || 0} n_root_spans=${nRootSpans} ` +
    `n_ftv1_reports=${nFtv1Reports} n_ftv1_nodes=${nFtv1Nodes}`
  );

  if (nRootSpans <= 0) {
    failures.push(
      `expected the router to send OTLP trace spans to mock-studio's /v1/traces, got n_root_spans=${nRootSpans} ` +
      '(check telemetry.apollo.otlp_tracing_sampler / experimental_otlp_endpoint in the router config)'
    );
  }

  if (nFtv1Reports > 0) {
    // Every decoded report has at least a root node, so nodes can never be fewer than reports.
    if (nFtv1Nodes < nFtv1Reports) {
      failures.push(
        `mock-studio recorded ${nFtv1Reports} FTV1 report(s) but only ${nFtv1Nodes} total node(s) - ` +
        'every report has at least a root node'
      );
    }

    for (const field of ANOMALY_FIELDS) {
      const val = (entry && entry[field]) || 0;
      if (val !== 0) {
        failures.push(`found FTV1 anomaly ${field}=${val}, expected 0`);
      }
    }
  } else {
    failures.push(
      'expected at least one subgraph FTV1 trace to be decoded, got n_ftv1_reports=0 ' +
      '(check field_level_instrumentation_sampler in the router config and subgraph-mock\'s ftv1 header handling)'
    );
  }

  if (failures.length > 0) {
    for (const f of failures) console.error(`FAIL: ${f}`);
    throw new Error(`FTV1 verification failed with ${failures.length} failure(s) - see logs above`);
  }

  console.log(`PASS: router forwarded ${nFtv1Reports} FTV1 trace(s) to mock-studio's /v1/traces with no anomalies`);
}
