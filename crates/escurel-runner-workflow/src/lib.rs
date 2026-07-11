//! Deterministic reducer for escurel **dynamic workflows**
//! (`docs/contract/dynamic-workflows.md`).
//!
//! A workflow is not a second execution model beside the runner — it is the
//! runner's existing reactive `event → agent → event` loop with its emit
//! policy generalized. `emit_cascade` is the degenerate case (width ≤ 1, no
//! join); this crate is the general one: a **pure planner** that reads the
//! immutable plan ([`WorkflowSkill`]) plus the run's instances/log and
//! returns the next batch of steps ([`StepIntent`]) to emit — calling no
//! LLM and doing no write reasoning. All intelligence lives inside harness
//! runs; this crate only owns control flow.
//!
//! Per the runner epic's independence rule, this crate depends only on
//! `escurel-types` (and, from PR-3, `escurel-client`) — never on
//! `escurel-server`/`escurel-index`.
//!
//! ## Scope so far
//!
//! - PR-2: the step vocabulary + the `§3.6` keystone — [`spec`] (parse the
//!   plan), [`step`] ([`StepIntent`] + its deterministic event/instance
//!   ids), and [`key`] (content-addressed [`key::step_key`]).
//! - PR-3: the pure [`reduce`] planner over a [`RunState`] — linear phase
//!   sequencing + basic `fan_out: { over }` enumeration. The quorum barrier
//!   close semantics (the `list_instances` ∪ ledger-terminal tally) land
//!   with PR-5.

pub mod barrier;
pub mod budget;
pub mod corpus;
pub mod key;
pub mod reduce;
pub mod spec;
pub mod step;

pub use barrier::{BarrierInput, ClaimOutcome, Vote, barrier_closed, survivors, tally_barrier};
pub use budget::{BudgetExceeded, DEFAULT_PHASE_FANOUT_CAP, check_budget, projected_fan_out};
pub use reduce::{ProducedInstance, RunState, reduce};
pub use spec::{DEFAULT_RUN_SKILL, FanOut, Phase, VerifyPolicy, WorkflowSkill};
pub use step::StepIntent;
