//! Example: auto-generate a recipe from artifact-defined recipes.
//!
//! Run with: `cargo run -p leek-pipeline --example auto_recipe`

use leek_mir::pipeline::MirArtifact;
use leek_pipeline::{Input, RecipeParams, pipeline_for_recipe};
use leek_span::SourceId;

fn main() {
    let params = RecipeParams::default();
    let pipeline = pipeline_for_recipe::<MirArtifact>(&params).expect("recipe");
    let run = pipeline.run(Input {
        source: SourceId::new(1).unwrap(),
        text: "function add(a, b) { return a + b; }\nvar x = add(1, 2);\n".into(),
        version_byte: 4,
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    });

    println!("mir? {}", run.get::<MirArtifact>().is_some());
    println!("diagnostics: {}", run.diagnostics().len());
}
