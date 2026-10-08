//! Engine surface inventory generator.
//!
//! Walks `crates/engine/src/` via `syn`, enumerates every `pub enum` and
//! its variants with file:line, doc comments, and CR annotations. Auto-detects
//! sibling-cluster smells (variants sharing a name root that look like
//! parameterization candidates per the workspace "Parameterize, don't proliferate"
//! principle).
//!
//! Output: `data/engine-inventory.json` — the canonical inventory consumed by
//! the `add-engine-variant` skill's Stage 1 existence/parameterization check.
//! Replaces hand-maintained CLAUDE.md lists that drifted with codebase changes.
//!
//! Regenerate: `cargo engine-inventory` (alias in `.cargo/config.toml`).

use anyhow::{Context, Result};
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::{Attribute, Expr, ExprLit, Fields, Item, ItemEnum, Lit, Meta, MetaNameValue};
use walkdir::WalkDir;

#[derive(Serialize)]
struct Inventory {
    /// Workspace-relative paths scanned for this inventory.
    sources: Vec<String>,
    /// Total enums catalogued.
    enum_count: usize,
    /// Total variants catalogued.
    variant_count: usize,
    /// Sibling-cluster smell summary: enums with parameterization candidates.
    smell_summary: Vec<ClusterSmell>,
    /// Full per-enum catalogue.
    enums: BTreeMap<String, EnumEntry>,
}

#[derive(Serialize)]
struct EnumEntry {
    /// The bare ident. Carried as a field because the map key is module-qualified: without it
    /// a quoted grep (`"LayoutKind"`) — a form the discoverability gate is written in — went
    /// from 1 hit to 0. MEASURED on this tree before the field was added.
    name: String,
    file: String,
    line: usize,
    doc: String,
    cr_refs: Vec<String>,
    variants: Vec<VariantEntry>,
    sibling_clusters: Vec<SiblingCluster>,
}

#[derive(Serialize)]
struct VariantEntry {
    name: String,
    line: usize,
    /// "unit", "tuple", or "struct"
    kind: &'static str,
    /// For struct variants: field names. For tuple variants: empty.
    field_names: Vec<String>,
    doc: String,
    cr_refs: Vec<String>,
}

#[derive(Serialize)]
struct SiblingCluster {
    /// Shared name root (longest common prefix among 2+ variants).
    shared_root: String,
    members: Vec<String>,
    /// HIGH = 3+ members and clear scope/qualifier suffixes (Opponent/Target/All).
    /// MEDIUM = 2-3 members with shared root.
    /// LOW = 2 members, ambiguous.
    smell_score: &'static str,
}

#[derive(Serialize)]
struct ClusterSmell {
    enum_name: String,
    cluster: SiblingCluster,
}

/// Every directory whose `pub enum`s are engine surface a variant proposal must be able to
/// discover. CLAUDE.md makes an inventory grep the mandatory discoverability gate before
/// proposing a variant and scopes it to "any other engine enum", so the walk is the WHOLE
/// engine crate rather than a hand-kept subset: a `types/` + `analysis/` walk leaves 85
/// top-level `pub enum`s under `crates/engine/src` structurally invisible to the gate
/// (`game/` 61, `ai_support/` 13, `parser/` 7, `database/` 4). That split is the standing
/// reason for the walk root, and it is re-derivable at any time by grouping the emitted
/// `file` fields. One root is also shorter than the list it replaces.
///
/// THE TOTALS ARE A SNAPSHOT, NOT A STANDING FACT — deliberately stated apart from the split
/// above, because pinning the two together is what let one stale digit rot the other. Measured
/// 2026-08-13: 655 enums, 5320 variants. If they move, RE-TAKE them rather than re-label; and
/// prefer not to read them here at all, since every run prints its own totals on the success
/// line, which is the only current answer. What must stay true is the invariant those totals
/// are evidence for: the catalogue holds one entry per DECLARATION, so `enum_count` and the
/// declaration count are the same number.
///
/// THE CATALOGUE KEY IS MODULE-QUALIFIED (`types::card::LayoutKind`), because the widened walk
/// makes ident collisions reachable and an ident key drops one side of every collision. Measured
/// on this tree: `LayoutKind` is declared in BOTH `types/card.rs` and `database/synthesis.rs`,
/// and the two are NOT variant-identical — `Omen` exists only in `card.rs`, `Specialize` only in
/// `synthesis.rs`. An ident key therefore answered the discoverability gate with ONE enum's
/// variant list, so an existence check for the shadowed variant returned a FALSE NEGATIVE — the
/// exact outcome CLAUDE.md makes this grep mandatory to prevent. It also made the output
/// irreproducible: which side survived was decided by `readdir` order.
///
/// Grep still works on the qualified key: `LayoutKind` is a substring of
/// `types::card::LayoutKind`, so the skill's `rg "<concept>" data/engine-inventory.json`
/// existence check is unaffected. Unique keys additionally make the `BTreeMap` order — and so
/// the emitted JSON — a function of the source tree alone.
const TARGET_DIRS: &[&str] = &["crates/engine/src"];
const OUTPUT: &str = "data/engine-inventory.json";

