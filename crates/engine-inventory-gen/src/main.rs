//! Engine surface inventory generator.
//!
//! Walks `crates/engine/src/types/` via `syn`, enumerates every `pub enum` and
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

const TARGET_DIR: &str = "crates/engine/src/types";
const OUTPUT: &str = "data/engine-inventory.json";

fn main() -> Result<()> {
    let workspace_root = find_workspace_root()?;
    let target = workspace_root.join(TARGET_DIR);
    let output = workspace_root.join(OUTPUT);

    let cr_re = Regex::new(r"CR \d{3}(?:\.\d+[a-z]?)?")?;

    let mut enums: BTreeMap<String, EnumEntry> = BTreeMap::new();
    let mut sources: Vec<String> = Vec::new();

    for entry in WalkDir::new(&target).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().map_or(true, |ext| ext != "rs") {
            continue;
        }
        let rel = path.strip_prefix(&workspace_root).unwrap_or(path);
        sources.push(rel.display().to_string());

        let content = fs::read_to_string(path)
            .with_context(|| format!("read {}", path.display()))?;
        let file = match syn::parse_file(&content) {
            Ok(f) => f,
            Err(_) => continue, // skip unparseable files (likely WIP)
        };

        for item in &file.items {
            if let Item::Enum(e) = item {
                if !is_pub(&e.vis) {
                    continue;
                }
                let entry = build_enum_entry(e, &content, rel, &cr_re);
                enums.insert(e.ident.to_string(), entry);
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

fn is_pub(vis: &syn::Visibility) -> bool {
    matches!(vis, syn::Visibility::Public(_))
}

fn build_enum_entry(
    e: &ItemEnum,
    _source: &str,
    rel_path: &Path,
    cr_re: &Regex,
) -> EnumEntry {
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
            groups
                .entry(stripped)
                .or_default()
                .push(v.name.clone());
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
            value: Expr::Lit(ExprLit { lit: Lit::Str(s), .. }),
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
    let mut out: Vec<String> = cr_re.find_iter(doc).map(|m| m.as_str().to_string()).collect();
    out.sort();
    out.dedup();
    out
}

