//! The client's breakpoint model.
//!
//! DAP's `setBreakpoints` is a *whole-file replace*: one request carries the
//! complete set of lines for one source, and an empty list clears that source.
//! [`BreakpointStore`] keeps exactly that, keyed by canonical path — two
//! spellings of one file (a relative `launch.json` path, a symlinked project
//! root) are one entry, not two.
//!
//! Requested lines stay requested here. Turning one into a line the debuggee
//! can actually stop on needs the compiled program, which exists only from
//! `configurationDone` onwards; that mapping is [`ProgramMap`], and
//! [`BreakpointStore::by_source`] applies it to produce what the debug
//! controller matches safepoints against.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use leek_span::paths::canonical_or_normalized;

/// One line the client asked to break on, after the store has taken it.
pub(crate) struct Requested {
    /// The line as the client sent it. Echoed back so the response array
    /// lines up with the request's entry for entry, as DAP requires.
    pub line: i64,
    /// The line as stored plus the id minted for it. `None` when `line` is
    /// not a line number any file can have — nothing was stored in that case.
    pub stored: Option<(u32, i64)>,
}

impl Requested {
    /// The breakpoint's id, for a response or a change event.
    pub(crate) fn id(&self) -> Option<i64> {
        self.stored.map(|(_, id)| id)
    }
}

/// Every breakpoint the client has set, by canonical source path.
#[derive(Default)]
pub(crate) struct BreakpointStore {
    /// Requested line → id, per file. Ordered so two lines snapping to the
    /// same safepoint always leave the same winner.
    by_path: HashMap<PathBuf, BTreeMap<u32, i64>>,
    next_id: i64,
}

impl BreakpointStore {
    /// Replace every breakpoint in `path` with `lines`, minting a fresh id
    /// per line. Returns one entry per requested line, in request order.
    ///
    /// The file's previous breakpoints are dropped wholesale — an empty
    /// `lines` is how DAP spells "clear this file".
    pub(crate) fn replace(&mut self, path: &Path, lines: &[i64]) -> Vec<Requested> {
        let mut stored: BTreeMap<u32, i64> = BTreeMap::new();
        let next_id = &mut self.next_id;
        let requested: Vec<Requested> = lines
            .iter()
            .map(|&line| {
                let Ok(key) = u32::try_from(line) else {
                    return Requested { line, stored: None };
                };
                // A line repeated within one request is one breakpoint; both
                // response entries name its id so a later change event
                // updates both.
                let id = *stored.entry(key).or_insert_with(|| {
                    *next_id += 1;
                    *next_id
                });
                Requested {
                    line,
                    stored: Some((key, id)),
                }
            })
            .collect();

        let path = canonical_or_normalized(path);
        if stored.is_empty() {
            self.by_path.remove(&path);
        } else {
            self.by_path.insert(path, stored);
        }
        requested
    }

    /// Every stored breakpoint as `(canonical path, requested line, id)`.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Path, u32, i64)> {
        self.by_path.iter().flat_map(|(path, lines)| {
            lines
                .iter()
                .map(move |(&line, &id)| (path.as_path(), line, id))
        })
    }

    /// The lines the debug controller matches safepoints against, keyed by
    /// raw `SourceId`: every stored breakpoint whose file the program was
    /// compiled from, snapped to the line it actually lands on.
    ///
    /// Breakpoints in files the program knows nothing about (another editor
    /// tab, an unsaved buffer) are left out rather than tested against some
    /// other file's line numbers.
    pub(crate) fn by_source(&self, program: &ProgramMap) -> HashMap<u32, HashMap<u32, i64>> {
        let mut by_source: HashMap<u32, HashMap<u32, i64>> = HashMap::new();
        for (path, line, id) in self.iter() {
            let Some(source) = program.source_of(path) else {
                continue;
            };
            let Some(line) = program.place(source, line) else {
                continue;
            };
            by_source
                .entry(source)
                .or_default()
                .entry(line)
                .or_insert(id);
        }
        by_source
    }
}

/// What the launched program compiled to, as far as breakpoints care: which
/// file is which `SourceId`, and which lines carry a debug safepoint.
///
/// Built once at `configurationDone`. The `Compiled` program itself is moved
/// into the run thread, so without this the path → `SourceId` mapping would be
/// gone the moment the debuggee starts — which is why breakpoints used to be
/// frozen at launch.
pub(crate) struct ProgramMap {
    /// Raw `SourceId` per canonical file path.
    sources: HashMap<PathBuf, u32>,
    /// Lines carrying a debug safepoint, per raw `SourceId`. The debuggee can
    /// stop on these lines and no others.
    safepoints: HashMap<u32, BTreeSet<u32>>,
}

impl ProgramMap {
    pub(crate) fn new(
        sources: HashMap<PathBuf, u32>,
        safepoints: HashMap<u32, BTreeSet<u32>>,
    ) -> Self {
        Self {
            sources,
            safepoints,
        }
    }

