#!/usr/bin/env bash
# Compile the upstream LeekScript Java sources + the EmitJava driver
# into a classes directory and a manifest jar, so the Rust-side
# parity tests can shell out to `java -jar leekscript-emitter.jar`.
#
# We bypass the project's Gradle build because the bundled wrapper
# (8.5) doesn't speak Java 25 class files. Compiling with javac
# against the Jackson jars Gradle has already cached gives us the
# same artifacts with far less moving parts.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
LEEK="$ROOT/official-generator/leek-wars-generator/leekscript"
OVERLAY="$ROOT/tools/java-emitter/overlay"
OUT_DIR="$ROOT/tools/java-emitter/build"
CLASSES="$OUT_DIR/classes"
JAR="$OUT_DIR/leekscript-emitter.jar"
source "$ROOT/tools/java-emitter/overlay.sh"

# Resolve Jackson 3.0.3 + annotations from Gradle's cache. Bail
# with an actionable message if the cache hasn't been populated yet.
GCACHE="$HOME/.gradle/caches/modules-2/files-2.1"
# Skip -sources / -javadoc jars (Gradle's hashed dir ordering doesn't
# guarantee the main artifact is first), and prefer the already-staged
# jars in $OUT_DIR/ if present so a one-off rebuild doesn't depend on
# the Gradle cache.
find_main_jar() {
  if [[ -f "$OUT_DIR/$2" ]]; then
    echo "$OUT_DIR/$2"
    return
  fi
  find "$1" -name "$2" ! -name "*-sources.jar" ! -name "*-javadoc.jar" | head -1
}
JACKSON_DB=$(find_main_jar "$GCACHE/tools.jackson.core/jackson-databind/3.0.3" "jackson-databind-3.0.3.jar")
JACKSON_CORE=$(find_main_jar "$GCACHE/tools.jackson.core/jackson-core/3.0.3" "jackson-core-3.0.3.jar")
JACKSON_ANN=$(find "$GCACHE/com.fasterxml.jackson.core/jackson-annotations" -name "*.jar" ! -name "*-sources.jar" ! -name "*-javadoc.jar" | sort -r | head -1 || true)
if [[ -z "${JACKSON_ANN}" && -f "$OUT_DIR/jackson-annotations-2.20.jar" ]]; then
  JACKSON_ANN="$OUT_DIR/jackson-annotations-2.20.jar"
fi

if [[ -z "${JACKSON_DB}" || -z "${JACKSON_CORE}" || -z "${JACKSON_ANN}" ]]; then
  echo "error: jackson jars not found under $GCACHE." >&2
  echo "       Try: cd '$LEEK' && gradle dependencies > /dev/null" >&2
  echo "       (or copy the jars in manually)" >&2
  exit 2
fi

CP="$JACKSON_DB:$JACKSON_CORE:$JACKSON_ANN"

mkdir -p "$CLASSES"
SOURCES=$(mktemp)
trap 'rm -f "$SOURCES"' EXIT
# Submodule sources + the EmitJava/RunEmittedJava drivers from the
# overlay (the submodule working tree stays pristine).
list_sources "$LEEK/src/main/java" "$OVERLAY/src/main/java" > "$SOURCES"

echo "compiling $(wc -l < "$SOURCES") sources into $CLASSES" >&2
javac -d "$CLASSES" -cp "$CP" --release 25 @"$SOURCES"

# Build a manifest jar with EmitJava as the main class and Jackson on
# the classpath via relative paths inside the jar. Jackson jars are
# kept separate (not fat-jar'd) so the build stays fast and the
# classpath is explicit.
cat > "$OUT_DIR/manifest.txt" <<EOF
Main-Class: leekscript.tools.EmitJava
Class-Path: $(basename "$JACKSON_DB") $(basename "$JACKSON_CORE") $(basename "$JACKSON_ANN")
EOF

jar cfm "$JAR" "$OUT_DIR/manifest.txt" -C "$CLASSES" .

# Side-by-side the jars so the manifest Class-Path resolves.
copy_if_different() {
  local src="$1"
  local dst="$2/$(basename "$1")"
  if [[ "$src" != "$dst" ]]; then
    cp -f "$src" "$dst"
  fi
}
copy_if_different "$JACKSON_DB" "$OUT_DIR"
copy_if_different "$JACKSON_CORE" "$OUT_DIR"
copy_if_different "$JACKSON_ANN" "$OUT_DIR"

echo "built $JAR"
