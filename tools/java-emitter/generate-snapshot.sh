#!/usr/bin/env bash
# Run a curated subset of upstream Java tests with the `LEEK_SNAPSHOT`
# probe enabled. Produces a TSV at
# `crates/backends/leek-backend-java/tests/fixtures/ops/snapshot.tsv`
# with one row per passing inline assertion.
#
# Both backends then cross-check against this corpus: the Rust side
# via `cargo test -p leek-backend-java`, the Java side already passes
# by construction (the rows come from passing runs).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LEEK="$ROOT/official-generator/leek-wars-generator/leekscript"
TOOL="$ROOT/tools/java-emitter"
OVERLAY="$TOOL/overlay"
MAIN_CLASSES="$TOOL/build/classes"
TEST_CLASSES="$TOOL/build/test-classes"
RUNNER_OUT="$TOOL/build/runner"
SNAPSHOT="$ROOT/crates/backends/leek-backend-java/tests/fixtures/ops/snapshot.tsv"
source "$TOOL/overlay.sh"

GCACHE="$HOME/.gradle/caches/modules-2/files-2.1"
# Exclude -sources/-javadoc jars: those carry .java, not .class, and
# feeding them to javac fails (e.g. junit-api's Execution.java).
jar_main() { find "$1" -name "*.jar" ! -name "*-sources.jar" ! -name "*-javadoc.jar" | sort -r | head -1; }
JACKSON_DB=$(jar_main "$GCACHE/tools.jackson.core/jackson-databind/3.0.3")
JACKSON_CORE=$(jar_main "$GCACHE/tools.jackson.core/jackson-core/3.0.3")
JACKSON_ANN=$(jar_main "$GCACHE/com.fasterxml.jackson.core/jackson-annotations")
JUNIT_API=$(jar_main "$GCACHE/org.junit.jupiter/junit-jupiter-api")
JUNIT_ENGINE=$(jar_main "$GCACHE/org.junit.jupiter/junit-jupiter-engine")
JUNIT_PLAT_COMM=$(jar_main "$GCACHE/org.junit.platform/junit-platform-commons")
JUNIT_PLAT_ENG=$(jar_main "$GCACHE/org.junit.platform/junit-platform-engine")
JUNIT_LAUNCHER=$(jar_main "$GCACHE/org.junit.platform/junit-platform-launcher")
OPENTEST=$(jar_main "$GCACHE/org.opentest4j/opentest4j")
APIGUARDIAN=$(jar_main "$GCACHE/org.apiguardian/apiguardian-api")

if [[ ! -d "$MAIN_CLASSES" ]]; then
  "$TOOL/build.sh"
fi
mkdir -p "$TEST_CLASSES" "$RUNNER_OUT" "$(dirname "$SNAPSHOT")"

# Recompile tests (cheap; idempotent).
CP_TEST="$MAIN_CLASSES:$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN:$JUNIT_API:$JUNIT_PLAT_COMM:$OPENTEST:$APIGUARDIAN"
SOURCES=$(mktemp)
trap 'rm -f "$SOURCES"' EXIT
list_sources "$LEEK/src/test/java" "$OVERLAY/src/test/java" > "$SOURCES"
javac -d "$TEST_CLASSES" -cp "$CP_TEST" --release 25 @"$SOURCES"

# Compile the snapshot generator main.
JUNIT_CP="$JUNIT_API:$JUNIT_ENGINE:$JUNIT_PLAT_COMM:$JUNIT_PLAT_ENG:$JUNIT_LAUNCHER:$OPENTEST:$APIGUARDIAN"
javac -d "$RUNNER_OUT" -cp "$JUNIT_CP" --release 25 "$TOOL/GenerateSnapshot.java"

# Run from the repo root so file-based tests' relative paths resolve
# correctly (we skip those in the snapshot but they still need to
# *attempt* to load without crashing the suite).
cd "$ROOT"
RUN_CP="$RUNNER_OUT:$TEST_CLASSES:$MAIN_CLASSES:$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN:$JUNIT_CP"
LEEK_SNAPSHOT="$SNAPSHOT" java -cp "$RUN_CP" GenerateSnapshot > /tmp/snapshot-run.log 2>&1 || true

# Report what we got.
ROWS=$(grep -cv '^#' "$SNAPSHOT" || true)
echo "wrote $SNAPSHOT ($ROWS rows)"
echo "log: /tmp/snapshot-run.log"
