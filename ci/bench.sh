#!/usr/bin/env bash
# Release CLI dehydrate/rehydrate timing + RSS + ratio JSON.
# Workload: 50×1 MiB synthetic JARs + overlap Maven corpus (CORPUS_DIR/apps).
# Not invoked by always-on cargo test (keep 50×1 MiB out of the default suite).
#
# Usage: ci/bench.sh
# Env:
#   AYZENPACK_BIN            default $ROOT/target/release/ayzenpack
#   CORPUS_DIR               default $ROOT/.corpus
#   BENCH_OUT                default $PWD/bench-results.json
#   BENCH_WORKDIR            optional; default mktemp
#   BENCH_SYNTHETIC_COPIES   default 50
#   BENCH_SYNTHETIC_BYTES    default 1048576 (1 MiB payload)
#   BENCH_SKIP_CORPUS=1      tests only: synthetic inputs, no Maven tree
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${AYZENPACK_BIN:-$ROOT/target/release/ayzenpack}"
CORPUS_DIR="${CORPUS_DIR:-$ROOT/.corpus}"
BENCH_OUT="${BENCH_OUT:-$PWD/bench-results.json}"
COPIES="${BENCH_SYNTHETIC_COPIES:-50}"
PAYLOAD_BYTES="${BENCH_SYNTHETIC_BYTES:-1048576}"
SKIP_CORPUS="${BENCH_SKIP_CORPUS:-0}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to write bench-results.json" >&2
  exit 1
fi
if [[ ! -x "$BIN" ]]; then
  echo "ayzenpack binary not found or not executable: $BIN" >&2
  echo "build the release CLI with: cargo build --release  (not cargo test --release)" >&2
  exit 1
fi
if ! [[ "$COPIES" =~ ^[1-9][0-9]*$ ]]; then
  echo "BENCH_SYNTHETIC_COPIES must be a positive integer, got '$COPIES'" >&2
  exit 1
fi
if ! [[ "$PAYLOAD_BYTES" =~ ^[1-9][0-9]*$ ]]; then
  echo "BENCH_SYNTHETIC_BYTES must be a positive integer, got '$PAYLOAD_BYTES'" >&2
  exit 1
fi

CLEANUP_WORKDIR=0
if [[ -n "${BENCH_WORKDIR:-}" ]]; then
  WORKDIR="$BENCH_WORKDIR"
  mkdir -p "$WORKDIR"
else
  WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/ayzenpack-bench.XXXXXX")"
  CLEANUP_WORKDIR=1
fi
if [[ "$CLEANUP_WORKDIR" -eq 1 ]]; then
  trap 'rm -rf "$WORKDIR"' EXIT
fi

SYNTHETIC_DIR="$WORKDIR/synthetic"
ARCHIVE="$WORKDIR/bench.ayz"
RESTORED="$WORKDIR/restored"
mkdir -p "$SYNTHETIC_DIR"

echo "synthetic: ${COPIES}×${PAYLOAD_BYTES} B JARs under $SYNTHETIC_DIR"
python3 - "$SYNTHETIC_DIR" "$COPIES" "$PAYLOAD_BYTES" <<'PY'
import os, shutil, sys, zipfile

out_dir, copies, nbytes = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
os.makedirs(out_dir, exist_ok=True)
proto = os.path.join(out_dir, "synthetic-0.jar")
payload = b"\x5a" * nbytes
with zipfile.ZipFile(proto, "w", compression=zipfile.ZIP_STORED) as zf:
    zf.writestr("payload.bin", payload)
for i in range(1, copies):
    dst = os.path.join(out_dir, f"synthetic-{i}.jar")
    try:
        os.link(proto, dst)
    except OSError:
        shutil.copy2(proto, dst)
PY

INPUTS=("$SYNTHETIC_DIR")
if [[ "$SKIP_CORPUS" != "1" ]]; then
  APPS="$CORPUS_DIR/apps"
  if [[ ! -d "$APPS" ]]; then
    echo "overlap corpus not found at $APPS (run ci/download-corpus.sh or set BENCH_SKIP_CORPUS=1)" >&2
    exit 1
  fi
  INPUTS+=("$APPS")
  echo "overlap corpus: $APPS"
else
  echo "skipping overlap corpus (BENCH_SKIP_CORPUS=1)"
fi

now_ms() {
  local t
  t="$(date +%s%3N 2>/dev/null || true)"
  if [[ "$t" =~ ^[0-9]+$ ]]; then
    printf '%s\n' "$t"
    return
  fi
  printf '%s\n' "$(($(date +%s) * 1000))"
}

# WALL_MS / PEAK_RSS_KB for the last run_timed invocation.
WALL_MS=0
PEAK_RSS_KB=0

