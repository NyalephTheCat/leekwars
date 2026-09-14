# The language server

How `leek-lsp` is put together: what state it holds, how it decides which
files can see each other, how requests reach that state, and where its
output goes.

This is the design document. For *running* the server — building it,
pointing an editor at it, the `LEEK_LSP_LOG` values — see
[`bins/leek-lsp/README.md`](../bins/leek-lsp/README.md). For the list of
protocol methods it answers, see the crate docs of
[`leek-lsp`](../crates/tools/leek-lsp/src/lib.rs) and, authoritatively,
the `ServerCapabilities` block in `server.rs`.

## The workspace

One server process holds exactly one `Workspace` (`workspace.rs`), and
everything analysis needs hangs off it:

- **`db`** — a salsa `LeekDb`. Every file the server knows about, open or
  not, is a salsa input, so an edit invalidates only what actually
  depended on the changed text and a per-keystroke re-run mostly hits
  cache.
- **`docs`** — the open buffers, keyed by URI. Each carries its salsa
  input handle, a `LineTable` for UTF-16 position conversion, a snapshot
  of the text, and the client's version number (echoed back on
  `publishDiagnostics` so the editor can drop a set computed against a
  revision it has already moved past).
- **`indexed`** — `.leek` files found on disk under a project root but not
  open in the editor. They exist so that cross-file features work before
  the user has opened the other file.
- **`roots`** — one `ProjectIndex` per workspace root. Language defaults
  (the version a file without an `@version` pragma is analyzed under) come
  from the root that *owns* a file, so several roots — or a project nested
  inside an outer one — each keep their own manifest rather than the first
  one winning.

A file is analyzed as an `AnalysisTarget`, which is the open buffer where
there is one and the indexed copy otherwise. Handlers ask the workspace
for targets rather than reaching into either map, so "the editor has this
file open" stops being a special case past that boundary.

### Program scope

Leekscript has a single flat namespace — but flat *per program*, where a
program is an entry file plus everything it transitively `include`s. Two
unrelated AIs in one workspace may each define a top-level `tick()`, and
those are different symbols. A function in a shared `util.leek` is the
*same* symbol for every AI that includes it.

So "which files does this declaration reach?" is not "the workspace" and
not "the file". For a symbol declared in file `D`, the scope is the union
of every include-closure that contains `D`:

```text
scope(D) = ⋃ { closure(E) : E ∈ workspace, D ∈ closure(E) }
```

A file `X` is in scope exactly when some program contains both `X` and
`D` — i.e. when they can see each other's flat-namespace symbols. This is
deliberately not an undirected connected-component search, which would
merge two independent AIs the moment they shared one library file.

`handlers/program_scope.rs` computes it, and the cross-file features
(references, rename, workspace symbols, call hierarchy) are bounded by
it. Rename in particular depends on it being right: renaming across a
scope that is too wide corrupts an unrelated AI.

Class names are the one deliberate exception. Upstream resolves
`getDefinedClass` program-wide, so the server feeds the sorted union of
every file's `class IDENT` declarations into every salsa input's
`extra_classes` — that is what lets any file use any project class as a
type head. The union is only written back when it actually changed, since
writing a salsa input re-parses.

## Threading

`tower-lsp` drives a tokio runtime and dispatches each request as a task.
The workspace sits behind a single `tokio::sync::Mutex`, and a handler
takes it for the duration of its work; analysis itself is synchronous.
So requests are decoded and queued concurrently, but only one of them is
ever inside the salsa database at a time.

Two consequences are worth knowing:

- **Handlers are panic-guarded.** `util/guard.rs` wraps each handler body
  in `catch_unwind`. The server is long-running and parses incomplete,
  untrusted buffers on every keystroke; a panic must fail one request,
  not the process (tower-lsp 0.20 does not catch handler panics — an
  unwind escaping one aborts the serve future and the process with it).
  A `tokio::sync::Mutex` does not poison, so the workspace guard is
  released cleanly on the caught unwind.
- **Shutdown is explicit.** tower-lsp's serve loop ends on stdin EOF, not
  on the `exit` notification, so `shutdown` raises a `Notify` the stdio
  driver awaits. Without it an editor's "restart server" can leave the
  old process alive. The blocking stdin reader is detached after a short
  grace period rather than joined, because it only wakes on the next byte
  — which, after a restart, never arrives.

### Saying no

Most handlers answer "nothing to offer" with `None`, which an editor
renders as a silent no-op. For a *destructive* request that is the wrong
answer: when the occurrence search behind a rename is known to be
unsound, the user needs to be told why rather than handed a corrupted
buffer. `handlers/refusal.rs` carries that answer, and the server turns
it into a JSON-RPC `RequestFailed` (`-32803`) whose message editors show
the user.

## Diagnostics

`diagnostics.rs` builds one set per file, and both transports serve that
same set: push (`textDocument/publishDiagnostics`, on open and change)
and pull (`textDocument/diagnostic`). `textDocument/codeAction` reads it
too, which is why the quick fixes an editor offers always match the
squiggles it is showing.

Each diagnostic carries `source: "leek"` so handlers can tell our own
diagnostics from another server's when a client hands some back, and — for
codes that have an extended write-up — a `codeDescription.href` pointing
at `explain/<ID>.md` in this repository. That URL is built from the
crate's `CARGO_PKG_REPOSITORY`, inherited from `[workspace.package]`, so
it cannot drift away from where the code actually lives.

A secondary label is resolved through its *own* file. One whose file the
workspace cannot name is dropped rather than pinned to the open document
at a range computed from the wrong text, which would send the editor
somewhere real and wrong.

## Configuration

The client owns settings (VS Code's `leek` section); the server mirrors
them in `settings.rs` and handlers consult the mirror. Parsing is lenient
on purpose — unknown keys are ignored and a missing key keeps its default
— so a partial or differently-shaped settings blob never breaks the
server. Today the mirror holds the formatter's `FormatOptions` and an
inlay-hint toggle.

## Logging

A language server has no terminal, so `log.rs` is the single seam and
everything goes through `tracing`:

- the **binaries** install the sink, never the library: a crate that
  grabs the global subscriber cannot be embedded;
- the stderr layer writes to the stream `vscode-languageclient` captures
  and forwards to the editor's output channel;
- `ClientLayer` additionally mirrors **warnings and errors** to the editor
  over `window/logMessage`, so a failure the user can feel — a refused
  format, a project that failed to index, a handler that panicked — is
  reported where they will see it.

Anything below `WARN` deliberately stays on stderr. `publishDiagnostics`
traces once per keystroke and would flood the editor's channel.

`LEEK_LSP_LOG` is read **once**, at init; the default filter is
`leek_lsp=info`, i.e. lifecycle events only. An unparseable value falls
back to that default rather than aborting the server before it can say
why. The accepted values are tabulated in `log.rs`.

A bare `println!`/`eprintln!` is a report a user's bug report can never
contain, so `clippy::print_stdout` and `clippy::print_stderr` are warned
on crate-wide.
