//! HIR → [`CostExpr`] analyser.
//!
//! Walks each user function and produces a [`Complexity`] summary.
//! Conceptually a symbolic generalisation of `leek-charge`'s
//! scalar walker: each construct emits a `CostExpr` rather than a
//! `u64`. Constant-cost statements contribute `Const(k)` exactly
//! the way `leek-charge` would charge them; loops multiply their
//! body's cost by a [`LoopBound`] recovered from the loop's
//! syntactic shape.
//!
//! ## Call-graph substitution
//!
//! [`analyze_file`] runs cycle detection + topological sort over
//! the file's call graph, then analyses non-recursive functions
//! in callees-first order. When walking a function body, a call
//! to a previously-analysed function looks up its `Complexity`
//! and substitutes `Size(callee_param_i)` → "size of the
//! caller's i-th argument" (a `CostExpr`). Recursive functions
//! (members of any cycle) emit `Unknown` for their own user-fn
//! calls so the formula doesn't depend on its own answer.
//!
//! ## Higher-order builtins
//!
//! `arrayMap` / `arrayFilter` / `arrayReduce` / etc. take a
//! lambda as their second argument. When that argument is an
//! `ExprKind::Lambda`, the analyser recursively walks the
//! lambda's body in the same caller context (so captured
//! parameter references survive) and emits `count(arr) ·
//! body_cost`. Non-lambda callbacks (e.g. a `NameRef` reference
//! to a user function) fall through to a linear cost — close
//! enough for big-O.

use std::collections::{HashMap, HashSet};

use leek_hir::{
    Block, Callee, Def, DefId, Expr, ExprKind, Function, HirFile, LambdaBody, NameRef, Stmt,
};

use crate::call_graph;
use crate::cost_expr::{CostExpr, SizeVar};
use crate::loop_bound::{
    BoundContext, LoopBound, ParamIndex, bound_of_for, bound_of_foreach, bound_of_while,
};

/// Per-statement / per-expression cost constants. Aligned with
/// `leek-charge::ChargeOpts::default()` (`per_stmt = per_expr = 1`)
/// so the static formula tracks `getOperations()` modulo dynamic
/// builtin costs.
const PER_STMT: u64 = 1;
const PER_EXPR: u64 = 1;
const BUILTIN_CALL: u64 = 1;
const USER_CALL_OVERHEAD: u64 = 2;
const RETURN: u64 = 1;
const LOOP_HEADER: u64 = 1;