fn main() -> Result<()> {
    let workspace_root = find_workspace_root()?;
    let output = workspace_root.join(OUTPUT);

    let cr_re = Regex::new(r"CR \d{3}(?:\.\d+[a-z]?)?")?;

    let mut enums: BTreeMap<String, EnumEntry> = BTreeMap::new();
    let mut sources: Vec<String> = Vec::new();

    for dir in TARGET_DIRS {
        let target = workspace_root.join(dir);
        // Sorted so the emitted `sources` list — and the walk itself — does not depend on
        // `readdir` order.
        //
        // ALL THREE per-file failures propagate: walk, read, AND parse. The parse arm used to
        // `continue` past unparseable files as "likely WIP" while `sources.push` ran BEFORE
        // it, so such a file was LISTED as scanned while contributing zero enums — the
        // inventory reported success over a file it never read, and the `add-engine-variant`
        // existence gate got a FALSE NEGATIVE indistinguishable from a true one. A WIP file is
        // a reason to fix the file, not to hand that gate a silent hole.
        for entry in WalkDir::new(&target).sort_by_file_name() {
            let entry = entry.with_context(|| format!("walk {}", target.display()))?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let module = module_path(path.strip_prefix(&target).unwrap_or(path));
            let rel = path.strip_prefix(&workspace_root).unwrap_or(path);

            let content =
                fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
            let file =
                syn::parse_file(&content).with_context(|| format!("parse {}", path.display()))?;
            // AFTER the parse, not before. The ordering is unobservable today (every failure
            // above aborts the run and writes nothing), so this is the structural form of the
            // fix rather than the fix: `sources` means "files this inventory actually read",
            // and position now enforces that instead of the error handling continuing to.
            sources.push(rel.display().to_string());

            for item in &file.items {
                if let Item::Enum(e) = item {
                    if !is_pub(&e.vis) {
                        continue;
                    }
                    let entry = build_enum_entry(e, &content, rel, &cr_re);
                    let key = if module.is_empty() {
                        e.ident.to_string()
                    } else {
                        format!("{module}::{}", e.ident)
                    };
                    // Rust cannot declare two same-named top-level enums in one file, so a
                    // collision here means the key stopped identifying a declaration. Loud,
                    // because a silent overwrite is the defect this key shape exists to close.
                    if let Some(prev) = enums.insert(key.clone(), entry) {
                        anyhow::bail!("duplicate inventory key {key}: already held {}", prev.file);
                    }
                }
            }
        }
    }

    // Compute global smell summary.
    let mut smell_summary: Vec<ClusterSmell> = Vec::new();
    for (name, entry) in &enums {
        for c in &entry.sibling_clusters {
            smell_summary.push(ClusterSmell {
                enum_name: name.clone(),
                cluster: SiblingCluster {
                    shared_root: c.shared_root.clone(),
                    members: c.members.clone(),
                    smell_score: c.smell_score,
                },
            });
        }
    }

    let variant_count: usize = enums.values().map(|e| e.variants.len()).sum();
    let inventory = Inventory {
        sources,
        enum_count: enums.len(),
        variant_count,
        smell_summary,
        enums,
    };

    fs::create_dir_all(output.parent().unwrap())?;
    let json = serde_json::to_string_pretty(&inventory)?;
    fs::write(&output, format!("{json}\n"))?;
    println!(
        "wrote {} enums, {} variants → {}",
        inventory.enum_count,
        inventory.variant_count,
        output.display()
    );
    Ok(())
}

fn find_workspace_root() -> Result<PathBuf> {
    let mut dir = std::env::current_dir()?;
    loop {
        if dir.join("Cargo.toml").exists() {
            // Verify it's the workspace by checking for [workspace]
            let toml = fs::read_to_string(dir.join("Cargo.toml"))?;
            if toml.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            anyhow::bail!("could not find workspace root");
        }
    }
}

/// The module path of a source file relative to the walk root: `types/card.rs` → `types::card`,
/// `game/mod.rs` → `game`, `lib.rs` → `` (crate root).
///
/// Derived from the PATH rather than from `syn`, which is exact here because only top-level
/// `file.items` are catalogued — an enum inside an inline `mod` is not walked at all.
fn module_path(rel_to_root: &Path) -> String {
    let mut parts: Vec<String> = rel_to_root
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some("mod" | "lib" | "main") = parts.last().map(String::as_str) {
        parts.pop();
    }
    parts.join("::")
}

