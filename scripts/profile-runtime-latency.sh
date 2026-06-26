#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/profile-runtime-latency.sh [options]

Measure Paneru runtime responsiveness after quiet idle. By default this is safe:
it waits past the adaptive grace window, runs query-only latency checks, captures
runtime diagnostics, and writes artifacts under perf-runs/.

Options:
  --out DIR              Artifact directory (default: perf-runs/<timestamp>-runtime-latency)
  --pid PID              Paneru daemon PID (default: auto-detect)
  --quiet-seconds N      Quiet-idle wait before measuring (default: 3)
  --queries N            Number of `paneru query active --json` runs (default: 20)
  --command-cycles N     Optional focus east/west cycles to measure (default: 0)
                         This changes focus but does not rearrange windows.
  --check-subscribe      Start `paneru subscribe --json`, send a safe focus command,
                         and require one subscriber line before timeout.
  --subscribe-timeout N  Seconds to wait for subscriber delivery (default: 5)
  --dry-run              Validate arguments and print planned actions only.
  -h, --help             Show this help.

Environment:
  PANERU_BIN             CLI used for queries/commands. Defaults to the running
                         process path, `paneru` on PATH, then target builds.

Examples:
  scripts/profile-runtime-latency.sh --quiet-seconds 3 --queries 20
  scripts/profile-runtime-latency.sh --command-cycles 3 --check-subscribe
  PANERU_LEGACY_IDLE_CADENCE=1 scripts/profile-runtime-latency.sh --out perf-runs/legacy-compare
USAGE
}

quiet_seconds=3
query_count=20
command_cycles=0
subscribe_timeout=5
check_subscribe=0
dry_run=0
pid=""
out_dir=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out)
      out_dir="${2:?--out requires a value}"
      shift 2
      ;;
    --pid)
      pid="${2:?--pid requires a value}"
      shift 2
      ;;
    --quiet-seconds)
      quiet_seconds="${2:?--quiet-seconds requires a value}"
      shift 2
      ;;
    --queries)
      query_count="${2:?--queries requires a value}"
      shift 2
      ;;
    --command-cycles)
      command_cycles="${2:?--command-cycles requires a value}"
      shift 2
      ;;
    --check-subscribe)
      check_subscribe=1
      shift
      ;;
    --subscribe-timeout)
      subscribe_timeout="${2:?--subscribe-timeout requires a value}"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
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

require_positive_int() {
  local name="$1"
  local value="$2"
  if ! [[ "$value" =~ ^[0-9]+$ ]] || [[ "$value" -lt 1 ]]; then
    echo "$name must be a positive integer, got: $value" >&2
    exit 2
  fi
}

require_nonnegative_int() {
  local name="$1"
  local value="$2"
  if ! [[ "$value" =~ ^[0-9]+$ ]]; then
    echo "$name must be a non-negative integer, got: $value" >&2
    exit 2
  fi
}

require_macos() {
  if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "This validation script is intended for macOS/Darwin." >&2
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
    echo "Could not find a running paneru process. Reload/start Paneru first, or pass --pid." >&2
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

  if [[ -x "$HOME/.local/bin/paneru" ]]; then
    echo "$HOME/.local/bin/paneru"
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

  echo "Could not find a paneru CLI. Set PANERU_BIN to the installed binary." >&2
  exit 1
}

now_utc() {
  date -u +%Y%m%dT%H%M%SZ
}

require_positive_int --quiet-seconds "$quiet_seconds"
require_positive_int --queries "$query_count"
require_nonnegative_int --command-cycles "$command_cycles"
require_positive_int --subscribe-timeout "$subscribe_timeout"
require_macos
pid="$(find_pid)"
bin="$(resolve_bin)"

if [[ ! -x "$bin" ]]; then
  echo "PANERU_BIN is not executable: $bin" >&2
  exit 1
fi
if ! ps -p "$pid" >/dev/null 2>&1; then
  echo "PID $pid is not running." >&2
  exit 1
fi

if [[ -z "$out_dir" ]]; then
  out_dir="perf-runs/$(now_utc)-runtime-latency-pid-${pid}"
fi

if [[ "$dry_run" -eq 1 ]]; then
  cat <<DRYRUN
Paneru runtime latency dry run
pid: $pid
paneru_bin: $bin
out_dir: $out_dir
quiet_seconds: $quiet_seconds
queries: $query_count
command_cycles: $command_cycles
check_subscribe: $check_subscribe
subscribe_timeout: $subscribe_timeout
DRYRUN
  exit 0
