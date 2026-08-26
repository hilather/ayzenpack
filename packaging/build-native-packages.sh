#!/usr/bin/env bash
# Build the release binary and .rpm (or tarball) with nfpm.
#
# Usage (repo root, after gcc + rustc):
#   ./packaging/build-native-packages.sh
#   OUT_DIR=dist VERSION=0.1.0 PACKAGE_FAMILY=rpm DISTRO_LABEL=rocky-8 ./packaging/build-native-packages.sh
#
# Env:
#   PACKAGE_FAMILY=deb|rpm|auto|none   (default auto; none = tarball only)
#   SKIP_BUILD=1                       skip cargo build
#   OUT_DIR=dist
#   DISTRO_LABEL=rocky-8               tag artifact names across CI matrix jobs
#   TARBALL_ONLY=1                     only emit the binary tarball (+ checksums)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

export PATH="${HOME}/.cargo/bin:${HOME}/.local/bin:/usr/local/bin:/usr/bin:/bin:${PATH}"
if [[ -f "${HOME}/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    . "${HOME}/.cargo/env"
fi

OUT_DIR="${OUT_DIR:-$ROOT/dist}"
NAME="${PACKAGE_NAME:-ayzenpack}"
MAINTAINER="${MAINTAINER:-ayzenpack contributors <noreply@localhost>}"
VERSION="${VERSION:-}"
if [[ -z "$VERSION" ]]; then
    VERSION="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
fi
VERSION="${VERSION#v}"

if [[ -z "${DISTRO_LABEL:-}" ]]; then
    if [[ -f /etc/os-release ]]; then
        # shellcheck source=/dev/null
        . /etc/os-release
        DISTRO_LABEL="${ID:-linux}${VERSION_ID:-}"
    else
        DISTRO_LABEL=linux
    fi
fi
DISTRO_LABEL="$(printf '%s' "$DISTRO_LABEL" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9._-' '-' | sed 's/-\+/-/g; s/^-//; s/-$//')"

ARCH_UNAME="$(uname -m)"
case "$ARCH_UNAME" in
    x86_64|amd64) ARCH_DEB=amd64; ARCH_RPM=x86_64; ARCH_NFPM=amd64 ;;
    aarch64|arm64) ARCH_DEB=arm64; ARCH_RPM=aarch64; ARCH_NFPM=arm64 ;;
    *) ARCH_DEB="$ARCH_UNAME"; ARCH_RPM="$ARCH_UNAME"; ARCH_NFPM="$ARCH_UNAME" ;;
esac

detect_family() {
    if [[ -n "${PACKAGE_FAMILY:-}" && "$PACKAGE_FAMILY" != auto ]]; then
        echo "$PACKAGE_FAMILY"
        return
    fi
    if [[ -f /etc/os-release ]]; then
        # shellcheck source=/dev/null
        . /etc/os-release
        case "${ID_LIKE:-$ID}" in
            *debian*|*ubuntu*) echo deb ;;
            *rhel*|*fedora*|*centos*|*rocky*|*alma*) echo rpm ;;
            *)
                case "${ID:-}" in
                    ubuntu|debian) echo deb ;;
                    rocky|almalinux|rhel|fedora|centos) echo rpm ;;
                    *) echo both ;;
                esac
                ;;
        esac
    else
        echo both
    fi
}

FAMILY="$(detect_family)"
mkdir -p "$OUT_DIR"

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    echo "==> cargo build --release --locked --bin ayzenpack"
    command -v cargo
    cargo --version
    rustc --version
    cargo build --release --locked --bin ayzenpack
fi
test -x target/release/ayzenpack
target/release/ayzenpack --help >/dev/null

