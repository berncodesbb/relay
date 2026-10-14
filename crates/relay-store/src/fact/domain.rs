//! Domain fact wiring: the typed seam between the generic fact layer and
//! `relay-domain`'s frozen fact contract.
//!
//! Three pieces:
//!
//! - [`to_input`]/[`from_record`] convert a [`DomainFact`] to and from the
//!   opaque [`FactInput`]/[`FactRecord`] envelope, delegating to the domain
//!   catalog (`fact_to_parts`/`fact_from_parts`). The payload is the fact's
//!   serde form with the `type` tag removed; the envelope `type` is the
//!   fact's stable wire name, and reassembly re-injects the tag, so the
//!   pair round-trips exactly.
//! - [`DomainFactValidator`] is the production [`FactValidator`]: unknown
//!   kinds and newer fact versions fail closed with upgrade guidance, and
//!   every accepted record must reassemble through `fact_from_parts`. It
//!   is the only validator the [`GraphFactStore`] uses —
//!   [`AcceptAllFacts`](crate::fact::AcceptAllFacts) is unreachable from
//!   these production types.
//! - [`GraphFactStore`] appends `&[DomainFact]`, reads back the typed
//!   timeline, folds the graph projection, and tracks the project revision
//!   (the fact count, matching
//!   [`ProjectRevision::from_count`](relay_domain::ProjectRevision)).

use relay_domain::DomainFact;
use relay_domain::ProjectGraph;
use relay_domain::ProjectId;
use relay_domain::ProjectRevision;
use relay_domain::current_fact_version;
use relay_domain::fact_from_parts;
use relay_domain::fact_to_parts;

use crate::canonical::Json;
use crate::error::Result;
use crate::error::StoreError;
use crate::fact::record::FactEntry;
use crate::fact::record::FactInput;
use crate::fact::record::FactRecord;
use crate::fact::record::Integrity;
use crate::fact::store::FactOpenReport;
use crate::fact::store::FactRebuildReport;
use crate::fact::store::FactStore;
use crate::fact::store::FactStoreOptions;
use crate::fact::validator::FactValidator;

/// Convert a typed fact into a proposed input: the wire name becomes the
/// envelope type, the untagged serde form becomes the payload, and the
/// version comes from the domain catalog.
pub fn to_input(fact: &DomainFact) -> Result<FactInput> {
    let (kind, payload) = fact_to_parts(fact);
    if !payload.is_object() {
        return Err(StoreError::Canonical {
            reason: format!("fact \"{kind}\" serialized to a non-object payload"),
        });
    }
    let version = current_fact_version(kind).ok_or_else(|| StoreError::Canonical {
        reason: format!(
            "fact kind \"{kind}\" is not in the domain catalog (KNOWN_FACT_KINDS); the domain \
             crate and this adapter are out of sync"
        ),
    })?;
    Ok(FactInput::new(kind, version, Json::from_serde(&payload)?))
}

/// Convert a validated record back into a typed fact. Fails closed when the
/// kind is unknown, the version is not understood, or the payload does not
/// reassemble.
pub fn from_record(record: &FactRecord) -> Result<DomainFact> {
    let fact = fact_from_parts(&record.fact_type, &record.payload.to_serde()).map_err(|error| {
        StoreError::Canonical {
            reason: format!(
                "fact \"{}\" at sequence {} does not decode: {error}",
                record.fact_type, record.sequence
            ),
        }
    })?;
    if fact.kind() != record.fact_type {
        return Err(StoreError::Canonical {
            reason: format!(
                "fact record at sequence {} claims type \"{}\" but its payload reassembles to \
                 \"{}\"",
                record.sequence,
                record.fact_type,
                fact.kind()
            ),
        });
    }
    Ok(fact)
}

/// The production validator: only real domain facts pass. Unknown kinds and
/// newer versions fail closed with upgrade guidance; older versions stay
/// readable (their payload still has to reassemble).
#[derive(Debug, Default, Clone, Copy)]
pub struct DomainFactValidator;

