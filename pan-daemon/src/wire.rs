//! # The Soul Protocol wire vocabulary.
//!
//! One NDJSON line per message, framed as `{"v", "seq", "re"?", "type", "body"}`.
//! Every body type here is a serde mirror of the JSON shapes in
//! `godot/framework/protocol/schemas/soul_message.schema.json` — and the inner
//! vocabulary types (`Goal`, `Context`, `Capability`, `ActionIntent`, `Decision`,
//! `Outcome`, `Trigger`) match `pan_core::schema` byte-for-byte when serialized.
//!
//! This module is the **wire**. It is intentionally a separate type tree from
//! `pan_core::schema` because:
//!
//! 1. The wire envelope (`v`/`seq`/`re`/`type`/`body`) is not a Pan concept —
//!    it is added by the Soul Protocol on top.
//! 2. The wire defines fixed `MindKind` strings (`"rules"`, `"behavior_tree"`,
//!    `"llm"`) and fixed `ErrorCode` strings that are the protocol's closed
//!    set, not the core's. Keeping them in `wire` keeps the core vocabulary
//!    clean of protocol detail.
//! 3. The decision and acknowledge responses are wire messages, not core
//!    types: a `Decision` from the core is wrapped in a `DecisionResponse` with
//!    the `soul_id` / `goal_id` / `goal_revision` fields the host needs to
//!    correlate.
//!
//! The conformance test (in `tests/conformance.rs`) round-trips every fixture
//! through these types. If a fixture deserializes but the field layout shifts,
//! the contract has drifted and the test fails loudly — *fix the contract
//! deliberately* (version bump + migration note), don't edit the fixture.

use serde::{Deserialize, Serialize};

/// Protocol version. v0 is the only version this crate speaks; the host's
/// `hello.body.protocol_version` is checked at handshake.
pub const PROTOCOL_VERSION: u32 = 0;

/// Server identity reported in the `welcome` body. Match the `server: "pan-serve/0.1.0"`
/// string in the canonical `02_welcome.json` fixture.
pub const SERVER_IDENTITY: &str = "pan-serve/0.1.0";

/// The set of minds this daemon can host. `welcome.minds` advertises this set
/// to the host. M1 ships `rules`; the others are admitted as future work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MindKind {
    Rules,
    BehaviorTree,
    Llm,
}

/// The wire's closed set of error codes (Soul Protocol v0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    #[serde(rename = "bad_frame")]
    BadFrame,
    #[serde(rename = "unknown_type")]
    UnknownType,
    #[serde(rename = "version_unsupported")]
    VersionUnsupported,
    #[serde(rename = "unknown_soul")]
    UnknownSoul,
    #[serde(rename = "unknown_capability")]
    UnknownCapability,
    #[serde(rename = "invalid_args")]
    InvalidArgs,
    #[serde(rename = "provider_failure")]
    ProviderFailure,
    #[serde(rename = "superseded")]
    Superseded,
}

impl ErrorCode {
    /// Stable string form, e.g. for logs. Matches the wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::BadFrame => "bad_frame",
            ErrorCode::UnknownType => "unknown_type",
            ErrorCode::VersionUnsupported => "version_unsupported",
            ErrorCode::UnknownSoul => "unknown_soul",
            ErrorCode::UnknownCapability => "unknown_capability",
            ErrorCode::InvalidArgs => "invalid_args",
            ErrorCode::ProviderFailure => "provider_failure",
            ErrorCode::Superseded => "superseded",
        }
    }
}

/// The wire envelope. Every NDJSON line is one of these.
///
/// `re` is absent on unsolicited messages (the schema marks it as not
/// `required`); serde handles its presence/absence for us.
/// The wire envelope. Every NDJSON line is one of these.
///
/// `re` is absent on unsolicited messages (the schema marks it as not
/// `required`); serde handles its presence/absence for us. The `body` is
/// dispatched by the envelope's `type` field via custom (de)serialization
/// — see [`Body`] for why a tagged enum doesn't fit the wire shape.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub v: u32,
    pub seq: u64,
    pub re: Option<u64>,
    pub ty: MessageType,
    pub body: Body,
}

