//! The lint registry — the single source of truth for "which lints exist".
//!
//! Every rule module declares itself with
//! [`declare_lint!`](crate::declare_lint), which expands to its
//! `static META: LintMeta` *and* a `REGISTRATION` constant. `rules/mod.rs`
//! lists the module names once (in the `lint_rules!` invocation) and derives
//! both the `pub mod` lines and [`REGISTRY`](crate::rules::REGISTRY) from that
//! one list, so [`crate::all_passes`] and [`crate::allow`]'s name lookup are
//! generated rather than hand-maintained.
//!
//! `tests/registry.rs` closes the loop against the diagnostic catalog: every
//! `L0xxx` code has exactly one registration and every registration's code is
//! in the catalog.
//!
//! A registration builds its pass through `fn() -> Box<dyn LintPass>`, so a
//! pass must be constructible with no arguments. That is deliberate: anything
//! a lint needs to know about the run (the language version, the enabled
//! groups) reaches it through [`LintCx`](crate::pass::LintCx) instead of
//! through a constructor, which is what keeps this list mechanical.

use crate::pass::{LintMeta, LintPass};

/// One lint's entry in the registry: its static description plus a
/// constructor for a fresh pass.
pub struct LintRegistration {
    pub meta: &'static LintMeta,
    pub make: fn() -> Box<dyn LintPass>,
}

/// Declare a lint: its `static META` and its registry entry.
///
/// ```ignore
/// declare_lint!(ApproxConstant, "approx-constant", codes::APPROX_CONSTANT, Pedantic,
///     "real literal approximating a known constant — use the builtin (`PI`, `E`)");
/// ```
///
/// The type must implement [`Default`] (every rule is a unit struct or derives
/// it) and [`LintPass`]; the `fn meta` body stays in the rule's `impl` block
/// since the trait has other methods the macro cannot fill in.
macro_rules! declare_lint {
    ($ty:ty, $name:literal, $code:expr, $group:ident, $desc:literal $(,)?) => {
        static META: $crate::pass::LintMeta = $crate::pass::LintMeta {
            name: $name,
            code: $code,
            group: $crate::group::LintGroup::$group,
            description: $desc,
        };

        /// This lint's entry in [`crate::rules::REGISTRY`].
        pub(crate) const REGISTRATION: $crate::registry::LintRegistration =
            $crate::registry::LintRegistration {
                meta: &META,
                make: || ::std::boxed::Box::new(<$ty>::default()),
            };
    };
}

pub(crate) use declare_lint;
