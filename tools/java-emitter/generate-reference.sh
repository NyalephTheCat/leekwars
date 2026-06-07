#!/usr/bin/env bash
# Run the *entire* upstream Java test package with the `LEEK_REFERENCE`
# probe enabled. Produces a TSV reference dataset at
# `crates/testing/leek-test-corpus/data/reference.tsv` with one row per
# passing value-bearing assertion:
#
#     version  strict  kind  value  jvm_ops  code  generated_java
#
# This is the golden corpus the `leek-test-corpus` build embeds (the
# official value, op count, and Java emission per case). It is a richer
# sibling of `generate-snapshot.sh` (which covers a curated 7-class
# slice for the Java-backend parity test) — here we run all `test.Test*`
# classes so the reference spans the whole corpus.
#
# Usage: tools/java-emitter/generate-reference.sh [OUTPUT_TSV]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LEEK="$ROOT/official-generator/leek-wars-generator/leekscript"
TOOL="$ROOT/tools/java-emitter"
MAIN_CLASSES="$TOOL/build/classes"
TEST_CLASSES="$LEEK/build/test-classes"
RUNNER_OUT="$TOOL/build/runner"
REFERENCE="${1:-$ROOT/crates/testing/leek-test-corpus/data/reference.tsv}"

GCACHE="$HOME/.gradle/caches/modules-2/files-2.1"
JACKSON_DB=$(find "$GCACHE/tools.jackson.core/jackson-databind/3.0.3" -name "*.jar" ! -name "*-sources.jar" ! -name "*-javadoc.jar" | head -1)
JACKSON_CORE=$(find "$GCACHE/tools.jackson.core/jackson-core/3.0.3" -name "*.jar" ! -name "*-sources.jar" ! -name "*-javadoc.jar" | head -1)
JACKSON_ANN=$(find "$GCACHE/com.fasterxml.jackson.core/jackson-annotations" -name "*.jar" ! -name "*-sources.jar" ! -name "*-javadoc.jar" | sort -r | head -1)
# Exclude -sources/-javadoc jars: those carry .java, not .class, and
# feeding them to javac fails (e.g. apiguardian's API.java not found).
jar_main() { find "$1" -name "*.jar" ! -name "*-sources.jar" ! -name "*-javadoc.jar" | sort -r | head -1; }
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
mkdir -p "$TEST_CLASSES" "$RUNNER_OUT" "$(dirname "$REFERENCE")"

# Recompile the upstream test sources (cheap; idempotent).
CP_TEST="$MAIN_CLASSES:$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN:$JUNIT_API:$JUNIT_PLAT_COMM:$OPENTEST:$APIGUARDIAN"
SOURCES=$(mktemp)
trap 'rm -f "$SOURCES"' EXIT
find "$LEEK/src/test/java" -name "*.java" > "$SOURCES"
javac -d "$TEST_CLASSES" -cp "$CP_TEST" --release 25 @"$SOURCES"

# Compile the reference-generator main.
JUNIT_CP="$JUNIT_API:$JUNIT_ENGINE:$JUNIT_PLAT_COMM:$JUNIT_PLAT_ENG:$JUNIT_LAUNCHER:$OPENTEST:$APIGUARDIAN"
javac -d "$RUNNER_OUT" -cp "$JUNIT_CP" --release 25 "$TOOL/GenerateReference.java"

# Run from the repo root so file-based tests' relative paths resolve
# (their rows are skipped, but the suite still tries to load them).
cd "$ROOT"
RUN_CP="$RUNNER_OUT:$TEST_CLASSES:$MAIN_CLASSES:$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN:$JUNIT_CP"
LEEK_REFERENCE="$REFERENCE" java -cp "$RUN_CP" GenerateReference > /tmp/reference-run.log 2>&1 || true

ROWS=$(grep -cv '^#' "$REFERENCE" 2>/dev/null || true)
echo "wrote $REFERENCE ($ROWS rows)"
echo "log: /tmp/reference-run.log"
