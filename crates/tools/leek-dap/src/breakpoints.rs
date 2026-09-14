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
//!
//! A breakpoint is more than a line: DAP hangs a `condition`, a `hitCondition`
//! and a `logMessage` off it, and each is an expression in the debugged
//! program's own language. They are compiled in [`BreakpointStore::by_source`]
//! rather than when the client sets them, because compiling needs the language
//! version and that is not known until the program is — which is exactly why
//! `configurationDone` re-answers every breakpoint.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use leek_span::paths::canonical_or_normalized;

use crate::expr::{self, CondExpr, HitCondition, LogMessage};

/// One breakpoint as the client asked for it: a line, plus the expressions
/// DAP hangs off it.
#[derive(Clone, Default)]
pub(crate) struct BreakpointSpec {
    pub line: i64,
    /// Stop only when this expression is truthy.
    pub condition: Option<String>,
    /// Stop only on the hits this selects (`5`, `>5`, `%3`).
    pub hit_condition: Option<String>,
    /// Log this instead of stopping; `{expression}` holes are interpolated.
    pub log_message: Option<String>,
}

impl BreakpointSpec {
    /// A plain line breakpoint, with nothing hung off it.
    #[cfg(test)]
    pub(crate) fn line(line: i64) -> Self {
        Self {
            line,
            ..Self::default()
        }
    }
}

/// One breakpoint the store has taken, with the id it minted for it.
pub(crate) struct StoredBreakpoint {
    pub id: i64,
    pub spec: BreakpointSpec,
}

/// One line the client asked to break on, after the store has taken it.
pub(crate) struct Requested {
    /// The line as the client sent it. Echoed back so the response array
    /// lines up with the request's entry for entry, as DAP requires.
    pub line: i64,
    /// The line as stored plus the id minted for it. `None` when `line` is
    /// not a line number any file can have — nothing was stored in that case.
    pub stored: Option<(u32, i64)>,
    /// What was hung off it, so the answer can report a condition that does
    /// not compile rather than arming a breakpoint that never fires.
    pub spec: BreakpointSpec,
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
    /// Requested line → breakpoint, per file. Ordered so two lines snapping
    /// to the same safepoint always leave the same winner.
    by_path: HashMap<PathBuf, BTreeMap<u32, StoredBreakpoint>>,
    next_id: i64,
}

