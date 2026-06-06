#!/bin/sh
# ============================================================================
# normalize-apk-version.sh — Convert semver version strings to apk format
# ============================================================================
#
# WHAT: Transforms a semver version string (from git tags or CI) into the
#       format required by apk-tools v3 packages (.apk).
#
# Go v1 REFERENCE: packaging/normalize-apk-version.sh in tollgate-module-basic-go
#   - Identical algorithm: v1.2.3 → 1.2.3-r0, v1.2.3-alpha1 → 1.2.3_alpha1-r0
#   - Branch builds fallback to 0.0.0_git<short_hash>-r0
#
# Rust DIFFERENCE: None. Version string format is package-system-specific,
#   not language-specific. Algorithm is byte-for-byte identical to Go v1.
#
# USAGE:
#   normalize-apk-version.sh <version_string>
#   normalize-apk-version.sh v1.2.3        → 1.2.3-r0
#   normalize-apk-version.sh v1.2.3-beta1   → 1.2.3_beta1-r0
#   normalize-apk-version.sh v1.2.3-rc2     → 1.2.3_rc2-r0
#   normalize-apk-version.sh my-branch      → 0.0.0_git<short_hash>-r0
# ============================================================================

set -eu

VERSION_INPUT="${1:?Usage: normalize-apk-version.sh <version_string>}"

# Strip leading 'v' if present (git tags are v-prefixed)
VERSION_INPUT="$(echo "$VERSION_INPUT" | sed 's/^v//')"

# Check if this looks like a semver version (X.Y.Z with optional prerelease)
case "$VERSION_INPUT" in
    [0-9]*.[0-9]*.[0-9]*)
        # Extract major.minor.patch and optional prerelease
        BASE_VERSION="$(echo "$VERSION_INPUT" | sed 's/-.*//')"
        PRERELEASE="$(echo "$VERSION_INPUT" | sed 's/^[0-9]*\.[0-9]*\.[0-9]*//' | sed 's/^-//')"

        if [ -n "$PRERELEASE" ]; then
            # Prerelease: 1.2.3-alpha1 → 1.2.3_alpha1-r0
            echo "${BASE_VERSION}_${PRERELEASE}-r0"
        else
            # Release: 1.2.3 → 1.2.3-r0
            echo "${BASE_VERSION}-r0"
        fi
        ;;
    *)
        # Non-semver (branch name, etc.): use 0.0.0_git<short_hash>-r0
        GIT_SHORT_HASH="$(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
        echo "0.0.0_git${GIT_SHORT_HASH}-r0"
        ;;
esac
