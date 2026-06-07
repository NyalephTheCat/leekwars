//! Macros for defining pipeline [`Step`](crate::Step) implementations.

/// Define a pipeline step whose driver returns `(artifact_inner, diagnostics)`.
///
/// ```ignore
/// fn run_pragma(cx: &Context<'_>) -> (Pragmas, Vec<Diagnostic>) { … }
/// define_step!(Pragma, "pragma", PragmasArtifact, run_pragma);
/// ```
#[macro_export]
macro_rules! define_step {
    ($step:ident, $step_name:literal, $artifact:ident, $runner:ident) => {
        pub struct $step;

        impl $crate::Step for $step {
            fn name(&self) -> &'static str {
                $step_name
            }

            fn run(&self, cx: &mut $crate::Context<'_>) -> Result<(), $crate::StepError> {
                let (out, diags) = $runner(cx);
                cx.emit_all(diags);
                cx.insert($artifact(out));
                Ok(())
            }
        }
    };
}

/// Define a step whose driver returns `Option<artifact_inner>` and may emit
/// diagnostics itself (e.g. MIR lowering when HIR is missing).
#[macro_export]
macro_rules! define_step_opt {
    ($step:ident, $step_name:literal, $artifact:ident, $runner:ident) => {
        pub struct $step;

        impl $crate::Step for $step {
            fn name(&self) -> &'static str {
                $step_name
            }

            fn run(&self, cx: &mut $crate::Context<'_>) -> Result<(), $crate::StepError> {
                if let Some(out) = $runner(cx) {
                    cx.insert($artifact(out));
                }
                Ok(())
            }
        }
    };
}