/// Result of analysing one user function.
#[derive(Debug, Clone)]
pub struct Complexity {
    pub name: String,
    pub params: Vec<ParamInfo>,
    pub formula: CostExpr,
    pub big_o: crate::big_o::BigO,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamInfo {
    pub name: String,
    pub size_var: Option<SizeVar>,
}

/// Analyse every user function in `hir`. See module doc for the
/// substitution + ordering story.
pub fn analyze_file(hir: &HirFile) -> Vec<Complexity> {
    let graph = call_graph::build(hir);
    let ordering = call_graph::order(&graph);

    let mut registry: HashMap<String, Complexity> = HashMap::new();
    let mut name_to_function: HashMap<String, &Function> = HashMap::new();
    let mut def_to_name: HashMap<DefId, String> = HashMap::new();
    for (idx, def) in hir.defs.iter().enumerate() {
        if let Def::Function(f) = def {
            name_to_function.insert(f.name.clone(), f);
            def_to_name.insert(
                DefId(u32::try_from(idx).expect("more than u32::MAX defs")),
                f.name.clone(),
            );
        }
    }

    // Non-recursive functions, callees-first.
    for name in &ordering.topo {
        if let Some(f) = name_to_function.get(name) {
            let c = analyze_with(f, &registry, &ordering.recursive, &def_to_name);
            registry.insert(name.clone(), c);
        }
    }
    // Recursive functions — registered last with their own name
    // marked recursive so user-call substitution from THEIR body
    // back to a same-cycle peer falls through to Unknown.
    for name in &ordering.recursive {
        if let Some(f) = name_to_function.get(name) {
            let c = analyze_with(f, &registry, &ordering.recursive, &def_to_name);
            registry.insert(name.clone(), c);
        }
    }

    // Main block.
    let pmap = ParamMap::empty();
    let ctx = BoundContext { params: &pmap };
    let walker = Walker {
        registry: &registry,
        recursive: &ordering.recursive,
        def_to_name: &def_to_name,
        ctx: &ctx,
        analysing: None,
    };
    let main_formula = walker.walk_stmts(&hir.main).simplify();
    let main_big_o = crate::big_o::big_o(&main_formula);

    let mut out = Vec::new();
    out.push(Complexity {
        name: "<main>".into(),
        params: Vec::new(),
        formula: main_formula,
        big_o: main_big_o,
    });
    for def in &hir.defs {
        if let Def::Function(f) = def
            && let Some(c) = registry.get(&f.name) {
                out.push(c.clone());
            }
    }
    out
}

/// Analyse a single function without consulting a registry.
/// User-function calls in the body become `Unknown` — use
/// [`analyze_file`] for the substitution-enabled path.
pub fn analyze_function(f: &Function) -> Complexity {
    let registry: HashMap<String, Complexity> = HashMap::new();
    let recursive: HashSet<String> = HashSet::new();
    let def_to_name: HashMap<DefId, String> = HashMap::new();
    analyze_with(f, &registry, &recursive, &def_to_name)
}

fn analyze_with(
    f: &Function,
    registry: &HashMap<String, Complexity>,
    recursive: &HashSet<String>,
    def_to_name: &HashMap<DefId, String>,
) -> Complexity {
    let params: Vec<ParamInfo> = f
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| ParamInfo {
            name: p.name.clone(),
            size_var: param_size_var(u32::try_from(i).expect("more than u32::MAX params"), p),
        })
        .collect();
    let pmap = ParamMap::from_params(&f.params);
    let ctx = BoundContext { params: &pmap };
    let walker = Walker {
        registry,
        recursive,
        def_to_name,
        ctx: &ctx,
        analysing: Some(f.name.as_str()),
    };
    let body_cost = match &f.body {
        Some(b) => walker.walk_block(b),
        None => CostExpr::Const(0),
    };
    let formula = CostExpr::sum(vec![CostExpr::Const(USER_CALL_OVERHEAD), body_cost]).simplify();
    let big_o = crate::big_o::big_o(&formula);
    Complexity {
        name: f.name.clone(),
        params,
        formula,
        big_o,
    }
}

fn param_size_var(idx: u32, p: &leek_hir::Param) -> Option<SizeVar> {
    use leek_hir::Type;
    let bearing = matches!(
        &p.ty,
        Some(Type::Array(_) | Type::Map(_, _) | Type::Set(_) | Type::String)
    );
    if bearing {
        Some(SizeVar::new(idx, p.name.clone()))
    } else {
        None
    }
}

struct ParamMap {
    inner: HashMap<DefId, (u32, String)>,
}

impl ParamMap {
    fn empty() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }
    fn from_params(params: &[leek_hir::Param]) -> Self {
        let mut inner = HashMap::new();
        for (i, p) in params.iter().enumerate() {
            inner.insert(
                p.def,
                (u32::try_from(i).expect("more than u32::MAX params"), p.name.clone()),
            );
        }
        Self { inner }
    }
}

impl ParamIndex for ParamMap {
    fn lookup(&self, def: DefId) -> Option<(u32, String)> {
        self.inner.get(&def).cloned()
    }
}

// ─── walker ────────────────────────────────────────────────────────

struct Walker<'a> {
    registry: &'a HashMap<String, Complexity>,
    recursive: &'a HashSet<String>,
    /// `DefId → user function name` for the file under analysis.
    /// Populated by [`analyze_file`]; empty in the standalone
    /// [`analyze_function`] path where user calls are always
    /// Unknown anyway.
    def_to_name: &'a HashMap<DefId, String>,
    ctx: &'a BoundContext<'a>,
    /// Name of the function currently being analysed (so calls
    /// from inside a recursive function to one of its same-cycle
    /// peers stay `Unknown`).
    analysing: Option<&'a str>,
}

