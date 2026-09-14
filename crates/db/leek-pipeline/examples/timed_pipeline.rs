use leek_pipeline::TimingSink;
use leek_project::Input;
use leek_session::{RecipeParams, Target};
use leek_span::SourceId;

fn main() {
    let sink = TimingSink::new();
    let pipeline = leek_session::plan(Target::Mir, &RecipeParams::permissive())
        .expect("recipe")
        .build_with(Some(&sink));
    let _run = pipeline.run(Input {
        source: SourceId::new(1).unwrap(),
        text: std::fs::read_to_string("/tmp/case7.leek").unwrap().into(),
        version_byte: 1,
        strict: false,
        flags: leek_span::FeatureFlags::from_env(),
    });
    println!("Per-step timings:");
    for t in sink.entries() {
        println!("  {:>12}: {:?}", t.step, t.duration);
    }
}
