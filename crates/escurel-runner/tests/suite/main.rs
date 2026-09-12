//! The crate's shared integration-test binary.
//!
//! Cargo compiles every `tests/*.rs` file into its own executable, each
//! statically linking the whole dependency graph. Files that sit *inside*
//! `tests/suite/` are not targets in their own right, so declaring them as
//! modules here collapses them into one binary.
//!
//! This crate was the workspace's largest single consumer of link time and disk:
//! 18 test files, each linking the runner, the gateway and DuckDB at ~111 MB
//! a piece — 2.2 GB of binaries for 35 tests.
//!
//! Adding a test file: put it in `tests/suite/` and add its `mod` line
//! below. A file that is not listed here is silently not compiled — nothing
//! warns you about it.
//!
//! The layout matters: it must be `tests/suite/main.rs`, not
//! `tests/suite.rs`. A test target's root file resolves `mod x;` against
//! its *own* directory, so `tests/suite.rs` would look for `tests/x.rs`.

mod agy_live;
mod autonomy_gate;
mod cascade_lineage;
mod cascade_produced;
mod cascade_trace;
mod claude_live;
mod codex_live;
mod confirm_unflagged;
mod delegate_a2a;
mod echo_end_to_end;
mod gateway_lost_mid_life;
mod gateway_not_ready;
mod gemini_end_to_end;
mod gemini_live;
mod harness_failure_vs_gateway;
mod healthz;
mod inbox_poll;
mod loop_controls;
mod minted_credential;
mod packager;
mod quota_throttle;
mod reconcile_retry;
mod review_no_op;
mod run_ledger;
mod sigterm_drain;
mod trigger;
mod workflow_end_to_end;
