//! Suite run reporting.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::runner::ScenarioOutcome;

#[derive(Debug, Clone)]
pub struct ScenarioReport {
    pub path: PathBuf,
    pub rule: String,
    pub section: u32,
    pub outcome: ScenarioOutcome,
}

#[derive(Debug, Clone, Default)]
pub struct SuiteReport {
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: usize,
    pub results: Vec<ScenarioReport>,
}

impl SuiteReport {
    pub fn total_considered(&self) -> usize {
        self.passed + self.failed + self.skipped + self.errors
    }

    pub fn by_section(&self) -> BTreeMap<u32, SectionStats> {
        let mut map: BTreeMap<u32, SectionStats> = BTreeMap::new();
        for r in &self.results {
            let entry = map.entry(r.section).or_default();
            entry.total += 1;
            match &r.outcome {
                ScenarioOutcome::Passed => entry.passed += 1,
                ScenarioOutcome::Failed(_) => entry.failed += 1,
                ScenarioOutcome::Skipped { .. } => entry.skipped += 1,
                ScenarioOutcome::Error { .. } => entry.errors += 1,
            }
        }
        map
    }

    pub fn render_summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "cr-suite: {} passed, {} failed, {} errors, {} skipped ({} considered)\n",
            self.passed,
            self.failed,
            self.errors,
            self.skipped,
            self.total_considered()
        ));

        for (section, stats) in self.by_section() {
            out.push_str(&format!(
                "  section {section:03}: {} passed / {} failed / {} errors / {} skipped\n",
                stats.passed, stats.failed, stats.errors, stats.skipped
            ));
        }

        for r in &self.results {
            match &r.outcome {
                ScenarioOutcome::Failed(f) => {
                    out.push_str(&format!(
                        "FAIL {} ({}) [{}]: {}\n",
                        r.rule,
                        r.path.display(),
                        f.kind,
                        f.detail
                    ));
                }
                ScenarioOutcome::Error { message } => {
                    out.push_str(&format!(
                        "ERROR {} ({}): {}\n",
                        r.rule,
                        r.path.display(),
                        message
                    ));
                }
                _ => {}
            }
        }
        out
    }

    pub fn success(&self) -> bool {
        self.failed == 0 && self.errors == 0
    }
}

#[derive(Debug, Clone, Default)]
pub struct SectionStats {
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: usize,
}