impl Serialize for Envelope {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // Field order matches the canonical wire spelling where serde
        // permits. `re` is omitted when None.
        let mut n_fields = 4; // v, seq, type, body
        if self.re.is_some() { n_fields += 1; }
        let mut st = ser.serialize_struct("Envelope", n_fields)?;
        st.serialize_field("v", &self.v)?;
        st.serialize_field("seq", &self.seq)?;
        if let Some(re) = &self.re {
            st.serialize_field("re", re)?;
        }
        st.serialize_field("type", self.ty.as_str())?;
        st.serialize_field("body", &self.body)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for Envelope {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::fmt;

        /// Holds the partially-decoded envelope as we walk the input.
        #[derive(Default)]
        struct Fields {
            v: Option<u32>,
            seq: Option<u64>,
            re: Option<Option<u64>>,
            ty: Option<MessageType>,
            body: Option<serde_json::Value>,
        }

        struct EnvVisitor;
        impl<'de> Visitor<'de> for EnvVisitor {
            type Value = Envelope;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a Soul Protocol envelope ({v, seq, type, body, re?})")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Envelope, M::Error> {
                let mut f = Fields::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "v" => f.v = Some(map.next_value()?),
                        "seq" => f.seq = Some(map.next_value()?),
                        "re" => f.re = Some(map.next_value()?),
                        "type" => f.ty = Some(map.next_value()?),
                        "body" => f.body = Some(map.next_value()?),
                        // Unknown optional envelope keys are ignored
                        // (forward-compat; protocol says "MUST ignore unknown
                        // optional envelope keys"). Required keys (`v`, `seq`,
                        // `type`, `body`) would have to be present anyway.
                        _ => { let _: serde::de::IgnoredAny = map.next_value()?; }
                    }
                }
                let v = f.v.ok_or_else(|| serde::de::Error::missing_field("v"))?;
                let seq = f.seq.ok_or_else(|| serde::de::Error::missing_field("seq"))?;
                let re = f.re.unwrap_or(None);
                let ty = f.ty.ok_or_else(|| serde::de::Error::missing_field("type"))?;
                let body_val = f.body.ok_or_else(|| serde::de::Error::missing_field("body"))?;
                let body = Body::from_value(ty, body_val)
                    .map_err(serde::de::Error::custom)?;
                Ok(Envelope { v, seq, re, ty, body })
            }
        }
        de.deserialize_map(EnvVisitor)
    }
}

/// The 10 message types the protocol defines. Each is its own tagged variant;
/// `body` decodes straight into the matching body type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    Hello,
    Welcome,
    RegisterCapabilities,
    InstantiateSoul,
    ReleaseSoul,
    Perceive,
    Decision,
    Ack,
    Error,
    Shutdown,
}

impl MessageType {
    /// Inverse of [`MessageType::as_str`]: recognize a wire spelling.
    /// `None` means the type is outside the protocol's closed set — the
    /// protocol says the daemon MUST reject it with `error: unknown_type`
    /// (distinct from `bad_frame`, which is for lines that aren't valid
    /// envelopes at all). The connection driver uses this to stage the
    /// parse; see `server::parse_envelope`.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "hello" => Some(MessageType::Hello),
            "welcome" => Some(MessageType::Welcome),
            "register_capabilities" => Some(MessageType::RegisterCapabilities),
            "instantiate_soul" => Some(MessageType::InstantiateSoul),
            "release_soul" => Some(MessageType::ReleaseSoul),
            "perceive" => Some(MessageType::Perceive),
            "decision" => Some(MessageType::Decision),
            "ack" => Some(MessageType::Ack),
            "error" => Some(MessageType::Error),
            "shutdown" => Some(MessageType::Shutdown),
            _ => None,
        }
    }

    /// Wire spelling (snake_case) for matching against schema `const` values.
    pub fn as_str(self) -> &'static str {
        match self {
            MessageType::Hello => "hello",
            MessageType::Welcome => "welcome",
            MessageType::RegisterCapabilities => "register_capabilities",
            MessageType::InstantiateSoul => "instantiate_soul",
            MessageType::ReleaseSoul => "release_soul",
            MessageType::Perceive => "perceive",
            MessageType::Decision => "decision",
            MessageType::Ack => "ack",
            MessageType::Error => "error",
            MessageType::Shutdown => "shutdown",
        }
    }
}

