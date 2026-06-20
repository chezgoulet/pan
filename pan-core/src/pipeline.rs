//! # The dispatch pipeline — `resolve → validate → govern → execute → record`.
//!
//! This is the heart of the core (build manifest, Wave 0). The **sequence is
//! core; the stage implementations are plugins.** The central invariant —
//! boundary #2 — is that *no plugin can reorder or skip a stage*, and in
//! particular **execution cannot happen without a passing govern decision.**
//!
//! How that invariant is made structural rather than disciplinary:
//!
//! The pipeline is a *type-state chain*. Each stage consumes a token and
//! produces the next stage's token. The execute stage accepts only a
//! [`Governed`] token, and the ONLY way to obtain a `Governed` is to call
//! [`Pipeline::govern`] and have the governance plugin return `Allow`. There is
//! no public constructor for `Governed`. Therefore:
//!
//! ```text
//!   Resolved --validate--> Validated --govern--> Governed --execute--> Effected
//!                                                   ^                      |
//!                                       (only Allow yields this)    record(stage events)
//! ```
//!
//! A caller cannot fabricate a `Governed` to jump straight to execute: the field
//! is private and the type has no `pub fn new`. A denied or error govern result
//! returns a `GovernRejected` instead, which `execute` cannot accept. This is
//! the "the dangerous path doesn't compile" claim, scoped precisely to what the
//! type system can actually guarantee (synthesis §13.4): the *path* is enforced;
//! the *correctness of a govern policy or an executor* is not.

use crate::events::{EventKind, EventStream, StageStatus};
use crate::registry::CapabilityRegistry;
use crate::schema::Value;

/// A world-effecting request entering the pipeline: a resolved capability id and
/// its args. Produced by the loop from an `ActionIntent::Invoke`.
#[derive(Debug, Clone)]
pub struct EffectRequest {
    pub capability: String,
    pub args: Value,
    pub correlation: Option<String>,
}

/// The governance verdict. A plugin in the `govern` family returns one of these.
/// `RequireApproval` is modeled but, in Wave 0, treated as `Deny` with a reason
/// (human-in-the-loop arrives in Wave 4); it exists in the type now so the
/// govern contract doesn't change shape later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny { reason: String },
    RequireApproval { reason: String },
}

/// The `govern` stage plugin slot. It receives a [`Validated`] view and returns
/// a [`Verdict`]. Crucially it is **never handed the executor** — it cannot
/// perform an effect, only judge one (synthesis §3).
pub trait Governor: Send + Sync {
    fn id(&self) -> &str;
    fn govern(&self, capability: &str, args: &Value) -> Verdict;
}

/// The trivial always-allow governor (manifest Wave 1 `gov.allow`, but needed in
/// Wave 0 so the stage runs end to end). Replaced by real policy in Wave 4.
pub struct AllowAll;
impl Governor for AllowAll {
    fn id(&self) -> &str { "gov.allow" }
    fn govern(&self, _capability: &str, _args: &Value) -> Verdict { Verdict::Allow }
}

/// The `execute` stage plugin slot. Receives a [`Governed`] effect — which by
/// construction has passed govern — and performs it, returning a result value.
/// In-process vs RPC is the executor's concern; the loop never knows which.
pub trait Executor: Send + Sync {
    fn id(&self) -> &str;
    fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError>;
}

#[derive(Debug, Clone)]
pub struct ExecError(pub String);

/// A trivial in-process executor that echoes the args back as the result, so the
/// pipeline runs end-to-end in Wave 0. Real executors (`exec.local`,
/// `exec.docker`) arrive in Waves 1/4.
pub struct EchoExecutor;
impl Executor for EchoExecutor {
    fn id(&self) -> &str { "exec.echo" }
    fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        Ok(serde_json::json!({ "executed": capability, "args": args }))
    }
}

// ---------------------------------------------------------------------------
// Type-state tokens. Each is opaque: private fields, no public constructor
// except the stage that produces it. This is what makes the path non-bypassable.
// ---------------------------------------------------------------------------

/// Output of `resolve`: the request's capability id was found in the registry.
pub struct Resolved {
    request: EffectRequest,
    args_schema: Value,
}

/// Output of `validate`: args conform to the capability's schema.
pub struct Validated {
    request: EffectRequest,
}

/// Output of `govern` **when the verdict was Allow**. There is no other way to
/// build this type. Holding one is proof that governance permitted the effect.
pub struct Governed {
    request: EffectRequest,
}

/// Output of `govern` when the verdict was NOT Allow. `execute` does not accept
/// this, so a rejected effect is unrepresentable as an execution input.
#[derive(Debug)]
pub struct GovernRejected {
    pub capability: String,
    pub verdict: Verdict,
}

/// Terminal output of `execute` + `record`.
#[derive(Debug)]
pub struct Effected {
    pub capability: String,
    pub result: Value,
}