run_timed() {
  local label="$1"
  shift
  local tfile="$WORKDIR/time.$label"
  local start end rss
  start="$(now_ms)"
  if [[ -x /usr/bin/time ]]; then
    /usr/bin/time -v -o "$tfile" -- "$@"
  else
    echo "warning: /usr/bin/time not found; peak RSS for $label will be 0" >&2
    "$@"
  fi
  end="$(now_ms)"
  WALL_MS=$((end - start))
  if (( WALL_MS < 0 )); then
    WALL_MS=0
  fi
  PEAK_RSS_KB=0
  if [[ -f "$tfile" ]]; then
    rss="$(LC_ALL=C awk '/Maximum resident set size/{print $NF}' "$tfile" | tail -n1)"
    if [[ "$rss" =~ ^[0-9]+$ ]]; then
      PEAK_RSS_KB="$rss"
    fi
  fi
}

echo "dehydrate: $BIN --sort-inputs -q -o $ARCHIVE --recursive ${INPUTS[*]}"
run_timed dehydrate "$BIN" dehydrate --sort-inputs -q -o "$ARCHIVE" --recursive "${INPUTS[@]}"
DEHYDRATE_WALL_MS="$WALL_MS"
DEHYDRATE_PEAK_RSS_KB="$PEAK_RSS_KB"
echo "dehydrate wall_ms=$DEHYDRATE_WALL_MS peak_rss_kb=$DEHYDRATE_PEAK_RSS_KB"

mkdir -p "$RESTORED"
echo "rehydrate: $BIN -q -i $ARCHIVE -d $RESTORED --overwrite"
run_timed rehydrate "$BIN" rehydrate -q -i "$ARCHIVE" -d "$RESTORED" --overwrite
REHYDRATE_WALL_MS="$WALL_MS"
REHYDRATE_PEAK_RSS_KB="$PEAK_RSS_KB"
echo "rehydrate wall_ms=$REHYDRATE_WALL_MS peak_rss_kb=$REHYDRATE_PEAK_RSS_KB"

ARCHIVE_SIZE="$(wc -c <"$ARCHIVE" | tr -d ' ')"

LOCKFILE="$ROOT/ci/corpus.lock.json"
GIT_SHA="${GITHUB_SHA:-}"
if [[ -z "$GIT_SHA" ]]; then
  GIT_SHA="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
fi

LIST_JSON="$WORKDIR/list.json"
"$BIN" list --json -i "$ARCHIVE" >"$LIST_JSON"

export ARCHIVE_SIZE GIT_SHA
export DEHYDRATE_WALL_MS REHYDRATE_WALL_MS
export DEHYDRATE_PEAK_RSS_KB REHYDRATE_PEAK_RSS_KB

python3 - "$LIST_JSON" "$BENCH_OUT" "$LOCKFILE" <<'PY'
import hashlib, json, os, sys

list_path, out_path, lock_path = sys.argv[1:4]
manifest = json.load(open(list_path, encoding="utf-8"))
stats = manifest.get("stats") or {}

def u64(name):
    v = stats.get(name, 0)
    if v is None:
        return 0
    return int(v)

bytes_in_jars = u64("bytes_in_jars")
bytes_unique = u64("bytes_unique_blobs")
bytes_uncomp = u64("bytes_uncompressed_entries")
archive_size = int(os.environ["ARCHIVE_SIZE"])
ratio_archive = (archive_size / bytes_in_jars) if bytes_in_jars else 0.0
ratio_unique = (bytes_unique / bytes_uncomp) if bytes_uncomp else 0.0

if os.path.isfile(lock_path):
    corpus_id = hashlib.sha256(open(lock_path, "rb").read()).hexdigest()
else:
    corpus_id = "unknown"

result = {
    "git_sha": os.environ["GIT_SHA"],
    "corpus_id": corpus_id,
    "bytes_in_jars": bytes_in_jars,
    "archive_size": archive_size,
    "bytes_unique_blobs": bytes_unique,
    "unique_blob_count": u64("unique_blob_count"),
    "file_entry_count": u64("file_entry_count"),
    "dehydrate_wall_ms": int(os.environ["DEHYDRATE_WALL_MS"]),
    "rehydrate_wall_ms": int(os.environ["REHYDRATE_WALL_MS"]),
    "dehydrate_peak_rss_kb": int(os.environ["DEHYDRATE_PEAK_RSS_KB"]),
    "rehydrate_peak_rss_kb": int(os.environ["REHYDRATE_PEAK_RSS_KB"]),
    "ratio_archive_to_jars": ratio_archive,
    "ratio_unique_to_uncompressed": ratio_unique,
}
os.makedirs(os.path.dirname(os.path.abspath(out_path)) or ".", exist_ok=True)
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(result, f, indent=2)
    f.write("\n")
print(json.dumps(result, indent=2))
PY
echo "wrote $BENCH_OUT"