/// All 10 message bodies. The host → daemon bodies carry the host's intent;
/// the daemon → host bodies carry what the daemon has decided / acked /
/// errored about. Each body type round-trips byte-identical with the
/// matching fixture in `tests/fixtures/*.json`.
///
/// `Body` is dispatched by the envelope's `type` field (not by an inner
/// tag) because the wire contract keeps the type discriminator in the
/// envelope, and the body is a plain object whose shape depends on the
/// type. We therefore (de)serialize the body as a `serde_json::Value` and
/// then route to the right concrete type via the envelope's `type` field.
/// This is the only way to round-trip the empty-body cases (Ack, Shutdown)
/// cleanly without a discriminator field appearing in the wire output.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Hello(HelloBody),
    Welcome(WelcomeBody),
    RegisterCapabilities(RegisterCapabilitiesBody),
    InstantiateSoul(InstantiateSoulBody),
    ReleaseSoul(ReleaseSoulBody),
    Perceive(PerceiveBody),
    Decision(DecisionBody),
    Ack(AckBody),
    Error(ErrorBody),
    Shutdown(ShutdownBody),
}

impl Body {
    pub fn ty(&self) -> MessageType {
        match self {
            Body::Hello(_) => MessageType::Hello,
            Body::Welcome(_) => MessageType::Welcome,
            Body::RegisterCapabilities(_) => MessageType::RegisterCapabilities,
            Body::InstantiateSoul(_) => MessageType::InstantiateSoul,
            Body::ReleaseSoul(_) => MessageType::ReleaseSoul,
            Body::Perceive(_) => MessageType::Perceive,
            Body::Decision(_) => MessageType::Decision,
            Body::Ack(_) => MessageType::Ack,
            Body::Error(_) => MessageType::Error,
            Body::Shutdown(_) => MessageType::Shutdown,
        }
    }
}

impl Serialize for Body {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        // Serialize the inner body type directly. Empty bodies (Ack, Shutdown)
        // serialize as `{}` exactly as the wire requires.
        match self {
            Body::Hello(b) => b.serialize(ser),
            Body::Welcome(b) => b.serialize(ser),
            Body::RegisterCapabilities(b) => b.serialize(ser),
            Body::InstantiateSoul(b) => b.serialize(ser),
            Body::ReleaseSoul(b) => b.serialize(ser),
            Body::Perceive(b) => b.serialize(ser),
            Body::Decision(b) => b.serialize(ser),
            Body::Ack(b) => b.serialize(ser),
            Body::Error(b) => b.serialize(ser),
            Body::Shutdown(b) => b.serialize(ser),
        }
    }
}

impl<'de> Deserialize<'de> for Body {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        // Deserialize as a generic Value; the caller is responsible for
        // matching the envelope's `type` to the right concrete body type via
        // [`Body::from_value`]. We do this in a custom Envelope deserializer
        // below so callers don't have to think about it.
        let v = serde_json::Value::deserialize(de)?;
        Err(serde::de::Error::custom(format!(
            "Body must be deserialized through Envelope (use a `{{...}}` with `type` discriminator); got {v}"
        )))
    }
}

