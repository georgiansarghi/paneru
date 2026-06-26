#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/profile-idle.sh [--seconds N] [--queries N] [--pid PID] [--out DIR]

Collect a local Paneru idle-performance snapshot: CPU/thread samples, optional
wakeups, `paneru query active` latency, and a stack sample.

Environment:
  PANERU_BIN   CLI used for query latency. Defaults to the running process path,
               `paneru` on PATH, then target/release or target/debug builds.

Examples:
  scripts/profile-idle.sh
  PANERU_BIN=/Applications/Paneru.app/Contents/MacOS/paneru scripts/profile-idle.sh
USAGE
}

seconds=30
query_count=10
pid=""
out_dir=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --seconds)
      seconds="${2:?--seconds requires a value}"
      shift 2
      ;;
    --queries)
      query_count="${2:?--queries requires a value}"
      shift 2
      ;;
    --pid)
      pid="${2:?--pid requires a value}"
      shift 2
      ;;
    --out)
      out_dir="${2:?--out requires a value}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

require_macos() {
  if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This profiling script is intended for macOS/Darwin." >&2
    exit 1
  fi
}

find_pid() {
  if [[ -n "$pid" ]]; then
    echo "$pid"
    return
  fi

  local found=""
  found="$(pgrep -x paneru 2>/dev/null | head -n 1 || true)"
  if [[ -z "$found" ]]; then
    found="$(pgrep -f '(^|/)paneru( |$)' 2>/dev/null | grep -v "$$" | head -n 1 || true)"
  fi

  if [[ -z "$found" ]]; then
    echo "Could not find a running paneru process. Start/reload Paneru first, or pass --pid." >&2
    exit 1
  fi
  echo "$found"
}

resolve_bin() {
  if [[ -n "${PANERU_BIN:-}" ]]; then
    echo "$PANERU_BIN"
    return
  fi

  local from_pid=""
  from_pid="$(ps -p "$pid" -o comm= 2>/dev/null | head -n 1 | xargs || true)"
  if [[ -n "$from_pid" && -x "$from_pid" ]]; then
    echo "$from_pid"
    return
  fi

  if command -v paneru >/dev/null 2>&1; then
    command -v paneru
    return
  fi

  if [[ -x target/release/paneru ]]; then
    echo "target/release/paneru"
    return
  fi

  if [[ -x target/debug/paneru ]]; then
    echo "target/debug/paneru"
    return
  fi

  echo "Could not find a paneru CLI. Set PANERU_BIN to the binary used for queries." >&2
  exit 1
}

now_utc() {
  date -u +%Y%m%dT%H%M%SZ
}

require_macos
pid="$(find_pid)"
bin="$(resolve_bin)"
if [[ -z "$out_dir" ]]; then
  out_dir="perf-runs/$(now_utc)-pid-${pid}"
fi
mkdir -p "$out_dir"

summary="$out_dir/summary.txt"

{
  echo "Paneru idle profile"
  echo "===================="
  echo "date_utc: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "pid: $pid"
  echo "paneru_bin: $bin"
  echo "duration_seconds: $seconds"
  echo "query_count: $query_count"
  echo "macos: $(sw_vers -productVersion 2>/dev/null || true) ($(sw_vers -buildVersion 2>/dev/null || true))"
  echo "arch: $(uname -m)"
  echo
} >"$summary"

if ! ps -p "$pid" >/dev/null 2>&1; then
  echo "PID $pid is not running." >&2
  exit 1
fi

# Capture beginning and ending counters around the idle window. `top` provides
# CPU and thread counts without requiring sudo. Wakeup columns differ across
# macOS releases, so failures are recorded but non-fatal.
ps -p "$pid" -o pid,ppid,etime,%cpu,nlwp,comm >"$out_dir/ps-before.txt" 2>&1 || true

if command -v top >/dev/null 2>&1; then
  top -l 2 -s "$seconds" -pid "$pid" -stats pid,cpu,threads,wakeups,power,command >"$out_dir/top.txt" 2>&1 || \
    top -l 2 -s "$seconds" -pid "$pid" -stats pid,cpu,threads,command >"$out_dir/top.txt" 2>&1 || true
else
  echo "top not found" >"$out_dir/top.txt"
  sleep "$seconds"
fi

ps -p "$pid" -o pid,ppid,etime,%cpu,nlwp,comm >"$out_dir/ps-after.txt" 2>&1 || true

# Optional task wakeup data. `powermetrics` often requires sudo/admin rights, so
# this is best-effort and intentionally does not fail the whole run.
if command -v powermetrics >/dev/null 2>&1; then
  timeout_cmd=()
  if command -v gtimeout >/dev/null 2>&1; then
    timeout_cmd=(gtimeout "$((seconds + 15))")
  elif command -v timeout >/dev/null 2>&1; then
    timeout_cmd=(timeout "$((seconds + 15))")
  fi
  if [[ ${#timeout_cmd[@]} -gt 0 ]]; then
    "${timeout_cmd[@]}" powermetrics --samplers tasks --show-process-energy -n 1 -i "$((seconds * 1000))" >"$out_dir/powermetrics.txt" 2>&1 || true
  else
    powermetrics --samplers tasks --show-process-energy -n 1 -i "$((seconds * 1000))" >"$out_dir/powermetrics.txt" 2>&1 || true
  fi
else
  echo "powermetrics not found" >"$out_dir/powermetrics.txt"
fi

# Stack sample. This may require Developer Tools or Accessibility/Debugging
# permission depending on local security settings.
if command -v sample >/dev/null 2>&1; then
  sample "$pid" 5 -file "$out_dir/sample.txt" >/dev/null 2>"$out_dir/sample.stderr" || true
else
  echo "sample not found" >"$out_dir/sample.txt"
fi

# Query latency must use the installed/running CLI, not `cargo run`, otherwise
# cargo startup dominates the measurement.
python3 - "$bin" "$query_count" >"$out_dir/query-active-latency.txt" <<'PY'
import statistics
import subprocess
import sys
import time

bin_path = sys.argv[1]
count = int(sys.argv[2])
latencies = []
failures = 0
for i in range(count):
    start = time.perf_counter()
    proc = subprocess.run(
        [bin_path, "query", "active"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
    )
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    if proc.returncode == 0:
        latencies.append(elapsed_ms)
        print(f"query_active_ms{{run={i+1}}} {elapsed_ms:.3f}")
    else:
        failures += 1
        stderr = proc.stderr.strip().replace("\n", " | ")
        print(f"query_active_failed{{run={i+1},code={proc.returncode}}} {stderr}")

print()
print(f"successes {len(latencies)}")
print(f"failures {failures}")
if latencies:
    print(f"min_ms {min(latencies):.3f}")
    print(f"median_ms {statistics.median(latencies):.3f}")
    print(f"max_ms {max(latencies):.3f}")
    if len(latencies) >= 2:
        print(f"mean_ms {statistics.mean(latencies):.3f}")
PY

{
  echo "Artifacts written under: $out_dir"
  echo
  echo "After sample:"
  cat "$out_dir/ps-after.txt"
  echo
  echo "Query active latency summary:"
  tail -n 8 "$out_dir/query-active-latency.txt" || true
  echo
  echo "Wakeup notes:"
  if grep -qi "wakeups" "$out_dir/top.txt"; then
    echo "top.txt includes a wakeups column on this macOS release."
  else
    echo "top wakeups column unavailable; check powermetrics.txt or use Activity Monitor > Energy."
  fi
} | tee -a "$summary"
