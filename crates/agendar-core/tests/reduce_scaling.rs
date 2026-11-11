//! Reducer scaling benchmark (ignored; run in release):
//! `cargo test --release -p agendar-core --test reduce_scaling -- --ignored --nocapture`
//!
//! One synthetic project (`workload/mod.rs`) with a dependency chain,
//! re-planned dependencies, a rejected cycle, sessions that turn over, asks
//! with answers and acks, answer deliveries, deliveries with observations and
//! reviews. It prints the reduce time at about 1.5k, 6k and 24k events per project, plus a digest of
//! the canonical state so a refactor can be checked for identical output.

mod support;
mod workload;

use std::time::Instant;

use agendar_core::model::canonical_json;
use agendar_core::reduce::{ReduceContext, reduce_resolved, resolve};
use sha2::{Digest, Sha256};

fn state_digest(state: &agendar_core::model::State) -> String {
    let text = canonical_json(&serde_json::to_value(state).expect("state serializes"));
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[test]
#[ignore = "release-mode scaling benchmark; prints timings"]
fn reduce_scales_near_linearly_per_project() {
    let sizes: Vec<usize> = std::env::var("REDUCE_SCALING_FEATURES")
        .ok()
        .map(|text| {
            text.split(',')
                .filter_map(|part| part.parse().ok())
                .collect()
        })
        .unwrap_or_else(|| vec![300, 1200, 4800]);
    let deep = std::env::var("REDUCE_SCALING_SHAPE").is_ok_and(|shape| shape == "deep");
    for features in sizes {
        let (events, as_of) = workload::workload(&workload::Shape::bench(features, deep));
        let ctx = ReduceContext { as_of };
        // Best of three runs: the shape of the growth is the result, not noise.
        let mut resolve_ms = f64::MAX;
        let mut reduce_ms = f64::MAX;
        let mut state = None;
        for _ in 0..3 {
            let start = Instant::now();
            let resolution = resolve(&events);
            resolve_ms = resolve_ms.min(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(
                resolution.ordered.len(),
                events.len(),
                "every synthetic event is eligible"
            );
            let start = Instant::now();
            let reduced = reduce_resolved(&resolution, &events, &ctx);
            reduce_ms = reduce_ms.min(start.elapsed().as_secs_f64() * 1000.0);
            state = Some(reduced);
        }
        let state = state.expect("reduced at least once");
        let project = state.projects.values().next().expect("one project");
        let mut kinds: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for diagnostic in &state.diagnostics {
            *kinds
                .entry(diagnostic.code.as_deref().unwrap_or("-"))
                .or_insert(0) += 1;
        }
        println!("  diagnostics: {kinds:?}");
        println!(
            "{:>6} events ({features} features, {} decisions, {} deliveries, {} needs): resolve {resolve_ms:9.1} ms, reduce {reduce_ms:9.1} ms, state {}",
            events.len(),
            project.decisions.len(),
            project.deliveries.len(),
            project.needs_you.len(),
            state_digest(&state),
        );
    }
}
