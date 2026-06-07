//! Generic, heterogeneous traversal of tree-shaped IRs.
//!
//! The framework is built from three pieces, none of which know any
//! concrete IR:
//!
//! - [`Flow`] — what a visitor decides after seeing a node: descend
//!   into its children ([`Flow::Walk`]), don't descend but keep going
//!   ([`Flow::Skip`]), or halt the whole traversal ([`Flow::Stop`]).
//! - [`Visit<T>`] / [`VisitMut<T>`] — *behaviour*, keyed by the node
//!   type. The default reaction is "do nothing, descend", so a concrete
//!   visitor writes an impl only for the node types it actually reacts
//!   to.
//! - [`Visitable`] / [`VisitableMut`] — *structure*: each node type
//!   knows how to present itself to the visitor and recurse into its
//!   children. The IR writes these (only it knows its variants).
//!
//! An IR also declares an **umbrella** trait listing its node types
//! once (e.g. `trait HirVisitor: Visit<Block> + Visit<Stmt> +
//! Visit<Expr> {}`) with a blanket impl, so any type implementing all
//! the `Visit<_>` parts is automatically a visitor for that IR. The
//! [`umbrella!`] macro generates both.
//!
//! ## Why a per-type `Visit<T>`
//!
//! A single visitor can react to a handful of node types and ignore the
//! rest, traversal control is explicit (prune a subtree with `Skip`,
//! abort with `Stop`), and the scheme scales to IRs with many node
//! kinds — MIR's `Statement`/`Terminator`/`Rvalue`/`Operand`/`Place`,
//! not just a block/stmt/expr trinity.
//!
//! ## Usage
//!
//! ```ignore
//! // React to expressions only; the `On<_>` adapter no-ops the rest.
//! program.walk(&mut On::<Expr, _>::new(|e: &Expr| {
//!     if interesting(e) { record(e); }
//!     Flow::Walk
//! }));
//! ```

use std::ops::ControlFlow;

/// What a visitor decides after seeing a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Descend into this node's children.
    Walk,
    /// Don't descend into this node, but continue the traversal.
    Skip,
    /// Halt the entire traversal immediately.
    Stop,
}

/// Read-only behaviour for visiting nodes of type `T`. The default
/// reaction is to descend ([`Flow::Walk`]) without doing anything, so a
/// concrete visitor only writes impls for the types it reacts to.
pub trait Visit<T: ?Sized> {
    fn visit(&mut self, node: &T) -> Flow {
        let _ = node;
        Flow::Walk
    }
}

/// Mutable mirror of [`Visit`]: react to (and optionally rewrite the
/// fields of) a node in place. To *replace* a node with a different one,
/// use a fold pass rather than `VisitMut`.
pub trait VisitMut<T: ?Sized> {
    fn visit_mut(&mut self, node: &mut T) -> Flow {
        let _ = node;
        Flow::Walk
    }
}

/// Structure: a node type that can present itself to a [`Visit`]-based
/// visitor `V` and recurse into its children. `V` is the IR's umbrella
/// visitor trait, so a single `walk` can reach every node kind.
pub trait Visitable<V> {
    fn walk(&self, v: &mut V) -> ControlFlow<()>;
}

/// Mutable mirror of [`Visitable`].
pub trait VisitableMut<V> {
    fn walk_mut(&mut self, v: &mut V) -> ControlFlow<()>;
}

// ---------------------------------------------------------------------------
// Method-resolution shims
//
// Inside a generic fn with a single `Visit<T>` bound, `v.visit(node)` is
// unambiguous even when `V` also implements `Visit<U>` for other `U` —
// the only `Visit` in scope is `Visit<T>`, and `T` is inferred from the
// node. The `enter!`/`enter_mut!` macros call these so node `walk`
// implementations never hit E0034 ("multiple applicable items").
// ---------------------------------------------------------------------------

#[doc(hidden)]
pub fn enter_node<T: ?Sized, V: Visit<T>>(v: &mut V, node: &T) -> Flow {
    v.visit(node)
}

#[doc(hidden)]
pub fn enter_node_mut<T: ?Sized, V: VisitMut<T>>(v: &mut V, node: &mut T) -> Flow {
    v.visit_mut(node)
}

/// Show `node` to the visitor and act on its decision: on [`Flow::Stop`]
/// abort the traversal (`return Break`), on [`Flow::Skip`] stop
/// descending into this node (`return Continue`), on [`Flow::Walk`] fall
/// through so the caller recurses into children. Use inside a
/// [`Visitable::walk`] body.
#[macro_export]
macro_rules! enter {
    ($v:expr, $node:expr) => {
        match $crate::tree::enter_node($v, $node) {
            $crate::tree::Flow::Stop => return ::core::ops::ControlFlow::Break(()),
            $crate::tree::Flow::Skip => return ::core::ops::ControlFlow::Continue(()),
            $crate::tree::Flow::Walk => {}
        }
    };
}

/// Mutable mirror of [`enter!`].
#[macro_export]
macro_rules! enter_mut {
    ($v:expr, $node:expr) => {
        match $crate::tree::enter_node_mut($v, $node) {
            $crate::tree::Flow::Stop => return ::core::ops::ControlFlow::Break(()),
            $crate::tree::Flow::Skip => return ::core::ops::ControlFlow::Continue(()),
            $crate::tree::Flow::Walk => {}
        }
    };
}

