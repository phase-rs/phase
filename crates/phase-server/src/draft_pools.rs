use std::collections::BTreeMap;
use std::path::Path;

use draft_core::pack_generator::PackGenerator;
use draft_core::set_pool::LimitedSetPool;

#[derive(Default)]
pub struct DraftPools {
    pools: BTreeMap<String, LimitedSetPool>,
}

impl DraftPools {
    pub fn from_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        // Read the whole file, then parse from the in-memory slice. serde_json's
        // `from_reader` over an unbuffered `File` issues a read syscall per token
        // and is pathologically slow on Windows, where per-syscall cost is high:
        // parsing this multi-megabyte pool file that way stalled the native-engine
        // server past the desktop shell's 20s health-check budget, so games fell
        // back to the in-browser engine. `from_slice` parses contiguous memory with
        // no per-read overhead (mirrors the `BufReader` already used in card_db.rs).
        let bytes = std::fs::read(path)?;
        let pools: BTreeMap<String, LimitedSetPool> = serde_json::from_slice(&bytes)?;
        let pools = pools
            .into_iter()
            .map(|(code, pool)| (code.to_lowercase(), pool))
            .collect();
        Ok(Self { pools })
    }

    pub fn len(&self) -> usize {
        self.pools.len()
    }

    pub fn contains_set(&self, set_code: &str) -> bool {
        self.pools.contains_key(&set_code.to_lowercase())
    }

    pub fn generator_for_set(&self, set_code: &str) -> Option<PackGenerator> {
        self.pools
            .get(&set_code.to_lowercase())
            .cloned()
            .map(PackGenerator::new)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn loads_pools_by_case_insensitive_set_code() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(
            file,
            r#"{{
                "TST": {{
                    "code": "TST",
                    "name": "Test Set",
                    "release_date": null,
                    "pack_variants": [],
                    "pack_variants_total_weight": 0,
                    "sheets": {{}},
                    "prints": [],
                    "basic_lands": []
                }}
            }}"#
        )
        .unwrap();

        let pools = DraftPools::from_path(file.path()).unwrap();

        assert_eq!(pools.len(), 1);
        assert!(pools.contains_set("TST"));
        assert!(pools.contains_set("tst"));
        assert!(pools.generator_for_set("TST").is_some());
        assert!(pools.generator_for_set("missing").is_none());
    }
}
