#!/usr/bin/env bash
# Prints the CHANGELOG.md section of one release (the lines between its `## [X.Y.Z]` heading and the
# next `## [` heading, trailing `---` separators and blank lines dropped): the GitHub release notes.
# Fails when the version has no section or the section is empty.
# Usage: tests/ci/release_notes.sh <X.Y.Z> [CHANGELOG.md]. Requires: bash, awk.
set -euo pipefail

version="${1:?usage: release_notes.sh <X.Y.Z> [CHANGELOG.md]}"
changelog="${2:-CHANGELOG.md}"

notes="$(awk -v heading="## [${version}]" '
  index($0, "## [") == 1 { if (found) exit; found = (index($0, heading) == 1); next }
  found { print }
' "$changelog" | awk '{ lines[NR] = $0 } END {
  last = NR
  while (last > 0 && (lines[last] ~ /^[[:space:]]*$/ || lines[last] == "---")) last--
  for (i = 1; i <= last; i++) print lines[i]
}')"

if [ -z "${notes//[[:space:]]/}" ]; then
  echo "release_notes.sh: no CHANGELOG entry for ${version} in ${changelog}" >&2
  exit 1
fi
printf '%s\n' "$notes"
