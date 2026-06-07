#!/usr/bin/env bash
# Build (if needed) and run the upstream Java ops-cost tests:
#   - `test.TestOpsCost`         — manually-written .equals(...).ops(...) cases
#   - `test.TestOpsCostCorpus`   — TSV-driven cases shared with the Rust side
#
# Both leverage the new `Case.equalsOps(value, ops)` and chained
# `Case.equals(...).ops(...)` API added to TestCommon.
#
# Why not Gradle: the project's bundled wrapper (8.5) can't process
# Java 25 class files, so we drive the JVM directly through the
# jars Gradle already cached. See `build.sh` for the rationale.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LEEK="$ROOT/official-generator/leek-wars-generator/leekscript"
TOOL="$ROOT/tools/java-emitter"
MAIN_CLASSES="$TOOL/build/classes"
TEST_CLASSES="$LEEK/build/test-classes"
RUNNER_OUT="$TOOL/build/runner"

GCACHE="$HOME/.gradle/caches/modules-2/files-2.1"
JACKSON_DB=$(find "$GCACHE/tools.jackson.core/jackson-databind/3.0.3" -name "*.jar" | head -1)
JACKSON_CORE=$(find "$GCACHE/tools.jackson.core/jackson-core/3.0.3" -name "*.jar" | head -1)
JACKSON_ANN=$(find "$GCACHE/com.fasterxml.jackson.core/jackson-annotations" -name "*.jar" | sort -r | head -1)
JUNIT_API=$(find "$GCACHE/org.junit.jupiter/junit-jupiter-api" -name "*.jar" | sort -r | head -1)
JUNIT_ENGINE=$(find "$GCACHE/org.junit.jupiter/junit-jupiter-engine" -name "*.jar" | sort -r | head -1)
JUNIT_PLAT_COMM=$(find "$GCACHE/org.junit.platform/junit-platform-commons" -name "*.jar" | sort -r | head -1)
JUNIT_PLAT_ENG=$(find "$GCACHE/org.junit.platform/junit-platform-engine" -name "*.jar" | sort -r | head -1)
JUNIT_LAUNCHER=$(find "$GCACHE/org.junit.platform/junit-platform-launcher" -name "*.jar" | sort -r | head -1)
OPENTEST=$(find "$GCACHE/org.opentest4j/opentest4j" -name "*.jar" | sort -r | head -1)
APIGUARDIAN=$(find "$GCACHE/org.apiguardian/apiguardian-api" -name "*.jar" | sort -r | head -1)

for v in JACKSON_DB JACKSON_CORE JACKSON_ANN JUNIT_API JUNIT_ENGINE JUNIT_PLAT_COMM JUNIT_PLAT_ENG JUNIT_LAUNCHER OPENTEST APIGUARDIAN; do
  if [[ -z "${!v}" ]]; then
    echo "error: $v jar not found under $GCACHE" >&2
    exit 2
  fi
done

# Main classes are produced by build.sh.
if [[ ! -d "$MAIN_CLASSES" ]]; then
  echo "main classes missing — running build.sh first" >&2
  "$TOOL/build.sh"
fi

# Compile the upstream test sources (including TestCommon and the two
# new ops-cost suites).
mkdir -p "$TEST_CLASSES"
CP_TEST="$MAIN_CLASSES:$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN:$JUNIT_API:$JUNIT_PLAT_COMM:$OPENTEST:$APIGUARDIAN"
SOURCES=$(mktemp)
trap 'rm -f "$SOURCES"' EXIT
find "$LEEK/src/test/java" -name "*.java" > "$SOURCES"
echo "compiling $(wc -l < "$SOURCES") test sources..."
javac -d "$TEST_CLASSES" -cp "$CP_TEST" --release 25 @"$SOURCES"

# Compile the small launcher main (idempotent).
mkdir -p "$RUNNER_OUT"
JUNIT_CP="$JUNIT_API:$JUNIT_ENGINE:$JUNIT_PLAT_COMM:$JUNIT_PLAT_ENG:$JUNIT_LAUNCHER:$OPENTEST:$APIGUARDIAN"
javac -d "$RUNNER_OUT" -cp "$JUNIT_CP" --release 25 "$TOOL/RunOpsCostTest.java"

# Run from the repo root so the TSV-resolution in `TestOpsCostCorpus`
# finds `crates/backends/leek-backend-java/tests/fixtures/ops/cases.tsv`.
cd "$ROOT"
RUN_CP="$RUNNER_OUT:$TEST_CLASSES:$MAIN_CLASSES:$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN:$JUNIT_CP"
exec java -cp "$RUN_CP" RunOpsCostTest
