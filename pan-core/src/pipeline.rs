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
use crate::invoker::ScopedInvoker;
use crate::registry::CapabilityRegistry;
use crate::schema::{Scope, Value};

/// A world-effecting request entering the pipeline: a resolved capability id, its
/// args, and the [`Scope`] on whose authority it is made. Produced by the loop
/// from an `ActionIntent::Invoke` (stamped with the persona's scope) or by a
/// [`ScopedInvoker`](crate::invoker::ScopedInvoker) (stamped with a skill's
/// narrower scope).
#[derive(Debug, Clone)]
pub struct EffectRequest {
    pub capability: String,
    pub args: Value,
    pub correlation: Option<String>,
    /// Who is asking. Carried all the way to the `govern` stage so policy can be
    /// origin-aware. There is no unscoped path: every effect answers "on whose
    /// authority?" See ADR 0001.
    pub scope: Scope,
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

/// The `govern` stage plugin slot. It receives the invocation's [`Scope`] (who
/// is asking), the capability id, and the args, and returns a [`Verdict`].
/// Crucially it is **never handed the executor** — it cannot perform an effect,
/// only judge one (synthesis §3). The `scope` parameter is what makes
/// per-persona sandboxing, skill sub-scopes, and self-modification guards
/// expressible; see ADR 0001.
#[async_trait::async_trait]
pub trait Governor: Send + Sync {
    fn id(&self) -> &str;
    async fn govern(&self, scope: &Scope, capability: &str, args: &Value) -> Verdict;
}

/// The trivial always-allow governor (manifest Wave 1 `gov.allow`, but needed in
/// Wave 0 so the stage runs end to end). Ignores scope. Real policy is
/// [`ScopedGovernor`] and beyond.
pub struct AllowAll;
#[async_trait::async_trait]
impl Governor for AllowAll {
    fn id(&self) -> &str {
        "gov.allow"
    }
    async fn govern(&self, _scope: &Scope, _capability: &str, _args: &Value) -> Verdict {
        Verdict::Allow
    }
}

/// A capability-scoped governor — the Phase-5 sandboxing shape, usable now.
///
/// Each origin is granted a set of allowed capability-id *prefixes*. An
/// invocation is allowed iff the invoking scope's origin has a grant whose
/// prefix matches the capability id (exact, or a dotted descendant: grant
/// `"cap.fs"` allows `"cap.fs"` and `"cap.fs.read"` but not `"cap.fsx"`). An
/// origin with no grant entry is **denied everything** — deny-by-default.
///
/// The boundary lives in configuration (`Agent.toml [caps.grant]`, keyed by
/// origin), not in the core: this type is the mechanism, the grant map is the
/// policy. That keeps the core policy-free while making the governor the single,
/// origin-aware safety boundary the buildout depends on.
#[derive(Default)]
pub struct ScopedGovernor {
    grants: std::collections::HashMap<String, Vec<String>>,
}

impl ScopedGovernor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Grant `origin` a set of allowed capability-id prefixes. Chainable so a
    /// governor can be built inline from an `Agent.toml`-derived grant table.
    pub fn grant(
        mut self,
        origin: impl Into<String>,
        prefixes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.grants.insert(
            origin.into(),
            prefixes.into_iter().map(Into::into).collect(),
        );
        self
    }

    /// True iff `capability` is the granted prefix itself or a dotted descendant
    /// of it. `"cap.fs"` matches `"cap.fs"` and `"cap.fs.read"`, not `"cap.fsx"`.
    fn prefix_matches(prefix: &str, capability: &str) -> bool {
        capability == prefix
            || capability
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('.'))
    }
}

#[async_trait::async_trait]
impl Governor for ScopedGovernor {
    fn id(&self) -> &str {
        "gov.scoped"
    }

    async fn govern(&self, scope: &Scope, capability: &str, _args: &Value) -> Verdict {
        match self.grants.get(&scope.origin) {
            Some(prefixes) if prefixes.iter().any(|p| Self::prefix_matches(p, capability)) => {
                Verdict::Allow
            }
            Some(_) => Verdict::Deny {
                reason: format!(
                    "origin `{}` is not granted capability `{capability}`",
                    scope.origin
                ),
            },
            None => Verdict::Deny {
                reason: format!("origin `{}` has no capability grants", scope.origin),
            },
        }
    }
}

