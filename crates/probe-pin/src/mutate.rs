//! §7 step 8's first half: sequential mutation, materialization, and the two gates that make
//! a mount-reach readback meaningful.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::manifest::{self, Mutation, Probe};
use crate::Abort;

/// One `mount --bind <mutant> <target>` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub mutant: PathBuf,
    pub target: PathBuf,
}

/// Apply every mutation in declaration order, materialize the result outside the workspace,
/// and gate it.
///
/// Each `Replace`'s `count == 1` check runs against the RUNNING text for that file, so a
/// two-op sequence whose second `find` only exists after the first op is expressible. The
/// no-op gate is per FILE on MATERIALIZED bytes — one comparison that covers `Replace`,
/// `Prepend { repeat: 0 }`, `Prepend { text: "" }` and sequences that cancel out.
pub fn apply(probe: &Probe, root: &Path, scratch: &Path) -> anyhow::Result<Vec<Mount>> {
    let mut running: Vec<(String, String, String)> = Vec::new(); // (rel, original, current)
    for (index, mutation) in probe.mutations.iter().enumerate() {
        for rel in mutation.files() {
            if !running.iter().any(|(f, _, _)| f == rel) {
                let text = std::fs::read_to_string(root.join(rel)).with_context(|| {
                    format!(
                        "probe-pin: {} cannot read {}",
                        probe.id,
                        root.join(rel).display()
                    )
                })?;
                running.push((rel.to_string(), text.clone(), text));
            }
        }
        match mutation {
            Mutation::Replace {
                file,
                find,
                replace,
            } => {
                let slot = running
                    .iter_mut()
                    .find(|(f, _, _)| f == file)
                    .expect("seeded above");
                let found = slot.2.matches(find.as_str()).count();
                if found != 1 {
                    return Err(Abort::FindCount {
                        probe: probe.id.clone(),
                        index,
                        file: PathBuf::from(file),
                        found,
                    }
                    .into());
                }
                slot.2 = slot.2.replacen(find.as_str(), replace, 1);
            }
            Mutation::Prepend {
                files,
                text,
                repeat,
            } => {
                let pad = text.repeat(*repeat as usize);
                for file in files {
                    let slot = running
                        .iter_mut()
                        .find(|(f, _, _)| f == file)
                        .expect("seeded above");
                    slot.2 = format!("{pad}{}", slot.2);
                }
            }
        }
    }

    let mut mounts = Vec::new();
    for (rel, original, mutant) in &running {
        // The write side of the same containment rule `validate_paths` applies to the read
        // side, asserted where the base finally exists: the scratch dir is created at §7 step 5.
        // Both joined keys go in at once — `probe.id` is lexically a plain name and `rel` is
        // lexically relative, and neither says where the join RESOLVES.
        let dest = manifest::resolve_contained(scratch, &format!("{}/{rel}", probe.id))
            .map_err(|why| anyhow::anyhow!("probe-pin: {} materializes its mutant outside probe-pin's scratch directory: {why}. The mutant path is <scratch>/<probe.id>/<file>, and a join that resolves out of the scratch dir writes into the tree probe-pin is measuring — the one thing this tool must never do. Aborting.", probe.id))?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Abort::MaterializeFailed {
                path: parent.to_path_buf(),
                err: e.to_string(),
            })?;
        }
        std::fs::write(&dest, mutant).map_err(|e| Abort::MaterializeFailed {
            path: dest.clone(),
            err: e.to_string(),
        })?;
        let materialized = std::fs::read(&dest).map_err(|e| Abort::MaterializeFailed {
            path: dest.clone(),
            err: e.to_string(),
        })?;
        if materialized == original.as_bytes() {
            return Err(Abort::NoOpMutation {
                probe: probe.id.clone(),
                file: PathBuf::from(rel),
            }
            .into());
        }
        mounts.push(Mount {
            mutant: dest,
            target: root.join(rel),
        });
    }

    assert_counts(probe, &running)?;
    Ok(mounts)
}

/// `[[probe.assert_count]]` against the FINAL mutant text, after all mutations have composed.
///
/// There is no pristine-tree fallback: `validate` refuses an `assert_count` naming a file this
/// probe does not mutate, so every one of them HAS a mutant here. The fallback that used to
/// stand in its place read the unmutated file and reported the result as "in the MUTANT of",
/// which is the message describing text it did not read.
fn assert_counts(probe: &Probe, running: &[(String, String, String)]) -> anyhow::Result<()> {
    for ac in &probe.assert_counts {
        let (_, _, text) = running
            .iter()
            .find(|(f, _, _)| *f == ac.file)
            .expect("validate() proved every assert_count names a file this probe mutates");
        let found = text.matches(ac.text.as_str()).count();
        if found != ac.count {
            return Err(Abort::AssertCount {
                probe: probe.id.clone(),
                file: PathBuf::from(&ac.file),
                text: ac.text.clone(),
                expected: ac.count,
                found,
            }
            .into());
        }
    }
    Ok(())
}
