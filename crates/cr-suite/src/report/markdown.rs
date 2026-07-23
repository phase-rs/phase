//! Markdown rendering of a suite run report (alternate to the plain summary).

use crate::report::SuiteReport;
use crate::runner::ScenarioOutcome;

/// Render a `SuiteReport` as a GitHub-flavored Markdown document.
pub fn render_markdown(report: &SuiteReport) -> String {
    let mut out = String::new();
    out.push_str("# cr-suite run report\n\n");
    out.push_str(&format!(
        "**{} passed** · {} failed · {} errors · {} skipped · {} considered\n\n",
        report.passed,
        report.failed,
        report.errors,
        report.skipped,
        report.total_considered()
    ));

    out.push_str("## By section\n\n");
    out.push_str("| Section | Passed | Failed | Errors | Skipped |\n");
    out.push_str("|--------:|-------:|-------:|-------:|--------:|\n");
    for (section, stats) in report.by_section() {
        out.push_str(&format!(
            "| {section:03} | {} | {} | {} | {} |\n",
            stats.passed, stats.failed, stats.errors, stats.skipped
        ));
    }

    let mut problems = report.results.iter().filter(|r| {
        matches!(
            r.outcome,
            ScenarioOutcome::Failed(_) | ScenarioOutcome::Error { .. }
        )
    });
    if problems.clone().next().is_some() {
        out.push_str("\n## Failures & errors\n\n");
        for r in problems.by_ref() {
            match &r.outcome {
                ScenarioOutcome::Failed(f) => {
                    out.push_str(&format!(
                        "- **FAIL** CR {} (`{}`) — {}: {}\n",
                        r.rule,
                        r.path.display(),
                        f.kind,
                        f.detail
                    ));
                }
                ScenarioOutcome::Error { message } => {
                    out.push_str(&format!(
                        "- **ERROR** CR {} (`{}`) — {}\n",
                        r.rule,
                        r.path.display(),
                        message
                    ));
                }
                _ => {}
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_header_and_table() {
        let report = SuiteReport {
            passed: 3,
            ..Default::default()
        };
        let md = render_markdown(&report);
        assert!(md.contains("# cr-suite run report"));
        assert!(md.contains("**3 passed**"));
        assert!(md.contains("| Section |"));
    }
}
