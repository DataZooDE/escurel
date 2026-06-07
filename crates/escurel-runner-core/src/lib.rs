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

mod config;
mod dispatch;
mod ledger;
mod packager;
mod trigger;

pub use config::{ConfigError, RunnerConfig};
pub use dispatch::{DispatchConsumer, DispatchQueue, EnqueueOutcome};
pub use ledger::{Ledger, LedgerDecision, LedgerError, RunId, RunRecord, RunStatus};
pub use packager::{ALLOWED_TOOLS, PackageError, TaskContext, package};
pub use secrecy::SecretString;
pub use trigger::{Lineage, Trigger};
