#!/bin/sh
# Thin orchestration wrapper for the orchestrator-k6-graphql scenario's docker.command
# override (see this test plan's test-plan.yaml): runs the k6 FTV1 volume test
# (scripts/verify-ftv1.js - load generation and mock-studio /request-stats assertions, all
# inside k6's own JS runtime) and, once it finishes, saves a copy of mock-studio's final
# stats as a run artifact.
#
# The stats save happens here rather than in verify-ftv1.js because k6 has no
# filesystem-write API without a custom xk6 build - see the comment at the top of that file.
# wget is used instead of curl: the grafana/k6 image has no curl (or jq), only busybox wget -
# confirmed by running it directly (`apk add curl jq` fails as the image's default non-root
# user with "Unable to open log: Permission denied", and RTF's DockerCommand has no way to
# request root). Same constraint independently noted in
# test-plans/orchestrator-compatible/diagnostics-plugin/scripts/diagnostics-k6-wrapper.sh in
# the rtf-morgue repo.
#
# Deliberately no `set -e`: it would make `K6_EXIT_CODE=$?` below unreachable on a failing
# k6 run, since set -e exits immediately on the first non-zero command rather than the next
# statement getting a chance to capture its status.

ulimit -n 250000 2>/dev/null || true
export K6_CONFIG_FILE="$K6_CONFIG_DIR/k6-config.json"

mkdir -p "$OUTDIR/results" 2>/dev/null || true

k6 run --out json="$OUTDIR/results/k6-results.json" "$K6_TEST_ENTRY"
K6_EXIT_CODE=$?

wget -q -O "$OUTDIR/results/mock-studio-request-stats.json" "$MOCK_STUDIO_URL/request-stats" 2>/dev/null || true

exit "$K6_EXIT_CODE"