impl DomainFactValidator {
    /// Shared acceptance over (kind, version, reassembly).
    fn check(
        kind: &str,
        version: u32,
        reassemble: impl FnOnce() -> std::result::Result<DomainFact, String>,
    ) -> std::result::Result<(), String> {
        let Some(current) = current_fact_version(kind) else {
            return Err(format!(
                "unknown fact type \"{kind}\". The fact was written by a newer Relay or by a \
                 workflow this build does not carry. Upgrade Relay — or open the project with \
                 the newer install — before using this store again."
            ));
        };
        if version > current {
            return Err(format!(
                "fact type \"{kind}\" has version {version}, but this build understands version \
                 {current}. Upgrade Relay — or open the project with the newer install — before \
                 using this store again."
            ));
        }
        match reassemble() {
            Ok(fact) if fact.kind() == kind => Ok(()),
            Ok(fact) => Err(format!(
                "fact record claims type \"{kind}\" but its payload reassembles to \"{}\"",
                fact.kind()
            )),
            Err(error) => Err(format!(
                "fact \"{kind}\" version {version} payload is invalid: {error}"
            )),
        }
    }
}

impl FactValidator for DomainFactValidator {
    fn validate(&self, record: &FactRecord) -> std::result::Result<(), String> {
        Self::check(&record.fact_type, record.fact_version, || {
            fact_from_parts(&record.fact_type, &record.payload.to_serde())
                .map_err(|error| error.to_string())
        })
    }

    fn validate_input(&self, input: &FactInput) -> std::result::Result<(), String> {
        Self::check(&input.fact_type, input.fact_version, || {
            fact_from_parts(&input.fact_type, &input.payload.to_serde())
                .map_err(|error| error.to_string())
        })
    }
}

/// One fact on the timeline: the typed fact plus where it landed.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineFact {
    /// 1-based sequence position (also the project revision at this fact).
    pub sequence: u64,
    /// The record checksum.
    pub checksum: String,
    /// The typed fact.
    pub fact: DomainFact,
}

/// What a typed append did.
#[derive(Debug, Clone)]
pub struct GraphAppendOutcome {
    /// The appended facts, as a timeline in sequence order.
    pub appended: Vec<TimelineFact>,
    /// The project revision after the append (the fact count).
    pub revision: ProjectRevision,
    /// Whether the SQLite projection could not be updated (the facts are
    /// durable; the next open replays the suffix).
    pub projection_lagging: bool,
    /// Whether the append rotated the active segment.
    pub rotated: bool,
}

impl GraphAppendOutcome {
    /// How many facts were appended.
    pub fn len(&self) -> usize {
        self.appended.len()
    }

    /// Whether nothing was appended.
    pub fn is_empty(&self) -> bool {
        self.appended.is_empty()
    }
}

/// The typed face of a fact store: callers append `&[DomainFact]` and read
/// back the typed timeline; the graph projection is folded by
/// [`crate::fact::graph::GraphFold`] on every apply. Readiness and blockers
/// are **not** stored — the app derives them with `relay-engine` over
/// [`GraphFactStore::project_graph`].
pub struct GraphFactStore {
    store: FactStore,
}

impl GraphFactStore {
    /// Create a new fact store for `project` under
    /// `<workspace>/.relay-graph`. The caller's first appended fact must be
    /// `DomainFact::ProjectCreated` (the domain fold enforces it).
    pub fn create(
        workspace_root: &std::path::Path,
        project: &ProjectId,
        integrity: Integrity,
        options: FactStoreOptions,
    ) -> Result<GraphFactStore> {
        Ok(GraphFactStore {
            store: FactStore::create(workspace_root, project.as_str(), integrity, options)?,
        })
    }

    /// Open an existing store (writer mode): validate every record through
    /// the domain gate, recover a crash tail, reconcile the graph
    /// projection — all under the writer lock.
    pub fn open(
        workspace_root: &std::path::Path,
        options: FactStoreOptions,
    ) -> Result<(GraphFactStore, FactOpenReport)> {
        let (store, report) = FactStore::open(
            workspace_root,
            options,
            &DomainFactValidator,
            &crate::fact::graph::GraphFold,
        )?;
        Ok((GraphFactStore { store }, report))
    }

    /// Open read-only: validate without writing; no lock, no projection.
    pub fn open_read_only(
        workspace_root: &std::path::Path,
        options: FactStoreOptions,
    ) -> Result<GraphFactStore> {
        Ok(GraphFactStore {
            store: FactStore::open_read_only(workspace_root, options, &DomainFactValidator)?,
        })
    }