impl Body {
    /// Decode a `serde_json::Value` (already-parsed body) into the right
    /// concrete body type given the envelope's `type` field. Returns an
    /// error message that names the field that failed to decode — useful for
    /// the protocol's `bad_frame` reply.
    pub fn from_value(ty: MessageType, v: serde_json::Value) -> Result<Self, String> {
        let _decode = |label: &str| {
            serde_json::from_value::<serde_json::Value>(v.clone())
                .ok()
                .and_then(|_| serde_json::from_value::<Self>(v.clone()).ok())
                .ok_or_else(|| format!("body for `{label}` does not match the wire schema"))
        };
        // The `decode` closure above is a sketch; we instead use the direct
        // conversion from `serde_json::Value` via a typed `from_value`. We
        // dispatch manually.
        match ty {
            MessageType::Hello => serde_json::from_value(v)
                .map(Body::Hello)
                .map_err(|e| format!("hello body: {e}")),
            MessageType::Welcome => serde_json::from_value(v)
                .map(Body::Welcome)
                .map_err(|e| format!("welcome body: {e}")),
            MessageType::RegisterCapabilities => serde_json::from_value(v)
                .map(Body::RegisterCapabilities)
                .map_err(|e| format!("register_capabilities body: {e}")),
            MessageType::InstantiateSoul => serde_json::from_value(v)
                .map(Body::InstantiateSoul)
                .map_err(|e| format!("instantiate_soul body: {e}")),
            MessageType::ReleaseSoul => serde_json::from_value(v)
                .map(Body::ReleaseSoul)
                .map_err(|e| format!("release_soul body: {e}")),
            MessageType::Perceive => serde_json::from_value(v)
                .map(Body::Perceive)
                .map_err(|e| format!("perceive body: {e}")),
            MessageType::Decision => serde_json::from_value(v)
                .map(Body::Decision)
                .map_err(|e| format!("decision body: {e}")),
            MessageType::Ack => serde_json::from_value(v)
                .map(Body::Ack)
                .map_err(|e| format!("ack body: {e}")),
            MessageType::Error => serde_json::from_value(v)
                .map(Body::Error)
                .map_err(|e| format!("error body: {e}")),
            MessageType::Shutdown => serde_json::from_value(v)
                .map(Body::Shutdown)
                .map_err(|e| format!("shutdown body: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Body types — host → daemon
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloBody {
    pub protocol_version: u32,
    pub profile: String,
    pub client: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegisterCapabilitiesBody {
    pub capabilities: Vec<pan_core::schema::Capability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InstantiateSoulBody {
    pub soul_id: String,
    pub mind: MindKind,
    /// Opaque birth-state (the `soul` field in the wire). The daemon stores
    /// these bytes verbatim and surfaces them to the provider on perceive.
    /// In M1 only `rules`-minded souls are exercised end-to-end; the field is
    /// always accepted.
    pub soul: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseSoulBody {
    pub soul_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerceiveBody {
    pub soul_id: String,
    pub goal: pan_core::schema::Goal,
    pub context: pan_core::schema::Context,
}

// ---------------------------------------------------------------------------
// Body types — daemon → host
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WelcomeBody {
    pub protocol_version: u32,
    pub server: String,
    pub minds: Vec<MindKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionBody {
    pub soul_id: String,
    pub goal_id: String,
    pub goal_revision: u64,
    pub decision: pan_core::schema::Decision,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AckBody {
    // Empty object: `{}`. No fields.
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ShutdownBody {
    // Empty object: `{}`. No fields.
}

// ---------------------------------------------------------------------------
// Convenience constructors for outgoing messages. The daemon is on the
// "writing" side of welcome / ack / decision / error; these helpers keep the
// seq/re tracking out of the call sites.
// ---------------------------------------------------------------------------

impl Envelope {
    /// Build an outgoing envelope with the given body. `re` is set when the
    /// message is a response to a specific inbound `seq`.
    pub fn outgoing(seq: u64, re: Option<u64>, body: Body) -> Self {
        Envelope { v: PROTOCOL_VERSION, seq, re, ty: body.ty(), body }
    }

    /// Convenience: decode an envelope from one NDJSON line.
    pub fn from_ndjson(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }

    /// Serialize this envelope to one NDJSON line (no trailing newline; the
    /// framing layer writes that).
    pub fn to_ndjson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pan_core::schema as v;

    /// A representative `welcome` envelope must serialize to the exact wire
    /// spelling used by fixture 02.
    #[test]
    fn welcome_envelope_matches_fixture_02() {
        let env = Envelope {
            v: 0,
            seq: 0,
            re: Some(0),
            ty: MessageType::Welcome,
            body: Body::Welcome(WelcomeBody {
                protocol_version: 0,
                server: "pan-serve/0.1.0".into(),
                minds: vec![MindKind::Rules, MindKind::Llm],
            }),
        };
        let s = env.to_ndjson().unwrap();
        // Canonical, compact. Match the fixture's field order where stable.
        let expected = r#"{"v":0,"seq":0,"re":0,"type":"welcome","body":{"protocol_version":0,"server":"pan-serve/0.1.0","minds":["rules","llm"]}}"#;
        assert_eq!(s, expected);
    }

    /// Decision body 06 round-trips: every field, including `correlation`
    /// inside the inner `Invoke` (where Some) and absent (where None).
    #[test]
    fn decision_invoke_with_correlation_round_trips() {
        let env = Envelope {
            v: 0,
            seq: 1,
            re: Some(3),
            ty: MessageType::Decision,
            body: Body::Decision(DecisionBody {
                soul_id: "example_pilot".into(),
                goal_id: "conv_00042".into(),
                goal_revision: 1,
                decision: v::Decision {
                    intents: vec![
                        v::ActionIntent::Express {
                            body: "hi".into(),
                        },
                        v::ActionIntent::Invoke {
                            capability: "npc.remember".into(),
                            args: serde_json::json!({"text": "x", "importance": 0.8}),
                            correlation: Some("toolu_01".into()),
                        },
                        v::ActionIntent::Conclude { outcome: v::Outcome::Achieved },
                    ],
                },
            }),
        };
        let s = env.to_ndjson().unwrap();
        let back = Envelope::from_ndjson(&s).unwrap();
        assert_eq!(env, back);
        // `correlation: "toolu_01"` survived; the next test confirms None is *not* serialized.
        assert!(s.contains("\"correlation\":\"toolu_01\""));
    }

    /// When `correlation` is `None`, it must not appear in the wire output —
    /// the schema allows it as an optional field, but writes should be minimal
    /// to match what non-LLM providers actually send.
    #[test]
    fn invoke_without_correlation_omits_the_field() {
        let intent = v::ActionIntent::Invoke {
            capability: "npc.move_to".into(),
            args: serde_json::json!({"room": "cockpit"}),
            correlation: None,
        };
        let s = serde_json::to_string(&intent).unwrap();
        assert!(!s.contains("correlation"),
            "None correlation must not serialize: {s}");
    }

    /// Ack body is the empty object `{}`. Round-trips.
    #[test]
    fn ack_body_is_empty_object() {
        let env = Envelope {
            v: 0, seq: 4, re: Some(2), ty: MessageType::Ack,
            body: Body::Ack(AckBody::default()),
        };
        let s = env.to_ndjson().unwrap();
        assert!(s.ends_with("\"body\":{}}"), "expected `body\":{{}}`, got: {s}");
        let back = Envelope::from_ndjson(&s).unwrap();
        assert_eq!(env, back);
    }

    /// Shutdown body is also `{}`. Round-trips.
    #[test]
    fn shutdown_body_is_empty_object() {
        let env = Envelope {
            v: 0, seq: 8, re: None, ty: MessageType::Shutdown,
            body: Body::Shutdown(ShutdownBody::default()),
        };
        let s = env.to_ndjson().unwrap();
        assert!(s.ends_with("\"body\":{}}"));
        let back = Envelope::from_ndjson(&s).unwrap();
        assert_eq!(env, back);
    }

    /// Error body uses the closed code set; the wire spelling is snake_case.
    #[test]
    fn error_body_round_trips() {
        let env = Envelope {
            v: 0, seq: 3, re: Some(4), ty: MessageType::Error,
            body: Body::Error(ErrorBody {
                code: ErrorCode::UnknownCapability,
                message: "provider requested 'npc.fly_ship' which was never registered".into(),
            }),
        };
        let s = env.to_ndjson().unwrap();
        assert!(s.contains("\"code\":\"unknown_capability\""));
        let back = Envelope::from_ndjson(&s).unwrap();
        assert_eq!(env, back);
    }

    /// `from_wire` is the exact inverse of `as_str` over the closed set of
    /// 10 types, and rejects anything else.
    #[test]
    fn message_type_from_wire_inverts_as_str() {
        let all = [
            MessageType::Hello, MessageType::Welcome,
            MessageType::RegisterCapabilities, MessageType::InstantiateSoul,
            MessageType::ReleaseSoul, MessageType::Perceive,
            MessageType::Decision, MessageType::Ack,
            MessageType::Error, MessageType::Shutdown,
        ];
        for ty in all {
            assert_eq!(MessageType::from_wire(ty.as_str()), Some(ty));
        }
        assert_eq!(MessageType::from_wire("frobnicate"), None);
        assert_eq!(MessageType::from_wire("Hello"), None, "wire spelling is snake_case only");
    }

    /// `re` is optional — its absence must serialize as the field being
    /// missing (not `"re": null`), and must round-trip cleanly.
    #[test]
    fn re_omitted_when_none() {
        let env = Envelope {
            v: 0, seq: 5, re: None, ty: MessageType::Perceive,
            body: Body::Perceive(PerceiveBody {
                soul_id: "x".into(),
                goal: v::Goal {
                    id: "g".into(), revision: 0, objective: "o".into(),
                    trigger: v::Trigger::Tick { sequence: 0 },
                },
                context: v::Context::default(),
            }),
        };
        let s = env.to_ndjson().unwrap();
        assert!(!s.contains("\"re\""), "re must be absent when None: {s}");
        let back = Envelope::from_ndjson(&s).unwrap();
        assert_eq!(back.re, None);
    }
}
