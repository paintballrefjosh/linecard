#!/usr/bin/env bash
# Print the CHANGELOG.md section for a version (without its heading or the
# blank lines around it). Used for release descriptions:
#   .github/scripts/changelog-section.sh v0.3.0
set -euo pipefail
version="${1#v}"
awk -v v="$version" '
  /^## / { if (found) exit; if ($2 == "v" v || $2 == v) { found = 1; next } }
  found { lines[++n] = $0 }
  END {
    first = 1; while (first <= n && lines[first] ~ /^[[:space:]]*$/) first++
    last = n; while (last >= first && lines[last] ~ /^[[:space:]]*$/) last--
    for (i = first; i <= last; i++) print lines[i]
  }
' "$(dirname "$0")/../../CHANGELOG.md"