    /// Open an existing store at an explicit root (migration staging and
    /// other non-default layouts).
    pub fn open_at(
        root: &std::path::Path,
        options: FactStoreOptions,
    ) -> Result<(GraphFactStore, FactOpenReport)> {
        let paths = crate::fact::paths::FactPaths::from_root(root);
        let (store, report) = FactStore::open_at(
            &paths,
            options,
            &DomainFactValidator,
            &crate::fact::graph::GraphFold,
        )?;
        Ok((GraphFactStore { store }, report))
    }

    /// Append typed facts, **fold-validated**: the whole candidate history
    /// (existing facts plus the batch) must fold through the domain's
    /// `ProjectGraph::from_facts` before anything is written. A fact that
    /// would introduce a duplicate, an unknown reference, a cycle, a
    /// capability violation, or a sequence violation aborts with the
    /// domain's error and the store is untouched. This is the
    /// "deterministic application code validates and applies" boundary for
    /// the work graph.
    pub fn append(&self, facts: &[DomainFact]) -> Result<GraphAppendOutcome> {
        self.append_at(None, facts)
    }

    /// Append typed facts that expect the stream to currently be at
    /// `expected_revision`, fold-validated like [`append`](Self::append).
    /// A stale expectation fails with an actionable error instead of racing
    /// another writer.
    ///
    /// The domain fold gate runs against the timeline read at the start of
    /// this call, and the append then pins that revision: if another writer
    /// appends in between, this append refuses with a stale-revision error
    /// rather than committing a batch that was validated against history
    /// that no longer is the tail. The fold itself runs outside the writer
    /// lock (the lock is held only for the short storage operation).
    pub fn append_at(
        &self,
        expected_revision: Option<ProjectRevision>,
        facts: &[DomainFact],
    ) -> Result<GraphAppendOutcome> {
        if facts.is_empty() {
            return Ok(GraphAppendOutcome {
                appended: Vec::new(),
                revision: self.revision()?,
                projection_lagging: false,
                rotated: false,
            });
        }
        // Read the authoritative history and resolve the pinned revision
        // first, so a stale caller expectation fails before any folding
        // work happens.
        let history = self.read_facts()?;
        let current = ProjectRevision::from_count(history.len() as u64);
        let pinned = match expected_revision {
            Some(expected) if expected != current => {
                return Err(StoreError::Append {
                    reason: format!(
                        "stale expected revision: this writer expected the project at sequence \
                         {}, but the log has advanced to sequence {} (another writer \
                         appended). Reload the project state and retry the operation.",
                        expected.get(),
                        current.get()
                    ),
                });
            }
            other => other.unwrap_or(current),
        };
        // The domain fold gate: the candidate stream must fold.
        let mut candidate = history;
        candidate.extend(facts.iter().cloned());
        ProjectGraph::from_facts(&candidate).map_err(|error| StoreError::Append {
            reason: format!(
                "the batch would not fold into a valid graph; nothing was written: {error}"
            ),
        })?;
        let inputs: Vec<FactInput> = facts.iter().map(to_input).collect::<Result<_>>()?;
        let outcome = self.store.append_facts_at(
            Some(pinned.get()),
            &inputs,
            &DomainFactValidator,
            &crate::fact::graph::GraphFold,
        )?;
        let appended = outcome
            .records
            .iter()
            .map(|record| {
                Ok(TimelineFact {
                    sequence: record.sequence,
                    checksum: record.checksum.clone(),
                    fact: from_record(record)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(GraphAppendOutcome {
            revision: ProjectRevision::from_count(self.store.tail()?.last_sequence),
            appended,
            projection_lagging: outcome.projection_lagging,
            rotated: outcome.rotated,
        })
    }

    /// The typed timeline (authoritative, from JSONL).
    pub fn read_timeline(&self) -> Result<Vec<TimelineFact>> {
        let entries = self.store.read_all(&DomainFactValidator)?;
        entries
            .iter()
            .map(|entry: &FactEntry| {
                Ok(TimelineFact {
                    sequence: entry.record.sequence,
                    checksum: entry.record.checksum.clone(),
                    fact: from_record(&entry.record)?,
                })
            })
            .collect()
    }

    /// The typed facts, in sequence order.
    pub fn read_facts(&self) -> Result<Vec<DomainFact>> {
        Ok(self
            .read_timeline()?
            .into_iter()
            .map(|timeline| timeline.fact)
            .collect())
    }

    /// Fold the authoritative fact log into a [`ProjectGraph`] with the
    /// domain's own fold. This — not the SQLite projection — is the input
    /// for engine-derived readiness, blockers, and recommendations.
    pub fn project_graph(&self) -> Result<ProjectGraph> {
        let facts = self.read_facts()?;
        ProjectGraph::from_facts(&facts).map_err(|error| StoreError::Canonical {
            reason: format!("the fact log does not fold into a valid graph: {error}"),
        })
    }

    /// The current project revision (the fact count).
    pub fn revision(&self) -> Result<ProjectRevision> {
        Ok(ProjectRevision::from_count(
            self.store.tail()?.last_sequence,
        ))
    }

    /// The stream (project) id, when the store is open.
    pub fn project_id(&self) -> Result<String> {
        self.store.stream_id()
    }

    /// The stream integrity level.
    pub fn integrity(&self) -> Result<Integrity> {
        self.store.integrity()
    }

    /// Run a query against the graph projection's SQLite connection (see
    /// [`crate::fact::graph`] for the query helpers). Fails when the
    /// projection is not open (read-only stores).
    pub fn projection<F, T>(&self, query: F) -> Result<T>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T>,
    {
        self.store.projection_query(query)
    }

    /// The projection checkpoint, when the projection is open.
    pub fn projection_checkpoint(&self) -> Result<Option<crate::fact::projection::FactCheckpoint>> {
        self.store.projection_checkpoint()
    }

    /// Quarantine the current projection and rebuild it purely from the
    /// fact log. Never touches the JSONL log.
    pub fn rebuild_projection(&self) -> Result<FactRebuildReport> {
        self.store
            .rebuild_projection(&DomainFactValidator, &crate::fact::graph::GraphFold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_domain::Goal;
    use relay_domain::GoalId;
    use relay_domain::Provenance;
    use relay_domain::ProvenanceSource;
    use relay_domain::WorkItemId;
    use relay_domain::WorkKind;
    use relay_domain::work::CompletionRule;
    use relay_domain::work::ExecutorClass;
    use relay_domain::work::WorkItem;

    const CLOCK_MS: i64 = 1_791_878_400_000;
    const PROJECT: &str = "prj_demo";

    fn invalid(reason: String) -> StoreError {
        StoreError::Invalid { reason }
    }

    fn project() -> Result<ProjectId> {
        ProjectId::parse(PROJECT).map_err(|e| invalid(e.to_string()))
    }

    fn facts() -> Result<Vec<DomainFact>> {
        Ok(vec![
            DomainFact::ProjectCreated {
                project: project()?,
            },
            DomainFact::GoalDeclared {
                goal: Goal::new(
                    GoalId::parse("goal_1").map_err(|e| invalid(e.to_string()))?,
                    "Ship the release gate",
                    Provenance {
                        source: ProvenanceSource::User,
                        statement: Some("Ship it".to_string()),
                        at_ms: CLOCK_MS,
                    },
                    CLOCK_MS,
                ),
            },
            DomainFact::WorkDeclared {
                work: WorkItem::new(
                    WorkItemId::parse("work_discovery").map_err(|e| invalid(e.to_string()))?,
                    WorkKind::parse("discover-opportunity").map_err(|e| invalid(e.to_string()))?,
                    ExecutorClass::Model,
                    "Understand the goal",
                    CompletionRule::Receipt,
                ),
            },
        ])
    }

    #[test]
    fn catalog_delegation_agrees_with_the_domain() {
        assert_eq!(current_fact_version("work_declared"), Some(1));
        assert_eq!(current_fact_version("invalidation_declared"), Some(1));
        assert_eq!(current_fact_version("mode_entered"), None);
    }

    #[test]
    fn input_record_round_trip_through_the_catalog() -> Result<()> {
        for fact in facts()? {
            let input = to_input(&fact)?;
            assert_eq!(input.fact_type, fact.kind());
            assert_eq!(input.fact_version, 1);
            // The payload is the untagged form; reassembly restores the fact.
            assert!(input.payload.get("type").is_none());
            let reassembled = fact_from_parts(&input.fact_type, &input.payload.to_serde())
                .map_err(|e| invalid(e.to_string()))?;
            assert_eq!(reassembled, fact);
            assert!(DomainFactValidator.validate_input(&input).is_ok());
        }
        Ok(())
    }

    #[test]
    fn validator_fails_closed() -> Result<()> {
        let unknown = FactInput::new("not_a_fact", 1, Json::object([("goal", Json::string("x"))]));
        let error = DomainFactValidator
            .validate_input(&unknown)
            .err()
            .unwrap_or_default();
        assert!(error.contains("unknown fact type"), "{error}");
        assert!(error.contains("Upgrade Relay"), "{error}");

        let newer = FactInput::new("goal_declared", 2, Json::Object(Default::default()));
        let error = DomainFactValidator
            .validate_input(&newer)
            .err()
            .unwrap_or_default();
        assert!(error.contains("version 2"), "{error}");
        assert!(error.contains("Upgrade Relay"), "{error}");

        // A payload that does not reassemble fails even at the current
        // version.
        let malformed = FactInput::new("goal_declared", 1, Json::Object(Default::default()));
        assert!(DomainFactValidator.validate_input(&malformed).is_err());
        Ok(())
    }

    #[test]
    fn graph_store_round_trip_and_revision() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = FactStoreOptions::deterministic(CLOCK_MS);
        let store = GraphFactStore::create(
            dir.path(),
            &project()?,
            Integrity::Checksums,
            options.clone(),
        )?;
        let outcome = store.append(&facts()?)?;
        assert_eq!(outcome.len(), 3);
        assert_eq!(outcome.revision, ProjectRevision::from_count(3));
        drop(store);

        let (reopened, report) = GraphFactStore::open(dir.path(), options)?;
        assert_eq!(
            report.reconcile,
            Some(crate::fact::projection::FactReconcileOutcome::Matched)
        );
        let timeline = reopened.read_timeline()?;
        assert_eq!(timeline.len(), 3);
        assert_eq!(timeline[0].sequence, 1);
        assert_eq!(timeline[0].fact.kind(), "project_created");
        assert_eq!(timeline[2].fact.kind(), "work_declared");
        assert_eq!(reopened.revision()?, ProjectRevision::from_count(3));
        assert_eq!(reopened.project_id()?, PROJECT.to_string());

        // The domain fold accepts the timeline.
        let graph = reopened.project_graph()?;
        assert_eq!(graph.revision(), ProjectRevision::from_count(3));
        assert_eq!(graph.works().count(), 1);

        // Stale expected revision refuses.
        let error = reopened
            .append_at(Some(ProjectRevision::from_count(1)), &facts()?[2..])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("stale expected revision"), "{error}");
        Ok(())
    }

    #[test]
    fn append_rejects_graph_invalid_batches() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let store = GraphFactStore::create(
            dir.path(),
            &project()?,
            Integrity::Checksums,
            FactStoreOptions::deterministic(CLOCK_MS),
        )?;
        store.append(&facts()?)?;

        // A duplicate work declaration passes the catalog gate but not the
        // graph fold: append (checked by default) refuses and nothing is
        // written.
        let duplicate = facts()?[2].clone();
        let error = store
            .append(&[duplicate])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("would not fold"), "{error}");
        assert!(error.contains("already declared"), "{error}");
        assert_eq!(store.revision()?, ProjectRevision::from_count(3));

        // A fact the domain fold rejects for a forward reference (the
        // evidence targets work that does not exist) also refuses.
        let dangling = DomainFact::EvidenceRecorded {
            evidence: relay_domain::Evidence::new(
                relay_domain::ids::EvidenceId::parse("ev_0").map_err(|e| invalid(e.to_string()))?,
                relay_domain::EvidenceKind::Review,
                relay_domain::EvidenceTarget::WorkItem(
                    WorkItemId::parse("work_ghost").map_err(|e| invalid(e.to_string()))?,
                ),
                relay_domain::EvidenceOutcome::Info,
                "no such work",
                CLOCK_MS,
            ),
        };
        assert!(store.append(&[dangling]).is_err());
        assert_eq!(store.revision()?, ProjectRevision::from_count(3));

        // A valid continuation appends normally.
        let evidence = DomainFact::EvidenceRecorded {
            evidence: relay_domain::Evidence::new(
                relay_domain::ids::EvidenceId::parse("ev_1").map_err(|e| invalid(e.to_string()))?,
                relay_domain::EvidenceKind::Review,
                relay_domain::EvidenceTarget::WorkItem(
                    WorkItemId::parse("work_discovery").map_err(|e| invalid(e.to_string()))?,
                ),
                relay_domain::EvidenceOutcome::Info,
                "checked append works",
                CLOCK_MS,
            ),
        };
        let outcome = store.append(&[evidence])?;
        assert_eq!(outcome.revision, ProjectRevision::from_count(4));
        Ok(())
    }

    #[test]
    fn graph_store_read_only_has_no_projection() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let options = FactStoreOptions::deterministic(CLOCK_MS);
        {
            let store =
                GraphFactStore::create(dir.path(), &project()?, Integrity::Checksums, options)?;
            store.append(&facts()?)?;
        }
        let reader =
            GraphFactStore::open_read_only(dir.path(), FactStoreOptions::deterministic(CLOCK_MS))?;
        assert_eq!(reader.read_facts()?.len(), 3);
        assert!(reader.projection_checkpoint()?.is_none());
        assert!(reader.projection(|_| Ok(())).is_err());
        Ok(())
    }

    /// A domain-valid conversation stream exercising all five fact kinds:
    /// two conversations; a completed turn with related annotations (which
    /// deliberately name ids that do not exist in the graph), a failed
    /// turn, a cancelled turn, and a started-but-never-terminated turn.
    fn conversation_facts() -> Result<Vec<DomainFact>> {
        fn id<T>(raw: &str) -> Result<T>
        where
            T: std::str::FromStr,
            T::Err: std::fmt::Display,
        {
            raw.parse::<T>()
                .map_err(|error| invalid(format!("{raw}: {error}")))
        }
        let archive: relay_domain::ConversationId = id("cnv_archive")?;
        let chat: relay_domain::ConversationId = id("cnv_chat")?;
        Ok(vec![
            DomainFact::ProjectCreated {
                project: project()?,
            },
            DomainFact::ConversationStarted {
                conversation: archive.clone(),
                started_at_ms: CLOCK_MS,
            },
            DomainFact::ConversationStarted {
                conversation: chat.clone(),
                started_at_ms: CLOCK_MS + 1,
            },
            DomainFact::UserMessageRecorded {
                conversation: chat.clone(),
                turn: id("turn_1")?,
                message: id("msg_u1")?,
                text: "Explain this repository".to_string(),
                at_ms: CLOCK_MS + 2,
            },
            DomainFact::AssistantMessageRecorded {
                conversation: chat.clone(),
                turn: id("turn_1")?,
                message: id("msg_a1")?,
                text: "It is a work-graph harness".to_string(),
                at_ms: CLOCK_MS + 3,
                // Annotations only: none of these ids exist in the graph.
                related_attempts: vec![id("att_ghost")?],
                related_decisions: vec![id("dec_ghost")?],
                related_work: vec![id("work_ghost")?],
            },
            DomainFact::UserMessageRecorded {
                conversation: chat.clone(),
                turn: id("turn_2")?,
                message: id("msg_u2")?,
                text: "Ship it".to_string(),
                at_ms: CLOCK_MS + 4,
            },
            DomainFact::ConversationTurnFailed {
                conversation: chat.clone(),
                turn: id("turn_2")?,
                safe_message: "provider unavailable".to_string(),
                at_ms: CLOCK_MS + 5,
            },
            DomainFact::UserMessageRecorded {
                conversation: chat.clone(),
                turn: id("turn_3")?,
                message: id("msg_u3")?,
                text: "Never answered".to_string(),
                at_ms: CLOCK_MS + 6,
            },
            DomainFact::UserMessageRecorded {
                conversation: archive,
                turn: id("turn_old")?,
                message: id("msg_old-u")?,
                text: "Old question".to_string(),
                at_ms: CLOCK_MS - 10,
            },
            DomainFact::ConversationTurnCancelled {
                conversation: id("cnv_archive")?,
                turn: id("turn_old")?,
                reason: "user interrupted".to_string(),
                at_ms: CLOCK_MS - 9,
            },
        ])
    }

    #[test]
    fn conversation_facts_encode_decode_and_validate() -> Result<()> {
        for fact in conversation_facts()?.into_iter().skip(1) {
            let input = to_input(&fact)?;
            assert_eq!(input.fact_type, fact.kind());
            assert_eq!(input.fact_version, 1);
            assert!(input.payload.get("type").is_none());
            let reassembled = fact_from_parts(&input.fact_type, &input.payload.to_serde())
                .map_err(|e| invalid(e.to_string()))?;
            assert_eq!(reassembled, fact);
            assert!(DomainFactValidator.validate_input(&input).is_ok());
        }
        Ok(())
    }

    #[test]
    fn conversation_records_fail_closed_on_newer_versions() -> Result<()> {
        // A hand-written conversation_started record at payload version 2
        // is rejected with upgrade guidance, and a malformed v1 payload is
        // rejected even at the current version.
        let started = DomainFact::ConversationStarted {
            conversation: relay_domain::ConversationId::parse("cnv_chat")
                .map_err(|e| invalid(e.to_string()))?,
            started_at_ms: CLOCK_MS,
        };
        let (kind, payload) = fact_to_parts(&started);
        let newer = FactInput::new(kind, 2, Json::from_serde(&payload)?);
        let error = DomainFactValidator
            .validate_input(&newer)
            .err()
            .unwrap_or_default();
        assert!(error.contains("version 2"), "{error}");
        assert!(error.contains("Upgrade Relay"), "{error}");

        let malformed = FactInput::new(
            "user_message_recorded",
            1,
            Json::object([("unrelated", Json::Bool(true))]),
        );
        assert!(DomainFactValidator.validate_input(&malformed).is_err());
        Ok(())
    }

    #[test]
    fn graph_store_round_trips_and_projects_conversations() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let store = GraphFactStore::create(
            dir.path(),
            &project()?,
            Integrity::Checksums,
            FactStoreOptions::deterministic(CLOCK_MS),
        )?;
        let facts = conversation_facts()?;
        let outcome = store.append(&facts)?;
        assert_eq!(outcome.len(), facts.len());
        assert_eq!(store.revision()?.get(), facts.len() as u64);

        // The typed timeline replays in order and the pure fold projects
        // the conversation state.
        let timeline = store.read_timeline()?;
        assert_eq!(timeline.len(), facts.len());
        assert_eq!(timeline[1].fact.kind(), "conversation_started");
        let graph = store.project_graph()?;
        assert_eq!(graph.conversations().count(), 2);
        assert_eq!(
            graph.latest_conversation().map(|id| id.to_string()),
            Some("cnv_chat".to_string()),
            "the most recently applied start wins"
        );
        let chat = relay_domain::ConversationId::parse("cnv_chat")?;
        assert_eq!(
            graph
                .interrupted_turns(&chat)
                .iter()
                .map(|turn| turn.to_string())
                .collect::<Vec<_>>(),
            vec!["turn_3".to_string()]
        );
        assert_eq!(
            graph.transcript_tail(&chat, 10).len(),
            4,
            "u1, a1, u2 (failed turn), u3 (interrupted turn)"
        );

        // SQLite stays a work-graph lookup index: the fold contributed no
        // conversation rows, but the revision stamp counts every fact.
        let meta = store.projection(crate::fact::graph::graph_meta)?;
        assert_eq!(meta.revision, facts.len() as u64);
        assert_eq!(meta.project.as_deref(), Some(PROJECT));

        // A fold-invalid conversation append (terminal without a started
        // turn) is refused and nothing is written.
        let error = store
            .append(&[DomainFact::ConversationTurnCancelled {
                conversation: chat,
                turn: "turn_ghost".parse()?,
                reason: "nothing to cancel".to_string(),
                at_ms: CLOCK_MS,
            }])
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(error.contains("would not fold"), "{error}");
        assert!(error.contains("without a started turn"), "{error}");
        assert_eq!(store.revision()?.get(), facts.len() as u64);
        Ok(())
    }
}
