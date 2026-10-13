#!/usr/bin/env bash
#
# Verify the monitoring artifacts that ship with the repository:
#
#   1. deploy/prometheus/openllm-alerts.yml passes `promtool check rules`
#   2. deploy/prometheus/openllm-alerts.test.yml passes `promtool test rules`,
#      which pins down every rule's expression, `for:` duration and rendered
#      annotations against synthetic series
#   3. deploy/grafana/openllm-dashboard.json is valid JSON
#   4. every openllm_* metric referenced by the alert rules, the rule tests and
#      the Grafana dashboard is actually exported by GET /metrics
#
# Check (4) compares against the metric names declared in src/api/metrics.rs so
# it works without a running gateway. Set OPENLLM_METRICS_URL to compare against
# a live scrape instead (stronger, and what CI should do):
#
#   OPENLLM_METRICS_URL=http://127.0.0.1:8080/metrics openllm-admin-token ./scripts/check-monitoring.sh
#
# promtool is taken from PATH, or from the official container when Docker is
# available. If neither exists the rule checks are skipped with a warning (the
# metric-name checks still run).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
rules="$root/deploy/prometheus/openllm-alerts.yml"
rules_test="$root/deploy/prometheus/openllm-alerts.test.yml"
dashboard="$root/deploy/grafana/openllm-dashboard.json"
metrics_src="$root/src/api/metrics.rs"

fail=0
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

note() { printf '%s\n' "$*"; }
bad() { printf 'FAIL: %s\n' "$*" >&2; fail=1; }

# ---------------------------------------------------------------- syntax ---
if [ -f "$dashboard" ]; then
  json_checker=""
  if command -v python3 >/dev/null 2>&1; then json_checker="python3"; fi
  if [ -z "$json_checker" ] && command -v python >/dev/null 2>&1; then json_checker="python"; fi
  if [ -n "$json_checker" ]; then
    if "$json_checker" -c "import json,sys; json.load(open(sys.argv[1]))" "$dashboard"; then
      note "ok: Grafana dashboard is valid JSON ($dashboard)"
    else
      bad "Grafana dashboard is not valid JSON: $dashboard"
    fi
  else
    note "skip: no python available to validate the dashboard JSON"
  fi
else
  bad "missing dashboard: $dashboard"
fi

# promtool is invoked with paths relative to the rules directory, so the same
# call works from PATH (cd first) and from the container (mounted at /rules).
if command -v promtool >/dev/null 2>&1; then
  run_promtool() { (cd "$(dirname "$rules")" && promtool "$@"); }
  promtool_note="promtool"
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
  run_promtool() {
    MSYS_NO_PATHCONV=1 docker run --rm --entrypoint promtool \
      -v "$(dirname "$rules"):/rules:ro" -w /rules prom/prometheus:latest "$@"
  }
  promtool_note="promtool (via docker)"
else
  run_promtool() { return 127; }
  promtool_note=""
fi

if [ -f "$rules" ]; then
  if [ -n "$promtool_note" ]; then
    if run_promtool check rules "$(basename "$rules")"; then
      note "ok: promtool accepted $rules ($promtool_note)"
    else
      bad "promtool rejected $rules"
    fi
    if [ -f "$rules_test" ]; then
      if run_promtool test rules "$(basename "$rules_test")"; then
        note "ok: every alert rule fires and stays silent as asserted by $rules_test"
      else
        bad "promtool rule tests failed: $rules_test"
      fi
    else
      bad "missing alert rule tests: $rules_test"
    fi
  else
    note "skip: neither promtool nor a usable docker found; rule syntax unchecked"
  fi
else
  bad "missing alert rules: $rules"
fi

# ----------------------------------------------------------- metric names ---
if [ -n "${OPENLLM_METRICS_URL:-}" ]; then
  note "using live scrape: $OPENLLM_METRICS_URL"
  if ! curl -fsS ${OPENLLM_ADMIN_TOKEN:+-H "Authorization: Bearer $OPENLLM_ADMIN_TOKEN"} \
    "$OPENLLM_METRICS_URL" -o "$work/scrape.txt"; then
    bad "could not fetch $OPENLLM_METRICS_URL"
  fi
  # Use the `# TYPE openllm_x gauge` declarations: every metric the endpoint
  # can export declares itself unconditionally, while series such as
  # openllm_provider_cooling only appear once a provider is configured. A
  # name that is declared but has no series is reported as a note, not a
  # failure.
  grep -E '^# TYPE openllm_' "$work/scrape.txt" | awk '{print $3}' | sort -u > "$work/exported.txt" || true
  grep -v '^#' "$work/scrape.txt" 2>/dev/null \
    | sed -E 's/[ {].*$//' | grep -E '^openllm_' | sort -u > "$work/series.txt" || true
else
  note "using declared metric names in src/api/metrics.rs (set OPENLLM_METRICS_URL for a live check)"
  grep -oE '"openllm_[a-z0-9_]+"' "$metrics_src" | tr -d '"' | sort -u > "$work/exported.txt"
fi

if [ ! -s "$work/exported.txt" ]; then
  bad "could not determine the set of exported metrics"
else
  referenced="$work/referenced.txt"
  : > "$referenced"
  for artifact in "$rules" "$rules_test" "$dashboard"; do
    [ -f "$artifact" ] || continue
    grep -oE 'openllm_[a-z0-9_]+' "$artifact" | sort -u >> "$referenced"
  done
  sort -u "$referenced" -o "$referenced"

  if missing="$(comm -23 "$referenced" "$work/exported.txt")" && [ -n "$missing" ]; then
    bad "these metric names are referenced but not exported by /metrics:"
    printf '  %s\n' $missing >&2
  else
    note "ok: all $(wc -l < "$referenced" | tr -d ' ') referenced metric names are exported"
  fi

  if [ -s "$work/series.txt" ]; then
    declared_only="$(comm -23 "$work/exported.txt" "$work/series.txt")"
    if [ -n "$declared_only" ]; then
      note "note: declared but with no series on this instance (nothing configured yet):"
      printf '  %s\n' $declared_only
    fi
  fi
fi

if [ "$fail" -ne 0 ]; then
  note "monitoring artifact check FAILED"
  exit 1
fi
note "monitoring artifact check passed"
