//! Per-file call graph + cycle detection + topological order.
//!
//! Lets [`analyze_file`](crate::analyze_file) analyse callees
//! before their callers, then substitute callee formulas into
//! call sites. Functions that participate in any cycle (self-
//! recursion or mutual recursion) are flagged separately; the
//! analyser treats their user-function calls as `Unknown` so the
//! formula doesn't depend on its own answer.
//!
//! No external graph library — we build directly from `HirFile`
//! and run a 3-color DFS for cycle detection plus a standard
//! reverse-post-order topo sort.

use std::collections::{HashMap, HashSet};

use leek_hir::{
    Block, Callee, Def, DefId, Expr, ExprKind, Flow, HirFile, NameRef, Stmt, Visit, Visitable,
};

/// Resolved call graph for the functions defined in one
/// [`HirFile`]. Keyed by function name (a function symbol is
/// uniquely identified by its name within a single file).
#[derive(Debug, Default)]
pub struct CallGraph {
    /// All function names in declaration order.
    pub names: Vec<String>,
    /// `name → set of called function names`. Only user-fn callees
    /// land here; builtins / dynamic dispatch are recorded
    /// separately on the analysis side.
    pub edges: HashMap<String, HashSet<String>>,
}

/// Result of cycle detection + ordering.
#[derive(Debug, Default)]
pub struct GraphOrder {
    /// Topological order of *non-recursive* functions. Each
    /// function's user-fn callees appear earlier in this list.
    pub topo: Vec<String>,
    /// Set of function names that lie on any cycle (including
    /// self-recursion). Empty for typical straight-line code.
    pub recursive: HashSet<String>,
}

/// Build a call graph from `hir`. Records edges only for
/// `Callee::Function(NameRef::Function(_))` — bare user-function
/// calls. Method calls and dynamic-expression calls are not
/// represented (the analyser handles those as Unknown at the call
/// site).
pub fn build(hir: &HirFile) -> CallGraph {
    let mut names = Vec::new();
    let mut def_to_name: HashMap<DefId, String> = HashMap::new();
    let mut edges: HashMap<String, HashSet<String>> = HashMap::new();

    for (idx, def) in hir.defs.iter().enumerate() {
        if let Def::Function(f) = def {
            let id = DefId(u32::try_from(idx).expect("more than u32::MAX defs"));
            def_to_name.insert(id, f.name.clone());
            names.push(f.name.clone());
            edges.insert(f.name.clone(), HashSet::new());
        }
    }

    for def in &hir.defs {
        if let Def::Function(f) = def {
            let entry = edges.entry(f.name.clone()).or_default();
            if let Some(body) = &f.body {
                let mut collector = CalleeCollector {
                    def_to_name: &def_to_name,
                    out: entry,
                };
                let _ = body.walk(&mut collector);
            }
        }
    }

    CallGraph { names, edges }
}

/// Run cycle detection + reverse-post-order topo sort on `graph`.
/// Topologically orders only the non-recursive subgraph; cycle
/// members are returned in `recursive` and excluded from `topo`.
pub fn order(graph: &CallGraph) -> GraphOrder {
    // Phase 1: detect cycles with iterative 3-color DFS. Any node
    // we revisit while it's GRAY (on the current DFS path) sits
    // on a back edge — every node from the back-edge target up
    // to the current top of stack is in a cycle.
    let mut color: HashMap<&str, Color> = graph
        .edges
        .keys()
        .map(|k| (k.as_str(), Color::White))
        .collect();
    let mut recursive: HashSet<String> = HashSet::new();

    for start in &graph.names {
        if color.get(start.as_str()) != Some(&Color::White) {
            continue;
        }
        dfs_for_cycles(start, graph, &mut color, &mut recursive);
    }

    // Phase 2: post-order topo sort, excluding recursive
    // functions. Iterative DFS appends a node when its subtree
    // is fully explored, so callees naturally finish before their
    // callers — exactly the order we want for substitution.
    let mut visited: HashSet<String> = HashSet::new();
    let mut finished: Vec<String> = Vec::new();
    for start in &graph.names {
        if recursive.contains(start) || visited.contains(start) {
            continue;
        }
        dfs_finish(start, graph, &recursive, &mut visited, &mut finished);
    }

    GraphOrder {
        topo: finished,
        recursive,
    }
}

