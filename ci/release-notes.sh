#!/usr/bin/env bash
# Prints the release notes for VERSION: its CHANGELOG.md section.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
version=$1
changes=$(awk -v v="$version" '/^## /{p = ($2 == v); next} p && (NF || n) {n = 1; print}' \
    "$root/CHANGELOG.md")
if [[ -z $changes ]]; then
    echo "CHANGELOG.md has no '## $version' section" >&2
    exit 1
fi
printf '%s\n' "$changes"