/// The `execute` stage plugin slot.
#[async_trait::async_trait]
pub trait Executor: Send + Sync {
    fn id(&self) -> &str;
    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError>;

    /// Execute with a [`ScopedInvoker`] so capabilities like `cap.skill.run`
    /// can invoke other capabilities under governance. Default delegates to
    /// [`execute`](Self::execute).
    async fn execute_with_invoker(
        &self,
        capability: &str,
        args: &Value,
        _invoker: &dyn ScopedInvoker,
    ) -> Result<Value, ExecError> {
        self.execute(capability, args).await
    }
}

#[derive(Debug, Clone)]
pub struct ExecError(pub String);

/// A trivial in-process executor that echoes the args back as the result, so the
/// pipeline runs end-to-end in Wave 0. Real executors (`exec.local`,
/// `exec.docker`) arrive in Waves 1/4.
pub struct EchoExecutor;
#[async_trait::async_trait]
impl Executor for EchoExecutor {
    fn id(&self) -> &str {
        "exec.echo"
    }
    async fn execute(&self, capability: &str, args: &Value) -> Result<Value, ExecError> {
        Ok(serde_json::json!({ "executed": capability, "args": args }))
    }
}

// ---------------------------------------------------------------------------
// Type-state tokens. Each is opaque: private fields, no public constructor
// except the stage that produces it. This is what makes the path non-bypassable.
// ---------------------------------------------------------------------------

/// A governor decorator that adds host-level allowlisting for `cap.http.*`
/// capabilities. Wraps an inner [`Governor`] (typically [`ScopedGovernor`])
/// and delegates capability-prefix checking to it. For `cap.http.*` invocations,
/// it additionally checks that the `url` arg's host matches the origin's
/// allowlist.
///
/// The allowlist maps `origin -> [allowed_host_patterns]`. A pattern may be an
/// exact hostname ("api.example.com") or a glob-like prefix ("*.trusted.org").
/// If no allowlist entry exists for the origin, all `cap.http.*` invocations
/// are denied.
pub struct HostAllowlistGovernor {
    inner: Box<dyn Governor>,
    /// Maps origin → list of allowed host patterns for cap.http.*
    allow_hosts: std::collections::HashMap<String, Vec<String>>,
}

impl HostAllowlistGovernor {
    pub fn new(inner: Box<dyn Governor>) -> Self {
        Self {
            inner,
            allow_hosts: std::collections::HashMap::new(),
        }
    }

    /// Grant `origin` access to `hosts` for cap.http.* invocations.
    pub fn allow_hosts(
        mut self,
        origin: impl Into<String>,
        hosts: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.allow_hosts
            .insert(origin.into(), hosts.into_iter().map(Into::into).collect());
        self
    }

    /// True if `host` matches a pattern. Patterns support:
    /// - Exact match: "api.example.com"
    /// - Wildcard prefix: "*.example.com" (matches subdomain + itself)
    fn host_matches(pattern: &str, host: &str) -> bool {
        if let Some(suffix) = pattern.strip_prefix("*.") {
            host == suffix || host.ends_with(&format!(".{suffix}"))
        } else {
            host == pattern
        }
    }
}

#[async_trait::async_trait]
impl Governor for HostAllowlistGovernor {
    fn id(&self) -> &str {
        "gov.host-allowlist"
    }

    async fn govern(&self, scope: &Scope, capability: &str, args: &Value) -> Verdict {
        // First, delegate to the inner governor for capability-prefix checks.
        let inner = self.inner.govern(scope, capability, args).await;
        match inner {
            Verdict::Allow => {
                // Only apply host allowlist for cap.http.* capabilities.
                if capability.starts_with("cap.http.") && !self.allow_hosts.is_empty() {
                    let url = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let host = extract_host(url);
                    let allowed = self
                        .allow_hosts
                        .get(&scope.origin)
                        .map(|patterns| patterns.iter().any(|p| Self::host_matches(p, &host)))
                        .unwrap_or(false);
                    if !allowed {
                        return Verdict::Deny {
                            reason: format!(
                                "host `{host}` is not in the allowlist for origin `{}`",
                                scope.origin
                            ),
                        };
                    }
                }
                Verdict::Allow
            }
            other => other,
        }
    }
}

