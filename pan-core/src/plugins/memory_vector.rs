//! # `memory.vector` — the durable facts layer (Wave 3).
//!
//! This is the Ragamuffin slot. It provides a searchable store for durable facts,
//! intended to be used by `context.memory` to inject relevant knowledge into 
//! provider decisions.
//!
//! Implementation:
//! - Dev/Test: Simple in-memory store using substring matching.
//! - Production: Implemented via a Ragamuffin client adapter (deferred).
//!
//! The core contract for retrieval is defined in [`crate::handles::MemoryQuery`].
//! This plugin is the resource OWNER ([`crate::handles::MemoryStore`]), meaning
//! it is the only part of the system that can write to the store.

use crate::handles::{Fact, MemoryQuery, Query};
use crate::registry::Plugin;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// The actual vector store implementation.
/// For Wave 3, we start with a simple in-memory store.
#[derive(Default)]
pub struct VectorMemory {
    /// Map of Fact ID -> Fact.
    /// In a real vector DB, this would be an index of embeddings.
    store: Arc<RwLock<HashMap<String, Fact>>>,
}

impl VectorMemory {
    pub fn new() -> Self {
        Self::default()
    }

    /// Owner-only write: add or update a fact.
    pub fn remember(&self, fact: Fact) {
        let mut lock = self.store.write().unwrap();
        lock.insert(fact.key.clone(), fact);
    }

    /// Mint a read-only handle for context plugins.
    pub fn grant_query(&self) -> Arc<dyn MemoryQuery> {
        Arc::new(QueryHandle { store: Arc::clone(&self.store) })
    }
}

impl Plugin for VectorMemory {
    fn id(&self) -> &str {
        "memory.vector"
    }
}

/// The read-only handle granted to context plugins.
struct QueryHandle {
    store: Arc<RwLock<HashMap<String, Fact>>>,
}

impl MemoryQuery for QueryHandle {
    fn retrieve(&self, q: &Query) -> Vec<Fact> {
        let lock = self.store.read().unwrap();
        lock.values()
            .filter(|f| f.body.contains(&q.needle) || f.key.contains(&q.needle))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve() {
        let mem = VectorMemory::new();
        let fact = Fact { key: "user_name".into(), body: "The user is Christopher".into() };
        mem.remember(fact.clone());
        
        let handle = mem.grant_query();
        let hits = handle.retrieve(&Query { needle: "Christopher".into() });
        
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "user_name");
        assert_eq!(hits[0].body, "The user is Christopher");
    }

    #[test]
    fn retrieve_empty_on_no_match() {
        let mem = VectorMemory::new();
        let handle = mem.grant_query();
        let hits = handle.retrieve(&Query { needle: "nonexistent".into() });
        assert!(hits.is_empty());
    }
}
