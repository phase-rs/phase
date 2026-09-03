use std::collections::BTreeMap;
use std::path::Path;

use draft_core::pack_generator::PackGenerator;
use draft_core::set_pool::LimitedSetPool;
use draft_core::types::SetLayout;

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

    /// The pool entry for `set_code`, matched case-insensitively.
    pub fn pool_for_set(&self, set_code: &str) -> Option<&LimitedSetPool> {
        self.pools.get(&set_code.to_lowercase())
    }

    /// A generator that opens one booster per entry of `set_codes`, in pack
    /// order. Names the first code with no pool data rather than silently
    /// dropping its packs; the same code may appear more than once, and its
    /// pool is still carried once.
    ///
    /// The whole sequence resolves here because a draft's boosters are a
    /// per-pack property (`DraftSource::Set`) — a multi-set pod that resolved
    /// only its first code would deal every later pack from the wrong set.
    pub fn generator_for_sequence(&self, set_codes: &[String]) -> Result<PackGenerator, String> {
        let mut pools: Vec<LimitedSetPool> = Vec::new();
        for code in set_codes {
            let pool = self
                .pool_for_set(code)
                .ok_or_else(|| format!("No draft pool data for set: {code}"))?;
            if !pools.iter().any(|held| held.code == pool.code) {
                pools.push(pool.clone());
            }
        }
        PackGenerator::for_sequence(pools, set_codes).map_err(|e| e.to_string())
    }

    /// Resolve and validate a private Chaos assignment once at server
    /// admission. The returned layout is persisted on the core session; this
    /// method never accepts client-supplied assignments.
    pub fn resolve_chaos_layout(
        &self,
        candidate_codes: &[String],
        pod_size: u8,
        pack_count: u8,
        assignment_seed: u64,
    ) -> Result<(SetLayout, u8), String> {
        let assignments = PackGenerator::chaos_assignments(
            candidate_codes,
            pod_size,
            pack_count,
            assignment_seed,
        )
        .map_err(|error| error.to_string())?;
        self.generator_for_chaos(candidate_codes, &assignments, pod_size, pack_count)?;
        // `generator_for_chaos` validates that every candidate has exactly one
        // declared pack size and that the sizes agree, so the first candidate
        // is now a sound session-level size representative.
        let cards_per_pack = self
            .pool_for_set(
                candidate_codes
                    .first()
                    .ok_or_else(|| "A Chaos pod must name at least one set".to_string())?,
            )
            .and_then(LimitedSetPool::cards_per_pack)
            .ok_or_else(|| {
                "Chaos candidate has no single MTGJSON pack size across its booster variants"
                    .to_string()
            })?;
        Ok((
            SetLayout::Chaos {
                candidate_codes: candidate_codes.to_vec(),
                assignments,
            },
            cards_per_pack,
        ))
    }

    /// Rebuild a generator from the server-persisted Chaos layout at draft
    /// start. Candidate preflight stays identical to admission, so a resumed
    /// session cannot discover a different pack compatibility rule mid-draft.
    pub fn generator_for_chaos(
        &self,
        candidate_codes: &[String],
        assignments: &[Vec<String>],
        pod_size: u8,
        pack_count: u8,
    ) -> Result<PackGenerator, String> {
        let mut pools: Vec<LimitedSetPool> = Vec::new();
        for code in candidate_codes {
            let pool = self
                .pool_for_set(code)
                .ok_or_else(|| format!("No draft pool data for set: {code}"))?;
            if !pools
                .iter()
                .any(|held| held.code.eq_ignore_ascii_case(&pool.code))
            {
                pools.push(pool.clone());
            }
        }
        PackGenerator::for_chaos(pools, candidate_codes, assignments, pod_size, pack_count)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use draft_core::set_pool::{PackSlot, PackVariant};

    use super::*;

    fn pool(code: &str, cards_per_pack: u8) -> LimitedSetPool {
        LimitedSetPool {
            code: code.to_string(),
            name: format!("Set {code}"),
            release_date: None,
            pack_variants: vec![PackVariant {
                contents: vec![PackSlot {
                    slot: "common".to_string(),
                    count: cards_per_pack,
                    choices: Vec::new(),
                }],
                weight: 1,
            }],
            pack_variants_total_weight: 1,
            sheets: BTreeMap::new(),
            prints: Vec::new(),
            basic_lands: Vec::new(),
        }
    }

    fn in_memory_pools(entries: impl IntoIterator<Item = (&'static str, u8)>) -> DraftPools {
        DraftPools {
            pools: entries
                .into_iter()
                .map(|(code, cards_per_pack)| (code.to_lowercase(), pool(code, cards_per_pack)))
                .collect(),
        }
    }

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
        assert!(pools.pool_for_set("TST").is_some());
        assert!(pools.pool_for_set("tst").is_some());
        assert!(pools.generator_for_sequence(&["TST".to_string()]).is_ok());
        assert!(pools
            .generator_for_sequence(&["missing".to_string()])
            .is_err());
    }

    /// A pod may draft the same set in several packs and different sets in
    /// others; both must resolve off one pool map, and a code with no data must
    /// name itself rather than yielding a short draft.
    #[test]
    fn resolves_a_repeated_and_mixed_pack_sequence() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "{}", two_set_pool_json()).unwrap();

        let pools = DraftPools::from_path(file.path()).unwrap();

        assert!(pools
            .generator_for_sequence(&["AAA".to_string(), "BBB".to_string(), "AAA".to_string()])
            .is_ok());
        assert!(pools
            .generator_for_sequence(&["aaa".to_string(), "bbb".to_string()])
            .is_ok());
        // `PackGenerator` derives no `Debug`, so the error is read off the
        // `Err` arm rather than through `unwrap_err`.
        assert_eq!(
            pools
                .generator_for_sequence(&["AAA".to_string(), "ZZZ".to_string()])
                .err(),
            Some("No draft pool data for set: ZZZ".to_string())
        );
        assert!(pools.generator_for_sequence(&[]).is_err());
    }

    #[test]
    fn chaos_admission_persists_exact_server_generated_assignments() {
        let pools = in_memory_pools([("AAA", 15), ("BBB", 15)]);
        let candidates = vec!["AAA".to_string(), "BBB".to_string()];

        let (layout, cards_per_pack) = pools
            .resolve_chaos_layout(&candidates, 3, 2, 7)
            .expect("equal-size candidates are a valid Chaos source");

        assert_eq!(cards_per_pack, 15);
        let SetLayout::Chaos {
            candidate_codes,
            assignments,
        } = layout
        else {
            panic!("Chaos admission must persist a Chaos layout");
        };
        assert_eq!(candidate_codes, candidates);
        assert_eq!(assignments.len(), 3);
        assert!(assignments.iter().all(|rounds| rounds.len() == 2));
        assert!(assignments
            .iter()
            .flatten()
            .all(|code| ["AAA", "BBB"].contains(&code.as_str())));
    }

    #[test]
    fn chaos_admission_rejects_different_candidate_pack_sizes() {
        let pools = in_memory_pools([("AAA", 15), ("BBB", 14)]);
        let error = pools
            .resolve_chaos_layout(&["AAA".to_string(), "BBB".to_string()], 2, 3, 9)
            .expect_err("Chaos cannot mix candidates with different pack sizes");

        assert!(error.contains("share one MTGJSON pack size"), "{error}");
    }

    fn two_set_pool_json() -> String {
        let entry = |code: &str| {
            format!(
                r#""{code}": {{
                    "code": "{code}",
                    "name": "Set {code}",
                    "release_date": null,
                    "pack_variants": [],
                    "pack_variants_total_weight": 0,
                    "sheets": {{}},
                    "prints": [],
                    "basic_lands": []
                }}"#
            )
        };
        format!("{{{}, {}}}", entry("AAA"), entry("BBB"))
    }
}
