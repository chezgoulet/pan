// EXPECT: E0412 (cannot find type `QueryHandle`)
// The concrete handle type is private, so the writer cannot be recovered. Must not compile.
use pan_core::handles::*;
fn main() {
    let store = MemoryStore::new();
    let handle = store.grant_query();
    let _qh: QueryHandle = handle;
}
