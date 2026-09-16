#!/usr/bin/env bash
# Run a leek-wars-generator fight between two Leekscript AIs, using the
# Java emitted by THIS project's compiler (leekc) rather than the
# generator's built-in Leekscript compiler.
#
# The generator is treated as a read-only library: it is never edited,
# so it can be updated freely. The integration works entirely through
# public, non-final hooks:
#   * leekc emits each AI as `class AI_<id> extends
#     com.leekwars.generator.fight.entity.EntityAI` (--base-class), with
#     game functions dispatched to com.leekwars.generator.classes.* via
#     `--library leekwars`.
#   * Harness.java injects each pre-compiled AI by subclassing AIFile and
#     overriding compile() to return our instance — the generator's
#     Fight.startFight rebuilds AIs through that exact call.
#
# Usage:  fight.sh <ai1.leek> <ai2.leek> [seed]
# Output: the fight Outcome as JSON on stdout (the same shape the real
#         generator's Main prints), plus a one-line winner summary on
#         stderr.
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "usage: $0 <ai1.leek> <ai2.leek> [seed]" >&2
  exit 2
fi

AI1_SRC=$1
AI2_SRC=$2
SEED=${3:-1}
BASE_CLASS="com.leekwars.generator.fight.entity.EntityAI"

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GEN="$ROOT/official-generator/leek-wars-generator"
HARNESS="$ROOT/tools/fight-harness/Harness.java"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# 0. The generator's sources target Java 25 (`invalid source release: 25`
#    under anything older), so its build needs a JDK 25. Point JAVA_HOME at
#    one if the default `java` is older:
#
#      curl -sSL -o /tmp/jdk25.tar.gz \
#        'https://api.adoptium.net/v3/binary/latest/25/ga/linux/x64/jdk/hotspot/normal/eclipse'
#      mkdir -p /opt/jdk25 && tar -xzf /tmp/jdk25.tar.gz -C /opt/jdk25 --strip-components=1
#      export JAVA_HOME=/opt/jdk25 PATH=/opt/jdk25/bin:$PATH
#
# 1. Ensure the generator's classes are compiled. Gradle 8.5 can't parse
#    init scripts under JDK 25, but `compileJava` itself works, so we
#    build (once) and read the class dirs directly.
if [[ ! -d "$GEN/build/classes/java/main/com" ]]; then
  echo "building generator classes (one-time)..." >&2
  (cd "$GEN" && ./gradlew -q compileJava)
fi

# 2. Assemble the generator's runtime classpath: its own classes, the
#    :leekscript subproject's classes, and every dependency jar in the
#    Gradle cache (skipping -sources/-javadoc artifacts and the test-only
#    ones). Taken wholesale rather than jar by jar because the generator
#    adds dependencies as it grows — v3.00 brought GraalVM Polyglot in for
#    JS/Python AIs, and a hand-listed classpath silently went missing a
#    class instead of saying what it wanted.
GC="$HOME/.gradle/caches/modules-2/files-2.1"
DEP_JARS=$(find "$GC" -name "*.jar" ! -name "*-sources.jar" ! -name "*-javadoc.jar" 2>/dev/null \
  | grep -v -e '/org\.junit' -e '/org\.opentest4j' | tr '\n' ':')
if [[ -z "$DEP_JARS" ]]; then
  echo "error: no generator dependency jars in the Gradle cache ($GC)." >&2
  echo "       run 'cd \"$GEN\" && ./gradlew compileJava' to populate it." >&2
  exit 3
fi
CP="$GEN/build/classes/java/main:$GEN/leekscript/build/classes/java/main:${DEP_JARS%:}"

# 3. Emit each AI to Java with leekc (generator-compatible base class +
#    leek-wars game-function dispatch).
emit() {
  local src=$1 id=$2
  cargo run -q -p leekc --manifest-path "$ROOT/Cargo.toml" -- \
    "$src" --emit java --clean --library leekwars --fold-constants \
    --base-class "$BASE_CLASS" --ai-id "$id" --out-dir "$WORK" >/dev/null
}
echo "emitting AIs..." >&2
emit "$AI1_SRC" 1
emit "$AI2_SRC" 2

# 4. Compile the harness + both emitted AIs against the generator.
echo "compiling Java..." >&2
javac -cp "$CP" -d "$WORK" "$HARNESS" "$WORK/AI_1.java" "$WORK/AI_2.java"

# 5. Run the fight. cwd must be the generator root so `new Generator()`
#    can read its relative `data/*.json`. The harness prints the Outcome
#    JSON on the line after the @@OUTCOME@@ marker.
echo "running fight (seed=$SEED)..." >&2
RAW="$(cd "$GEN" && java -cp "$CP:$WORK" Harness AI_1 AI_2 "$SEED")"
JSON="$(printf '%s\n' "$RAW" | sed -n '/@@OUTCOME@@/,$p' | tail -n +2)"

WINNER="$(printf '%s' "$JSON" | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d.get("winner"))' 2>/dev/null || echo '?')"
echo "winner: team $WINNER" >&2
printf '%s\n' "$JSON"
