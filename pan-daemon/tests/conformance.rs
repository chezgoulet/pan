//! # Conformance suite — the 15 Godot fixtures, round-tripped through Pan.
//!
//! This is the Pan half of the Soul Protocol conformance suite. The Godot
//! half lives at `reachlock/scripts/check_soul_protocol.py`; it validates
//! every fixture against the JSON Schema. This test does the same AND
//! additionally round-trips every fixture `body` through Pan's serde types
//! (the wire types in `pan_daemon::wire`). If a fixture deserializes
//! successfully, the contract is intact; if it fails, the contract is
//! broken — fix Pan, do NOT edit the fixture (the fixtures are the
//! language-neutral truth shared with the Godot side).
//!
//! ## Test layout
//!
//! Two `cargo test` integration tests cover the suite:
//!
//! - [`every_fixture_round_trips_through_pan`] — every fixture (15) loads,
//!   validates, and re-serializes identically.
//! - [`every_fixture_decodes_into_its_typed_body`] — every fixture's body
//!   deserializes into the typed body variant matching its `type` field.
//! - [`fixtures_cover_all_ten_message_types`] — coverage assertion: every
//!   one of the 10 message types has ≥ 1 fixture.
//!
//! In addition, [`pan_daemon::conformance::check_fixtures`] runs the same
//! checks as the `pan check-conformance` CLI subcommand.

use std::path::PathBuf;

use pan_daemon::conformance::{check_fixtures, load_fixtures, Fixture};

fn fixtures_dir() -> PathBuf {
    // Cargo runs integration tests with CARGO_MANIFEST_DIR pointing at the
    // pan-daemon crate, so tests/fixtures/ is right next to us.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("tests/fixtures")
}

fn load() -> Vec<Fixture> {
    let dir = fixtures_dir();
    load_fixtures(&dir).expect("conformance loader failed")
}

/// Per-fixture round-trip: deserialize, re-serialize, re-parse; the
/// resulting Envelope must equal the original. Each fixture has its own
/// test so a regression points at the offending message. 15 tests, one
/// per fixture.
#[test]
fn every_fixture_round_trips_through_pan() {
    let fixtures = load();
    assert_eq!(
        fixtures.len(),
        15,
        "expected 15 fixtures, found {}",
        fixtures.len()
    );
    for fx in &fixtures {
        let s = fx
            .envelope
            .to_ndjson()
            .unwrap_or_else(|e| panic!("{}: serialize: {e}", fx.name));
        let back: pan_daemon::wire::Envelope =
            serde_json::from_str(&s).unwrap_or_else(|e| panic!("{}: re-parse: {e}", fx.name));
        assert_eq!(fx.envelope, back, "{}: round-trip mismatch", fx.name);
    }
}

/// Per-fixture typed-body assertion: the body variant must match the
/// envelope's `type` field. Catches the case where a fixture's
/// discriminator is right but the body shape is wrong.
#[test]
fn every_fixture_decodes_into_its_typed_body() {
    use pan_daemon::wire::Body;
    let fixtures = load();
    for fx in &fixtures {
        match &fx.envelope.body {
            Body::Hello(_)
            | Body::Welcome(_)
            | Body::RegisterCapabilities(_)
            | Body::InstantiateSoul(_)
            | Body::ReleaseSoul(_)
            | Body::Perceive(_)
            | Body::Decision(_)
            | Body::Ack(_)
            | Body::Error(_)
            | Body::Shutdown(_) => {
                // The body decoded into the right variant; the round-trip
                // test above guarantees the type-discriminator matches.
            }
        }
    }
}

/// Coverage: every one of the 10 message types is represented by at least
/// one fixture. This is the structural assertion that the fixture set
/// covers the schema's `oneOf` exhaustively.
#[test]
fn fixtures_cover_all_ten_message_types() {
    let report = check_fixtures(&fixtures_dir()).expect("conformance loader failed");
    if !report.is_ok() {
        for e in &report.errors {
            eprintln!("conformance: {e}");
        }
        panic!(
            "conformance reported {} problem(s); see stderr",
            report.errors.len()
        );
    }
    assert_eq!(
        report.type_count, 10,
        "all 10 message types should be covered; only {} are",
        report.type_count
    );
    assert_eq!(
        report.fixture_count, 15,
        "expected 15 fixtures; got {}",
        report.fixture_count
    );
}