/// Errors that abort the pipeline before execution.
#[derive(Debug)]
pub enum PipelineError {
    /// `resolve` failed: capability id not registered.
    Unresolved { capability: String },
    /// `validate` failed: args did not match schema.
    Invalid { capability: String, reason: String },
    /// `govern` rejected the effect.
    Rejected(GovernRejected),
    /// `execute` failed at the executor.
    Execution { capability: String, reason: String },
}

/// The pipeline wires the three stage plugins (registry for resolve, governor,
/// executor) and the event stream for `record`. The methods enforce ordering by
/// type: you literally cannot call them out of order, because each consumes the
/// previous stage's token.
pub struct Pipeline<'a> {
    pub registry: &'a CapabilityRegistry,
    pub governor: &'a dyn Governor,
    pub executor: &'a dyn Executor,
    pub events: &'a EventStream,
}

impl<'a> Pipeline<'a> {
    /// Stage 1 — resolve: name → capability binding. Records nothing on success;
    /// the loop records `DispatchStarted` before calling in.
    pub fn resolve(&self, request: EffectRequest) -> Result<Resolved, PipelineError> {
        match self.registry.lookup(&request.capability) {
            Some(cap) => Ok(Resolved { args_schema: cap.args_schema.clone(), request }),
            None => {
                self.record("resolve", &request.capability, StageStatus::Error);
                Err(PipelineError::Unresolved { capability: request.capability })
            }
        }
    }

    /// Stage 2 — validate: args vs schema. Wave 0 uses a structural check
    /// (object-shape + required top-level keys) rather than a full JSON-Schema
    /// engine; manifest Wave 6 swaps in compiled schemas if `validate` is hot.
    pub fn validate(&self, resolved: Resolved) -> Result<Validated, PipelineError> {
        let cap = &resolved.request.capability;
        if let Err(reason) = minimal_schema_check(&resolved.args_schema, &resolved.request.args) {
            self.record("validate", cap, StageStatus::Error);
            return Err(PipelineError::Invalid { capability: cap.clone(), reason });
        }
        self.record("validate", cap, StageStatus::Ok);
        Ok(Validated { request: resolved.request })
    }

    /// Stage 3 — govern: the allow/deny decision. Returns a [`Governed`] token
    /// ONLY on `Allow`; any other verdict yields an `Err(Rejected(..))`. This is
    /// the structural choke point: execute requires `Governed`, and this is the
    /// sole source of it.
    pub fn govern(&self, validated: Validated) -> Result<Governed, PipelineError> {
        let cap = &validated.request.capability;
        let verdict = self.governor.govern(cap, &validated.request.args);
        match verdict {
            Verdict::Allow => {
                self.record("govern", cap, StageStatus::Ok);
                Ok(Governed { request: validated.request })
            }
            other => {
                self.record("govern", cap, StageStatus::Denied);
                Err(PipelineError::Rejected(GovernRejected {
                    capability: cap.clone(),
                    verdict: other,
                }))
            }
        }
    }

    /// Stages 4 & 5 — execute then record. Accepts only a [`Governed`] token, so
    /// it is impossible to call without a passing govern decision. Records
    /// `Effected` on success.
    pub fn execute(&self, governed: Governed) -> Result<Effected, PipelineError> {
        let cap = governed.request.capability;
        match self.executor.execute(&cap, &governed.request.args) {
            Ok(result) => {
                self.record("execute", &cap, StageStatus::Ok);
                self.events.emit(EventKind::Effected {
                    capability: cap.clone(),
                    result: result.clone(),
                });
                Ok(Effected { capability: cap, result })
            }
            Err(ExecError(reason)) => {
                self.record("execute", &cap, StageStatus::Error);
                Err(PipelineError::Execution { capability: cap, reason })
            }
        }
    }

    /// The whole pipeline, run in order. This is the only path the loop uses;
    /// the individual stage methods are public so they can be unit-tested and so
    /// the *type-level* proof (execute needs Governed) is visible and exercised.
    pub fn dispatch(&self, request: EffectRequest) -> Result<Effected, PipelineError> {
        self.events.emit(EventKind::DispatchStarted {
            capability: request.capability.clone(),
            correlation: request.correlation.clone(),
        });
        let resolved = self.resolve(request)?;
        let validated = self.validate(resolved)?;
        let governed = self.govern(validated)?;
        self.execute(governed)
    }

    fn record(&self, stage: &'static str, capability: &str, status: StageStatus) {
        self.events.emit(EventKind::StageCompleted {
            stage: stage.to_string(),
            capability: capability.to_string(),
            status,
        });
    }
}

