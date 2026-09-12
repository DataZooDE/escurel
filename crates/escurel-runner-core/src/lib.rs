//! Harness-agnostic engine for the escurel agent runner.
//!
//! This crate is the runner's inner core: it owns the runtime
//! [`RunnerConfig`] and — as later work-items of the
//! `escurel-agent-runner` epic land — the trigger lifecycle, the
//! bounded dispatch queue, the cascade emitter, the loop-control
//! ledger, and the skill/context packager (see
//! [`docs/contract/agent-orchestration.md`] §Architecture).
//!
//! Per the epic's dependency constraint this crate depends **only** on
//! `escurel-client` + `escurel-types` (never on `escurel-server` /
//! `escurel-index`), so the runner deploys as an independent process.
//!
//! [`docs/contract/agent-orchestration.md`]: https://github.com/DataZooDE/escurel/blob/main/docs/contract/agent-orchestration.md

mod admit;
mod auth;
mod cascade;
mod config;
mod dispatch;
mod ledger;
mod packager;
mod quota;
mod reconciler;
mod recovery;
mod trigger;
mod workflow;

pub use admit::{Admission, LoopLimits, admit};
pub use auth::{AuthError, Signer, TokenSource};
pub use cascade::{CascadeError, CascadeOutcome, emit_cascade};
pub use config::{ConfigError, RunnerConfig};
pub use dispatch::{DispatchConsumer, DispatchQueue, EnqueueOutcome};
pub use escurel_runner_workflow::OperationStatus;
pub use ledger::{
    DeadLetterReason, Ledger, LedgerDecision, LedgerError, RunId, RunRecord, RunStatus,
};
pub use packager::{
    ALLOWED_TOOLS, Autonomy, Delegation, PackageError, REVIEW_TOOLS, TaskContext,
    WORKFLOW_STEP_TOOLS, package,
};

/// The harness selector for a step that delegates to the agent over A2A
/// (async-ops Phase 4 slice 3c). Shared here so the packager (which builds the
/// [`Delegation`]) and the harness adapter (which consumes it) name it
/// identically without the core→harness dependency the reverse would require.
pub const DELEGATE_HARNESS: &str = "delegate";
pub use quota::{Governor, QuotaDecision, QuotaLimits, RunSlot, ThrottleReason};
pub use reconciler::{
    ConfirmedEffect, ReconcileError, RunFailure, RunReport, assign_confirmed_write,
    classify_client_error, confirm_draft, confirm_effect, instance_version, run_with_retry,
};
pub use recovery::{RecoveryReport, recover_pending};
pub use secrecy::SecretString;
pub use trigger::{Lineage, Trigger};
pub use workflow::{
    OPERATION_STATUS_LABEL, StepTerminal, TerminalDelivery, WorkflowDriveError,
    WorkflowDriveOutcome, drive_workflow, operation_has_terminal_status, record_status_best_effort,
    recover_workflows,
};
