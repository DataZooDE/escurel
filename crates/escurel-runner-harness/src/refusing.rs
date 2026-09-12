//! A harness that refuses to run, for a workflow-declared harness this runner
//! cannot build.
//!
//! `resolve_harness` returns this INSTEAD of silently falling back to the
//! runner's default harness. Falling back would run a DIFFERENT harness (e.g.
//! `echo`) against a real workflow step — writing a fabricated, deterministic
//! stand-in result into the tenant's knowledge base and marking real events
//! processed. A step whose declared harness is unavailable must **fail closed**:
//! the refusal maps to [`HarnessError::Unsupported`] → a PERMANENT reconcile
//! failure, so the step dead-letters with a reason naming the harness the
//! deployment lacks. The operator fixes the selector (or deploys a runner that
//! provides it), never the retry budget — the same posture the `gemini` arm
//! takes when its key is missing.

use async_trait::async_trait;
use escurel_runner_core::TaskContext;

use crate::harness::{Harness, HarnessError, HarnessOutcome};

/// See the module docs. Holds the declared name only for the diagnostic.
pub struct RefusingHarness {
    declared: String,
}

impl RefusingHarness {
    /// A refusing harness for a workflow that declared `declared`, which this
    /// runner cannot build.
    pub fn new(declared: impl Into<String>) -> Self {
        Self {
            declared: declared.into(),
        }
    }
}

#[async_trait]
impl Harness for RefusingHarness {
    fn name(&self) -> &str {
        "refusing"
    }

    async fn run(&self, _task: &TaskContext) -> Result<HarnessOutcome, HarnessError> {
        Err(HarnessError::Unsupported {
            harness: "refusing",
            reason: format!(
                "the workflow declares harness `{declared}`, which this runner cannot build; \
                 refusing rather than falling back to the default harness, which would write a \
                 fabricated stand-in result into a real corpus. Deploy a runner that provides \
                 `{declared}`, or fix the plan's `harness:` declaration.",
                declared = self.declared
            ),
        })
    }
}
