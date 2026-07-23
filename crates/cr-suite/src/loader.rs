//! Load CR scenario fixtures from disk.

use std::path::{Path, PathBuf};

use thiserror::Error;
use walkdir::WalkDir;

use crate::schema::ScenarioFile;

#[derive(Debug, Error)]
pub enum ScenarioLoadError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("toml parse error at {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
}

/// Load a single scenario TOML file.
pub fn load_scenario_file(path: &Path) -> Result<ScenarioFile, ScenarioLoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| ScenarioLoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| ScenarioLoadError::Toml {
        path: path.to_path_buf(),
        source,
    })
}

/// Recursively load all `*.toml` scenario fixtures under `root`.
pub fn load_scenarios(root: &Path) -> Result<Vec<(PathBuf, ScenarioFile)>, ScenarioLoadError> {
    let mut out = Vec::new();
    if !root.exists() {
        return Ok(out);
    }

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
    {
        let path = entry.path().to_path_buf();
        let scenario = load_scenario_file(&path)?;
        out.push((path, scenario));
    }

    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ScenarioFile, ScenarioStatus};
    use tempfile::tempdir;

    #[test]
    fn loads_sorted_fixtures() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("704");
        std::fs::create_dir_all(&a).unwrap();
        let f = ScenarioFile::skeleton("704.5a", 704, "life", "SBA");
        std::fs::write(
            a.join("cr_704_5a.toml"),
            crate::generate::render_fixture(&f),
        )
        .unwrap();

        let loaded = load_scenarios(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].1.status, ScenarioStatus::Skeleton);
    }
}
