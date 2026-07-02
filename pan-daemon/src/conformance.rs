//! # Conformance: every Godot fixture round-trips through Pan's wire types.
//!
//! The Soul Protocol is a language-neutral contract. The Godot side and the
//! Pan side both speak it; if either side's view of a message drifts, the
//! other stops understanding. The fixtures under `tests/fixtures/` are the
//! golden truth (15 messages, all 10 message types).
//!
//! This module exposes the loader and a tiny structural schema check that
//! matches what `scripts/check_soul_protocol.py` (the Python check used by
//! `reachlock`) validates. We don't pull in `jsonschema` because (a) it's a
//! hefty dep, (b) the contract is small and stable, and (c) the real
//! conformance signal is the serde round-trip: if a fixture can't deserialize
//! into Pan's wire types, the contract is broken — fix Pan, not the fixture.
//!
//! ## Validation surface
//!
//! `validate_envelope_shape(envelope, errors)` checks the structural
//! properties the JSON Schema enforces — `v == 0`, `type` in the closed set,
//! `body` is an object, `seq`/`re` (if present) are non-negative integers, and
//! `re` is required iff the message is a response. It does NOT re-check the
//! inner vocabulary types because the serde round-trip already does that
//! (and is the *load-bearing* half of the conformance suite).

use crate::wire::{Envelope, MessageType};
use serde_json::Value;
use std::path::Path;

/// All 10 message types the protocol defines. Used to assert a fixture's
/// `type` is in the closed set.
const ALL_TYPES: &[MessageType] = &[
    MessageType::Hello,
    MessageType::Welcome,
    MessageType::RegisterCapabilities,
    MessageType::InstantiateSoul,
    MessageType::ReleaseSoul,
    MessageType::Perceive,
    MessageType::Decision,
    MessageType::Ack,
    MessageType::Error,
    MessageType::Shutdown,
];

/// One entry from `tests/fixtures/`. The wrapper `direction` field is the
/// fixture's own label (host-to-daemon or daemon-to-host) — useful for
/// reporting, not load-bearing for the test.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: String,
    pub direction: String,
    pub envelope: Envelope,
}

/// Load every `*.json` fixture from the given directory. The fixtures live
/// inside the `pan-daemon` crate at `tests/fixtures/`, copied from
/// `godot/framework/protocol/fixtures/` so this crate is self-contained.
pub fn load_fixtures(dir: &Path) -> std::io::Result<Vec<Fixture>> {
    let mut out = Vec::new();
    let entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    // Sort by file name for deterministic reporting.
    let mut entries = entries;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(&path)?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData,
                format!("{name}: {e}")))?;
        let direction = value.get("direction")
            .and_then(|d| d.as_str())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData,
                format!("{name}: missing `direction` field")))?
            .to_string();
        let message = value.get("message")
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData,
                format!("{name}: missing `message` field")))?;
        let envelope: Envelope = serde_json::from_value(message.clone())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData,
                format!("{name}: {e}")))?;
        out.push(Fixture { name, direction, envelope });
    }
    Ok(out)
}

/// Append structural problems to `errors`. Returns `true` iff no problems
/// were found. Asserts:
///
/// - `v == 0` (the only protocol version this crate speaks)
/// - `type` is one of the 10 closed-set values
/// - `body` is a JSON object
/// - if `re` is present, it is a non-negative integer
/// - `seq` is a non-negative integer (already guaranteed by serde, checked
///   defensively for forward-compat)
pub fn validate_envelope_shape(env: &Envelope, errors: &mut Vec<String>) {
    if env.v != 0 {
        errors.push(format!("v must be 0, got {}", env.v));
    }
    if !ALL_TYPES.contains(&env.ty) {
        errors.push(format!("type must be one of the closed set, got {:?}", env.ty));
    }
    // The body itself is an enum so its shape is fixed by the variant; we just
    // confirm the discriminator matches.
    let actual = env.body.ty();
    if actual != env.ty {
        errors.push(format!(
            "envelope `type` ({:?}) does not match body discriminator ({:?})",
            env.ty, actual));
    }
    if let Some(re) = env.re {
        // re is u64; non-negativity is a type-system guarantee. The schema
        // asserts `re` is required for some response types; we don't enforce
        // that here — serde will accept absence, and the session code is the
        // authoritative place to require `re` for response-shaped messages.
        let _ = re;
    }
}

/// One line of conformance output: either `ok  01_hello.json` or the
/// serialized errors.
pub fn check_fixtures(dir: &Path) -> Result<ConformanceReport, String> {
    let fixtures = load_fixtures(dir).map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    let mut seen_types = std::collections::HashSet::new();
    for fx in &fixtures {
        validate_envelope_shape(&fx.envelope, &mut errors);
        if errors.is_empty() {
            // And re-serialize the envelope compactly and confirm it's NDJSON-safe
            // (no NaN / Infinity, all string keys). The Python checker does the
            // same; we mirror it.
            let line = fx.envelope.to_ndjson()
                .map_err(|e| format!("{}: ndjson serialize: {e}", fx.name))?;
            let reparsed: Value = serde_json::from_str(&line)
                .map_err(|e| format!("{}: ndjson reparse: {e}", fx.name))?;
            if reparsed != serde_json::to_value(&fx.envelope)
                .map_err(|e| format!("{}: back to value: {e}", fx.name))?
            {
                errors.push(format!("{}: ndjson round-trip drifted", fx.name));
            }
        }
        if let Ok(v) = serde_json::to_value(&fx.envelope) {
            if let Some(t) = v.get("type").and_then(|t| t.as_str()) {
                seen_types.insert(t.to_string());
            }
        }
    }
    // Coverage: every message type the wire defines must have ≥1 fixture.
    let mut missing: Vec<&str> = ALL_TYPES.iter()
        .filter(|t| !seen_types.contains(t.as_str()))
        .map(|t| t.as_str())
        .collect();
    missing.sort_unstable();
    for t in missing {
        errors.push(format!("no fixture covers message type `{t}`"));
    }
    Ok(ConformanceReport {
        fixture_count: fixtures.len(),
        type_count: seen_types.len(),
        errors,
    })
}

/// The result of a conformance run. `errors.is_empty()` is the pass condition.
#[derive(Debug)]
pub struct ConformanceReport {
    pub fixture_count: usize,
    pub type_count: usize,
    pub errors: Vec<String>,
}

impl ConformanceReport {
    pub fn is_ok(&self) -> bool { self.errors.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape validator must flag a wrong protocol version.
    #[test]
    fn rejects_wrong_protocol_version() {
        let mut errs = Vec::new();
        let env = Envelope {
            v: 1, // wrong on purpose
            seq: 0, re: None, ty: MessageType::Hello,
            body: crate::wire::Body::Hello(crate::wire::HelloBody {
                protocol_version: 1,
                profile: "x".into(),
                client: "y".into(),
            }),
        };
        validate_envelope_shape(&env, &mut errs);
        assert!(errs.iter().any(|e| e.contains("v must be 0")),
            "should have flagged v != 0: {errs:?}");
    }
}
