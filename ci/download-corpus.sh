#!/usr/bin/env bash
# Download pinned Maven JARs from ci/corpus.lock.json into $CORPUS_DIR.
# Verifies SHA-256; never uses "latest". CI does not vendor JARs in git.
#
# --record is lockfile maintenance only (pin bumps), never in CI: re-download
# each URL, rewrite sha256 and size, atomically replace the lockfile. URLs must
# already be present. Never leaves "sha256": "".
#
# Usage: ci/download-corpus.sh [--record]
# Env: CORPUS_DIR (default: $repo/.corpus)
#      LOCKFILE   (default: $repo/ci/corpus.lock.json)
set -euo pipefail

RECORD=0
if [[ "${1:-}" == "--record" ]]; then
  RECORD=1
  shift
fi
if [[ $# -ne 0 ]]; then
  echo "usage: $0 [--record]" >&2
  exit 2
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOCKFILE="${LOCKFILE:-$ROOT/ci/corpus.lock.json}"
CORPUS_DIR="${CORPUS_DIR:-$ROOT/.corpus}"

if ! command -v python3 >/dev/null 2>&1; then
  echo "python3 is required to parse $LOCKFILE" >&2
  exit 1
fi
if ! command -v curl >/dev/null 2>&1; then
  echo "curl is required to download corpus artifacts" >&2
  exit 1
fi
if ! command -v sha256sum >/dev/null 2>&1; then
  echo "sha256sum is required to verify corpus artifacts" >&2
  exit 1
fi
if [[ ! -f "$LOCKFILE" ]]; then
  echo "lockfile not found: $LOCKFILE" >&2
  exit 1
fi

mkdir -p "$CORPUS_DIR"
WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/ayzenpack-corpus.XXXXXX")"
trap 'rm -rf "$WORKDIR"' EXIT

# dest<TAB>url<TAB>sha256<TAB>size
list_artifacts() {
  python3 - "$LOCKFILE" <<'PY'
import json, sys
lock = json.load(open(sys.argv[1], encoding="utf-8"))
arts = lock.get("artifacts")
if not isinstance(arts, list) or not arts:
    sys.exit("lockfile must have a non-empty artifacts array")
for a in arts:
    dest = a.get("dest") or ""
    url = a.get("url") or ""
    sha = a.get("sha256") or ""
    size = a["size"] if a.get("size") is not None else ""
    if "\t" in dest or "\n" in dest or "\t" in url or "\n" in url:
        sys.exit("tab/newline in dest or url")
    print(f"{dest}\t{url}\t{sha}\t{size}")
PY
}

unsafe_dest() {
  local dest="$1"
  case "$dest" in
    "" | . | .. | */* | *\\* | ~*) return 0 ;;
  esac
  return 1
}

sha256_ok() {
  local sha="$1"
  [[ "$sha" =~ ^[0-9a-f]{64}$ ]]
}

UPDATES_JSON="$WORKDIR/updates.json"
printf '{}\n' >"$UPDATES_JSON"
list_artifacts >"$WORKDIR/arts.tsv"

while IFS=$'\t' read -r dest url sha size; do
  if unsafe_dest "$dest"; then
    echo "unsafe dest '$dest' (must be a basename)" >&2
    exit 1
  fi
  if [[ -z "$url" ]]; then
    echo "empty url for dest $dest" >&2
    exit 1
  fi
  if [[ "$url" == *latest* ]]; then
    echo "refusing 'latest' URL for $dest: $url" >&2
    exit 1
  fi
  if [[ "$RECORD" -eq 0 ]]; then
    if [[ -z "$sha" ]]; then
      echo "empty sha256 for $dest (merge blocker; use --record only to fill pins)" >&2
      exit 1
    fi
    if ! sha256_ok "$sha"; then
      echo "sha256 for $dest must be 64 lowercase hex, got '$sha'" >&2
      exit 1
    fi
  fi

  dest_path="$CORPUS_DIR/$dest"
  if [[ "$RECORD" -eq 0 && -f "$dest_path" ]]; then
    if echo "${sha}  ${dest_path}" | sha256sum -c - >/dev/null 2>&1; then
      echo "skip $dest (hash match)"
      continue
    fi
  fi

  tmp="$WORKDIR/$dest"
  echo "download $dest"
  curl -fsSL --retry 3 --max-time 60 -o "$tmp" "$url"

  if [[ "$RECORD" -eq 1 ]]; then
    sha="$(sha256sum "$tmp" | awk '{print $1}')"
    size="$(wc -c <"$tmp" | tr -d ' ')"
    if [[ -z "$sha" ]]; then
      echo "refusing empty sha256 for $dest after --record download" >&2
      exit 1
    fi
    python3 - "$UPDATES_JSON" "$dest" "$sha" "$size" <<'PY'
import json, sys
path, dest, sha, size = sys.argv[1:5]
data = json.load(open(path, encoding="utf-8"))
data[dest] = {"sha256": sha, "size": int(size)}
with open(path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2)
    f.write("\n")
PY
  else
    if ! echo "${sha}  ${tmp}" | sha256sum -c -; then
      echo "SHA-256 mismatch for $dest (url $url)" >&2
      exit 1
    fi
  fi
  mv -f "$tmp" "$dest_path"
done < "$WORKDIR/arts.tsv"

if [[ "$RECORD" -eq 1 ]]; then
  out="$WORKDIR/corpus.lock.json"
  python3 - "$LOCKFILE" "$UPDATES_JSON" "$out" <<'PY'
import json, sys
lock_path, updates_path, out_path = sys.argv[1:4]
lock = json.load(open(lock_path, encoding="utf-8"))
updates = json.load(open(updates_path, encoding="utf-8"))
for a in lock["artifacts"]:
    dest = a["dest"]
    if dest not in updates:
        sha = a.get("sha256") or ""
        if not sha:
            sys.exit(f"refusing empty sha256 for {dest}")
        continue
    sha = updates[dest]["sha256"]
    if not sha:
        sys.exit(f"refusing empty sha256 for {dest}")
    a["sha256"] = sha
    a["size"] = int(updates[dest]["size"])
with open(out_path, "w", encoding="utf-8") as f:
    json.dump(lock, f, indent=2)
    f.write("\n")
PY
  mv -f "$out" "$LOCKFILE"
  echo "updated $LOCKFILE"
fi

# Overlap app trees (hardlink when possible; never download twice).
python3 - "$LOCKFILE" "$CORPUS_DIR" <<'PY'
import json, os, sys
lock_path, corpus = sys.argv[1:3]
lock = json.load(open(lock_path, encoding="utf-8"))
for copy in lock.get("copies") or []:
    rel = copy["dir"]
    if os.path.isabs(rel) or ".." in rel.split("/"):
        sys.exit(f"unsafe copies dir {rel!r}")
    dest_dir = os.path.join(corpus, rel)
    os.makedirs(dest_dir, exist_ok=True)
    for name in copy["artifacts"]:
        if "/" in name or "\\" in name or name in (".", ".."):
            sys.exit(f"unsafe copy artifact {name!r}")
        src = os.path.join(corpus, name)
        dst = os.path.join(dest_dir, name)
        if not os.path.isfile(src):
            sys.exit(f"missing artifact {src}")
        if os.path.lexists(dst):
            os.remove(dst)
        try:
            os.link(src, dst)
        except OSError:
            import shutil
            shutil.copy2(src, dst)
PY

echo "corpus ready under $CORPUS_DIR"