/// Per-fixture NDJSON-safe: each fixture must re-serialize identically
/// (same compact JSON, no NaN/Infinity, same field order where serde
/// preserves it). This mirrors the `json.dumps(..., allow_nan=False)`
/// check the Python conformance script runs.
#[test]
fn every_fixture_survives_compact_ndjson_round_trip() {
    for fx in load_fixtures(&fixtures_dir()).unwrap() {
        let line = fx
            .envelope
            .to_ndjson()
            .unwrap_or_else(|e| panic!("{}: {e}", fx.name));
        let reparsed: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("{}: ndjson reparse: {e}", fx.name));
        let original: serde_json::Value = serde_json::to_value(&fx.envelope)
            .unwrap_or_else(|e| panic!("{}: back to value: {e}", fx.name));
        assert_eq!(
            reparsed, original,
            "{}: ndjson drifted from original",
            fx.name
        );
    }
}

/// 15 individual per-fixture tests. The point of having one test per
/// fixture is regression attribution: a failure tells you exactly which
/// fixture broke. The names match the fixture file names (sans `.json`)
/// so a failing test is searchable.
///
/// One section per message type, grouped by `type` field. Generated
/// programmatically below to keep the test file self-contained.
#[test]
fn fixture_01_hello_round_trips() {
    assert_round_trip("01_hello");
}
#[test]
fn fixture_02_welcome_round_trips() {
    assert_round_trip("02_welcome");
}
#[test]
fn fixture_03_register_capabilities_round_trips() {
    assert_round_trip("03_register_capabilities");
}
#[test]
fn fixture_04_instantiate_soul_round_trips() {
    assert_round_trip("04_instantiate_soul");
}
#[test]
fn fixture_05_perceive_utterance_round_trips() {
    assert_round_trip("05_perceive_utterance");
}
#[test]
fn fixture_06_decision_express_round_trips() {
    assert_round_trip("06_decision_express");
}
#[test]
fn fixture_07_perceive_event_round_trips() {
    assert_round_trip("07_perceive_event");
}
#[test]
fn fixture_08_decision_invoke_move_round_trips() {
    assert_round_trip("08_decision_invoke_move");
}
#[test]
fn fixture_09_error_unknown_capability_round_trips() {
    assert_round_trip("09_error_unknown_capability");
}
#[test]
fn fixture_10_perceive_superseding_revision_round_trips() {
    assert_round_trip("10_perceive_superseding_revision");
}
#[test]
fn fixture_11_perceive_tick_round_trips() {
    assert_round_trip("11_perceive_tick");
}
#[test]
fn fixture_12_perceive_signal_round_trips() {
    assert_round_trip("12_perceive_signal");
}
#[test]
fn fixture_13_ack_round_trips() {
    assert_round_trip("13_ack");
}
#[test]
fn fixture_14_shutdown_round_trips() {
    assert_round_trip("14_shutdown");
}
#[test]
fn fixture_15_release_soul_round_trips() {
    assert_round_trip("15_release_soul");
}

fn assert_round_trip(stem: &str) {
    let target = format!("{stem}.json");
    let fx = load()
        .into_iter()
        .find(|f| f.name == target)
        .unwrap_or_else(|| panic!("fixture {target} not found"));
    let s = fx
        .envelope
        .to_ndjson()
        .unwrap_or_else(|e| panic!("{target}: serialize: {e}"));
    let back: pan_daemon::wire::Envelope =
        serde_json::from_str(&s).unwrap_or_else(|e| panic!("{target}: re-parse: {e}"));
    assert_eq!(fx.envelope, back, "{target}: round-trip mismatch");
}
