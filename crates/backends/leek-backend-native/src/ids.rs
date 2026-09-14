//! The one place the HIR → runtime id mapping is defined.
//!
//! [`leek_runtime::ClassId`] / [`leek_runtime::FnId`] are opaque handles the
//! runtime only stores and compares (see their docs); this backend is the
//! loader that mints them, and it numbers them by the class's / function's
//! `DefId` — the same key the dispatch tables (`USER_FN_IDX`, `CLASS_PARENT`,
//! `CLASS_REFLECT`, `DISPATCH`) are built from in [`crate::define_program`].
//! Keeping both directions here means the numbering is stated once instead of
//! being re-derived at every `Value` construction site.

use leek_hir::DefId;
use leek_runtime::{ClassId, FnId};

/// Runtime handle for the class declared at `def`.
pub const fn class_id(def: DefId) -> ClassId {
    ClassId(def.0)
}

/// Runtime handle for the function declared at `def`.
pub const fn fn_id(def: DefId) -> FnId {
    FnId(def.0)
}

#[cfg(test)]
mod tests {
    use super::{class_id, fn_id};
    use leek_hir::DefId;
    use leek_runtime::{ClassId, FnId};

    /// The dispatch tables installed by `define_program` are keyed by the raw
    /// `DefId.0`, and the shims look them up through `ClassId.0` / `FnId.0`
    /// (`USER_FN_IDX`, `CLASS_PARENT`, `CLASS_REFLECT`). If the mapping ever
    /// stopped being the identity, every dynamic class-ref and
    /// `Function::User` dispatch would miss.
    #[test]
    fn the_mapping_preserves_the_raw_def_index() {
        assert_eq!(class_id(DefId(0)), ClassId(0));
        assert_eq!(class_id(DefId(7)), ClassId(7));
        assert_eq!(fn_id(DefId(0)), FnId(0));
        assert_eq!(fn_id(DefId(7)), FnId(7));
    }
}
