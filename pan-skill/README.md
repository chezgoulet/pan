# pan-skill — the Python skill runtime

A **skill** is a plain Python program. It reaches the world only by asking the
host to invoke capabilities on its behalf, and every such request runs through
pan-core's governed pipeline (`resolve → validate → govern → execute`) under the
scope the skill was granted. This crate is the bridge.

This is the full resolution of the *"a skill is not an `Executor`"* point in
[ADR 0001](../docs/decisions/0001-scope-invoker-components.md) (D2): a skill
emitting `cap.invoke(...)` is a **governed invoker driven across a process
boundary**, not a leaf effector. The transport is thin; the guarantee lives in
Rust, in the pipeline the invoker routes through.

## How it works

```
 python3 skill.py                          SkillRunner (Rust, async)
 ─────────────────                         ─────────────────────────
 import pan
 x = pan.invoke("cap.fs.read", {…})  ──▶   ScopedInvoker::invoke  ──▶  resolve
                                                                        validate
                                           {"type":"result", …}  ◀──    govern  (scope!)
 pan.done({…})                       ──▶   run() returns Value          execute
```

- **Protocol** — newline-delimited JSON. Skill→host on stdout
  (`invoke` / `return`); host→skill on stdin (`result`). Input is passed once via
  `PAN_SKILL_INPUT`, so the stdin/stdout channel is purely the invoke↔result
  conversation. stderr is out-of-band diagnostics (captured for tracebacks).
- **The client** — `src/pan.py` is embedded (`PAN_PY`) and materialized into the
  skill's `PYTHONPATH`. Skills `import pan` and call `pan.invoke`, `pan.done`,
  `pan.input`, `pan.log`.
- **Async + cancellable** — a skill blocked on an invoke is a suspended future,
  not a blocked thread; the child is `kill_on_drop`, so abandoning the run future
  (e.g. a superseded decision) tears the subprocess down.

## The guarantee (and the honest limit)

**Guaranteed:** the subprocess is handed no Pan capability object, so every
*sanctioned* effect flows through the governor. The end-to-end tests prove an
out-of-scope invoke surfaces as `PanDenied` *inside the Python process* —
governance crosses the boundary.

**Not yet enforced:** OS-level denial of *ambient* fs/network (a skill that calls
`open()` directly still hits the real OS). That hardening — namespaces / seccomp,
or a launcher like `bwrap` / `nsjail` — plugs into `SkillRunner::with_program`.
The runner does not fake it.

## Run it

```sh
cargo test -p pan-skill      # spawns real python3 skills; skips if absent
```
