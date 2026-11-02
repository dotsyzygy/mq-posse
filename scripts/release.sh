#!/usr/bin/env bash
# Builds MQ2Posse.dll and publishes it as a GitHub release; see release.ps1.
# Usage: scripts/release.sh [--dry-run]
set -euo pipefail

args=()
for arg in "$@"; do
    case "$arg" in
        --dry-run) args+=(-DryRun) ;;
        *) echo "usage: $0 [--dry-run]" >&2; exit 2 ;;
    esac
done

ps1="$(cygpath -w "$(dirname "$0")/release.ps1")"
exec pwsh -NoProfile -ExecutionPolicy Bypass -File "$ps1" "${args[@]}"
