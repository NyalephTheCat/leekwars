//! Tunables for the charge pass.

/// Tunables for the charge pass.
#[derive(Debug, Clone, Copy)]
pub struct ChargeOpts {
    /// Ops debited per statement executed. Default 1.
    pub per_stmt: u64,
    /// Ops debited per expression evaluated. Default 1.
    pub per_expr: u64,
}

impl Default for ChargeOpts {
    fn default() -> Self {
        Self {
            per_stmt: 1,
            per_expr: 1,
        }
    }
}
