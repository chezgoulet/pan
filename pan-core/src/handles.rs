//! # Capability handles — scoped, typed grants (synthesis §4, §12).
//!
//! The plugin model replaces a shared mutable god-state with **capability
//! handles granted at wiring time**. A context plugin that needs to read memory
//! receives a read-only handle; it "cannot write memory and holds no reference
//! to the memory plugin itself."
//!
//! The manifest flags this as *the one piece needing real code to confirm
//! ergonomics*: build the smallest version that injects one read-only handle and
//! **refuses at compile time to let it write.** That is exactly what this module
//! demonstrates, with `MemoryQuery` as the worked example.
//!
//! The enforcement mechanism: the resource owner ([`MemoryStore`]) holds the
//! only mutating methods. What it hands out is a [`MemoryQuery`] — a trait object
//! whose surface is read-only. A holder of `MemoryQuery` has no method to write
//! and no way to recover the underlying store, so a write is not "discouraged",
//! it is *unspeakable* — there is no syntax for it. The "does not compile"
//! assertions are documented in the tests.

use std::sync::Arc;

/// A retrieved fact. Opaque to the core; the memory family defines meaning.
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub key: String,
    pub body: String,
}

/// A retrieval query. Minimal in Wave 0 (substring match); a real client
/// (`memory.vector.ragamuffin`) implements semantic retrieval behind the same
/// trait.
#[derive(Debug, Clone)]
pub struct Query {
    pub needle: String,
}

/// The **read-only** handle granted to context-family plugins. This is the only
/// surface a grantee sees. There is deliberately no `store`, `write`, `insert`,
/// or `inner` method — the trait simply does not expose mutation, so a grantee
/// cannot write no matter how it is implemented.
pub trait MemoryQuery: Send + Sync {
    fn retrieve(&self, q: &Query) -> Vec<Fact>;
}

/// The resource OWNER. It holds the mutating methods (`remember`). It is held by
/// the memory family alone and is never handed to other families. What it grants
/// out is an `Arc<dyn MemoryQuery>` — read-only by type.
#[derive(Default)]
pub struct MemoryStore {
    facts: Arc<std::sync::RwLock<Vec<Fact>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self { facts: Arc::new(std::sync::RwLock::new(Vec::new())) }
    }

    /// Owner-only write. Note this lives on `MemoryStore`, NOT on `MemoryQuery`.
    /// A plugin that was granted only a `MemoryQuery` cannot reach this method.
    pub fn remember(&self, fact: Fact) {
        self.facts.write().unwrap().push(fact);
    }

    /// Mint a read-only handle to grant to a context plugin at provision time.
    /// The returned trait object shares the same backing storage (so later
    /// writes by the owner are visible) but exposes only `retrieve`.
    pub fn grant_query(&self) -> Arc<dyn MemoryQuery> {
        Arc::new(QueryHandle { facts: Arc::clone(&self.facts) })
    }
}

/// The concrete read-only handle. Private fields; the only way to get one is
/// [`MemoryStore::grant_query`]. Its sole public surface is `MemoryQuery`.
struct QueryHandle {
    facts: Arc<std::sync::RwLock<Vec<Fact>>>,
}

impl MemoryQuery for QueryHandle {
    fn retrieve(&self, q: &Query) -> Vec<Fact> {
        self.facts
            .read()
            .unwrap()
            .iter()
            .filter(|f| f.body.contains(&q.needle) || f.key.contains(&q.needle))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granted_handle_can_read_owner_writes() {
        let store = MemoryStore::new();
        let handle = store.grant_query(); // a context plugin would hold this
        store.remember(Fact { key: "name".into(), body: "the user is Sam".into() });
        let hits = handle.retrieve(&Query { needle: "Sam".into() });
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "name");
    }

    #[test]
    fn handle_sees_writes_made_after_granting() {
        let store = MemoryStore::new();
        let handle = store.grant_query();
        assert!(handle.retrieve(&Query { needle: "later".into() }).is_empty());
        store.remember(Fact { key: "k".into(), body: "added later".into() });
        assert_eq!(handle.retrieve(&Query { needle: "later".into() }).len(), 1);
    }

    // COMPILE-TIME ENFORCEMENT (the actual point of this module).
    //
    // A plugin is given `handle: Arc<dyn MemoryQuery>`. None of the following
    // compiles, which is the guarantee that a read grant cannot write:
    //
    //   handle.remember(fact);   // E0599: no method `remember` on `dyn MemoryQuery`
    //   handle.facts;            // E0609: no field `facts` (it's on QueryHandle, private)
    //   let s: &MemoryStore = &handle;  // E0308: mismatched types
    //
    // There is no downcast path either: `QueryHandle` is private to this module,
    // so a grantee in another crate/module cannot name it to attempt recovery of
    // the writer. The read-only-ness is a property of the type the grantee holds,
    // not of anyone's discipline.
    //
    // To prove the *positive* — that the owner alone can write — is the two tests
    // above; `remember` is only reachable through `MemoryStore`.
    #[test]
    fn writer_is_not_reachable_through_the_query_trait() {
        // This test documents the boundary at runtime by construction: we hold a
        // `dyn MemoryQuery` and the ONLY method available is `retrieve`.
        fn only_reads(q: &dyn MemoryQuery) -> usize {
            q.retrieve(&Query { needle: "".into() }).len()
            // there is no `q.remember(...)` to call here — it does not exist.
        }
        let store = MemoryStore::new();
        store.remember(Fact { key: "a".into(), body: "x".into() });
        let handle = store.grant_query();
        assert_eq!(only_reads(handle.as_ref()), 1);
    }
}
