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
    Block, Callee, Def, DefId, Expr, ExprKind, Flow, HirFile, NameRef, Stmt, Type, Visit,
    Visitable,
};

/// Resolved call graph for the functions and methods defined in one
/// [`HirFile`]. Free functions are keyed by name; methods by a
/// `Class.method` qualified name (a symbol is uniquely identified by
/// that key within a single file).
#[derive(Debug, Default)]
pub struct CallGraph {
    /// All node keys (function names + `Class.method`) in
    /// declaration order.
    pub names: Vec<String>,
    /// `node → set of called node keys`. Only user-fn / user-method
    /// callees land here; builtins / dynamic dispatch are recorded
    /// separately on the analysis side.
    pub edges: HashMap<String, HashSet<String>>,
    /// `method name → classes that define a method by that name`.
    /// Drives method-call resolution (receiver type, then a
    /// unique-name fallback). Shared with the analyser so both build
    /// the same edges and substitutions.
    pub method_owners: HashMap<String, Vec<String>>,
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

/// Build a call graph from `hir`. Records edges for
/// `Callee::Function(NameRef::Function(_))` (bare user-function
/// calls) and for `Callee::Method` calls that resolve to a user
/// method (via the receiver's class, then a unique-method-name
/// fallback — see [`resolve_method_qualified`]). Dynamic-expression
/// calls and unresolvable method calls are not represented (the
/// analyser handles those as Unknown at the call site).
pub fn build(hir: &HirFile) -> CallGraph {
    let mut names = Vec::new();
    let mut def_to_name: HashMap<DefId, String> = HashMap::new();
    let mut edges: HashMap<String, HashSet<String>> = HashMap::new();
    let mut method_owners: HashMap<String, Vec<String>> = HashMap::new();

    // Pass 1: register every node (function + method) so edges can
    // reference forward-declared callees.
    for (idx, def) in hir.defs.iter().enumerate() {
        match def {
            Def::Function(f) if is_real_function(f) => {
                let id = DefId(u32::try_from(idx).expect("more than u32::MAX defs"));
                def_to_name.insert(id, f.name.clone());
                names.push(f.name.clone());
                edges.insert(f.name.clone(), HashSet::new());
            }
            Def::Class(c) => {
                for m in &c.methods {
                    let q = qualified(&c.name, &m.name);
                    names.push(q.clone());
                    edges.insert(q, HashSet::new());
                    method_owners
                        .entry(m.name.clone())
                        .or_default()
                        .push(c.name.clone());
                }
            }
            _ => {}
        }
    }

    // Pass 2: collect edges out of each node's body.
    for def in &hir.defs {
        match def {
            Def::Function(f) if is_real_function(f) => {
                if let Some(body) = &f.body {
                    collect_into(&mut edges, &f.name, body, &def_to_name, &method_owners, None);
                }
            }
            Def::Class(c) => {
                for m in &c.methods {
                    if let Some(body) = &m.body {
                        let key = qualified(&c.name, &m.name);
                        collect_into(
                            &mut edges,
                            &key,
                            body,
                            &def_to_name,
                            &method_owners,
                            Some(c.name.as_str()),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    CallGraph {
        names,
        edges,
        method_owners,
    }
}

/// `Class.method` node key.
pub(crate) fn qualified(class: &str, method: &str) -> String {
    format!("{class}.{method}")
}

/// A `Function` def worth treating as a real callable: it has a body,
/// or it's a signature-backed function (carries backend directives).
///
/// Lowering emits a *bodiless, directive-less* `Function` for every
/// class method (a resolution artifact) alongside the `Class` def that
/// actually owns it. Those would otherwise show up as spurious `O(1)`
/// entries; the method is analysed through its `Class.method` node
/// instead, so we skip them here.
pub(crate) fn is_real_function(f: &leek_hir::Function) -> bool {
    f.body.is_some() || !f.backend_directives.is_empty()
}

/// Walk one node's `body`, collecting its user-fn / user-method
/// callee edges into `edges[key]`.
fn collect_into(
    edges: &mut HashMap<String, HashSet<String>>,
    key: &str,
    body: &Block,
    def_to_name: &HashMap<DefId, String>,
    method_owners: &HashMap<String, Vec<String>>,
    current_class: Option<&str>,
) {
    let class_env = build_class_env(&body.stmts, current_class);
    let mut out = HashSet::new();
    let mut collector = CalleeCollector {
        def_to_name,
        method_owners,
        current_class,
        class_env: &class_env,
        out: &mut out,
    };
    let _ = body.walk(&mut collector);
    if let Some(entry) = edges.get_mut(key) {
        entry.extend(out);
    }
}

/// Resolve a `receiver.method(args)` call to the `Class.method` node
/// key of the user method it dispatches to, or `None` if it can't be
/// pinned to a user method.
///
/// Resolution, in order:
/// 1. The receiver's class — `this`/`super`/`class` use the enclosing
///    `current_class`; `new C()` uses `C`; otherwise the receiver
///    expression's [`Type::ClassInstance`] (when type info is present).
///    If that class defines `method`, use it.
/// 2. Otherwise, if exactly one class in the file defines `method`,
///    use that one (a best-effort fallback for when the receiver type
///    is unknown — e.g. an untyped `Any` parameter).
pub(crate) fn resolve_method_qualified(
    receiver: &Expr,
    method: &str,
    current_class: Option<&str>,
    class_env: &HashMap<DefId, String>,
    method_owners: &HashMap<String, Vec<String>>,
) -> Option<String> {
    let owners = method_owners.get(method)?;
    if owners.is_empty() {
        return None;
    }
    let chosen = match receiver_class(receiver, current_class, class_env) {
        // Receiver class known and it defines the method — exact hit.
        Some(c) if owners.iter().any(|o| o == &c) => c,
        // Class unknown, or known but not a direct owner (e.g. an
        // inherited method): fall back to a unique owner if there is
        // exactly one.
        _ if owners.len() == 1 => owners[0].clone(),
        _ => return None,
    };
    Some(qualified(&chosen, method))
}

/// Best-effort class of a method receiver. Reliable for `this`/`super`,
/// `new C()`, and a local tracked back to one of those (`var c = new
/// Cat()`); otherwise reads the receiver's static type (often `Any`
/// before type-checking, in which case this returns `None`).
fn receiver_class(
    receiver: &Expr,
    current_class: Option<&str>,
    class_env: &HashMap<DefId, String>,
) -> Option<String> {
    match &receiver.kind {
        ExprKind::Name(NameRef::This | NameRef::Super | NameRef::Class_) => {
            current_class.map(str::to_string)
        }
        ExprKind::New(n) => Some(n.class.clone()),
        ExprKind::Name(NameRef::Local(id)) => class_env
            .get(id)
            .cloned()
            .or_else(|| class_name_of_type(&receiver.ty)),
        _ => class_name_of_type(&receiver.ty),
    }
}

fn class_name_of_type(ty: &Type) -> Option<String> {
    match ty {
        Type::ClassInstance(name, _) => Some(name.clone()),
        Type::Nullable(inner) => class_name_of_type(inner),
        _ => None,
    }
}

// ─── per-body local → class environment ─────────────────────────────

/// Track which class each local holds, for receiver resolution. Maps a
/// local's `DefId` to a class name when its declaration pins it down:
/// `C x = …` (typed), `var x = new C()`, `var x = this`, or `var x = y`
/// where `y` is itself tracked. Built per function/method body.
pub(crate) fn build_class_env(stmts: &[Stmt], current_class: Option<&str>) -> HashMap<DefId, String> {
    let mut env: HashMap<DefId, String> = HashMap::new();
    let mut builder = ClassEnvBuilder {
        current_class,
        env: &mut env,
    };
    for s in stmts {
        let _ = s.walk(&mut builder);
    }
    env
}

struct ClassEnvBuilder<'a> {
    current_class: Option<&'a str>,
    env: &'a mut HashMap<DefId, String>,
}

impl Visit<Stmt> for ClassEnvBuilder<'_> {
    fn visit(&mut self, s: &Stmt) -> Flow {
        if let Stmt::VarDecl(v) = s {
            let class = class_name_of_type(v.ty.as_ref().unwrap_or(&Type::Any)).or_else(|| {
                v.init
                    .as_ref()
                    .and_then(|init| expr_class(init, self.current_class, self.env))
            });
            if let Some(class) = class {
                self.env.insert(v.def, class);
            }
        }
        Flow::Walk
    }
}
impl Visit<Block> for ClassEnvBuilder<'_> {}
impl Visit<Expr> for ClassEnvBuilder<'_> {}

/// The class an initializer expression yields, if statically known.
fn expr_class(e: &Expr, current_class: Option<&str>, env: &HashMap<DefId, String>) -> Option<String> {
    match &e.kind {
        ExprKind::New(n) => Some(n.class.clone()),
        ExprKind::Name(NameRef::This | NameRef::Super | NameRef::Class_) => {
            current_class.map(str::to_string)
        }
        ExprKind::Name(NameRef::Local(id)) => {
            env.get(id).cloned().or_else(|| class_name_of_type(&e.ty))
        }
        _ => class_name_of_type(&e.ty),
    }
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

/// Walks a function/method body and records every direct call to
/// another *user* function or method (by node key). Builtins and
/// unresolved names are ignored. The default [`Visitor`] recursion
/// descends into lambda bodies and parameter defaults, so callees
/// buried in a lambda are still attributed to the enclosing node.
struct CalleeCollector<'a> {
    def_to_name: &'a HashMap<DefId, String>,
    method_owners: &'a HashMap<String, Vec<String>>,
    current_class: Option<&'a str>,
    class_env: &'a HashMap<DefId, String>,
    out: &'a mut HashSet<String>,
}

impl Visit<Expr> for CalleeCollector<'_> {
    fn visit(&mut self, e: &Expr) -> Flow {
        if let ExprKind::Call(c) = &e.kind {
            match &c.callee {
                Callee::Function(NameRef::Function(def_id)) => {
                    if let Some(name) = self.def_to_name.get(def_id) {
                        self.out.insert(name.clone());
                    }
                }
                Callee::Method {
                    receiver, method, ..
                } => {
                    if let Some(q) = resolve_method_qualified(
                        receiver,
                        method,
                        self.current_class,
                        self.class_env,
                        self.method_owners,
                    ) {
                        self.out.insert(q);
                    }
                }
                _ => {}
            }
        }
        // Keep descending — including into lambda bodies, so callees buried
        // in a lambda are attributed to the enclosing node.
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
