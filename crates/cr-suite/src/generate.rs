//! Skeleton fixture generation from CompRules.txt.

use std::fs;
use std::path::{Path, PathBuf};

use crate::catalog::{rule_to_stem, section_title};
use crate::comp_rules::{parse_comp_rules, CompRule};
use crate::schema::{ScenarioFile, ScenarioStatus};

/// Options for skeleton generation.
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    pub comp_rules: PathBuf,
    pub out_dir: PathBuf,
    /// When true, do not overwrite fixtures that are already `executable` /
    /// `not-applicable` / `deferred`.
    pub preserve_authored: bool,
    /// Optional section filter (generate only these sections).
    pub sections: Option<Vec<u32>>,
}

/// Summary of a generate pass.
#[derive(Debug, Clone, Default)]
pub struct GenerateStats {
    pub rules_seen: usize,
    pub written: usize,
    pub preserved: usize,
    pub skipped_filter: usize,
}

/// Generate (or refresh) skeleton TOML fixtures under `out_dir/<section>/`.
pub fn generate_skeletons(opts: &GenerateOptions) -> Result<GenerateStats, String> {
    let rules = parse_comp_rules(&opts.comp_rules)?;
    let mut stats = GenerateStats {
        rules_seen: rules.len(),
        ..Default::default()
    };

    for rule in &rules {
        if let Some(sections) = &opts.sections {
            if !sections.contains(&rule.section) {
                stats.skipped_filter += 1;
                continue;
            }
        }

        let dir = opts.out_dir.join(format!("{:03}", rule.section));
        fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;

        let path = dir.join(format!("{}.toml", rule_to_stem(&rule.number)));

        if opts.preserve_authored && path.exists() {
            // Preserve any authored (non-skeleton) fixture. Critically, if the
            // file cannot be read or parsed, we still PRESERVE it rather than
            // clobbering — a read/parse failure means the file is hand-authored
            // in a shape the generator does not understand (or is mid-edit by a
            // concurrent agent), and silently overwriting it would destroy work.
            match fs::read_to_string(&path) {
                Ok(existing) => match toml::from_str::<ScenarioFile>(&existing) {
                    Ok(parsed) if parsed.status != ScenarioStatus::Skeleton => {
                        stats.preserved += 1;
                        continue;
                    }
                    Ok(_) => {} // parsed skeleton → safe to refresh
                    Err(_) => {
                        // Unparseable existing file → preserve, don't overwrite.
                        stats.preserved += 1;
                        continue;
                    }
                },
                Err(_) => {
                    // Unreadable existing file → preserve, don't overwrite.
                    stats.preserved += 1;
                    continue;
                }
            }
        }

        let fixture = ScenarioFile::skeleton(
            &rule.number,
            rule.section,
            &rule.text,
            section_title(rule.section),
        );
        write_fixture(&path, &fixture)?;
        stats.written += 1;
    }

    Ok(stats)
}

/// Write a fixture atomically-ish (write then nothing fancy — fine for tools).
pub fn write_fixture(path: &Path, fixture: &ScenarioFile) -> Result<(), String> {
    let body = render_fixture(fixture);
    fs::write(path, body).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(())
}

/// Stable, review-friendly TOML rendering for fixtures.
pub fn render_fixture(fixture: &ScenarioFile) -> String {
    // Prefer serde for authored executable fixtures (full fidelity).
    // Skeletons get a compact hand-shaped layout for greppability.
    if fixture.status != ScenarioStatus::Skeleton
        || fixture.setup.is_some()
        || !fixture.steps.is_empty()
        || !fixture.assertions.is_empty()
    {
        return toml::to_string_pretty(fixture).unwrap_or_else(|_| String::new());
    }

    let mut out = String::new();
    out.push_str(&format!("rule = {:?}\n", fixture.rule));
    out.push_str(&format!("section = {}\n", fixture.section));
    out.push_str(&format!("title = {:?}\n", fixture.title));
    out.push_str("status = \"skeleton\"\n");
    out.push_str(&format!("text = {:?}\n", fixture.text));
    if !fixture.notes.is_empty() {
        out.push_str(&format!("notes = {:?}\n", fixture.notes));
    }
    out
}

/// Classify a CompRule into an initial status hint (still emits skeleton;
/// agents promote to executable / not-applicable).
#[allow(dead_code)]
pub fn classify_rule_hint(rule: &CompRule) -> ScenarioStatus {
    let lower = rule.text.to_lowercase();
    if lower.contains("see rule") && lower.len() < 80 {
        return ScenarioStatus::NotApplicable;
    }
    if lower.starts_with("example") {
        return ScenarioStatus::NotApplicable;
    }
    ScenarioStatus::Skeleton
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn render_skeleton_is_compact() {
        let f = ScenarioFile::skeleton("100.1", 100, "These Magic rules apply.", "General");
        let body = render_fixture(&f);
        assert!(body.contains("status = \"skeleton\""));
        assert!(body.contains("rule = \"100.1\""));
    }

    #[test]
    fn generate_writes_section_dirs() {
        let dir = tempdir().unwrap();
        let rules_path = dir.path().join("rules.txt");
        std::fs::write(
            &rules_path,
            r#"
100. General
100. General
100.1. These Magic rules apply to any Magic game.
119.1. Each player begins the game with a starting life total of 20.
"#,
        )
        .unwrap();

        let out = dir.path().join("scenarios");
        let stats = generate_skeletons(&GenerateOptions {
            comp_rules: rules_path,
            out_dir: out.clone(),
            preserve_authored: true,
            sections: None,
        })
        .unwrap();

        assert_eq!(stats.written, 2);
        assert!(out.join("100").join("cr_100_1.toml").exists());
        assert!(out.join("119").join("cr_119_1.toml").exists());
    }

    #[test]
    fn preserve_authored_keeps_unparseable_existing_file() {
        let dir = tempdir().unwrap();
        let rules_path = dir.path().join("rules.txt");
        std::fs::write(
            &rules_path,
            "100. General\n100. General\n119.1. Each player begins with 20 life.\n",
        )
        .unwrap();

        let out = dir.path().join("scenarios");
        let target = out.join("119").join("cr_119_1.toml");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        // Deliberately corrupt / hand-authored-in-unknown-shape content.
        let sentinel = "this is not valid = = = toml @@@ authored";
        std::fs::write(&target, sentinel).unwrap();

        let stats = generate_skeletons(&GenerateOptions {
            comp_rules: rules_path,
            out_dir: out.clone(),
            preserve_authored: true,
            sections: None,
        })
        .unwrap();

        // The unparseable file must be preserved untouched, not overwritten.
        assert_eq!(stats.preserved, 1, "unparseable file should be preserved");
        assert_eq!(stats.written, 0, "must not overwrite unparseable file");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), sentinel);
    }
}
