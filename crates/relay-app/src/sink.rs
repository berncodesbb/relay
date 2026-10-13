//! [`StoreFactSink`]: the application's implementation of the executor
//! [`FactSink`] port over [`relay_store::GraphFactStore`].
//!
//! Each append takes the storage writer lock for that append alone — the
//! store's own checked-append protocol — so no lock is ever held across
//! executor work. Reads come from the authoritative JSONL timeline.

use std::sync::Arc;

use relay_domain::{DomainFact, ProjectGraph};
use relay_executor::{FactSink, FactSinkError};
use relay_store::GraphFactStore;

/// A fact sink over an open graph fact store. Cheap to clone via [`Arc`].
pub struct StoreFactSink {
    store: Arc<GraphFactStore>,
}

impl StoreFactSink {
    /// Wrap an open store.
    pub fn new(store: Arc<GraphFactStore>) -> Self {
        StoreFactSink { store }
    }

    /// The underlying store handle.
    pub fn store(&self) -> Arc<GraphFactStore> {
        Arc::clone(&self.store)
    }
}

impl FactSink for StoreFactSink {
    fn append(&self, fact: DomainFact) -> Result<(), FactSinkError> {
        // The store's checked append validates the whole candidate history
        // before writing anything; a refusal is a rejection, not a failure
        // of the sink itself.
        self.store.append(&[fact]).map_err(|error| {
            let message = error.to_string();
            if message.contains("would not fold") || message.contains("stale expected revision") {
                FactSinkError::Rejected(message)
            } else {
                FactSinkError::Failed(message)
            }
        })?;
        Ok(())
    }

    fn facts(&self) -> Result<Vec<DomainFact>, FactSinkError> {
        self.store
            .read_facts()
            .map_err(|error| FactSinkError::Failed(error.to_string()))
    }

    fn graph(&self) -> Result<ProjectGraph, FactSinkError> {
        self.store
            .project_graph()
            .map_err(|error| FactSinkError::Failed(error.to_string()))
    }
}
