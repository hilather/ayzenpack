#!/usr/bin/env bash
# Regression: GitHub Releases reject empty blobs. Flatten/upload must only
# include non-empty files (empty file-info.txt broke ratarmount-rs v0.1.8).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/release"
: >"$TMP/release/file-info.txt"
echo "ok" >"$TMP/release/file-info-rocky9.txt"
echo "rpm-bytes" >"$TMP/release/ayzenpack-0.0.0-1.x86_64.rpm"
printf '' >"$TMP/release/empty.cosign.bundle"

mapfile -t files < <(
  find "$TMP/release" -maxdepth 1 -type f ! -name '.*' -size +0c -printf '%f\n' | sort
)
mapfile -t empty_files < <(
  find "$TMP/release" -maxdepth 1 -type f ! -name '.*' -size 0c -printf '%f\n' | sort || true
)

echo "non-empty: ${files[*]}"
echo "empty: ${empty_files[*]}"

printf '%s\n' "${files[@]}" | grep -qx 'ayzenpack-0.0.0-1.x86_64.rpm'
printf '%s\n' "${files[@]}" | grep -qx 'file-info-rocky9.txt'
if printf '%s\n' "${files[@]}" | grep -qx 'file-info.txt'; then
  echo "FAIL: empty file-info.txt must not be in upload list" >&2
  exit 1
fi
if printf '%s\n' "${files[@]}" | grep -qx 'empty.cosign.bundle'; then
  echo "FAIL: empty cosign bundle must not be in upload list" >&2
  exit 1
fi
[[ "${#files[@]}" -eq 2 ]] || {
  echo "FAIL: expected 2 non-empty assets, got ${#files[@]}" >&2
  exit 1
}
[[ "${#empty_files[@]}" -eq 2 ]] || {
  echo "FAIL: expected 2 empty files detected, got ${#empty_files[@]}" >&2
  exit 1
}

echo "OK: release asset filter skips empty files ($ROOT)"
