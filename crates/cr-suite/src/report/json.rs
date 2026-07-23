//! JSON rendering of a suite run report (machine-consumable alternate format).

use serde_json::{json, Value};

use crate::report::SuiteReport;
use crate::runner::ScenarioOutcome;

/// Render a `SuiteReport` as a `serde_json::Value`.
pub fn to_json(report: &SuiteReport) -> Value {
    let sections: Vec<Value> = report
        .by_section()
        .into_iter()
        .map(|(section, stats)| {
            json!({
                "section": section,
                "passed": stats.passed,
                "failed": stats.failed,
                "errors": stats.errors,
                "skipped": stats.skipped,
                "total": stats.total,
            })
        })
        .collect();

    let results: Vec<Value> = report
        .results
        .iter()
        .map(|r| {
            let (outcome, detail) = match &r.outcome {
                ScenarioOutcome::Passed => ("passed", Value::Null),
                ScenarioOutcome::Failed(f) => {
                    ("failed", json!({ "kind": f.kind, "detail": f.detail }))
                }
                ScenarioOutcome::Skipped { reason } => ("skipped", json!({ "reason": reason })),
                ScenarioOutcome::Error { message } => ("error", json!({ "message": message })),
            };
            json!({
                "rule": r.rule,
                "section": r.section,
                "path": r.path.display().to_string(),
                "outcome": outcome,
                "detail": detail,
            })
        })
        .collect();

    json!({
        "summary": {
            "passed": report.passed,
            "failed": report.failed,
            "errors": report.errors,
            "skipped": report.skipped,
            "considered": report.total_considered(),
            "success": report.success(),
        },
        "sections": sections,
        "results": results,
    })
}

/// Render a `SuiteReport` as a pretty-printed JSON string.
pub fn render_json(report: &SuiteReport) -> String {
    serde_json::to_string_pretty(&to_json(report)).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_has_summary_and_success_flag() {
        let report = SuiteReport {
            passed: 2,
            ..Default::default()
        };
        let v = to_json(&report);
        assert_eq!(v["summary"]["passed"], 2);
        assert_eq!(v["summary"]["success"], true);
        let s = render_json(&report);
        assert!(s.contains("\"summary\""));
    }
}
