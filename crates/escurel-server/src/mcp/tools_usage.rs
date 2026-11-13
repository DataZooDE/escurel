//! `usage_report` (#821): what runs cost, by skill, agent, page, model or
//! day, folded from `run-finished` by [`escurel_index::UsageFold`]. Admin
//! scope (the dispatcher's `require_admin`): a bill spans every page and
//! skill of the tenant, so it is the operator's view, not a per-event read.

use escurel_index::{Indexer, UsageDim, UsageFold};
use serde::Deserialize;
use serde_json::{Value, json};

use super::tools_write::time_arg;
use super::{JsonRpcError, parse_args};

#[derive(Deserialize)]
pub(super) struct UsageReportArgs {
    #[serde(default)]
    group_by: Vec<String>,
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    skill: Option<String>,
}

pub(super) async fn tool_usage_report(
    indexer: &Indexer,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let a: UsageReportArgs = parse_args(args, "usage_report")?;
    let dims = a
        .group_by
        .iter()
        .map(|g| {
            UsageDim::parse(g).ok_or_else(|| {
                JsonRpcError::invalid_params(format!(
                    "usage_report: `group_by` entries are skill | agent | page | model | day, got `{g}`"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if dims.is_empty() {
        return Err(JsonRpcError::invalid_params(
            "usage_report: `group_by` names at least one of skill | agent | page | model | day"
                .to_owned(),
        ));
    }
    let since = time_arg("usage_report", "since", a.since.as_deref())?;
    let until = time_arg("usage_report", "until", a.until.as_deref())?;
    let fold = indexer
        .usage_report(since, until, UsageFold::new(&dims, a.skill))
        .await
        .map_err(|e| JsonRpcError::internal(format!("usage_report: {e}")))?;
    let mut out = fold.to_json();
    out["since"] = json!(a.since.filter(|s| !s.trim().is_empty()));
    out["until"] = json!(a.until.filter(|s| !s.trim().is_empty()));
    Ok(out)
}
