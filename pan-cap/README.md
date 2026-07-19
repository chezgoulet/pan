# pan-cap — concrete capability components

The `cap.*` components a stock agent runs. Each implements pan-core's
`CapabilityProvider` (declare capabilities + execute them) and composes into a
[`Toolbox`](../pan-core/src/toolbox.rs) that becomes **both** the pipeline's
capability registry (for `resolve`/`validate`) and its executor (for `execute`).

This is the layer that lets an assembled agent actually *do* things:

- the **governor** decides *whether* a persona may reach `cap.fs`;
- these components are *what runs* when it may — with their own defense in depth.

## Components

- **`cap.state`** — an in-memory key/value store. `cap.state.set` / `cap.state.get`.
  No external dependency; the honest baseline for exercising the stack.
- **`cap.fs`** — rooted filesystem access. `cap.fs.read` / `cap.fs.write` /
  `cap.fs.list`, all confined to a root directory. Even a persona *granted*
  `cap.fs` cannot escape: absolute paths and `..` traversal are refused at the
  executor, independent of the governor.
- **`cap.shell`** — run a program. `cap.shell.run` executes a program *directly*
  (no shell, so no metacharacter interpretation or injection; `args` is an
  explicit list) and returns exit code + stdout + stderr. Powerful and opt-in
  twice (enable + grant); an arg-level program allowlist is a future governor
  concern.

## The composed stack

```rust
use pan_core::toolbox::Toolbox;
use pan_cap::{FsCaps, StateCaps};

let toolbox = Toolbox::new()
    .with(Box::new(StateCaps::new())).unwrap()
    .with(Box::new(FsCaps::new(agent_root))).unwrap();

let registry = toolbox.registry();          // -> pipeline's CapabilityRegistry
let pipeline = Pipeline { registry: &registry, governor, executor: &toolbox, events };
```

The end-to-end tests drive the whole thing: a behavior-tree provider decides to
invoke `cap.fs.write`, one loop span runs it through `resolve → validate → govern
→ execute`, and a real file appears — while an ungranted origin is denied at
`govern` and the file is left untouched.

## Run it

```sh
cargo test -p pan-cap
```
