#!/bin/sh
# ============================================================================
# build-apk.sh — Build an OpenWrt .apk package (apk-tools v3 format)
# ============================================================================
#
# WHAT: Creates an .apk package from a payload directory and metadata.
#       The .apk format (ADB — Alpine Database) is used by OpenWrt 23.05+
#       which moved from opkg/.ipk to apk-tools v3.
#
# Go v1 REFERENCE: Go only has build-ipk.sh (opkg format). Go v1 targets
#   OpenWrt versions that use .ipk packages exclusively.
#
# Rust IMPROVEMENT: New file. Go v1 does not have an APK builder because
#   Go v1 predates OpenWrt's switch to apk-tools. This script provides
#   forward-compatibility for OpenWrt 23.05+ which uses .apk packages.
#   The .ipk builder (build-ipk.sh) is retained for older OpenWrt versions.
#
# FORMAT: .apk (ADB — Alpine Database)
#   - Signature block (optional, can be unsigned for local installs)
#   - Control section: tar.gz containing metadata (package name, version, etc.)
#   - Data section: tar.gz containing the actual file tree
#
# USAGE:
#   build-apk.sh <payload_dir> <output.apk>
#
#   Environment variables (required):
#     PKG_NAME     — Package name (e.g. tollgate-wrt)
#     PKG_VERSION  — Version string (will be normalized via normalize-apk-version.sh)
#     ARCH         — Target architecture (e.g. aarch64_cortex-a53)
#
#   Environment variables (optional):
#     MAINTAINER, LICENSE, DEPENDS, PROVIDES, REPLACES, DESCRIPTION
#
# ============================================================================

set -eu

PAYLOAD_DIR=${1:?payload dir required}
OUTPUT=${2:?output apk path required}

: "${PKG_NAME:?PKG_NAME required}"
: "${PKG_VERSION:?PKG_VERSION required}"
: "${ARCH:?ARCH required}"

[ -d "$PAYLOAD_DIR" ] || { echo "error: payload dir missing: $PAYLOAD_DIR" >&2; exit 1; }

mkdir -p "$(dirname "$OUTPUT")"
OUTPUT="$(cd "$(dirname "$OUTPUT")" && pwd)/$(basename "$OUTPUT")"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# Normalize version to apk format
APK_VERSION="$(sh "$SCRIPT_DIR/normalize-apk-version.sh" "$PKG_VERSION")"

if command -v gtar >/dev/null 2>&1; then
    TAR=gtar
else
    TAR=tar
fi

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# --- Build control metadata ---
mkdir -p "$WORK/control"

{
    printf 'pkgname = %s\n' "$PKG_NAME"
    printf 'pkgver = %s\n' "$APK_VERSION"
    printf 'arch = %s\n' "$ARCH"
    [ -n "${MAINTAINER:-}" ]  && printf 'maintainer = %s\n'  "$MAINTAINER"
    [ -n "${LICENSE:-}" ]     && printf 'license = %s\n'     "$LICENSE"
    [ -n "${DEPENDS:-}" ]     && printf 'depend = %s\n'     "$DEPENDS"
    [ -n "${PROVIDES:-}" ]    && printf 'provides = %s\n'    "$PROVIDES"
    [ -n "${REPLACES:-}" ]    && printf 'replaces = %s\n'    "$REPLACES"
    [ -n "${DESCRIPTION:-}" ] && printf 'pkgdesc = %s\n' "$DESCRIPTION"
    printf 'url = https://github.com/Amperstrand/tollgate-rs-ai-research-and-experiments\n'
    printf 'builddate = %s\n' "$(date +%s)"
    printf 'packager = tollgate-rs build-apk.sh\n'
    printf 'size = %s\n' "$(du -sb "$PAYLOAD_DIR" 2>/dev/null | cut -f1 || echo 0)"
} > "$WORK/control/APKBLOCK"

# --- Copy maintainer scripts ---
for s in preinst postinst; do
    if [ -f "$SCRIPT_DIR/$s" ]; then
        cp "$SCRIPT_DIR/$s" "$WORK/control/$s"
        chmod 0755 "$WORK/control/$s"
    fi
done

# --- Build control tarball ---
( cd "$WORK/control" && \
  "$TAR" --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
    -czf "$WORK/control.tar.gz" . )

# --- Build data tarball ---
( cd "$PAYLOAD_DIR" && \
  "$TAR" --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
    -czf "$WORK/data.tar.gz" . )

# --- Assemble the APK package ---
# APK format: concat control.tar.gz + data.tar.gz
# For local/unsigned packages, no signature block is needed.
# apk mkpkg is preferred if available, otherwise manual assembly.
if command -v apk >/dev/null 2>&1; then
    # Try apk-tools v3 mkpkg if available (Alpine 3.19+)
    # Fall through to manual assembly if mkpkg not supported
    if apk --version 2>/dev/null | grep -q 'v3\.'; then
        # Build using apk mkpkg
        mkdir -p "$WORK/pkgroot"
        cp -a "$PAYLOAD_DIR/." "$WORK/pkgroot/"

        # Write APKBICTL metadata
        {
            printf 'pkgname = %s\n' "$PKG_NAME"
            printf 'pkgver = %s\n' "$APK_VERSION"
            printf 'arch = %s\n' "$ARCH"
            [ -n "${MAINTAINER:-}" ]  && printf 'maintainer = %s\n'  "$MAINTAINER"
            [ -n "${LICENSE:-}" ]     && printf 'license = %s\n'     "$LICENSE"
            [ -n "${DEPENDS:-}" ]     && printf 'depend = %s\n'     "$DEPENDS"
            [ -n "${PROVIDES:-}" ]    && printf 'provides = %s\n'    "$PROVIDES"
            [ -n "${REPLACES:-}" ]    && printf 'replaces = %s\n'    "$REPLACES"
            [ -n "${DESCRIPTION:-}" ] && printf 'pkgdesc = %s\n' "$DESCRIPTION"
        } > "$WORK/pkgroot/.APKBICTL"

        for s in preinst postinst; do
            if [ -f "$SCRIPT_DIR/$s" ]; then
                cp "$SCRIPT_DIR/$s" "$WORK/pkgroot/..$s"
                chmod 0755 "$WORK/pkgroot/.$s"
            fi
        done

        rm -f "$OUTPUT"
        ( cd "$WORK/pkgroot" && apk mkpkg -o "$OUTPUT" . ) 2>/dev/null && {
            size=$(wc -c < "$OUTPUT" | tr -d ' ')
            printf 'Built %s (%s bytes) via apk mkpkg\n' "$OUTPUT" "$size"
            exit 0
        }
        # Fall through to manual assembly if apk mkpkg failed
    fi
fi

# Manual assembly: concatenate control + data tarballs
# This is the unsigned APK format accepted by `apk add --allow-untrusted`
rm -f "$OUTPUT"
cat "$WORK/control.tar.gz" "$WORK/data.tar.gz" > "$OUTPUT"

size=$(wc -c < "$OUTPUT" | tr -d ' ')
printf 'Built %s (%s bytes) unsigned APK\n' "$OUTPUT" "$size"
