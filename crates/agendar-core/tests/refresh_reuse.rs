//! A refresh that ingests returns the state its commit computed. These tests
//! hold it equal to the reload path (`refresh(now, true)` reads the committed
//! snapshot back from SQLite and reduces it again) on the oracle seeds and on
//! the iOS-sized workload, after a cold ingest and after an appended batch.
//!
//! The ignored `refresh_timings` measures cold open, relaunch +50 events and
//! warm refresh on this machine:
//! `cargo test --release -p agendar-core --test refresh_reuse -- --ignored --nocapture`

mod support;
mod workload;

use std::path::Path;
use std::time::Instant;

use agendar_core::model::{Event, UtcTime};
use agendar_core::runtime::HostIdentity;
use agendar_core::service::{Fresh, Service};
use agendar_core::vault::{Clock, SystemClock, UlidIdSource, Vault};

const SEEDS: &[&str] = &[
    "answer-conflict",
    "concurrent-consumption",
    "concurrent-observations",
    "delivery-seen-review",
    "eta-five-features-mixed",
    "eta-one-feature-five-attempts",
    "eta-overlapping-pauses",
    "plan-cycle",
    "ranking-two-asks",
    "transitive-taint",
];

fn open_service(vault: &Path, data: &Path, now: UtcTime) -> Service {
    HostIdentity::load_or_create(data, now).expect("host identity");
    Service::open(vault, data).expect("open service")
}

fn new_vault(root: &Path, events: &[Event]) -> Vault {
    let vault = Vault::init(root, &SystemClock, &mut UlidIdSource).expect("vault init");
    append(&vault, events);
    vault
}

fn append(vault: &Vault, events: &[Event]) {
    for event in events {
        support::append_event_to_vault(vault, event, 1).expect("append event");
    }
}

/// The ingesting refresh equals the reload of what it committed.
fn assert_reused_equals_reloaded(service: &Service, now: UtcTime, label: &str) {
    let reused = service.refresh(now, false).expect("ingesting refresh");
    let reloaded = service.refresh(now, true).expect("reload");
    assert_same(&reused, &reloaded, label);
}

fn assert_same(reused: &Fresh, reloaded: &Fresh, label: &str) {
    assert_eq!(reused.events, reloaded.events, "{label}: events");
    assert_eq!(
        reused.resolution, reloaded.resolution,
        "{label}: resolution"
    );
    assert_eq!(reused.state, reloaded.state, "{label}: state");
    assert_eq!(
        reused.generation, reloaded.generation,
        "{label}: generation"
    );
    assert_eq!(
        reused.source_digest, reloaded.source_digest,
        "{label}: source digest"
    );
    let reused_freshness = Service::freshness(reused, false).expect("freshness");
    let reloaded_freshness = Service::freshness(reloaded, false).expect("freshness");
    assert_eq!(
        reused_freshness.event_set_digest, reloaded_freshness.event_set_digest,
        "{label}: event set digest"
    );
    assert_eq!(
        reused_freshness.event_set_digest, reused.resolution.event_set_digest,
        "{label}: stored digest is the resolution's"
    );
}

#[test]
fn reused_state_equals_reloaded_state_on_every_seed() {
    for name in SEEDS {
        let seed = support::load_seed(name);
        let temp = tempfile::tempdir().expect("tempdir");
        let half = seed.events.len() / 2;
        let vault = new_vault(&temp.path().join("vault"), &seed.events[..half]);
        let service = open_service(vault.root(), &temp.path().join("data"), seed.as_of);
        assert_reused_equals_reloaded(&service, seed.as_of, &format!("{name} cold"));
        append(&vault, &seed.events[half..]);
        assert_reused_equals_reloaded(&service, seed.as_of, &format!("{name} appended"));
        // A fresh process on the same data dir (a relaunch) after the rest.
        let relaunched = open_service(vault.root(), &temp.path().join("data"), seed.as_of);
        assert_reused_equals_reloaded(&relaunched, seed.as_of, &format!("{name} relaunch"));
    }
}

fn ios_shape(base: UtcTime) -> workload::Shape {
    workload::Shape {
        features: 1082,
        deep: false,
        agent_hosts: 3,
        open_ask: Some(980),
        base,
    }
}

/// The iOS fixture's workload: the Vault part and five batches of 50 events.
fn ios_workload() -> (Vec<Event>, Vec<Vec<Event>>, UtcTime) {
    let base = UtcTime::parse("2026-01-01T00:00:00.000Z").expect("base");
    let (events, as_of) = workload::workload(&ios_shape(base));
    let cut = events
        .iter()
        .position(|event| {
            let value = serde_json::to_value(event).expect("event serializes");
            value["kind"] == "plan.feature" && value["refs"]["node"] == "n1022"
        })
        .expect("cut feature");
    let (vault, tail) = events.split_at(cut);
    let batches = tail.chunks(50).take(5).map(<[Event]>::to_vec).collect();
    (vault.to_vec(), batches, as_of)
}

#[test]
fn reused_state_equals_reloaded_state_on_the_ios_workload() {
    let (events, batches, as_of) = ios_workload();
    let temp = tempfile::tempdir().expect("tempdir");
    let vault = new_vault(&temp.path().join("vault"), &events);
    let data = temp.path().join("data");
    let service = open_service(vault.root(), &data, as_of);
    assert_reused_equals_reloaded(&service, as_of, "ios cold");
    append(&vault, &batches[0]);
    let relaunched = open_service(vault.root(), &data, as_of);
    assert_reused_equals_reloaded(&relaunched, as_of, "ios relaunch +50");
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

#[test]
#[ignore = "timing; run with --release --ignored --nocapture"]
fn refresh_timings() {
    let (events, batches, _) = ios_workload();
    let temp = tempfile::tempdir().expect("tempdir");
    let vault = new_vault(&temp.path().join("vault"), &events);
    let mut cold = Vec::new();
    for run in 0..5 {
        let data = temp.path().join(format!("cold-{run}"));
        let start = Instant::now();
        let service = open_service(vault.root(), &data, SystemClock.now());
        service.refresh(SystemClock.now(), false).expect("cold");
        cold.push(ms(start));
    }
    let data = temp.path().join("cold-4");
    let service = open_service(vault.root(), &data, SystemClock.now());
    let mut warm = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        service.refresh(SystemClock.now(), false).expect("warm");
        warm.push(ms(start));
    }
    drop(service);
    let mut relaunch = Vec::new();
    for batch in &batches {
        append(&vault, batch);
        let start = Instant::now();
        let service = open_service(vault.root(), &data, SystemClock.now());
        service.refresh(SystemClock.now(), false).expect("relaunch");
        relaunch.push(ms(start));
    }
    println!(
        "refresh_timings: {} events; cold median {:.1} ms {cold:.1?}; relaunch +50 median {:.1} ms {relaunch:.1?}; warm median {:.1} ms {warm:.1?}",
        events.len(),
        median(cold.clone()),
        median(relaunch.clone()),
        median(warm.clone()),
    );
}
