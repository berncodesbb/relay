//! Attempt specifications and the runtime handle executors receive.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use relay_domain::{
    ArtifactVersionRef, AttemptId, CapabilityGrant, DecisionId, GrantId, ProjectGraph,
    ProjectRevision, WorkItemId,
};
use relay_providers::contract::CancelToken;
use relay_tools::ToolRegistry;

use crate::budget::BudgetGuard;
use crate::model::ModelClient;

/// The immutable inputs for one attempt, assembled by the orchestrator from
/// domain data. The grant is the single source of authority: model route
/// and budgets live inside it, never duplicated here.
#[derive(Debug, Clone)]
pub struct AttemptSpec {
    /// Addressable attempt identity.
    pub attempt: AttemptId,
    /// The work item being attempted.
    pub work: WorkItemId,
    /// The bounded objective, copied from the work item.
    pub objective: String,
    /// Exact input versions resolved at scheduling time.
    pub inputs: Vec<ArtifactVersionRef>,
    /// The decision this attempt waits on, when the work completes through
    /// a human decision.
    pub decision: Option<DecisionId>,
    /// The capability grant bound to this attempt (the durable lease).
    pub grant: CapabilityGrant,
    /// The project revision the attempt was computed against.
    pub expected_revision: ProjectRevision,
    /// Wall-clock milliseconds when the attempt started.
    pub started_at_ms: i64,
}

/// Supplies read-only project-graph snapshots to executors. The default
/// snapshots the fact sink once at attempt start (no storage lock is held
/// into executor work); the app may override with its own projection.
pub type GraphView = Arc<dyn Fn() -> ProjectGraph + Send + Sync>;

/// Everything an executor needs at run time. Cheap to clone per attempt;
/// the cancellation signal and budget guard are per-attempt.
#[derive(Clone)]
pub struct RunContext {
    /// Resolved absolute workspace root; never escaped by tools.
    pub workspace_root: PathBuf,
    /// The attempt's cancellation signal. Budget exhaustion, timeouts, and
    /// user cancellation all fire it; tools and the process supervisor
    /// poll it.
    pub signal: CancelToken,
    /// The model client, already bound to no particular provider.
    pub model: Arc<dyn ModelClient>,
    /// The tool registry (schema-validated, capability-classified).
    pub tools: Arc<ToolRegistry>,
    /// The attempt's budget guard.
    pub budget: Arc<BudgetGuard>,
    /// A read-only project-graph snapshot taken at attempt start.
    pub graph: GraphView,
    /// Wall-clock milliseconds captured when the context was built.
    pub now_ms: i64,
}

/// A wall-clock milliseconds source. Production uses [`system_clock`];
/// tests inject a deterministic one.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The real system clock in Unix epoch milliseconds.
pub fn system_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default()
    })
}

/// A manually advanced clock for deterministic tests.
pub struct ManualClock {
    now: AtomicI64,
}

impl ManualClock {
    /// A clock pinned at `start_ms`.
    pub fn new(start_ms: i64) -> Self {
        ManualClock {
            now: AtomicI64::new(start_ms),
        }
    }

    /// Advance the clock by `milliseconds` and return the new reading.
    pub fn advance(&self, milliseconds: i64) -> i64 {
        self.now.fetch_add(milliseconds, Ordering::SeqCst) + milliseconds
    }

    /// The current reading.
    pub fn now(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }

    /// Wrap a shared clock as a [`Clock`] port; later [`advance`](Self::advance)
    /// calls on the shared handle are visible through the port.
    pub fn into_clock(self: Arc<Self>) -> Clock {
        Arc::new(move || self.now())
    }
}

/// A joined handle for spawned attempt jobs, abstract over the runtime.
pub trait JoinHandle: Send {
    /// Wait for the job to finish.
    fn join(self: Box<Self>);
}

impl JoinHandle for std::thread::JoinHandle<()> {
    fn join(self: Box<Self>) {
        let _ = (*self).join();
    }
}

/// Spawns attempt jobs. Production uses real threads
/// ([`thread_spawn`]); deterministic tests run jobs inline
/// ([`inline_spawn`]). The orchestrator never schedules work itself — the
/// caller supplies the concurrency mechanism, keeping this crate
/// runtime-agnostic (no async runtime dependency).
pub type Spawn = Arc<dyn Fn(Box<dyn FnOnce() + Send>) -> Box<dyn JoinHandle> + Send + Sync>;

/// Spawn each job on its own OS thread.
pub fn thread_spawn() -> Spawn {
    Arc::new(|job| Box::new(std::thread::spawn(job)))
}

/// Run each job to completion on the calling thread.
pub fn inline_spawn() -> Spawn {
    Arc::new(|job| {
        job();
        Box::new(NoopJoin)
    })
}

struct NoopJoin;

impl JoinHandle for NoopJoin {
    fn join(self: Box<Self>) {}
}

/// Deterministic id minting for attempts (and grant ids for grant
/// factories). Ids are sequential and process-unique; the kernel itself
/// never mints ids.
#[derive(Debug, Default)]
pub struct IdSource {
    counter: AtomicU64,
}

impl IdSource {
    /// A fresh source counting from 1.
    pub fn new() -> Self {
        IdSource {
            counter: AtomicU64::new(0),
        }
    }