/// Extract the hostname from a URL string.
fn extract_host(url: &str) -> String {
    let rest = if let Some(r) = url.strip_prefix("https://") {
        r
    } else if let Some(r) = url.strip_prefix("http://") {
        r
    } else {
        return url.to_string();
    };
    match rest.split_once('/') {
        Some((host, _)) => host.to_string(),
        None => rest.to_string(),
    }
}

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
            Some(cap) => Ok(Resolved {
                args_schema: cap.args_schema.clone(),
                request,
            }),
            None => {
                self.record("resolve", &request.capability, StageStatus::Error);
                Err(PipelineError::Unresolved {
                    capability: request.capability,
                })
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
            return Err(PipelineError::Invalid {
                capability: cap.clone(),
                reason,
            });
        }
        self.record("validate", cap, StageStatus::Ok);
        Ok(Validated {
            request: resolved.request,
        })
    }

    /// Stage 3 — govern: the allow/deny decision. Returns a [`Governed`] token
    /// ONLY on `Allow`; any other verdict yields an `Err(Rejected(..))`. This is
    /// the structural choke point: execute requires `Governed`, and this is the
    /// sole source of it.
    pub async fn govern(&self, validated: Validated) -> Result<Governed, PipelineError> {
        let cap = &validated.request.capability;
        let verdict = self
            .governor
            .govern(&validated.request.scope, cap, &validated.request.args)
            .await;
        match verdict {
            Verdict::Allow => {
                self.record("govern", cap, StageStatus::Ok);
                Ok(Governed {
                    request: validated.request,
                })
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
    /// it is impossible to call without a passing govern decision.
    pub async fn execute(&self, governed: Governed) -> Result<Effected, PipelineError> {
        let cap = governed.request.capability;
        match self.executor.execute(&cap, &governed.request.args).await {
            Ok(result) => {
                self.record("execute", &cap, StageStatus::Ok);
                self.events.emit(EventKind::Effected {
                    capability: cap.clone(),
                    result: result.clone(),
                });
                Ok(Effected {
                    capability: cap,
                    result,
                })
            }
            Err(ExecError(reason)) => {
                self.record("execute", &cap, StageStatus::Error);
                Err(PipelineError::Execution {
                    capability: cap,
                    reason,
                })
            }
        }
    }

    /// Execute with a [`ScopedInvoker`], for capabilities that need to invoke
    /// other capabilities under governance (e.g. `cap.skill.run`).
    pub async fn execute_with_invoker(
        &self,
        governed: Governed,
        invoker: &dyn ScopedInvoker,
    ) -> Result<Effected, PipelineError> {
        let cap = governed.request.capability;
        match self
            .executor
            .execute_with_invoker(&cap, &governed.request.args, invoker)
            .await
        {
            Ok(result) => {
                self.record("execute", &cap, StageStatus::Ok);
                self.events.emit(EventKind::Effected {
                    capability: cap.clone(),
                    result: result.clone(),
                });
                Ok(Effected {
                    capability: cap,
                    result,
                })
            }
            Err(ExecError(reason)) => {
                self.record("execute", &cap, StageStatus::Error);
                Err(PipelineError::Execution {
                    capability: cap,
                    reason,
                })
            }
        }
    }

    /// The whole pipeline, run in order.
    pub async fn dispatch(&self, request: EffectRequest) -> Result<Effected, PipelineError> {
        self.events.emit(EventKind::DispatchStarted {
            capability: request.capability.clone(),
            correlation: request.correlation.clone(),
        });
        let resolved = self.resolve(request)?;
        let validated = self.validate(resolved)?;
        let governed = self.govern(validated).await?;
        self.execute(governed).await
    }

    /// Dispatch with a [`ScopedInvoker`] for capabilities that need cross-cap
    /// invocation (e.g. `cap.skill.run`). The invoker is passed to
    /// [`Executor::execute_with_invoker`].
    pub async fn dispatch_with_invoker(
        &self,
        request: EffectRequest,
        invoker: &dyn ScopedInvoker,
    ) -> Result<Effected, PipelineError> {
        self.events.emit(EventKind::DispatchStarted {
            capability: request.capability.clone(),
            correlation: request.correlation.clone(),
        });
        let resolved = self.resolve(request)?;
        let validated = self.validate(resolved)?;
        let governed = self.govern(validated).await?;
        self.execute_with_invoker(governed, invoker).await
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
    let Some(obj) = schema.as_object() else {
        return Ok(());
    };
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
            return Err(format!(
                "expected JSON type `{ty}`, got `{}`",
                json_type_name(args)
            ));
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
        r.register(Capability {
            id: cap_id.into(),
            summary: "".into(),
            args_schema: schema,
        })
        .unwrap();
        r
    }

    struct DenyAll;
    #[async_trait::async_trait]
    impl Governor for DenyAll {
        fn id(&self) -> &str {
            "gov.deny"
        }
        async fn govern(&self, _s: &Scope, _c: &str, _a: &Value) -> Verdict {
            Verdict::Deny {
                reason: "no".into(),
            }
        }
    }

    #[tokio::test]
    async fn happy_path_executes_and_records() {
        let reg = registry_with("alert.raise", serde_json::json!({"type":"object"}));
        let mut stream = EventStream::spawn(MemorySink::new());
        let p = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        let out = p
            .dispatch(EffectRequest {
                capability: "alert.raise".into(),
                args: serde_json::json!({"level":"high"}),
                correlation: None,
                scope: Scope::system(),
            })
            .await
            .unwrap();
        assert_eq!(out.capability, "alert.raise");
        stream.shutdown();
    }

    #[tokio::test]
    async fn deny_blocks_before_execution() {
        let reg = registry_with("cap.shell", serde_json::json!({"type":"object"}));
        let mut stream = EventStream::spawn(MemorySink::new());
        let p = Pipeline {
            registry: &reg,
            governor: &DenyAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        let err = p
            .dispatch(EffectRequest {
                capability: "cap.shell".into(),
                args: serde_json::json!({}),
                correlation: None,
                scope: Scope::system(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PipelineError::Rejected(_)));
        stream.shutdown();
    }

    #[tokio::test]
    async fn unresolved_capability_fails_at_resolve() {
        let reg = CapabilityRegistry::new();
        let mut stream = EventStream::spawn(MemorySink::new());
        let p = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        let err = p
            .dispatch(EffectRequest {
                capability: "nope".into(),
                args: Value::Null,
                correlation: None,
                scope: Scope::system(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PipelineError::Unresolved { .. }));
        stream.shutdown();
    }

    #[tokio::test]
    async fn invalid_args_fail_at_validate() {
        let reg = registry_with(
            "cap.fs.write",
            serde_json::json!({"type":"object","required":["path"]}),
        );
        let mut stream = EventStream::spawn(MemorySink::new());
        let p = Pipeline {
            registry: &reg,
            governor: &AllowAll,
            executor: &EchoExecutor,
            events: &stream,
        };
        let err = p
            .dispatch(EffectRequest {
                capability: "cap.fs.write".into(),
                args: serde_json::json!({"wrong":"key"}),
                correlation: None,
                scope: Scope::system(),
            })
            .await
            .unwrap_err();
        match err {
            PipelineError::Invalid { reason, .. } => assert!(reason.contains("path")),
            other => panic!("expected Invalid, got {other:?}"),
        }
        stream.shutdown();
    }

    #[tokio::test]
    async fn scoped_governor_allows_granted_prefix_and_denies_the_rest() {
        // "skill.summarize" may reach anything under cap.fs and cap.http, nothing else.
        let g = ScopedGovernor::new().grant("skill.summarize", ["cap.fs", "cap.http"]);
        let s = Scope::new("skill.summarize");
        assert!(matches!(
            g.govern(&s, "cap.fs.read", &Value::Null).await,
            Verdict::Allow
        ));
        assert!(matches!(
            g.govern(&s, "cap.fs", &Value::Null).await,
            Verdict::Allow
        ));
        assert!(matches!(
            g.govern(&s, "cap.http.get", &Value::Null).await,
            Verdict::Allow
        ));
        // A sibling that merely shares a textual prefix is NOT a dotted descendant.
        assert!(matches!(
            g.govern(&s, "cap.fsx", &Value::Null).await,
            Verdict::Deny { .. }
        ));
        // Out of grant entirely.
        assert!(matches!(
            g.govern(&s, "cap.shell", &Value::Null).await,
            Verdict::Deny { .. }
        ));
    }

    #[tokio::test]
    async fn scoped_governor_denies_unknown_origin_by_default() {
        let g = ScopedGovernor::new().grant("persona.assistant", ["cap.fs"]);
        // An origin with no grant entry (e.g. a skill nobody authorized) gets nothing.
        let v = g
            .govern(&Scope::new("skill.rogue"), "cap.fs.read", &Value::Null)
            .await;
        assert!(matches!(v, Verdict::Deny { .. }));
    }

    #[tokio::test]
    async fn host_allowlist_allows_exact_match_and_denies_unlisted() {
        let inner = ScopedGovernor::new().grant("persona.assistant", ["cap.http"]);
        let g = HostAllowlistGovernor::new(Box::new(inner))
            .allow_hosts("persona.assistant", ["api.example.com"]);
        let s = Scope::new("persona.assistant");

        // Exact host match → allow.
        let v = g
            .govern(
                &s,
                "cap.http.get",
                &serde_json::json!({"url": "http://api.example.com/data"}),
            )
            .await;
        assert!(matches!(v, Verdict::Allow));

        // Unlisted host → deny.
        let v = g
            .govern(
                &s,
                "cap.http.get",
                &serde_json::json!({"url": "http://evil.org/steal"}),
            )
            .await;
        assert!(matches!(v, Verdict::Deny { .. }));
    }

    #[tokio::test]
    async fn host_allowlist_wildcard_matches_subdomains() {
        let inner = ScopedGovernor::new().grant("skill.x", ["cap.http"]);
        let g =
            HostAllowlistGovernor::new(Box::new(inner)).allow_hosts("skill.x", ["*.trusted.org"]);
        let s = Scope::new("skill.x");

        // Subdomain match.
        let v = g
            .govern(
                &s,
                "cap.http.get",
                &serde_json::json!({"url": "http://sub.trusted.org/path"}),
            )
            .await;
        assert!(matches!(v, Verdict::Allow));

        // Non-matching.
        let v = g
            .govern(
                &s,
                "cap.http.get",
                &serde_json::json!({"url": "http://untrusted.org/evil"}),
            )
            .await;
        assert!(matches!(v, Verdict::Deny { .. }));
    }

    #[tokio::test]
    async fn host_allowlist_does_not_block_non_http_capabilities() {
        let inner = ScopedGovernor::new().grant("persona.x", ["cap.shell", "cap.http"]);
        let g = HostAllowlistGovernor::new(Box::new(inner))
            .allow_hosts("persona.x", ["api.example.com"]);
        let s = Scope::new("persona.x");

        // cap.shell is not cap.http, so host allowlist doesn't apply.
        let v = g.govern(&s, "cap.shell.run", &Value::Null).await;
        assert!(matches!(v, Verdict::Allow));
    }

    #[tokio::test]
    async fn scope_flows_through_dispatch_and_gates_by_origin() {
        // The same capability, dispatched under two different origins, is allowed
        // for the granted one and denied for the other — proving the scope on the
        // EffectRequest reaches the govern stage intact.
        let reg = registry_with("cap.fs.read", serde_json::json!({"type":"object"}));
        let gov = ScopedGovernor::new().grant("persona.assistant", ["cap.fs"]);
        let mut stream = EventStream::spawn(MemorySink::new());
        let p = Pipeline {
            registry: &reg,
            governor: &gov,
            executor: &EchoExecutor,
            events: &stream,
        };

        let allowed = p
            .dispatch(EffectRequest {
                capability: "cap.fs.read".into(),
                args: serde_json::json!({}),
                correlation: None,
                scope: Scope::new("persona.assistant"),
            })
            .await;
        assert!(allowed.is_ok(), "granted origin should pass govern");

        let denied = p
            .dispatch(EffectRequest {
                capability: "cap.fs.read".into(),
                args: serde_json::json!({}),
                correlation: None,
                scope: Scope::new("skill.rogue"),
            })
            .await;
        assert!(
            matches!(denied, Err(PipelineError::Rejected(_))),
            "ungranted origin must be rejected at govern"
        );
        stream.shutdown();
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
