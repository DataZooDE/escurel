//! Optional pass/fail gate: compare a report's metrics against thresholds.
//!
//! Thresholds come from a flat `key = value` file (one per line, `#` comments)
//! — no TOML dep. Keys are optional; only those present are checked. This is a
//! **manual** pre-deployment gate, not a CI step. Because SciFact's absolute
//! nDCG differs from the ADR-0001 460-block target, thresholds are supplied by
//! the operator rather than hard-coded.
//!
//! Recognized keys (all optional):
//! - `min_ndcg_at_10`, `min_recall_at_100` — applied to every config.
//! - `max_p50_ms`, `max_p95_ms` — applied to every config's sequential latency.
//! - `min_qps` — applied to every config that measured QPS.

use crate::report::EvalReport;

#[derive(Debug, Clone, Default)]
pub struct Thresholds {
    pub min_ndcg_at_10: Option<f64>,
    pub min_recall_at_100: Option<f64>,
    pub max_p50_ms: Option<f64>,
    pub max_p95_ms: Option<f64>,
    pub min_qps: Option<f64>,
}

impl Thresholds {
    /// Parse a flat `key = value` thresholds file. Unknown keys error so a typo
    /// can't silently disable a gate.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut t = Self::default();
        for (i, raw) in s.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, val) = line
                .split_once('=')
                .ok_or_else(|| format!("line {}: expected `key = value`", i + 1))?;
            let key = key.trim();
            let val: f64 = val
                .trim()
                .parse()
                .map_err(|_| format!("line {}: `{}` is not a number", i + 1, val.trim()))?;
            match key {
                "min_ndcg_at_10" => t.min_ndcg_at_10 = Some(val),
                "min_recall_at_100" => t.min_recall_at_100 = Some(val),
                "max_p50_ms" => t.max_p50_ms = Some(val),
                "max_p95_ms" => t.max_p95_ms = Some(val),
                "min_qps" => t.min_qps = Some(val),
                other => return Err(format!("line {}: unknown key `{other}`", i + 1)),
            }
        }
        Ok(t)
    }
}

#[derive(Debug, Clone)]
pub struct GateCheck {
    pub config: String,
    pub metric: String,
    pub value: f64,
    pub threshold: f64,
    pub ok: bool,
}

#[derive(Debug, Clone)]
pub struct GateOutcome {
    pub checks: Vec<GateCheck>,
}

impl GateOutcome {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
}

/// Evaluate every threshold against every config in the report.
#[must_use]
pub fn evaluate(report: &EvalReport, t: &Thresholds) -> GateOutcome {
    let mut checks = Vec::new();
    let mut min = |cfg: &str, metric: &str, value: f64, thr: Option<f64>| {
        if let Some(thr) = thr {
            checks.push(GateCheck {
                config: cfg.to_owned(),
                metric: metric.to_owned(),
                value,
                threshold: thr,
                ok: value >= thr,
            });
        }
    };
    for r in &report.results {
        min(&r.config, "min_ndcg_at_10", r.ndcg_at_10, t.min_ndcg_at_10);
        min(
            &r.config,
            "min_recall_at_100",
            r.recall_at_100,
            t.min_recall_at_100,
        );
        if let Some(q) = &r.qps {
            min(&r.config, "min_qps", q.qps, t.min_qps);
        }
    }
    // Max-style checks (value must be <= threshold).
    for r in &report.results {
        if let Some(thr) = t.max_p50_ms {
            checks.push(GateCheck {
                config: r.config.clone(),
                metric: "max_p50_ms".to_owned(),
                value: r.latency.p50_ms,
                threshold: thr,
                ok: r.latency.p50_ms <= thr,
            });
        }
        if let Some(thr) = t.max_p95_ms {
            checks.push(GateCheck {
                config: r.config.clone(),
                metric: "max_p95_ms".to_owned(),
                value: r.latency.p95_ms,
                threshold: thr,
                ok: r.latency.p95_ms <= thr,
            });
        }
    }
    GateOutcome { checks }
}