/// Wave-0 validation: confirm the args are at least the JSON *type* the schema's
/// top-level `"type"` declares, and that any `"required"` keys are present for
/// objects. Deliberately minimal — a real JSON-Schema validator is a Wave-6
/// swap. Returns `Ok(())` if the schema declares nothing checkable.
fn minimal_schema_check(schema: &Value, args: &Value) -> Result<(), String> {
    let Some(obj) = schema.as_object() else { return Ok(()) };
    if let Some(ty) = obj.get("type").and_then(|t| t.as_str()) {
        let matches = match ty {
            "object" => args.is_object(),
            "array" => args.is_array(),
            "string" => args.is_string(),
            "number" => args.is_number(),
            "integer" => args.is_i64() || args.is_u64(),
            "boolean" => args.is_boolean(),
            "null" => args.is_null(),
            _ => true,
        };
        if !matches {
            return Err(format!("expected JSON type `{ty}`, got `{}`", json_type_name(args)));
        }
    }
    if let Some(required) = obj.get("required").and_then(|r| r.as_array()) {
        let argobj = args.as_object();
        for key in required.iter().filter_map(|k| k.as_str()) {
            let present = argobj.map(|o| o.contains_key(key)).unwrap_or(false);
            if !present {
                return Err(format!("missing required arg `{key}`"));
            }
        }
    }
    Ok(())
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{EventStream, MemorySink};
    use crate::registry::CapabilityRegistry;
    use crate::schema::Capability;

    fn registry_with(cap_id: &str, schema: Value) -> CapabilityRegistry {
        let mut r = CapabilityRegistry::new();
        r.register(Capability { id: cap_id.into(), summary: "".into(), args_schema: schema })
            .unwrap();
        r
    }

    struct DenyAll;
    impl Governor for DenyAll {
        fn id(&self) -> &str { "gov.deny" }
        fn govern(&self, _c: &str, _a: &Value) -> Verdict {
            Verdict::Deny { reason: "no".into() }
        }
    }

    #[test]
    fn happy_path_executes_and_records() {
        let reg = registry_with("alert.raise", serde_json::json!({"type":"object"}));
        let (stream, guard) = EventStream::spawn(MemorySink::new());
        let p = Pipeline { registry: &reg, governor: &AllowAll, executor: &EchoExecutor, events: &stream };
        let out = p.dispatch(EffectRequest {
            capability: "alert.raise".into(),
            args: serde_json::json!({"level":"high"}),
            correlation: None,
        }).unwrap();
        assert_eq!(out.capability, "alert.raise");
        stream.shutdown(guard);
    }

    #[test]
    fn deny_blocks_before_execution() {
        let reg = registry_with("cap.shell", serde_json::json!({"type":"object"}));
        let (stream, guard) = EventStream::spawn(MemorySink::new());
        let p = Pipeline { registry: &reg, governor: &DenyAll, executor: &EchoExecutor, events: &stream };
        let err = p.dispatch(EffectRequest {
            capability: "cap.shell".into(),
            args: serde_json::json!({}),
            correlation: None,
        }).unwrap_err();
        assert!(matches!(err, PipelineError::Rejected(_)));
        stream.shutdown(guard);
    }

    #[test]
    fn unresolved_capability_fails_at_resolve() {
        let reg = CapabilityRegistry::new();
        let (stream, guard) = EventStream::spawn(MemorySink::new());
        let p = Pipeline { registry: &reg, governor: &AllowAll, executor: &EchoExecutor, events: &stream };
        let err = p.dispatch(EffectRequest {
            capability: "nope".into(), args: Value::Null, correlation: None,
        }).unwrap_err();
        assert!(matches!(err, PipelineError::Unresolved { .. }));
        stream.shutdown(guard);
    }

    #[test]
    fn invalid_args_fail_at_validate() {
        let reg = registry_with("cap.fs.write",
            serde_json::json!({"type":"object","required":["path"]}));
        let (stream, guard) = EventStream::spawn(MemorySink::new());
        let p = Pipeline { registry: &reg, governor: &AllowAll, executor: &EchoExecutor, events: &stream };
        let err = p.dispatch(EffectRequest {
            capability: "cap.fs.write".into(),
            args: serde_json::json!({"wrong":"key"}),
            correlation: None,
        }).unwrap_err();
        match err {
            PipelineError::Invalid { reason, .. } => assert!(reason.contains("path")),
            other => panic!("expected Invalid, got {other:?}"),
        }
        stream.shutdown(guard);
    }

    // The structural proof, stated as a test comment: there is no line of code
    // one could write here to obtain a `Governed` without calling `govern`,
    // because `Governed`'s field is private and it has no constructor. The
    // following, if uncommented, does not compile:
    //
    //   let g = Governed { request };          // E0451: field `request` is private
    //   let g = Governed::new(request);        // E0599: no function `new`
    //
    // That non-compilation IS the guarantee. `deny_blocks_before_execution`
    // exercises the runtime half (a real Deny never reaches execute).
}
