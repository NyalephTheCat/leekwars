# Sourced helpers (not executable) shared by the java-emitter scripts:
# javac source-list construction with the overlay shadowing the upstream
# submodule, and the scratch directory the JVM runs from.
#
# The upstream `leekscript` submodule stays PRISTINE — our
# instrumentation lives in `tools/java-emitter/overlay/src/…` instead
# of being patched into the submodule working tree:
#
#   main/java/leekscript/tools/EmitJava.java        emitter CLI (jar main class)
#   main/java/leekscript/tools/RunEmittedJava.java  batch compile+run harness
#   test/java/test/TestCommon.java                  + LEEK_SNAPSHOT / LEEK_REFERENCE
#                                                   probes, chainable Case API
#   test/java/test/TestOperators.java               adapted to the chainable API
#   test/java/test/TestOpsCost.java                 hand-written .equalsOps cases
#   test/java/test/TestOpsCostCorpus.java           TSV-driven, shared with Rust
#
# `list_sources <upstream-src-root> <overlay-src-root>` emits the
# union for a javac @sources file: every upstream file whose relative
# path is NOT shadowed by an overlay file, then every overlay file.
# To change an upstream class, copy it into the overlay at the same
# relative path and edit the copy.
list_sources() {
  local up="$1" ov="$2" f rel
  while IFS= read -r f; do
    rel="${f#"$up"/}"
    [[ -f "$ov/$rel" ]] || printf '%s\n' "$f"
  done < <(find "$up" -name '*.java')
  if [[ -d "$ov" ]]; then
    find "$ov" -name '*.java'
  fi
}

# `jvm_workdir <java-emitter-dir>` creates and echoes the directory the
# JVM should be run from. Nothing may run it from the repository root:
# the upstream compiler hard-codes its output to `./ai`
# (`JavaCompiler.IA_PATH`) relative to the working directory, and
# `TestCommon` writes `opérations.txt` the same way, so a run from the
# root drops hundreds of generated files next to `Cargo.toml` — which is
# exactly how an `ai/` tree once ended up committed. `build/` is
# git-ignored at any depth, so the scratch dir never shows up in
# `git status`.
jvm_workdir() {
  local dir="$1/build/jvm-cwd"
  mkdir -p "$dir"
  printf '%s\n' "$dir"
}