fn dfs_for_cycles<'a>(
    start: &'a str,
    graph: &'a CallGraph,
    color: &mut HashMap<&'a str, Color>,
    recursive: &mut HashSet<String>,
) {
    use Color::{Black, Gray, White};
    // Iterative DFS with an explicit stack of "frame = (node, child iterator)".
    let mut stack: Vec<(&str, std::vec::IntoIter<&str>)> = Vec::new();
    let mut path: Vec<&str> = Vec::new();
    color.insert(start, Gray);
    path.push(start);
    let init_kids: Vec<&str> = graph
        .edges
        .get(start)
        .map(|s| s.iter().map(String::as_str).collect())
        .unwrap_or_default();
    stack.push((start, init_kids.into_iter()));

    while let Some((node, mut iter)) = stack.pop() {
        let mut advanced = false;
        for next in iter.by_ref() {
            match color.get(next).copied().unwrap_or(White) {
                White => {
                    // Push back the current frame with its iterator
                    // continuing past `next`, then descend.
                    stack.push((node, iter));
                    color.insert(next, Gray);
                    path.push(next);
                    let kids: Vec<&str> = graph
                        .edges
                        .get(next)
                        .map(|s| s.iter().map(String::as_str).collect())
                        .unwrap_or_default();
                    stack.push((next, kids.into_iter()));
                    advanced = true;
                    break;
                }
                Gray => {
                    // Back edge — everyone on `path` from `next`
                    // onward is in the cycle.
                    let cycle_start = path.iter().position(|n| *n == next).unwrap();
                    for n in &path[cycle_start..] {
                        recursive.insert((*n).to_string());
                    }
                }
                Black => {}
            }
        }
        if !advanced {
            color.insert(node, Black);
            // Pop from path. (Should be `node`.)
            let popped = path.pop();
            debug_assert_eq!(popped, Some(node));
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Color {
    White,
    Gray,
    Black,
}

fn dfs_finish<'a>(
    start: &'a str,
    graph: &'a CallGraph,
    recursive: &HashSet<String>,
    visited: &mut HashSet<String>,
    finished: &mut Vec<String>,
) {
    let mut stack: Vec<(&str, std::vec::IntoIter<&str>)> = Vec::new();
    visited.insert(start.to_string());
    let init_kids: Vec<&str> = graph
        .edges
        .get(start)
        .map(|s| {
            s.iter()
                .filter(|n| !recursive.contains(n.as_str()))
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default();
    stack.push((start, init_kids.into_iter()));

    while let Some((node, mut iter)) = stack.pop() {
        let mut advanced = false;
        for next in iter.by_ref() {
            if visited.contains(next) {
                continue;
            }
            visited.insert(next.to_string());
            // Re-push current frame, then descend.
            stack.push((node, iter));
            let kids: Vec<&str> = graph
                .edges
                .get(next)
                .map(|s| {
                    s.iter()
                        .filter(|n| !recursive.contains(n.as_str()))
                        .map(String::as_str)
                        .collect()
                })
                .unwrap_or_default();
            stack.push((next, kids.into_iter()));
            advanced = true;
            break;
        }
        if !advanced {
            finished.push(node.to_string());
        }
    }
}

// ─── callee collection ─────────────────────────────────────────────

/// Walks a function body and records every direct call to another
/// *user* function (by name). Builtins and unresolved names are
/// ignored. The default [`Visitor`] recursion descends into lambda
/// bodies and parameter defaults, so callees buried in a lambda are
/// still attributed to the enclosing function.
struct CalleeCollector<'a> {
    def_to_name: &'a HashMap<DefId, String>,
    out: &'a mut HashSet<String>,
}

impl Visit<Expr> for CalleeCollector<'_> {
    fn visit(&mut self, e: &Expr) -> Flow {
        if let ExprKind::Call(c) = &e.kind
            && let Callee::Function(NameRef::Function(def_id)) = &c.callee
            && let Some(name) = self.def_to_name.get(def_id)
        {
            self.out.insert(name.clone());
        }
        // Keep descending — including into lambda bodies, so callees buried
        // in a lambda are attributed to the enclosing function.
        Flow::Walk
    }
}

// Only expressions matter; blocks/statements use the `Visit` default no-op
// so the `HirVisitor` umbrella is satisfied.
impl Visit<Block> for CalleeCollector<'_> {}
impl Visit<Stmt> for CalleeCollector<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_from(edges: &[(&str, &[&str])]) -> CallGraph {
        let mut g = CallGraph::default();
        for (name, _) in edges {
            g.names.push((*name).to_string());
            g.edges.insert((*name).to_string(), HashSet::new());
        }
        for (name, callees) in edges {
            let set = g.edges.entry((*name).to_string()).or_default();
            for c in *callees {
                set.insert((*c).to_string());
            }
        }
        g
    }

    #[test]
    fn linear_chain_orders_callees_first() {
        // a → b → c. Topo expects c, b, a (callees ahead of caller).
        let g = graph_from(&[("a", &["b"]), ("b", &["c"]), ("c", &[])]);
        let o = order(&g);
        assert!(o.recursive.is_empty());
        let pos = |n| o.topo.iter().position(|s| s == n).unwrap();
        assert!(pos("c") < pos("b"), "topo = {:?}", o.topo);
        assert!(pos("b") < pos("a"), "topo = {:?}", o.topo);
    }

    #[test]
    fn self_recursion_is_flagged() {
        let g = graph_from(&[("f", &["f"])]);
        let o = order(&g);
        assert!(o.recursive.contains("f"));
        assert!(!o.topo.contains(&"f".to_string()));
    }

    #[test]
    fn mutual_recursion_is_flagged() {
        let g = graph_from(&[("a", &["b"]), ("b", &["a"])]);
        let o = order(&g);
        assert!(o.recursive.contains("a"));
        assert!(o.recursive.contains("b"));
    }

    #[test]
    fn non_recursive_caller_of_recursive_callee_still_topo_sorted() {
        // a → b → b (b is self-recursive). a is non-recursive and
        // should still appear in topo. Substitution will see b's
        // formula as Unknown.
        let g = graph_from(&[("a", &["b"]), ("b", &["b"])]);
        let o = order(&g);
        assert!(o.recursive.contains("b"));
        assert!(o.topo.contains(&"a".to_string()));
        assert!(!o.topo.contains(&"b".to_string()));
    }

    #[test]
    fn diamond_orders_consistently() {
        // a → b, a → c, b → d, c → d. d must come before b/c, which
        // both come before a.
        let g = graph_from(&[("a", &["b", "c"]), ("b", &["d"]), ("c", &["d"]), ("d", &[])]);
        let o = order(&g);
        assert!(o.recursive.is_empty());
        let pos = |n: &str| o.topo.iter().position(|s| s == n).unwrap();
        assert!(pos("d") < pos("b"));
        assert!(pos("d") < pos("c"));
        assert!(pos("b") < pos("a"));
        assert!(pos("c") < pos("a"));
    }
}