    /// Raw `SourceId` of a file the program was compiled from.
    pub(crate) fn source_of(&self, path: &Path) -> Option<u32> {
        self.sources.get(&canonical_or_normalized(path)).copied()
    }

    /// The line a breakpoint on `line` actually lands on: `line` itself when
    /// it carries a safepoint, else the next line below it that does — a
    /// breakpoint on a blank line or a comment slides down to the next
    /// statement, the way every other debugger behaves. `None` when nothing
    /// executable follows it in that file.
    pub(crate) fn place(&self, source: u32, line: u32) -> Option<u32> {
        self.safepoints.get(&source)?.range(line..).next().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that need not exist: `canonical_or_normalized` falls
    /// back to the lexically normalized form for a path that does not
    /// resolve, so the store keys on it deterministically either way.
    const DIR: &str = "/tmp/leek-dap-store";

    fn store_with(path: &Path, lines: &[i64]) -> BreakpointStore {
        let mut store = BreakpointStore::default();
        store.replace(path, lines);
        store
    }

    /// Lines held for `path`, low to high.
    fn lines_of(store: &BreakpointStore, path: &Path) -> Vec<u32> {
        let path = canonical_or_normalized(path);
        let mut lines: Vec<u32> = store
            .iter()
            .filter(|(p, _, _)| *p == path)
            .map(|(_, line, _)| line)
            .collect();
        lines.sort_unstable();
        lines
    }

    #[test]
    fn a_second_request_replaces_the_files_lines() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = store_with(&main, &[2, 3]);
        store.replace(&main, &[5]);
        assert_eq!(lines_of(&store, &main), vec![5]);
    }

    #[test]
    fn an_empty_request_clears_the_file() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = store_with(&main, &[2, 3]);
        store.replace(&main, &[]);
        assert!(lines_of(&store, &main).is_empty());
    }

    #[test]
    fn replacing_one_file_leaves_the_others_alone() {
        let main = Path::new(DIR).join("main.leek");
        let lib = Path::new(DIR).join("lib.leek");
        let mut store = store_with(&main, &[12]);
        store.replace(&lib, &[12]);
        store.replace(&main, &[]);
        assert!(lines_of(&store, &main).is_empty());
        assert_eq!(lines_of(&store, &lib), vec![12]);
    }

    #[test]
    fn every_line_gets_its_own_id() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = BreakpointStore::default();
        let ids: Vec<Option<i64>> = store
            .replace(&main, &[2, 3, 2])
            .iter()
            .map(Requested::id)
            .collect();
        assert_eq!(ids[0], ids[2], "a repeated line is one breakpoint");
        assert_ne!(ids[0], ids[1], "distinct lines get distinct ids");
        // A later request must not hand back an id the client already holds.
        let reused = store.replace(&main, &[2]);
        assert!(!ids.contains(&reused[0].id()), "ids were not re-minted");
    }

    #[test]
    fn a_line_no_file_can_have_is_stored_nowhere() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = BreakpointStore::default();
        let requested = store.replace(&main, &[-1]);
        assert!(requested[0].stored.is_none());
        assert!(lines_of(&store, &main).is_empty());
    }

    #[test]
    fn a_breakpoint_snaps_down_to_the_next_line_with_code() {
        let program = ProgramMap::new(HashMap::new(), HashMap::from([(1, BTreeSet::from([2, 7]))]));
        assert_eq!(program.place(1, 2), Some(2), "a line with code stays put");
        assert_eq!(program.place(1, 4), Some(7), "a blank line slides down");
        assert_eq!(program.place(1, 9), None, "nothing executable follows");
        assert_eq!(program.place(2, 2), None, "a source with no safepoints");
    }

    #[test]
    fn breakpoints_in_two_files_do_not_merge() {
        let (main, lib) = (
            Path::new(DIR).join("main.leek"),
            Path::new(DIR).join("lib.leek"),
        );
        let mut store = BreakpointStore::default();
        store.replace(&main, &[12]);
        store.replace(&lib, &[12]);

        let program = ProgramMap::new(
            HashMap::from([
                (canonical_or_normalized(&main), 1),
                (canonical_or_normalized(&lib), 2),
            ]),
            HashMap::from([(1, BTreeSet::from([12])), (2, BTreeSet::from([12]))]),
        );
        let by_source = store.by_source(&program);
        assert_eq!(by_source[&1].len(), 1);
        assert_eq!(by_source[&2].len(), 1);
        assert_ne!(
            by_source[&1][&12], by_source[&2][&12],
            "the two files' line-12 breakpoints are different breakpoints"
        );
    }

    #[test]
    fn a_breakpoint_in_an_unknown_file_reaches_no_source() {
        let mut store = BreakpointStore::default();
        store.replace(&Path::new(DIR).join("scratch.leek"), &[1]);
        let program = ProgramMap::new(
            HashMap::from([(
                canonical_or_normalized(&Path::new(DIR).join("main.leek")),
                1,
            )]),
            HashMap::from([(1, BTreeSet::from([1]))]),
        );
        assert!(store.by_source(&program).is_empty());
    }
}
