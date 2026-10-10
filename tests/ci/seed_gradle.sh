#!/usr/bin/env bash
# Seeds the Gradle distribution the java-gradle E2E fixture's wrapper names (Gradle 8.9) into
# ${GRADLE_USER_HOME:-~/.gradle}/wrapper/dists, so jdtls never downloads it during a suite.
# Uses a `gradle` on PATH when there is one, else the official gradle-8.9-bin.zip.
# Usage: tests/ci/seed_gradle.sh. Requires: bash (3.2+), curl, unzip, java. Linux and macOS.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
props="$root/tests/e2e/fixtures/java-gradle/gradle/wrapper/gradle-wrapper.properties"
dists="${GRADLE_USER_HOME:-$HOME/.gradle}/wrapper/dists/gradle-8.9-bin"

if ls -d "$dists"/*/gradle-8.9 >/dev/null 2>&1; then
  echo "Gradle 8.9 already cached in $dists"
  exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

gradle_bin="$(command -v gradle || true)"
if [ -z "$gradle_bin" ]; then
  curl -fsSL --retry 3 -o "$tmp/gradle.zip" "https://services.gradle.org/distributions/gradle-8.9-bin.zip"
  unzip -q "$tmp/gradle.zip" -d "$tmp"
  gradle_bin="$tmp/gradle-8.9/bin/gradle"
fi

# A throw-away project whose wrapper uses the fixture's distribution URL: running the wrapper once
# downloads and unpacks that distribution exactly where jdtls looks for it.
project="$tmp/project"
mkdir -p "$project"
: > "$project/settings.gradle"
(cd "$project" && "$gradle_bin" --no-daemon -q wrapper)
cp "$props" "$project/gradle/wrapper/gradle-wrapper.properties"
(cd "$project" && ./gradlew --no-daemon -q --version >/dev/null)

ls -d "$dists"/*/gradle-8.9 >/dev/null 2>&1 || { echo "Gradle 8.9 was not seeded into $dists" >&2; exit 1; }
echo "Gradle 8.9 seeded in $dists"