    /// Advance the counter past every numeric `att_`/`grant_` id already
    /// recorded in the graph, so a new process opening an existing store
    /// never mints an id the fold would reject as a duplicate. Non-numeric
    /// slugs never collide with this source's decimal output and are
    /// ignored. Returns the source for chaining.
    pub fn seed_from(&self, graph: &ProjectGraph) -> &Self {
        let mut max = self.counter.load(Ordering::SeqCst);
        for (_, attempt) in graph.attempts() {
            if let Some(n) = numeric_suffix(attempt.id.as_str(), "att_") {
                max = max.max(n);
            }
            if let Some(n) = numeric_suffix(attempt.grant.id.as_str(), "grant_") {
                max = max.max(n);
            }
        }
        self.counter.fetch_max(max, Ordering::SeqCst);
        self
    }

    /// The next id in the source's sequence.
    fn next(&self, prefix: &str) -> String {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        format!("{prefix}{n}")
    }

    /// Mint an [`AttemptId`] (`att_1`, `att_2`, …).
    pub fn next_attempt_id(&self) -> AttemptId {
        AttemptId::parse(&self.next("att_")).unwrap_or_else(|error| unreachable_id("att_", &error))
    }

    /// Mint a [`GrantId`] (`grant_1`, `grant_2`, …).
    pub fn next_grant_id(&self) -> GrantId {
        GrantId::parse(&self.next("grant_"))
            .unwrap_or_else(|error| unreachable_id("grant_", &error))
    }
}

/// Minter output is `<prefix><decimal>` and always parses; any failure is
/// a programming error that must abort loudly rather than be papered over.
#[allow(clippy::panic)] // scoped: minted ids are `<prefix><decimal>`, failure is unreachable
fn unreachable_id(prefix: &str, error: &relay_domain::IdParseError) -> ! {
    let message = format!("id source produced an invalid {prefix} id: {error}");
    panic!("{message}");
}

/// The decimal tail of `<prefix><number>`, when the id has exactly that
/// shape.
fn numeric_suffix(id: &str, prefix: &str) -> Option<u64> {
    id.strip_prefix(prefix)?.parse().ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn id_sources_mint_sequential_ids() {
        let ids = IdSource::new();
        assert_eq!(ids.next_attempt_id().as_str(), "att_1");
        assert_eq!(ids.next_grant_id().as_str(), "grant_2");
        assert_eq!(ids.next_attempt_id().as_str(), "att_3");
    }

    #[test]
    fn manual_clocks_advance_deterministically() {
        let clock = ManualClock::new(1_000);
        assert_eq!(clock.now(), 1_000);
        assert_eq!(clock.advance(500), 1_500);
        assert_eq!(clock.now(), 1_500);
    }

    #[test]
    fn inline_spawn_runs_jobs_immediately() {
        let fired = Arc::new(AtomicI64::new(0));
        let probe = Arc::clone(&fired);
        inline_spawn()(Box::new(move || {
            probe.store(7, Ordering::SeqCst);
        }))
        .join();
        assert_eq!(fired.load(Ordering::SeqCst), 7);
    }

    #[test]
    fn seeding_advances_past_existing_ids() {
        let graph = relay_domain::ProjectGraph::default();
        let ids = IdSource::new();
        ids.seed_from(&graph);
        assert_eq!(ids.next_attempt_id().as_str(), "att_1");

        // A graph holding att_3 under grant_7: the next mints continue
        // past both, and never re-mint an id the fold would reject.
        use relay_domain as domain;
        let mut facts = vec![
            domain::DomainFact::ProjectCreated {
                project: domain::ProjectId::parse("prj_seed").unwrap(),
            },
            domain::DomainFact::GoalDeclared {
                goal: domain::Goal {
                    id: domain::GoalId::parse("goal_seed").unwrap(),
                    outcome: "seed ids".into(),
                    constraints: Vec::new(),
                    success_definition: "ids never repeat".into(),
                    provenance: domain::Provenance {
                        source: domain::ProvenanceSource::User,
                        statement: None,
                        at_ms: 0,
                    },
                    declared_at_ms: 0,
                },
            },
            domain::DomainFact::WorkDeclared {
                work: domain::WorkItem::new(
                    domain::WorkItemId::parse("work_seed").unwrap(),
                    domain::WorkKind::parse("seed-work").unwrap(),
                    domain::ExecutorClass::Model,
                    "hold ids",
                    domain::CompletionRule::Receipt,
                ),
            },
        ];
        for n in 1..=3u64 {
            facts.push(domain::DomainFact::AttemptStarted {
                attempt: domain::AttemptId::parse(&format!("att_{n}")).unwrap(),
                work: domain::WorkItemId::parse("work_seed").unwrap(),
                expected_revision: domain::ProjectRevision::from_count(2 + n),
                grant: domain::CapabilityGrant::read_only(
                    domain::GrantId::parse(&format!("grant_{}", n + 4)).unwrap(),
                    vec![],
                ),
                started_at_ms: n as i64,
            });
        }
        let graph = relay_domain::ProjectGraph::from_facts(facts.iter()).expect("folds");
        ids.seed_from(&graph);
        // One shared counter mints both prefixes, so seeding continues
        // past the highest number seen under EITHER prefix (att_3,
        // grant_7): the next mints are att_8 and grant_9 — never a
        // repeat of anything the fold already holds.
        assert_eq!(ids.next_attempt_id().as_str(), "att_8");
        assert_eq!(ids.next_grant_id().as_str(), "grant_9");
    }
}
