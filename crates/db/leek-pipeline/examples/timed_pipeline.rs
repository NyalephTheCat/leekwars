use leek_pipeline::{Input, TimingSink};
use leek_recipes::{RecipeParams, Target};
use leek_span::SourceId;

fn main() {
    let sink = TimingSink::new();
    let pipeline = leek_recipes::pipeline_timed(Target::Mir, &RecipeParams::permissive(), &sink)
        .expect("recipe");
    let _run = pipeline.run(Input {
        source: SourceId::new(1).unwrap(),
        text: std::fs::read_to_string("/tmp/case7.leek").unwrap().into(),
        version_byte: 1,
        strict: false,
        flags: leek_pipeline::FeatureFlags::from_env(),
    });
    println!("Per-step timings:");
    for t in sink.entries() {
        println!("  {:>12}: {:?}", t.step, t.duration);
    }
}
