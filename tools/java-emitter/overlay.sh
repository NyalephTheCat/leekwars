# Sourced helper (not executable): javac source-list construction with
# the overlay shadowing the upstream submodule.
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