impl Walker<'_> {
    fn walk_block(&self, b: &Block) -> CostExpr {
        self.walk_stmts(&b.stmts)
    }

    fn walk_stmts(&self, stmts: &[Stmt]) -> CostExpr {
        let parts: Vec<CostExpr> = stmts.iter().map(|s| self.walk_stmt(s)).collect();
        CostExpr::sum(parts)
    }

    fn walk_stmt(&self, s: &Stmt) -> CostExpr {
        let own = CostExpr::Const(PER_STMT);
        let body = match s {
            Stmt::Expr(e) => self.walk_expr(e),
            Stmt::VarDecl(v) => v
                .init
                .as_ref()
                .map_or(CostExpr::zero(), |e| self.walk_expr(e)),
            Stmt::Return(e) => CostExpr::sum(vec![
                CostExpr::Const(RETURN),
                e.as_ref()
                    .map_or(CostExpr::zero(), |x| self.walk_expr(x)),
            ]),
            Stmt::If(i) => {
                let cond = self.walk_expr(&i.cond);
                let then_c = self.walk_stmt(&i.then_branch);
                let else_c = i
                    .else_branch
                    .as_ref()
                    .map_or(CostExpr::Const(0), |e| self.walk_stmt(e));
                CostExpr::sum(vec![cond, CostExpr::max(vec![then_c, else_c])])
            }
            Stmt::While(w) => {
                let bound = bound_of_while(w, self.ctx);
                self.loop_cost(&bound, &w.cond, &w.body)
            }
            Stmt::DoWhile(dw) => {
                let synth_while = leek_hir::WhileStmt {
                    cond: dw.cond.clone(),
                    body: dw.body.clone(),
                    span: dw.span,
                };
                let bound = bound_of_while(&synth_while, self.ctx);
                self.loop_cost(&bound, &dw.cond, &dw.body)
            }
            Stmt::For(f) => {
                let bound = bound_of_for(f, self.ctx);
                let header = CostExpr::sum(vec![
                    CostExpr::Const(LOOP_HEADER),
                    f.init
                        .as_ref()
                        .map_or(CostExpr::zero(), |s| self.walk_stmt(s)),
                ]);
                let body_cost = self.walk_stmt(&f.body);
                let per_iter = CostExpr::sum(vec![
                    f.cond
                        .as_ref()
                        .map_or(CostExpr::zero(), |e| self.walk_expr(e)),
                    f.step
                        .as_ref()
                        .map_or(CostExpr::zero(), |e| self.walk_expr(e)),
                    body_cost,
                ]);
                CostExpr::sum(vec![
                    header,
                    CostExpr::product(vec![bound.to_cost_expr(), per_iter]),
                ])
            }
            Stmt::Foreach(fe) => {
                let bound = bound_of_foreach(fe, self.ctx);
                let header =
                    CostExpr::sum(vec![CostExpr::Const(LOOP_HEADER), self.walk_expr(&fe.iter)]);
                let body_cost = self.walk_stmt(&fe.body);
                CostExpr::sum(vec![
                    header,
                    CostExpr::product(vec![bound.to_cost_expr(), body_cost]),
                ])
            }
            Stmt::Block(b) => self.walk_block(b),
            Stmt::Switch(s) => {
                let disc = self.walk_expr(&s.discriminant);
                let arms: Vec<CostExpr> = s.arms.iter().map(|a| self.walk_stmts(&a.body)).collect();
                CostExpr::sum(vec![disc, CostExpr::max(arms)])
            }
            Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::Include(_)
            | Stmt::Import(_)
            | Stmt::Charge(_) => CostExpr::Const(0),
        };
        CostExpr::sum(vec![own, body])
    }

    fn loop_cost(&self, bound: &LoopBound, cond: &Expr, body: &Stmt) -> CostExpr {
        let cond_c = self.walk_expr(cond);
        let body_c = self.walk_stmt(body);
        let per_iter = CostExpr::sum(vec![cond_c, body_c]);
        CostExpr::sum(vec![
            CostExpr::Const(LOOP_HEADER),
            CostExpr::product(vec![bound.to_cost_expr(), per_iter]),
        ])
    }

    fn walk_expr(&self, e: &Expr) -> CostExpr {
        let own = CostExpr::Const(PER_EXPR);
        let children = match &e.kind {
            // A call's cost is modelled specially (user-fn body,
            // builtin growth, HOF lambda bodies), not as a plain
            // sum of receiver + args.
            ExprKind::Call(c) => self.call_cost(c),
            // Only one ternary branch runs, so the branches are
            // `max`'d rather than summed.
            ExprKind::Ternary(c, t, f) => CostExpr::sum(vec![
                self.walk_expr(c),
                CostExpr::max(vec![self.walk_expr(t), self.walk_expr(f)]),
            ]),
            // Every other expression costs the sum of its immediate
            // sub-expressions. `walk_expr_children` enumerates them
            // (and treats a lambda as a leaf — its body is costed
            // only when the lambda is actually invoked).
            _ => {
                let mut parts = Vec::new();
                leek_hir::walk_expr_children(e, &mut |child| parts.push(self.walk_expr(child)));
                CostExpr::sum(parts)
            }
        };
        CostExpr::sum(vec![own, children])
    }

    fn call_cost(&self, c: &leek_hir::Call) -> CostExpr {
        let args_c = CostExpr::sum(c.args.iter().map(|a| self.walk_expr(a)).collect());
        match &c.callee {
            Callee::Function(NameRef::Builtin(name)) => {
                let growth = self.builtin_growth(name, &c.args);
                CostExpr::sum(vec![CostExpr::Const(BUILTIN_CALL), args_c, growth])
            }
            Callee::Function(NameRef::Function(def_id)) => {
                // Look the callee up in the registry — its name is
                // unique in the file, so we identify it via the
                // registry's matching entry by name.
                let callee_name = self.callee_name_for(*def_id);
                let cost = match callee_name {
                    Some(name) => self.user_call_cost(&name, &c.args),
                    None => CostExpr::Unknown("unresolved user-function call"),
                };
                CostExpr::sum(vec![CostExpr::Const(USER_CALL_OVERHEAD), args_c, cost])
            }
            Callee::Method { receiver, .. } => CostExpr::sum(vec![
                CostExpr::Const(USER_CALL_OVERHEAD),
                self.walk_expr(receiver),
                args_c,
                CostExpr::Unknown("method call (no type-aware lookup yet)"),
            ]),
            Callee::Expr(callee_e) => CostExpr::sum(vec![
                CostExpr::Const(USER_CALL_OVERHEAD),
                self.walk_expr(callee_e),
                args_c,
                CostExpr::Unknown("dynamic call"),
            ]),
            // Other `Function` name-refs (locals, `this`, class refs) carry no
            // extra cost beyond their arguments.
            Callee::Function(_) => args_c,
        }
    }

    fn callee_name_for(&self, def_id: DefId) -> Option<String> {
        self.def_to_name.get(&def_id).cloned()
    }

    /// Substitute the callee's formula at this call site. If the
    /// callee is recursive (member of any cycle) OR we're
    /// currently analysing a function that's recursive AND the
    /// callee is in the same cycle, return Unknown.
    fn user_call_cost(&self, callee_name: &str, args: &[Expr]) -> CostExpr {
        // Recursion guard: if either the callee or the function
        // we're analysing is recursive, don't substitute.
        let callee_recursive = self.recursive.contains(callee_name);
        let we_are_recursive = self
            .analysing
            .is_some_and(|n| self.recursive.contains(n));
        if callee_recursive || (we_are_recursive && self.analysing == Some(callee_name)) {
            return CostExpr::Unknown("recursive call");
        }
        let Some(callee) = self.registry.get(callee_name) else {
            return CostExpr::Unknown("callee not yet analysed");
        };
        // Build the param_index → arg-size substitution.
        let mut sub: HashMap<u32, CostExpr> = HashMap::new();
        for (i, arg) in args.iter().enumerate() {
            let arg_size = self.arg_size_expr(arg).unwrap_or(CostExpr::Const(0));
            sub.insert(u32::try_from(i).expect("more than u32::MAX args"), arg_size);
        }
        callee.formula.substitute(&sub).simplify()
    }

    /// Convert a call-site argument expression into a CostExpr
    /// suitable as a "size" substitution. Recognises:
    /// - a caller parameter → `Size(caller_param)`
    /// - `count(p)` / `length(p)` → same
    /// - an integer literal → `Const(n)` (so `f(arr, 5)` propagates
    ///   the literal into a callee formula like `n · k`)
    /// - otherwise → `Const(0)`, which folds the size factor out
    fn arg_size_expr(&self, e: &Expr) -> Option<CostExpr> {
        match &e.kind {
            ExprKind::Name(NameRef::Local(id)) => {
                let (idx, name) = self.ctx.params.lookup(*id)?;
                Some(CostExpr::Size(SizeVar::new(idx, name)))
            }
            ExprKind::Literal(leek_hir::Literal::Int(v)) if *v >= 0 => {
                Some(CostExpr::Const(u64::try_from(*v).expect("non-negative by guard")))
            }
            // Array / map / set literals contribute their literal
            // length as a constant. Useful for the empirical
            // harness where main passes `[1, 2, ..., n]` to a
            // callee — the literal length flows into the callee's
            // size variable and the whole formula collapses to a
            // scalar.
            ExprKind::Array(items) | ExprKind::Set(items) => {
                Some(CostExpr::Const(items.len() as u64))
            }
            ExprKind::Map(pairs) => Some(CostExpr::Const(pairs.len() as u64)),
            ExprKind::Call(call) => match &call.callee {
                Callee::Function(NameRef::Builtin(b))
                    if matches!(b.as_str(), "count" | "length" | "size" | "mapSize") =>
                {
                    let arg = call.args.first()?;
                    self.arg_size_expr(arg)
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn builtin_growth(&self, name: &str, args: &[Expr]) -> CostExpr {
        let first_size = || self.size_or_zero(args.first());
        let second_size = || self.size_or_zero(args.get(1));

        // Higher-order builtins: if the second arg is a Lambda,
        // we walk its body in the same caller context and pair it
        // with count(arr).
        if is_hof(name) {
            return self.hof_growth(name, args);
        }

        match name {
            "reverse" | "arrayReverse" | "shuffle" | "arrayShuffle" | "subArray" | "arraySlice"
            | "fill" | "indexOf" | "lastIndexOf" | "search" | "contains" | "inArray" | "join"
            | "stringJoin" | "stringReverse" | "arrayFlatten" | "flatten" | "arrayDistinct"
            | "arrayUnique" | "arrayKeys" | "arrayValues" | "entries" | "mapKeys" | "mapValues"
            | "arrayCopy" | "clone" | "arrayMax" | "arrayMin" | "arrayCount" | "arrayProduct"
            | "arrayAvg" | "arrayAdd" => first_size(),

            "concat" | "arrayConcat" => CostExpr::sum(vec![first_size(), second_size()]),

            "sort" | "arraySort" | "intervalSort" => {
                CostExpr::product(vec![first_size(), CostExpr::Log(Box::new(first_size()))])
            }

            "arrayIntersect" | "arrayUnion" | "arrayDifference" => {
                CostExpr::product(vec![first_size(), second_size()])
            }

            _ => CostExpr::Const(0),
        }
    }

    fn size_or_zero(&self, e: Option<&Expr>) -> CostExpr {
        e.and_then(|e| self.arg_size_expr(e))
            .unwrap_or(CostExpr::Const(0))
    }

    /// Growth contribution for a higher-order builtin
    /// (`arrayMap` and friends). Cost is `count(arr) ·
    /// lambda_body_cost` when arg[1] is a Lambda; otherwise just
    /// `count(arr)` (treats a function-reference callback as O(1)
    /// per element, which is conservative but matches our default
    /// "unknown user-fn cost = Unknown" rule).
    fn hof_growth(&self, name: &str, args: &[Expr]) -> CostExpr {
        let arr_size = self.size_or_zero(args.first());
        let body_cost = if let Some(arg1) = args.get(1) {
            match &arg1.kind {
                ExprKind::Lambda(lam) => match &lam.body {
                    LambdaBody::Block(b) => self.walk_block(b),
                    LambdaBody::Expr(e) => self.walk_expr(e),
                },
                ExprKind::Name(NameRef::Function(_)) => {
                    // A passed function reference. We could
                    // substitute its formula here, but we don't
                    // know the elementwise param's size; emit a
                    // small constant per call.
                    CostExpr::Const(USER_CALL_OVERHEAD)
                }
                _ => CostExpr::Const(0),
            }
        } else {
            CostExpr::Const(0)
        };
        let _ = name; // future: distinguish reducers from mappers
        CostExpr::product(vec![arr_size, body_cost])
    }
}

/// Which builtins take a function argument that's invoked per
/// element of the first argument.
fn is_hof(name: &str) -> bool {
    matches!(
        name,
        "arrayMap"
            | "arrayFilter"
            | "arrayReduce"
            | "arrayReduceRight"
            | "arrayFoldLeft"
            | "arrayFoldRight"
            | "arrayForeach"
            | "forEach"
            | "arrayIter"
            | "arrayPartition"
            | "arrayEvery"
            | "arraySome"
            | "mapFilter"
            | "mapMap"
            | "mapForEach"
            | "setForEach"
            | "setForeach"
            | "intervalMap"
            | "intervalFilter"
            | "intervalForeach"
            | "intervalForEach"
            | "intervalReduce"
            | "intervalReduceRight"
    )
}
