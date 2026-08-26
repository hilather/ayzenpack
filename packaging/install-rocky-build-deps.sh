#!/usr/bin/env bash
# Install compile deps inside Rocky Linux 8 or 9 containers.
#
# Hardening copied from ratarmount-rs packages.yml (Rocky exit 127 / dnf conflicts):
#   - do not install full `coreutils` (images ship `coreutils-single`; they conflict)
#   - `curl` vs `curl-minimal` needs --allowerasing
#   - Rocky 8: PowerTools; Rocky 9: CRB
#   - PATH later (rustup) is the caller's job
set -euo pipefail

release="${1:-}"
if [[ -z "$release" && -f /etc/os-release ]]; then
    # shellcheck source=/dev/null
    . /etc/os-release
    release="${VERSION_ID%%.*}"
fi
if [[ -z "$release" ]]; then
    echo "usage: $0 8|9" >&2
    exit 2
fi

dnf -y install epel-release || true
dnf -y install 'dnf-command(config-manager)' dnf-plugins-core || true

case "$release" in
    8)
        dnf config-manager --set-enabled powertools 2>/dev/null \
            || dnf config-manager --set-enabled PowerTools 2>/dev/null \
            || true
        ;;
    9)
        dnf config-manager --set-enabled crb 2>/dev/null || true
        if command -v crb >/dev/null 2>&1; then
            crb enable || true
        fi
        ;;
    *)
        echo "warning: unknown Rocky release '$release'; trying crb then powertools" >&2
        dnf config-manager --set-enabled crb 2>/dev/null || true
        dnf config-manager --set-enabled powertools 2>/dev/null || true
        ;;
esac

# --allowerasing: curl-minimal vs curl. Do not pull `coreutils` (conflicts).
dnf -y install --allowerasing \
    gcc gcc-c++ make pkgconf-pkg-config git ca-certificates \
    which findutils tar gzip \
    binutils \
    perl-IPC-Cmd

if ! command -v curl >/dev/null 2>&1; then
    dnf -y install --allowerasing curl
fi

command -v gcc
command -v tar
command -v gzip
command -v git
