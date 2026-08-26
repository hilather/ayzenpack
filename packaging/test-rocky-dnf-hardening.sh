#!/usr/bin/env bash
# Guards: Rocky dnf install must not pull full coreutils (conflicts with
# coreutils-single) and must --allowerasing for curl vs curl-minimal.
# Also require both Rocky 8 PowerTools and Rocky 9 CRB in packages.yml.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
YML="${ROOT}/.github/workflows/packages.yml"
DEPS="${ROOT}/packaging/install-rocky-build-deps.sh"

[[ -f "$YML" ]] || { echo "FAIL: missing $YML" >&2; exit 1; }
[[ -f "$DEPS" ]] || { echo "FAIL: missing $DEPS" >&2; exit 1; }

fail=0
pass() { echo "PASS: $1"; }
bad() { echo "FAIL: $1" >&2; fail=$((fail + 1)); }

grep -q 'rockylinux/rockylinux:8' "$YML" && pass "packages.yml has Rocky 8 image" || bad "no Rocky 8 image"
grep -q 'rockylinux/rockylinux:9' "$YML" && pass "packages.yml has Rocky 9 image" || bad "no Rocky 9 image"
grep -q 'install-rocky-build-deps.sh' "$YML" && pass "workflow calls dnf helper" || bad "workflow does not call dnf helper"

if grep -E '^\s+coreutils(\s|$)' "$DEPS"; then
  bad "dnf helper must not install full coreutils (conflicts with coreutils-single)"
else
  pass "dnf helper does not install full coreutils"
fi
grep -q -- '--allowerasing' "$DEPS" && pass "dnf helper uses --allowerasing" || bad "missing --allowerasing"
grep -q powertools "$DEPS" && pass "Rocky 8 PowerTools" || bad "missing powertools"
grep -q crb "$DEPS" && pass "Rocky 9 CRB" || bad "missing crb"

grep -q 'shell: /usr/bin/bash' "$YML" && pass "container jobs force /usr/bin/bash" || bad "container jobs must defaults.run.shell /usr/bin/bash"
grep -q 'bash file' "$DEPS" && pass "dnf helper installs bash and file" || bad "dnf helper must install bash and file"
if grep -q 'cargo test --release' "$YML"; then
  bad "packages.yml must not cargo test --release (panic=abort)"
else
  pass "packages.yml does not cargo test --release"
fi
grep -q 'cargo build --release --locked' "$YML" "$ROOT/packaging/build-native-packages.sh" \
  && pass "release build is cargo build --release --locked" \
  || bad "missing cargo build --release --locked"
grep -q 'rename_rpm_with_distro' "$ROOT/packaging/build-native-packages.sh" \
  && pass "RPMs renamed with DISTRO_LABEL so Rocky 8/9 do not clobber" \
  || bad "RPM flatten would overwrite rocky-8 with rocky-9"

[[ "$fail" -eq 0 ]] || exit 1
echo "OK: Rocky dnf / packages.yml hardening ($ROOT)"