fn is_pub(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

fn build_enum_entry(e: &ItemEnum, _source: &str, rel_path: &Path, cr_re: &Regex) -> EnumEntry {
    let line = e.ident.span().start().line;
    let doc = extract_docs(&e.attrs);
    let cr_refs = extract_crs(&doc, cr_re);

    let variants: Vec<VariantEntry> = e
        .variants
        .iter()
        .map(|v| {
            let line = v.ident.span().start().line;
            let v_doc = extract_docs(&v.attrs);
            let v_crs = extract_crs(&v_doc, cr_re);
            let (kind, field_names) = match &v.fields {
                Fields::Unit => ("unit", Vec::new()),
                Fields::Unnamed(_) => ("tuple", Vec::new()),
                Fields::Named(named) => (
                    "struct",
                    named
                        .named
                        .iter()
                        .filter_map(|f| f.ident.as_ref().map(|i| i.to_string()))
                        .collect(),
                ),
            };
            VariantEntry {
                name: v.ident.to_string(),
                line,
                kind,
                field_names,
                doc: v_doc,
                cr_refs: v_crs,
            }
        })
        .collect();

    let sibling_clusters = detect_clusters(&variants);

    EnumEntry {
        name: e.ident.to_string(),
        file: rel_path.display().to_string(),
        line,
        doc,
        cr_refs,
        variants,
        sibling_clusters,
    }
}

/// Detect parameterization candidates by shared name root.
///
/// Heuristic:
/// - Group variants whose names share a >=4-char prefix or contain a known
///   scope suffix (Opponent, Target, All, Each, Self, Source, Triggering).
/// - 3+ members with multiple distinct scope qualifiers → HIGH smell
/// - 3+ members sharing a long root → MEDIUM smell
/// - 2 members sharing a root → LOW (worth noting, not necessarily a bug)
fn detect_clusters(variants: &[VariantEntry]) -> Vec<SiblingCluster> {
    let scope_qualifiers = [
        "Opponent",
        "Target",
        "All",
        "Each",
        "Self",
        "Source",
        "Triggering",
        "Controller",
        "Active",
        "Defending",
        "Attacking",
    ];

    // Build groups by stripping known qualifiers from variant names.
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for v in variants {
        let stripped = strip_qualifiers(&v.name, &scope_qualifiers);
        if !stripped.is_empty() && stripped != v.name {
            groups.entry(stripped).or_default().push(v.name.clone());
        }
    }
    // Also include variants whose unstripped name matches a stripped form
    // (e.g., LifeTotal sits next to OpponentLifeTotal).
    for v in variants {
        for (root, members) in groups.iter_mut() {
            if &v.name == root && !members.contains(&v.name) {
                members.push(v.name.clone());
            }
        }
    }

    let mut clusters: Vec<SiblingCluster> = Vec::new();
    for (root, members) in groups {
        if members.len() < 2 {
            continue;
        }
        let qualifier_count = members
            .iter()
            .filter(|m| {
                scope_qualifiers
                    .iter()
                    .any(|q| m.starts_with(q) || m.contains(q))
            })
            .count();
        let score = if members.len() >= 3 && qualifier_count >= 2 {
            "HIGH"
        } else if members.len() >= 3 {
            "MEDIUM"
        } else {
            "LOW"
        };
        let mut sorted = members;
        sorted.sort();
        clusters.push(SiblingCluster {
            shared_root: root,
            members: sorted,
            smell_score: score,
        });
    }
    clusters.sort_by(|a, b| {
        b.members
            .len()
            .cmp(&a.members.len())
            .then_with(|| a.shared_root.cmp(&b.shared_root))
    });
    clusters
}

fn strip_qualifiers(name: &str, qualifiers: &[&str]) -> String {
    for q in qualifiers {
        if let Some(rest) = name.strip_prefix(q) {
            // require remaining length to be substantive
            if rest.len() >= 4 {
                return rest.to_string();
            }
        }
        if let Some(rest) = name.strip_suffix(q) {
            if rest.len() >= 4 {
                return rest.to_string();
            }
        }
    }
    name.to_string()
}

fn extract_docs(attrs: &[Attribute]) -> String {
    let mut out = String::new();
    for a in attrs {
        if !a.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(MetaNameValue {
            value: Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }),
            ..
        }) = &a.meta
        {
            let line = s.value();
            let trimmed = line.strip_prefix(' ').unwrap_or(&line);
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(trimmed);
        }
    }
    out
}

fn extract_crs(doc: &str, cr_re: &Regex) -> Vec<String> {
    let mut out: Vec<String> = cr_re
        .find_iter(doc)
        .map(|m| m.as_str().to_string())
        .collect();
    out.sort();
    out.dedup();
    out
}