impl BreakpointStore {
    /// Replace every breakpoint in `path` with `specs`, minting a fresh id
    /// per line. Returns one entry per requested line, in request order.
    ///
    /// The file's previous breakpoints are dropped wholesale — an empty
    /// `specs` is how DAP spells "clear this file".
    pub(crate) fn replace(&mut self, path: &Path, specs: &[BreakpointSpec]) -> Vec<Requested> {
        let mut stored: BTreeMap<u32, StoredBreakpoint> = BTreeMap::new();
        let next_id = &mut self.next_id;
        let requested: Vec<Requested> = specs
            .iter()
            .map(|spec| {
                let line = spec.line;
                let Ok(key) = u32::try_from(line) else {
                    return Requested {
                        line,
                        stored: None,
                        spec: spec.clone(),
                    };
                };
                // A line repeated within one request is one breakpoint; both
                // response entries name its id so a later change event
                // updates both. The first spelling wins, since there is one
                // breakpoint and it can only carry one condition.
                let id = stored
                    .entry(key)
                    .or_insert_with(|| {
                        *next_id += 1;
                        StoredBreakpoint {
                            id: *next_id,
                            spec: spec.clone(),
                        }
                    })
                    .id;
                Requested {
                    line,
                    stored: Some((key, id)),
                    spec: spec.clone(),
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

    /// Every stored breakpoint as `(canonical path, requested line, entry)`.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&Path, u32, &StoredBreakpoint)> {
        self.by_path.iter().flat_map(|(path, lines)| {
            lines
                .iter()
                .map(move |(&line, stored)| (path.as_path(), line, stored))
        })
    }

    /// The lines the debug controller matches safepoints against, keyed by
    /// raw `SourceId`: every stored breakpoint whose file the program was
    /// compiled from, snapped to the line it actually lands on.
    ///
    /// Breakpoints in files the program knows nothing about (another editor
    /// tab, an unsaved buffer) are left out rather than tested against some
    /// other file's line numbers.
    /// A breakpoint whose condition, hit condition or log message does not
    /// compile is left out too — it is reported `verified: false` with the
    /// compiler's own message, and a breakpoint the client is told is dead
    /// must not fire.
    pub(crate) fn by_source(&self, program: &ProgramMap) -> HashMap<u32, HashMap<u32, Trigger>> {
        let mut by_source: HashMap<u32, HashMap<u32, Trigger>> = HashMap::new();
        for (path, line, stored) in self.iter() {
            let Some(source) = program.source_of(path) else {
                continue;
            };
            let Some(line) = program.place(source, line) else {
                continue;
            };
            let Ok(trigger) = Trigger::compile(stored, program.version()) else {
                continue;
            };
            by_source
                .entry(source)
                .or_default()
                .entry(line)
                .or_insert(trigger);
        }
        by_source
    }
}

/// What the debug controller tests when a safepoint lands on a breakpoint's
/// line: the id to blame the stop on, plus whatever the client hung off it.
///
/// Owned plain data on purpose. It is handed to a controller that reads it
/// from the debuggee thread, so it holds neither a rowan node (thread-local)
/// nor a [`leek_runtime::Value`] (`Rc`-based) — only the compiled, `Send`
/// forms from [`crate::expr`].
#[derive(Clone)]
pub(crate) struct Trigger {
    pub id: i64,
    /// Stop only when this is truthy; `None` stops every time.
    pub condition: Option<Arc<CondExpr>>,
    /// Stop only on the hits this selects; `None` stops on every one.
    pub hit: Option<HitCondition>,
    /// Log this and keep running, instead of stopping.
    pub log: Option<Arc<LogMessage>>,
    /// The debugged program's language version. `==` and `/` mean different
    /// things across versions, so it travels with the expression rather than
    /// being read off whatever thread evaluates it.
    pub version: u8,
}

impl Trigger {
    /// Compile one stored breakpoint's expressions, or report the first that
    /// does not compile.
    fn compile(stored: &StoredBreakpoint, version: u8) -> Result<Self, String> {
        let compile_one = |text: &Option<String>| -> Result<Option<Arc<CondExpr>>, String> {
            text.as_deref()
                .filter(|text| !text.trim().is_empty())
                .map(|text| expr::compile(text, version).map(Arc::new))
                .transpose()
        };
        Ok(Self {
            id: stored.id,
            condition: compile_one(&stored.spec.condition)?,
            hit: stored
                .spec
                .hit_condition
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .map(HitCondition::parse)
                .transpose()?,
            log: stored
                .spec
                .log_message
                .as_deref()
                .filter(|text| !text.is_empty())
                .map(|text| expr::compile_log(text, version).map(Arc::new))
                .transpose()?,
            version,
        })
    }

    /// The expressions a stored breakpoint carries, compiled — or why they
    /// could not be. Used by the response/change-event answer, which reports
    /// the failure rather than arming a breakpoint that can never fire.
    pub(crate) fn check(stored: &StoredBreakpoint, version: u8) -> Result<(), String> {
        Self::compile(stored, version).map(|_| ())
    }

    /// A breakpoint with nothing hung off it: stop on every arrival.
    #[cfg(test)]
    pub(crate) fn plain(id: i64) -> Self {
        Self {
            id,
            condition: None,
            hit: None,
            log: None,
            version: leek_span::pragma::LATEST_VERSION,
        }
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
    /// The language version the program was compiled at, which every
    /// expression the client sends must be compiled and evaluated at too.
    version: u8,
}

impl ProgramMap {
    pub(crate) fn new(
        sources: HashMap<PathBuf, u32>,
        safepoints: HashMap<u32, BTreeSet<u32>>,
        version: u8,
    ) -> Self {
        Self {
            sources,
            safepoints,
            version,
        }
    }

    /// The language version the program was compiled at.
    pub(crate) fn version(&self) -> u8 {
        self.version
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

    /// Every line in `start ..= end` the debuggee can stop on — what
    /// `breakpointLocations` answers, so an editor can grey out the gutter of
    /// a comment or a blank line instead of offering a breakpoint there.
    pub(crate) fn lines_between(&self, source: u32, start: u32, end: u32) -> Vec<u32> {
        self.safepoints
            .get(&source)
            .map(|lines| lines.range(start..=end).copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that need not exist: `canonical_or_normalized` falls
    /// back to the lexically normalized form for a path that does not
    /// resolve, so the store keys on it deterministically either way.
    const DIR: &str = "/tmp/leek-dap-store";

    /// Latest language version, which a program with no `@version` pragma
    /// compiles at.
    const LATEST: u8 = leek_span::pragma::LATEST_VERSION;

    /// Plain line breakpoints, the shape most of these tests care about.
    fn lines(lines: &[i64]) -> Vec<BreakpointSpec> {
        lines.iter().copied().map(BreakpointSpec::line).collect()
    }

    fn store_with(path: &Path, at: &[i64]) -> BreakpointStore {
        let mut store = BreakpointStore::default();
        store.replace(path, &lines(at));
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

    /// A one-file program: `main.leek` is source 1, with safepoints on
    /// `safepoints`.
    fn program(path: &Path, safepoints: &[u32]) -> ProgramMap {
        ProgramMap::new(
            HashMap::from([(canonical_or_normalized(path), 1)]),
            HashMap::from([(1, safepoints.iter().copied().collect::<BTreeSet<u32>>())]),
            LATEST,
        )
    }

    #[test]
    fn a_second_request_replaces_the_files_lines() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = store_with(&main, &[2, 3]);
        store.replace(&main, &lines(&[5]));
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
        store.replace(&lib, &lines(&[12]));
        store.replace(&main, &[]);
        assert!(lines_of(&store, &main).is_empty());
        assert_eq!(lines_of(&store, &lib), vec![12]);
    }

    #[test]
    fn every_line_gets_its_own_id() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = BreakpointStore::default();
        let ids: Vec<Option<i64>> = store
            .replace(&main, &lines(&[2, 3, 2]))
            .iter()
            .map(Requested::id)
            .collect();
        assert_eq!(ids[0], ids[2], "a repeated line is one breakpoint");
        assert_ne!(ids[0], ids[1], "distinct lines get distinct ids");
        // A later request must not hand back an id the client already holds.
        let reused = store.replace(&main, &lines(&[2]));
        assert!(!ids.contains(&reused[0].id()), "ids were not re-minted");
    }

    #[test]
    fn a_line_no_file_can_have_is_stored_nowhere() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = BreakpointStore::default();
        let requested = store.replace(&main, &lines(&[-1]));
        assert!(requested[0].stored.is_none());
        assert!(lines_of(&store, &main).is_empty());
    }

    #[test]
    fn a_breakpoint_snaps_down_to_the_next_line_with_code() {
        let program = ProgramMap::new(
            HashMap::new(),
            HashMap::from([(1, BTreeSet::from([2, 7]))]),
            LATEST,
        );
        assert_eq!(program.place(1, 2), Some(2), "a line with code stays put");
        assert_eq!(program.place(1, 4), Some(7), "a blank line slides down");
        assert_eq!(program.place(1, 9), None, "nothing executable follows");
        assert_eq!(program.place(2, 2), None, "a source with no safepoints");
    }

    #[test]
    fn the_lines_a_range_can_stop_on_are_the_ones_that_carry_a_safepoint() {
        let program = ProgramMap::new(
            HashMap::new(),
            HashMap::from([(1, BTreeSet::from([2, 3, 7]))]),
            LATEST,
        );
        assert_eq!(program.lines_between(1, 1, 9), vec![2, 3, 7]);
        assert_eq!(program.lines_between(1, 3, 3), vec![3], "one line");
        assert!(
            program.lines_between(1, 4, 6).is_empty(),
            "a range of comment lines offers nothing"
        );
        assert!(program.lines_between(2, 1, 9).is_empty(), "unknown source");
    }

    #[test]
    fn breakpoints_in_two_files_do_not_merge() {
        let (main, lib) = (
            Path::new(DIR).join("main.leek"),
            Path::new(DIR).join("lib.leek"),
        );
        let mut store = BreakpointStore::default();
        store.replace(&main, &lines(&[12]));
        store.replace(&lib, &lines(&[12]));

        let program = ProgramMap::new(
            HashMap::from([
                (canonical_or_normalized(&main), 1),
                (canonical_or_normalized(&lib), 2),
            ]),
            HashMap::from([(1, BTreeSet::from([12])), (2, BTreeSet::from([12]))]),
            LATEST,
        );
        let by_source = store.by_source(&program);
        assert_eq!(by_source[&1].len(), 1);
        assert_eq!(by_source[&2].len(), 1);
        assert_ne!(
            by_source[&1][&12].id, by_source[&2][&12].id,
            "the two files' line-12 breakpoints are different breakpoints"
        );
    }

    #[test]
    fn a_breakpoint_in_an_unknown_file_reaches_no_source() {
        let mut store = BreakpointStore::default();
        store.replace(&Path::new(DIR).join("scratch.leek"), &lines(&[1]));
        let program = program(&Path::new(DIR).join("main.leek"), &[1]);
        assert!(store.by_source(&program).is_empty());
    }

    #[test]
    fn a_condition_is_compiled_at_the_programs_language_version() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = BreakpointStore::default();
        store.replace(
            &main,
            &[BreakpointSpec {
                line: 1,
                condition: Some("i == 3".to_string()),
                ..BreakpointSpec::default()
            }],
        );
        let by_source = store.by_source(&program(&main, &[1]));
        let trigger = &by_source[&1][&1];
        assert!(trigger.condition.is_some(), "the condition was dropped");
        assert_eq!(trigger.version, LATEST);
    }

    #[test]
    fn a_breakpoint_whose_condition_does_not_compile_never_reaches_the_debuggee() {
        let main = Path::new(DIR).join("main.leek");
        let mut store = BreakpointStore::default();
        let requested = store.replace(
            &main,
            &[BreakpointSpec {
                line: 1,
                condition: Some("i ==".to_string()),
                ..BreakpointSpec::default()
            }],
        );
        let program = program(&main, &[1]);
        assert!(
            store.by_source(&program).is_empty(),
            "a condition that does not compile armed the breakpoint anyway"
        );
        // And the client is told why, rather than left with a live marker.
        let stored = store.iter().next().expect("the stored breakpoint").2;
        assert!(Trigger::check(stored, program.version()).is_err());
        assert_eq!(requested[0].line, 1);
    }

    #[test]
    fn an_empty_condition_is_no_condition_at_all() {
        // Editors send `""` for a condition the user cleared; reading that as
        // an expression would reject the breakpoint for no reason.
        let main = Path::new(DIR).join("main.leek");
        let mut store = BreakpointStore::default();
        store.replace(
            &main,
            &[BreakpointSpec {
                line: 1,
                condition: Some(String::new()),
                hit_condition: Some("  ".to_string()),
                ..BreakpointSpec::default()
            }],
        );
        let by_source = store.by_source(&program(&main, &[1]));
        let trigger = &by_source[&1][&1];
        assert!(trigger.condition.is_none() && trigger.hit.is_none());
    }
}
