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
OVERLAY="$TOOL/overlay"
MAIN_CLASSES="$TOOL/build/classes"
TEST_CLASSES="$TOOL/build/test-classes"
RUNNER_OUT="$TOOL/build/runner"
REFERENCE="${1:-$ROOT/crates/testing/leek-test-corpus/data/reference.tsv}"
source "$TOOL/overlay.sh"

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
list_sources "$LEEK/src/test/java" "$OVERLAY/src/test/java" > "$SOURCES"
javac -d "$TEST_CLASSES" -cp "$CP_TEST" --release 25 @"$SOURCES"

# Compile the reference-generator main.
JUNIT_CP="$JUNIT_API:$JUNIT_ENGINE:$JUNIT_PLAT_COMM:$JUNIT_PLAT_ENG:$JUNIT_LAUNCHER:$OPENTEST:$APIGUARDIAN"
javac -d "$RUNNER_OUT" -cp "$JUNIT_CP" --release 25 "$TOOL/GenerateReference.java"

# What the dataset holds now: a refresh that produces far fewer rows than
# this is a partial run, not an update (checked after the JVM run below).
PREV_ROWS=0
if [[ -f "$REFERENCE" ]]; then
  PREV_ROWS=$(grep -cv '^#' "$REFERENCE" || true)
fi

# Per-run, per-user log: a fixed /tmp path is shared between users and
# between concurrent runs.
LOG="${TMPDIR:-/tmp}/reference-run.$$.log"
echo "log: $LOG"

# Run from a scratch directory (see `jvm_workdir`): the upstream compiler
# writes its `ai/` output tree relative to the cwd. The file-based tests
# (`ai/euler/*.leek`) live in the generator submodule and resolve from
# neither location; their rows are skipped either way.
cd "$(jvm_workdir "$TOOL")"
RUN_CP="$RUNNER_OUT:$TEST_CLASSES:$MAIN_CLASSES:$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN:$JUNIT_CP"
# No `|| true` here: a crashed JVM used to leave a truncated dataset
# behind and still exit 0, which read as a successful refresh (#148).
LEEK_REFERENCE="$REFERENCE" java -cp "$RUN_CP" GenerateReference > "$LOG" 2>&1

ROWS=$(grep -cv '^#' "$REFERENCE" 2>/dev/null || true)

# Derived from the file being replaced rather than hard-coded, so the
# floor cannot rot as the corpus grows. 90% leaves room for upstream
# genuinely dropping a few cases; a real collapse fails here.
FLOOR=${LEEK_REFERENCE_MIN_ROWS:-$(( PREV_ROWS * 9 / 10 ))}
if (( FLOOR < 1 )); then
  FLOOR=1
fi
if (( ROWS < FLOOR )); then
  echo "error: $REFERENCE has $ROWS rows, expected at least $FLOOR (was $PREV_ROWS)" >&2
  echo "       the run is incomplete — see $LOG; set LEEK_REFERENCE_MIN_ROWS to override" >&2
  exit 1
fi

echo "wrote $REFERENCE ($ROWS rows)"