/// Recurse into a child, propagating a [`ControlFlow::Break`] (a
/// [`Flow::Stop`] that occurred deeper in the tree) up and out. Use
/// inside a [`Visitable::walk`] body: `descend!(child.walk(v));`.
#[macro_export]
macro_rules! descend {
    ($e:expr) => {
        if let ::core::ops::ControlFlow::Break(()) = $e {
            return ::core::ops::ControlFlow::Break(());
        }
    };
}

/// Declare an IR's umbrella visitor trait (read-only) over the listed
/// node types, with the blanket impl that makes any type implementing
/// all the `Visit<_>` parts a visitor for that IR.
///
/// ```ignore
/// umbrella!(HirVisitor: Block, Stmt, Expr);
/// ```
#[macro_export]
macro_rules! umbrella {
    ($name:ident: $($node:ty),+ $(,)?) => {
        pub trait $name: $($crate::tree::Visit<$node> +)+ {}
        impl<V> $name for V where V: $($crate::tree::Visit<$node> +)+ {}
    };
}

/// Mutable mirror of [`umbrella!`].
#[macro_export]
macro_rules! umbrella_mut {
    ($name:ident: $($node:ty),+ $(,)?) => {
        pub trait $name: $($crate::tree::VisitMut<$node> +)+ {}
        impl<V> $name for V where V: $($crate::tree::VisitMut<$node> +)+ {}
    };
}

// ---------------------------------------------------------------------------
// Closure adapters
//
// `On<T, F>` reacts to nodes of one type `T` via a closure and no-ops
// every other type by the `Visit` default. To satisfy an IR umbrella
// (which requires `Visit<U>` for each of its node types), the IR also
// supplies blanket no-op impls of `Visit<U> for On<T, _>` for its other
// node types — `noop_other_visits!` generates those.
// ---------------------------------------------------------------------------

/// A visitor that reacts to a single node type `T` through a closure and
/// ignores all others. Pair with the IR's `noop_other_visits!` so it
/// satisfies that IR's umbrella.
pub struct On<T: ?Sized, F> {
    f: F,
    _marker: std::marker::PhantomData<fn(&T)>,
}

impl<T: ?Sized, F: FnMut(&T) -> Flow> On<T, F> {
    pub fn new(f: F) -> Self {
        On {
            f,
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: ?Sized, F: FnMut(&T) -> Flow> Visit<T> for On<T, F> {
    fn visit(&mut self, node: &T) -> Flow {
        (self.f)(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ops::ControlFlow;

    // A pocket-sized tree IR to exercise the framework end to end.
    // `pub` so the generated `NodeVisitor` umbrella's bounds aren't more
    // private than the trait (the macro emits a `pub trait`).
    #[derive(Debug)]
    pub enum Node {
        Leaf(i32),
        Branch(Vec<Node>),
    }

    umbrella!(NodeVisitor: Node);

    impl<V: NodeVisitor> Visitable<V> for Node {
        fn walk(&self, v: &mut V) -> ControlFlow<()> {
            enter!(v, self);
            if let Node::Branch(kids) = self {
                for k in kids {
                    descend!(k.walk(v));
                }
            }
            ControlFlow::Continue(())
        }
    }

    fn sample() -> Node {
        Node::Branch(vec![
            Node::Leaf(1),
            Node::Branch(vec![Node::Leaf(2), Node::Leaf(3)]),
            Node::Leaf(4),
        ])
    }

    #[test]
    fn walk_visits_every_node() {
        struct Sum(i32);
        impl Visit<Node> for Sum {
            fn visit(&mut self, n: &Node) -> Flow {
                if let Node::Leaf(x) = n {
                    self.0 += x;
                }
                Flow::Walk
            }
        }
        let mut s = Sum(0);
        assert!(sample().walk(&mut s).is_continue());
        assert_eq!(s.0, 10);
    }

    #[test]
    fn skip_prunes_subtree() {
        // Skip the inner branch (and thus 2 and 3); still see 1 and 4.
        struct Sum(i32);
        impl Visit<Node> for Sum {
            fn visit(&mut self, n: &Node) -> Flow {
                match n {
                    Node::Leaf(x) => {
                        self.0 += x;
                        Flow::Walk
                    }
                    // The root branch has >2 kids; the nested one has 2.
                    Node::Branch(k) if k.len() == 2 => Flow::Skip,
                    Node::Branch(_) => Flow::Walk,
                }
            }
        }
        let mut s = Sum(0);
        assert!(sample().walk(&mut s).is_continue());
        assert_eq!(s.0, 5, "inner branch pruned");
    }

    #[test]
    fn stop_halts_traversal() {
        struct FindTwo {
            seen: Vec<i32>,
        }
        impl Visit<Node> for FindTwo {
            fn visit(&mut self, n: &Node) -> Flow {
                if let Node::Leaf(x) = n {
                    self.seen.push(*x);
                    if *x == 2 {
                        return Flow::Stop;
                    }
                }
                Flow::Walk
            }
        }
        let mut f = FindTwo { seen: Vec::new() };
        assert!(sample().walk(&mut f).is_break(), "Stop breaks out");
        assert_eq!(f.seen, vec![1, 2], "halted right after 2");
    }

    #[test]
    fn on_adapter_reacts_to_one_type() {
        let mut count = 0;
        {
            let mut adapter = On::<Node, _>::new(|n: &Node| {
                if matches!(n, Node::Leaf(_)) {
                    count += 1;
                }
                Flow::Walk
            });
            let _ = sample().walk(&mut adapter);
        }
        assert_eq!(count, 4);
    }
}
