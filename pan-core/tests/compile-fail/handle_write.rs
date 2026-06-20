// EXPECT: E0599 (no method named `remember`)
// A read-only MemoryQuery grant must expose no write. Must not compile.
use pan_core::handles::*;
fn main() {
    let store = MemoryStore::new();
    let handle = store.grant_query();
    handle.remember(Fact { key: "k".into(), body: "b".into() });
}