TARBALL_BASE="${NAME}-${VERSION}-${DISTRO_LABEL}-${ARCH_UNAME}"
TARBALL="$OUT_DIR/${TARBALL_BASE}.tar.gz"
STAGE="$OUT_DIR/.tarball-stage-$$"
mkdir -p "$STAGE"
cp -a target/release/ayzenpack "$STAGE/"
cat >"$STAGE/README.txt" <<EOF
${NAME} ${VERSION}
Built on: ${DISTRO_LABEL} (${ARCH_UNAME})
Install:  install -m 755 ayzenpack /usr/local/bin/
Usage:    ayzenpack dehydrate -o libs.ayz app.jar lib/*.jar
          ayzenpack rehydrate -i libs.ayz -d restored/
EOF
tar -C "$STAGE" -czf "$TARBALL" ayzenpack README.txt
rm -rf "$STAGE"
echo "Wrote $TARBALL"
(
    cd "$OUT_DIR"
    sha256sum "$(basename "$TARBALL")" | tee "$(basename "$TARBALL").sha256"
)

if [[ "${TARBALL_ONLY:-0}" == "1" || "${PACKAGE_FAMILY:-}" == "none" ]]; then
    echo "==> TARBALL_ONLY/PACKAGE_FAMILY=none — skipping .deb/.rpm"
    ls -la "$OUT_DIR"
    exit 0
fi

install_nfpm() {
    if command -v nfpm >/dev/null 2>&1; then
        return 0
    fi
    echo "==> installing nfpm"
    local ver="${NFPM_VERSION:-v2.41.3}"
    local url="https://github.com/goreleaser/nfpm/releases/download/${ver}/nfpm_${ver#v}_Linux_x86_64.tar.gz"
    if [[ "$ARCH_UNAME" == "aarch64" ]]; then
        url="https://github.com/goreleaser/nfpm/releases/download/${ver}/nfpm_${ver#v}_Linux_arm64.tar.gz"
    fi
    curl -fsSL --retry 3 --max-time 60 "$url" | tar -xz -C /tmp nfpm
    mkdir -p "${HOME}/.local/bin"
    if install -m 755 /tmp/nfpm "${HOME}/.local/bin/nfpm" 2>/dev/null; then
        :
    elif command -v sudo >/dev/null 2>&1 && sudo install -m 755 /tmp/nfpm /usr/local/bin/nfpm; then
        :
    else
        install -m 755 /tmp/nfpm /usr/local/bin/nfpm
    fi
    export PATH="${HOME}/.local/bin:/usr/local/bin:${PATH}"
    command -v nfpm >/dev/null
}

write_nfpm_config() {
    local family=$1
    local conf="$OUT_DIR/nfpm-${family}.yaml"
    local arch
    if [[ "$family" == deb ]]; then
        arch="$ARCH_NFPM"
    else
        arch="$ARCH_RPM"
    fi
    sed \
        -e "s/@NAME@/${NAME}/g" \
        -e "s/@VERSION@/${VERSION}/g" \
        -e "s/@ARCH@/${arch}/g" \
        -e "s|@MAINTAINER@|${MAINTAINER}|g" \
        "$SCRIPT_DIR/nfpm.yaml.tmpl" > "$conf"
    echo "$conf"
}

pack_with_nfpm() {
    local family=$1
    install_nfpm
    local conf
    conf="$(write_nfpm_config "$family")"
    echo "==> nfpm pkg --packager $family"
    (
        cd "$ROOT"
        nfpm pkg --packager "$family" --config "$conf" --target "$OUT_DIR"
    )
}

rename_rpm_with_distro() {
    # nfpm emits ayzenpack-VERSION-1.x86_64.rpm for every distro. Matrix jobs
    # would clobber each other in the GitHub Release flatten step.
    shopt -s nullglob
    local f bn dest
    for f in "$OUT_DIR"/*.rpm; do
        bn="$(basename "$f")"
        case "$bn" in
            *."${DISTRO_LABEL}".*) continue ;;
        esac
        if [[ "$bn" =~ ^(.*)\.(x86_64|aarch64|noarch)\.rpm$ ]]; then
            dest="${BASH_REMATCH[1]}.${DISTRO_LABEL}.${BASH_REMATCH[2]}.rpm"
            mv -f "$f" "$OUT_DIR/$dest"
            echo "Renamed RPM $bn -> $dest"
        fi
    done
}

case "$FAMILY" in
    deb) pack_with_nfpm deb ;;
    rpm)
        pack_with_nfpm rpm
        rename_rpm_with_distro
        ;;
    both)
        pack_with_nfpm deb || echo "warning: deb packaging failed"
        pack_with_nfpm rpm || echo "warning: rpm packaging failed"
        rename_rpm_with_distro
        ;;
    *)
        echo "Unknown PACKAGE_FAMILY=$FAMILY; tarball only"
        ;;
esac

echo "==> artifacts in $OUT_DIR"
ls -la "$OUT_DIR"
# GitHub Releases reject 0-byte blobs; fail the job if we produced any.
empty="$(find "$OUT_DIR" -maxdepth 1 -type f -size 0c -print || true)"
if [[ -n "$empty" ]]; then
    echo "error: empty artifacts (GitHub will reject these):" >&2
    echo "$empty" >&2
    exit 1
fi