fi

mkdir -p "$out_dir"
summary_txt="$out_dir/summary.txt"
summary_json="$out_dir/summary.json"

{
  echo "Paneru runtime latency profile"
  echo "=============================="
  echo "date_utc: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  echo "pid: $pid"
  echo "paneru_bin: $bin"
  echo "quiet_seconds: $quiet_seconds"
  echo "query_count: $query_count"
  echo "command_cycles: $command_cycles"
  echo "check_subscribe: $check_subscribe"
  echo "macos: $(sw_vers -productVersion 2>/dev/null || true) ($(sw_vers -buildVersion 2>/dev/null || true))"
  echo "arch: $(uname -m)"
  echo
} >"$summary_txt"

"$bin" --version >"$out_dir/paneru-version.txt" 2>&1 || {
  echo "Failed to execute $bin --version" >&2
  exit 1
}
"$bin" query runtime-diagnostics --json >"$out_dir/runtime-diagnostics-before.json"
"$bin" query active --json >"$out_dir/active-before.json"
ps -p "$pid" -o pid,ppid,etime,%cpu,nlwp,comm >"$out_dir/ps-before.txt" 2>&1 || true

printf 'Waiting %s seconds for quiet idle...\n' "$quiet_seconds" | tee -a "$summary_txt"
if command -v top >/dev/null 2>&1; then
  top -l 2 -s "$quiet_seconds" -pid "$pid" -stats pid,cpu,threads,wakeups,power,command >"$out_dir/top-quiet.txt" 2>&1 || \
    top -l 2 -s "$quiet_seconds" -pid "$pid" -stats pid,cpu,threads,command >"$out_dir/top-quiet.txt" 2>&1 || true
else
  echo "top not found" >"$out_dir/top-quiet.txt"
  sleep "$quiet_seconds"
fi

python3 - "$bin" "$query_count" "$command_cycles" "$check_subscribe" "$subscribe_timeout" "$out_dir" >"$out_dir/latency-metrics.txt" <<'PY'
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

bin_path = sys.argv[1]
query_count = int(sys.argv[2])
command_cycles = int(sys.argv[3])
check_subscribe = bool(int(sys.argv[4]))
subscribe_timeout = float(sys.argv[5])
out_dir = Path(sys.argv[6])

def run_timed(args, stdout=subprocess.DEVNULL, timeout=10):
    start = time.perf_counter()
    proc = subprocess.run(args, stdout=stdout, stderr=subprocess.PIPE, text=True, timeout=timeout)
    elapsed_ms = (time.perf_counter() - start) * 1000.0
    return proc, elapsed_ms

def summarize(name, values):
    print(f"{name}_successes {len(values)}")
    if not values:
        return {"successes": 0, "min_ms": None, "median_ms": None, "max_ms": None, "mean_ms": None}
    result = {
        "successes": len(values),
        "min_ms": min(values),
        "median_ms": statistics.median(values),
        "max_ms": max(values),
        "mean_ms": statistics.mean(values),
    }
    for key in ("min_ms", "median_ms", "max_ms", "mean_ms"):
        print(f"{name}_{key} {result[key]:.3f}")
    return result

query_latencies = []
query_failures = []
for i in range(query_count):
    proc, elapsed = run_timed([bin_path, "query", "active", "--json"])
    if proc.returncode == 0:
        query_latencies.append(elapsed)
        print(f"query_active_ms{{run={i+1}}} {elapsed:.3f}")
    else:
        stderr = proc.stderr.strip().replace("\n", " | ")
        query_failures.append({"run": i + 1, "code": proc.returncode, "stderr": stderr})
        print(f"query_active_failed{{run={i+1},code={proc.returncode}}} {stderr}")

command_latencies = []
command_failures = []
commands = []
for _ in range(command_cycles):
    commands.append(["window", "focus", "east"])
    commands.append(["window", "focus", "west"])
for i, command in enumerate(commands):
    proc, elapsed = run_timed([bin_path, "send-cmd", *command])
    if proc.returncode == 0:
        command_latencies.append(elapsed)
        print(f"send_cmd_ms{{run={i+1},cmd={'_'.join(command)}}} {elapsed:.3f}")
    else:
        stderr = proc.stderr.strip().replace("\n", " | ")
        command_failures.append({"run": i + 1, "command": command, "code": proc.returncode, "stderr": stderr})
        print(f"send_cmd_failed{{run={i+1},code={proc.returncode}}} {stderr}")

