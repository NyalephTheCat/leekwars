//! Example: a third-party step that counts how many `if` statements
//! the source contains, reusing the canonical pipeline's HIR.
//!
//! Run with: `cargo run -p leek-pipeline --example custom_step`.

use leek_hir::pipeline::HirArtifact;
use leek_hir::{Block, Stmt};
use leek_pipeline::{Artifact, Context, Input, Step, StepError};
use leek_recipes::{RecipeParams, Target};

/// Output of [`CountIfs`].
struct IfCount(u32);
impl Artifact for IfCount {}

/// Walks HIR and counts every `Stmt::If` it sees.
struct CountIfs;

impl Step for CountIfs {
    fn name(&self) -> &'static str {
        "count-ifs"
    }
    fn run(&self, cx: &mut Context<'_>) -> Result<(), StepError> {
        let Some(hir) = cx.get::<HirArtifact>() else {
            return Ok(());
        };
        let mut n = 0u32;
        for def in &hir.0.defs {
            if let leek_hir::Def::Function(f) = def
                && let Some(body) = &f.body
            {
                walk_block(body, &mut n);
            }
        }
        walk_stmts(&hir.0.main, &mut n);
        cx.insert(IfCount(n));
        Ok(())
    }
}

fn walk_block(b: &Block, n: &mut u32) {
    walk_stmts(&b.stmts, n);
}

fn walk_stmts(stmts: &[Stmt], n: &mut u32) {
    for s in stmts {
        if matches!(s, Stmt::If(_)) {
            *n += 1;
        }
    }
}

fn main() {
    let pipeline = leek_recipes::pipeline(Target::Hir, &RecipeParams::permissive())
        .expect("recipe")
        .with(CountIfs);
    let run = pipeline.run(Input {
        source: leek_span::SourceId::new(1).unwrap(),
        text: "if (1 > 0) { var a = 1; } if (a == 1) { var b = 2; }".into(),
        version_byte: 4,
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    });

    let count = run.get::<IfCount>().map_or(0, |c| c.0);
    println!("ifs: {count}");
    println!("diagnostics: {}", run.diagnostics().len());
}