subscribe = {"checked": check_subscribe, "delivered": None, "latency_ms": None, "error": None}
if check_subscribe:
    sub_path = out_dir / "subscribe-line.jsonl"
    with sub_path.open("w") as sub_out:
        proc = subprocess.Popen([bin_path, "subscribe", "--json"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            time.sleep(0.2)
            start = time.perf_counter()
            trigger = subprocess.run([bin_path, "send-cmd", "window", "focus", "first"], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, timeout=5)
            if trigger.returncode != 0:
                subscribe["error"] = trigger.stderr.strip()
            deadline = time.perf_counter() + subscribe_timeout
            line = ""
            while time.perf_counter() < deadline:
                if proc.stdout is None:
                    break
                # readline can block, so poll by waiting briefly through select on macOS.
                import select
                ready, _, _ = select.select([proc.stdout], [], [], 0.1)
                if ready:
                    line = proc.stdout.readline()
                    if line:
                        break
            if line:
                elapsed = (time.perf_counter() - start) * 1000.0
                sub_out.write(line)
                subscribe.update({"delivered": True, "latency_ms": elapsed})
                print(f"subscribe_delivery_ms {elapsed:.3f}")
            else:
                subscribe.update({"delivered": False})
                print("subscribe_delivery_failed timeout")
        finally:
            proc.terminate()
            try:
                _, stderr = proc.communicate(timeout=1)
            except subprocess.TimeoutExpired:
                proc.kill()
                _, stderr = proc.communicate(timeout=1)
            if stderr:
                (out_dir / "subscribe.stderr").write_text(stderr)

print()
query_summary = summarize("query_active", query_latencies)
command_summary = summarize("send_cmd", command_latencies)

summary = {
    "query_active": {**query_summary, "failures": query_failures},
    "send_cmd": {**command_summary, "failures": command_failures, "cycles": command_cycles},
    "subscribe": subscribe,
}
(out_dir / "latency-summary.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
PY

"$bin" query runtime-diagnostics --json >"$out_dir/runtime-diagnostics-after.json"
"$bin" query active --json >"$out_dir/active-after.json"
ps -p "$pid" -o pid,ppid,etime,%cpu,nlwp,comm >"$out_dir/ps-after.txt" 2>&1 || true

python3 - "$out_dir" "$summary_json" >>"$summary_txt" <<'PY'
import json
import sys
from pathlib import Path

out_dir = Path(sys.argv[1])
summary_json = Path(sys.argv[2])
latency = json.loads((out_dir / "latency-summary.json").read_text())
try:
    before = json.loads((out_dir / "runtime-diagnostics-before.json").read_text())
    after = json.loads((out_dir / "runtime-diagnostics-after.json").read_text())
except Exception as exc:
    before = {}
    after = {"diagnostics_parse_error": str(exc)}

def guard_hits(doc):
    return int(doc.get("dirty_settle_guard_hits", 0) or 0)

summary = {
    "artifacts": str(out_dir),
    "query_active_median_ms": latency["query_active"].get("median_ms"),
    "query_active_max_ms": latency["query_active"].get("max_ms"),
    "send_cmd_median_ms": latency["send_cmd"].get("median_ms"),
    "send_cmd_max_ms": latency["send_cmd"].get("max_ms"),
    "subscribe": latency["subscribe"],
    "dirty_settle_guard_hits_before": guard_hits(before),
    "dirty_settle_guard_hits_after": guard_hits(after),
    "dirty_settle_guard_hits_delta": guard_hits(after) - guard_hits(before),
    "last_runner_deadline_reason_after": after.get("last_runner_deadline_reason"),
    "native_tab_deadline_policy_after": after.get("native_tab_deadline_policy"),
    "last_due_deadline_reasons_after": after.get("last_due_deadline_reasons"),
    "last_rescheduled_deadline_reasons_after": after.get("last_rescheduled_deadline_reasons"),
}
summary_json.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")

print()
print("Concise summary")
print("---------------")
for key, value in summary.items():
    print(f"{key}: {value}")
if summary["dirty_settle_guard_hits_delta"]:
    print("WARNING: dirty settle guard count changed during validation")
PY

cat <<DONE | tee -a "$summary_txt"

Artifacts written under: $out_dir
Machine summary JSON: $summary_json
Raw latency metrics: $out_dir/latency-metrics.txt
DONE
