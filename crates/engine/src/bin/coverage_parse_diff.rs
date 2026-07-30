//! `coverage-parse-diff` — diff the `parse_details` parse-trees of two
//! `coverage-data.json` snapshots and emit a clustered, review-oriented report.
//!
//! Purpose: the existing coverage-regression gate only reports `supported`
//! flips (Unimplemented <-> Supported). This tool surfaces *field-level* parse
//! changes — a target filter that gained a clause, an amount that changed from
//! Fixed to Variable, a condition that was swapped — even when `supported`
//! stays `true`. The clustered Markdown is posted as a PR comment so a
//! reviewing LLM gets the structural delta without re-deriving it.
//!
//! Baseline semantics live in CI (the caller passes the PR's merge-base
//! snapshot, never a lagging deployed-main snapshot); this binary is a pure
//! function of the two files it is handed.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::process;

use engine::game::coverage::{CardCoverageResult, ParseCategory, ParsedItem};
use serde::{Deserialize, Serialize};

/// Minimal view of `coverage-data.json` — only the per-card array is read; the
/// summary's other fields are ignored by serde, decoupling us from their shape.
#[derive(Deserialize)]
struct CoverageFile {
    #[serde(default)]
    cards: Vec<CardCoverageResult>,
}

fn cat_str(c: &ParseCategory) -> &'static str {
    match c {
        ParseCategory::Keyword => "keyword",
        ParseCategory::Ability => "ability",
        ParseCategory::Trigger => "trigger",
        ParseCategory::Static => "static",
        ParseCategory::Replacement => "replacement",
        ParseCategory::Cost => "cost",
    }
}

/// Kind of a single field-level change within a card's parse tree.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ChangeKind {
    FieldChanged,
    ItemAdded,
    ItemRemoved,
    SupportFlip,
}

impl ChangeKind {
    fn label(self) -> &'static str {
        match self {
            ChangeKind::FieldChanged => "field",
            ChangeKind::ItemAdded => "added",
            ChangeKind::ItemRemoved => "removed",
            ChangeKind::SupportFlip => "support",
        }
    }

    fn section_heading(self) -> &'static str {
        match self {
            ChangeKind::ItemAdded => "🟢 Added",
            ChangeKind::ItemRemoved => "🔴 Removed",
            ChangeKind::FieldChanged => "🟡 Modified fields",
            ChangeKind::SupportFlip => "🔵 Support status",
        }
    }

    fn marker(self) -> &'static str {
        match self {
            ChangeKind::ItemAdded => "➕",
            ChangeKind::ItemRemoved => "➖",
            ChangeKind::FieldChanged => "🔄",
            ChangeKind::SupportFlip => "↕️",
        }
    }
}

/// One field-level change, attributed to a card.
struct Change {
    category: &'static str,
    label: String,
    kind: ChangeKind,
    key: String,
    before: String,
    after: String,
}

/// Canonical identity of an item for multiset exact-match: category, label,
/// source_text, supported, sorted details, and recursively-canonicalized
/// children (sorted). Two items with the same canon string are "unchanged".
fn canon(item: &ParsedItem) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        "{}|{}|{}|{}|",
        cat_str(&item.category),
        item.label,
        item.source_text.as_deref().unwrap_or(""),
        item.supported,
    );
    let mut dets: Vec<&(String, String)> = item.details.iter().collect();
    dets.sort();
    s.push('{');
    for (k, v) in dets {
        let _ = write!(s, "{k}={v};");
    }
    s.push_str("}[");
    let mut kids: Vec<String> = item.children.iter().map(canon).collect();
    kids.sort();
    for k in kids {
        s.push_str(&k);
        s.push(',');
    }
    s.push(']');
    s
}

/// Weak key for residual reconciliation — discards `details`/`children` (the
/// fields a value-change lives in) so paired items can be field-diffed.
fn weak_key(item: &ParsedItem) -> (String, String, String) {
    (
        cat_str(&item.category).to_string(),
        item.label.clone(),
        item.source_text.clone().unwrap_or_default(),
    )
}

/// Compact one-line summary of an item (for add/remove change values).
fn summarize(item: &ParsedItem) -> String {
    if item.details.is_empty() {
        item.label.clone()
    } else {
        let mut dets: Vec<&(String, String)> = item.details.iter().collect();
        dets.sort();
        let body: Vec<String> = dets.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{} ({})", item.label, body.join(", "))
    }
}

/// Diff a matched item pair: support flip, detail key adds/removes/changes,
/// then recurse into children.
fn diff_items(base: &ParsedItem, head: &ParsedItem, out: &mut Vec<Change>) {
    let category = cat_str(&head.category);
    if base.supported != head.supported {
        out.push(Change {
            category,
            label: head.label.clone(),
            kind: ChangeKind::SupportFlip,
            key: String::new(),
            before: base.supported.to_string(),
            after: head.supported.to_string(),
        });
    }
    let bmap: BTreeMap<&str, &str> = base
        .details
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let hmap: BTreeMap<&str, &str> = head
        .details
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    for (k, bv) in &bmap {
        match hmap.get(k) {
            Some(hv) if hv != bv => out.push(Change {
                category,
                label: head.label.clone(),
                kind: ChangeKind::FieldChanged,
                key: (*k).to_string(),
                before: (*bv).to_string(),
                after: (*hv).to_string(),
            }),
            None => out.push(Change {
                category,
                label: head.label.clone(),
                kind: ChangeKind::FieldChanged,
                key: (*k).to_string(),
                before: (*bv).to_string(),
                after: "∅".to_string(),
            }),
            _ => {}
        }
    }
    for (k, hv) in &hmap {
        if !bmap.contains_key(k) {
            out.push(Change {
                category,
                label: head.label.clone(),
                kind: ChangeKind::FieldChanged,
                key: (*k).to_string(),
                before: "∅".to_string(),
                after: (*hv).to_string(),
            });
        }
    }
    diff_level(&base.children, &head.children, out);
}

/// Diff a sibling list (top-level or children): cancel structurally-identical
/// items as a multiset, then reconcile residuals by weak key — pairing leftover
/// items as value-changes and reporting the rest as adds/removes. Cannot
/// mis-pair: ambiguous residuals degrade to truthful add+remove.
fn diff_level(base_items: &[ParsedItem], head_items: &[ParsedItem], out: &mut Vec<Change>) {
    // Cancel exact structural matches as a multiset.
    let mut base_left: Vec<&ParsedItem> = Vec::new();
    let mut head_counts: BTreeMap<String, usize> = BTreeMap::new();
    for h in head_items {
        *head_counts.entry(canon(h)).or_insert(0) += 1;
    }
    for b in base_items {
        let c = canon(b);
        if let Some(n) = head_counts.get_mut(&c) {
            if *n > 0 {
                *n -= 1;
                continue; // structurally identical → unchanged
            }
        }
        base_left.push(b);
    }
    let head_left: Vec<&ParsedItem> = head_items
        .iter()
        .filter(|h| {
            // Keep heads whose canon budget was not consumed by a base match.
            // Recompute remaining budget lazily: a head is "matched" iff its
            // canon still has count earmarked. We decrement here to mirror.
            let c = canon(h);
            match head_counts.get_mut(&c) {
                Some(n) if *n > 0 => {
                    *n -= 1;
                    true
                }
                _ => false,
            }
        })
        .collect();

    // Group residuals by weak key.
    let mut bgroups: BTreeMap<(String, String, String), Vec<&ParsedItem>> = BTreeMap::new();
    let mut hgroups: BTreeMap<(String, String, String), Vec<&ParsedItem>> = BTreeMap::new();
    for b in &base_left {
        bgroups.entry(weak_key(b)).or_default().push(b);
    }
    for h in &head_left {
        hgroups.entry(weak_key(h)).or_default().push(h);
    }
    let mut keys: Vec<(String, String, String)> = bgroups.keys().cloned().collect();
    for k in hgroups.keys() {
        if !bgroups.contains_key(k) {
            keys.push(k.clone());
        }
    }
    for k in keys {
        let bs = bgroups.get(&k).cloned().unwrap_or_default();
        let hs = hgroups.get(&k).cloned().unwrap_or_default();
        let paired = bs.len().min(hs.len());
        for i in 0..paired {
            diff_items(bs[i], hs[i], out);
        }
        for b in bs.iter().skip(paired) {
            out.push(Change {
                category: cat_str(&b.category),
                label: b.label.clone(),
                kind: ChangeKind::ItemRemoved,
                key: String::new(),
                before: summarize(b),
                after: "∅".to_string(),
            });
        }
        for h in hs.iter().skip(paired) {
            out.push(Change {
                category: cat_str(&h.category),
                label: h.label.clone(),
                kind: ChangeKind::ItemAdded,
                key: String::new(),
                before: "∅".to_string(),
                after: summarize(h),
            });
        }
    }
}

/// Replace case-insensitive occurrences of the card name with `~` so a
/// per-card value (e.g. a target naming the card itself) clusters across cards.
fn template(val: &str, card_name: &str) -> String {
    if card_name.is_empty() {
        return val.to_string();
    }
    let lower_val = val.to_lowercase();
    let lower_name = card_name.to_lowercase();
    let mut out = String::with_capacity(val.len());
    let mut idx = 0;
    while let Some(pos) = lower_val[idx..].find(&lower_name) {
        let start = idx + pos;
        out.push_str(&val[idx..start]);
        out.push('~');
        idx = start + lower_name.len();
    }
    out.push_str(&val[idx..]);
    out
}

struct Cluster {
    category: &'static str,
    label: String,
    kind: ChangeKind,
    key: String,
    before: String,
    after: String,
    cards: Vec<String>,
}

fn load(path: &str) -> CoverageFile {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("coverage-parse-diff: cannot read {path}: {e}");
            process::exit(2);
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("coverage-parse-diff: cannot parse {path}: {e}");
            process::exit(2);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut positional: Vec<String> = Vec::new();
    let mut markdown_out: Option<String> = None;
    let mut json_out: Option<String> = None;
    let mut base_sha = String::from("unknown");
    let mut max_clusters = 25usize;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--markdown" => markdown_out = args.next(),
            "--json" => json_out = args.next(),
            "--base-sha" => base_sha = args.next().unwrap_or(base_sha),
            "--max-clusters" => {
                max_clusters = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(max_clusters)
            }
            other => positional.push(other.to_string()),
        }
    }
    if positional.len() != 2 {
        eprintln!("usage: coverage-parse-diff <baseline.json> <head.json> [--base-sha SHA] [--markdown OUT] [--json OUT] [--max-clusters N]");
        process::exit(2);
    }
    let base = load(&positional[0]);
    let head = load(&positional[1]);

    let bmap: BTreeMap<String, &CardCoverageResult> = base
        .cards
        .iter()
        .map(|c| (c.card_name.to_ascii_lowercase(), c))
        .collect();
    let hmap: BTreeMap<String, &CardCoverageResult> = head
        .cards
        .iter()
        .map(|c| (c.card_name.to_ascii_lowercase(), c))
        .collect();

    let mut sig_to_cluster: BTreeMap<(String, String, String, String, String, String), Cluster> =
        BTreeMap::new();
    let mut oracle_changed = 0usize;
    let mut added_cards: Vec<String> = Vec::new();
    let mut removed_cards: Vec<String> = Vec::new();
    let mut changed_card_set: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();

    for (k, h) in &hmap {
        let Some(b) = bmap.get(k) else {
            added_cards.push(h.card_name.clone());
            continue;
        };
        // Oracle-text change → parse legitimately differs for a non-parser
        // reason (errata/reprint). Carve out; do not attribute to the PR.
        if b.oracle_text != h.oracle_text {
            oracle_changed += 1;
            continue;
        }
        let mut changes = Vec::new();
        diff_level(&b.parse_details, &h.parse_details, &mut changes);
        if changes.is_empty() {
            continue;
        }
        changed_card_set.insert(h.card_name.clone());
        for ch in changes {
            let before_t = template(&ch.before, &h.card_name);
            let after_t = template(&ch.after, &h.card_name);
            let sig = (
                ch.category.to_string(),
                ch.label.clone(),
                ch.kind.label().to_string(),
                ch.key.clone(),
                before_t.clone(),
                after_t.clone(),
            );
            let cluster = sig_to_cluster.entry(sig).or_insert_with(|| Cluster {
                category: ch.category,
                label: ch.label.clone(),
                kind: ch.kind,
                key: ch.key.clone(),
                before: before_t,
                after: after_t,
                cards: Vec::new(),
            });
            cluster.cards.push(h.card_name.clone());
        }
    }
    for (k, b) in &bmap {
        if !hmap.contains_key(k) {
            removed_cards.push(b.card_name.clone());
        }
    }

    let mut clusters: Vec<Cluster> = sig_to_cluster.into_values().collect();
    // Dedup card lists within a cluster (a card may hit the same signature
    // more than once via repeated structures) and sort by impact.
    for c in &mut clusters {
        c.cards.sort();
        c.cards.dedup();
    }
    clusters.sort_by(|a, b| {
        b.cards
            .len()
            .cmp(&a.cards.len())
            .then(a.label.cmp(&b.label))
    });

    let md = render_markdown(
        &base_sha,
        &clusters,
        max_clusters,
        changed_card_set.len(),
        oracle_changed,
        &added_cards,
        &removed_cards,
    );
    match &markdown_out {
        Some(p) => {
            if let Err(e) = fs::write(p, &md) {
                eprintln!("coverage-parse-diff: cannot write {p}: {e}");
                process::exit(2);
            }
        }
        None => println!("{md}"),
    }

    if let Some(p) = &json_out {
        let json = render_json(&clusters, &added_cards, &removed_cards, oracle_changed);
        if let Err(e) = fs::write(p, json) {
            eprintln!("coverage-parse-diff: cannot write {p}: {e}");
            process::exit(2);
        }
    }
}

/// Truncate to at most `n` chars, appending `…`. Unimplemented items use their
/// full Oracle fragment as the label, so bound it for display.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let head: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// One-line description of a cluster, shared by the headline list and the
/// `<details>` tail. Omits the (empty) detail key for add/remove/support kinds
/// and bounds long labels/values.
fn describe(c: &Cluster) -> String {
    let label = truncate(&c.label, 80);
    match c.kind {
        ChangeKind::FieldChanged => format!(
            "{}/{} · changed field `{}`: `{}` → `{}`",
            c.category,
            label,
            c.key,
            truncate(&c.before, 120),
            truncate(&c.after, 120),
        ),
        ChangeKind::SupportFlip => {
            let before = if c.before == "true" {
                "supported"
            } else {
                "unsupported"
            };
            let after = if c.after == "true" {
                "supported"
            } else {
                "unsupported"
            };
            format!(
                "{}/{} · support: `{}` → `{}`",
                c.category, label, before, after
            )
        }
        ChangeKind::ItemAdded => {
            format!(
                "{}/{} · added: `{}`",
                c.category,
                label,
                truncate(&c.after, 160)
            )
        }
        ChangeKind::ItemRemoved => {
            format!(
                "{}/{} · removed: `{}`",
                c.category,
                label,
                truncate(&c.before, 160)
            )
        }
    }
}

const CHANGE_KIND_ORDER: [ChangeKind; 4] = [
    ChangeKind::ItemAdded,
    ChangeKind::ItemRemoved,
    ChangeKind::FieldChanged,
    ChangeKind::SupportFlip,
];

fn render_cluster_sections(s: &mut String, clusters: &[Cluster], show_cards: bool) {
    for kind in CHANGE_KIND_ORDER {
        let signature_count = clusters.iter().filter(|c| c.kind == kind).count();
        if signature_count == 0 {
            continue;
        }
        let signature_label = if signature_count == 1 {
            "signature"
        } else {
            "signatures"
        };
        let _ = writeln!(
            s,
            "#### {} ({} {})\n",
            kind.section_heading(),
            signature_count,
            signature_label,
        );

        for c in clusters.iter().filter(|c| c.kind == kind) {
            let card_label = if c.cards.len() == 1 { "card" } else { "cards" };
            let _ = writeln!(
                s,
                "- **{} {}** · {} {}",
                c.cards.len(),
                card_label,
                c.kind.marker(),
                describe(c),
            );
            if show_cards {
                let cards: Vec<&str> = c.cards.iter().take(3).map(String::as_str).collect();
                let more = c.cards.len().saturating_sub(cards.len());
                let _ = write!(s, "  - Affected (first 3): {}", cards.join(", "));
                if more > 0 {
                    let _ = write!(s, " (+{more} more)");
                }
                s.push('\n');
            }
        }
        s.push('\n');
    }
}

#[allow(clippy::too_many_arguments)]
fn render_markdown(
    base_sha: &str,
    clusters: &[Cluster],
    max_clusters: usize,
    changed_cards: usize,
    oracle_changed: usize,
    added: &[String],
    removed: &[String],
) -> String {
    let mut s = String::new();
    s.push_str("<!-- coverage-parse-diff -->\n");
    if clusters.is_empty() && added.is_empty() && removed.is_empty() {
        s.push_str("### Parse changes introduced by this PR\n\n");
        s.push_str("✓ No card-parse changes detected.\n");
        return s;
    }
    let short = base_sha.get(..12).unwrap_or(base_sha);
    let _ = write!(
        s,
        "### Parse changes introduced by this PR · {} card(s), {} signature(s)  (baseline: main `{}`)\n\n",
        changed_cards,
        clusters.len(),
        short,
    );

    let shown = clusters.len().min(max_clusters);
    render_cluster_sections(&mut s, &clusters[..shown], true);

    if clusters.len() > shown {
        let tail = &clusters[shown..];
        let tail_cards: usize = tail.iter().map(|c| c.cards.len()).sum();
        let tail_shown = tail.len().min(200);
        let _ = write!(
            s,
            "<details><summary>… {} more signature(s) ({} card-changes) — showing first {}; see <code>parse-diff.json</code></summary>\n\n",
            tail.len(),
            tail_cards,
            tail_shown,
        );
        render_cluster_sections(&mut s, &tail[..tail_shown], false);
        s.push_str("\n</details>\n\n");
    }

    if oracle_changed > 0 {
        let _ = writeln!(
            s,
            "_{oracle_changed} card(s) had Oracle-text changes (errata/reprint) — excluded as non-parser._",
        );
    }
    if !added.is_empty() {
        let _ = writeln!(s, "_New cards in head: {}._", added.len());
    }
    if !removed.is_empty() {
        let _ = writeln!(s, "_Cards only in baseline: {}._", removed.len());
    }
    s
}

/// Drill-down artifact written to `parse-diff.json`. Serialized by serde — no
/// hand-rolled escaping/joining.
#[derive(Serialize)]
struct DiffReport<'a> {
    oracle_changed: usize,
    added_cards: &'a [String],
    removed_cards: &'a [String],
    clusters: Vec<ClusterJson<'a>>,
}

#[derive(Serialize)]
struct ClusterJson<'a> {
    category: &'a str,
    label: &'a str,
    kind: &'a str,
    key: &'a str,
    before: &'a str,
    after: &'a str,
    count: usize,
    cards: &'a [String],
}

fn render_json(
    clusters: &[Cluster],
    added: &[String],
    removed: &[String],
    oracle_changed: usize,
) -> String {
    let report = DiffReport {
        oracle_changed,
        added_cards: added,
        removed_cards: removed,
        clusters: clusters
            .iter()
            .map(|c| ClusterJson {
                category: c.category,
                label: &c.label,
                kind: c.kind.label(),
                key: &c.key,
                before: &c.before,
                after: &c.after,
                count: c.cards.len(),
                cards: &c.cards,
            })
            .collect(),
    };
    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a childless ability item with the given label/details/support.
    fn item(label: &str, details: &[(&str, &str)], supported: bool) -> ParsedItem {
        ParsedItem {
            category: ParseCategory::Ability,
            label: label.to_string(),
            source_text: None,
            supported,
            details: details
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            children: vec![],
        }
    }

    fn diff(base: &[ParsedItem], head: &[ParsedItem]) -> Vec<Change> {
        let mut out = Vec::new();
        diff_level(base, head, &mut out);
        out
    }

    fn cluster(
        kind: ChangeKind,
        label: &str,
        key: &str,
        before: &str,
        after: &str,
        cards: &[&str],
    ) -> Cluster {
        Cluster {
            category: "ability",
            label: label.to_string(),
            kind,
            key: key.to_string(),
            before: before.to_string(),
            after: after.to_string(),
            cards: cards.iter().map(|card| (*card).to_string()).collect(),
        }
    }

    #[test]
    fn identical_items_produce_no_change() {
        let base = vec![item("DealDamage", &[("target", "creature")], true)];
        let head = vec![item("DealDamage", &[("target", "creature")], true)];
        assert!(diff(&base, &head).is_empty());
    }

    #[test]
    fn field_value_change_is_detected() {
        let base = vec![item("DealDamage", &[("target", "creature")], true)];
        let head = vec![item(
            "DealDamage",
            &[("target", "creature or battle")],
            true,
        )];
        let changes = diff(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::FieldChanged);
        assert_eq!(changes[0].key, "target");
        assert_eq!(changes[0].before, "creature");
        assert_eq!(changes[0].after, "creature or battle");
    }

    #[test]
    fn support_flip_is_detected() {
        let base = vec![item("Mill", &[("amount", "2")], false)];
        let head = vec![item("Mill", &[("amount", "2")], true)];
        let changes = diff(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::SupportFlip);
    }

    #[test]
    fn added_and_removed_items_are_attributed() {
        let small = vec![item("A", &[], true)];
        let big = vec![item("A", &[], true), item("B", &[], true)];

        let added = diff(&small, &big);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].kind, ChangeKind::ItemAdded);
        assert_eq!(added[0].label, "B");

        let removed = diff(&big, &small);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].kind, ChangeKind::ItemRemoved);
        assert_eq!(removed[0].label, "B");
    }

    #[test]
    fn markdown_groups_signatures_by_kind_with_direction_markers() {
        let clusters = vec![
            cluster(
                ChangeKind::FieldChanged,
                "DealDamage",
                "target",
                "creature",
                "creature or battle",
                &["Field Card"],
            ),
            cluster(
                ChangeKind::SupportFlip,
                "Mill",
                "",
                "false",
                "true",
                &["Support Card"],
            ),
            cluster(
                ChangeKind::ItemRemoved,
                "static_structure",
                "",
                "static_structure",
                "∅",
                &["Removed Card"],
            ),
            cluster(
                ChangeKind::ItemAdded,
                "CastWithKeyword(Cascade)",
                "",
                "∅",
                "CastWithKeyword(Cascade) (affects=in hand)",
                &["Added Card", "Second Added Card"],
            ),
        ];

        let markdown = render_markdown("e085a8d5fa08", &clusters, 4, 5, 0, &[], &[]);

        for section in [
            "#### 🟢 Added (1 signature)",
            "#### 🔴 Removed (1 signature)",
            "#### 🟡 Modified fields (1 signature)",
            "#### 🔵 Support status (1 signature)",
        ] {
            assert!(markdown.contains(section), "missing section: {section}");
        }
        assert!(markdown.contains(
            "- **2 cards** · ➕ ability/CastWithKeyword(Cascade) · added: `CastWithKeyword(Cascade) (affects=in hand)`"
        ));
        assert!(markdown
            .contains("- **1 card** · ➖ ability/static_structure · removed: `static_structure`"));
        assert!(markdown.contains(
            "- **1 card** · 🔄 ability/DealDamage · changed field `target`: `creature` → `creature or battle`"
        ));
        assert!(markdown
            .contains("- **1 card** · ↕️ ability/Mill · support: `unsupported` → `supported`"));
        assert!(markdown.contains("Affected (first 3): Added Card, Second Added Card"));

        let added = markdown.find("#### 🟢 Added").unwrap();
        let removed = markdown.find("#### 🔴 Removed").unwrap();
        let field = markdown.find("#### 🟡 Modified fields").unwrap();
        let support = markdown.find("#### 🔵 Support status").unwrap();
        assert!(added < removed && removed < field && field < support);
    }

    #[test]
    fn markdown_keeps_direction_markers_in_collapsed_tail() {
        let clusters = vec![
            cluster(
                ChangeKind::FieldChanged,
                "DealDamage",
                "target",
                "creature",
                "creature or battle",
                &["Field Card"],
            ),
            cluster(
                ChangeKind::SupportFlip,
                "Mill",
                "",
                "false",
                "true",
                &["Support Card"],
            ),
            cluster(
                ChangeKind::ItemRemoved,
                "static_structure",
                "",
                "static_structure",
                "∅",
                &["Removed Card"],
            ),
            cluster(
                ChangeKind::ItemAdded,
                "CastWithKeyword(Cascade)",
                "",
                "∅",
                "CastWithKeyword(Cascade)",
                &["Added Card"],
            ),
        ];

        let markdown = render_markdown("e085a8d5fa08", &clusters, 1, 4, 0, &[], &[]);

        assert!(markdown.contains(
            "<details><summary>… 3 more signature(s) (3 card-changes) — showing first 3;"
        ));
        for marker in ["➕", "➖", "↕️"] {
            assert!(markdown.contains(marker), "missing tail marker: {marker}");
        }
        assert!(!markdown.contains("Affected (first 3): Added Card"));
    }

    /// Regression guard for the sibling-collision case: two items share
    /// (category, label, source_text); the identical one must cancel as a
    /// multiset and the residual pair must reconcile to ONE field-change —
    /// never mis-pair into spurious churn.
    #[test]
    fn sibling_collision_reconciles_to_single_field_change() {
        let base = vec![
            item("Pump", &[("amount", "1")], true),
            item("Pump", &[("amount", "2")], true),
        ];
        let head = vec![
            item("Pump", &[("amount", "1")], true),
            item("Pump", &[("amount", "3")], true),
        ];
        let changes = diff(&base, &head);
        assert_eq!(changes.len(), 1, "only the 2→3 sibling changed");
        assert_eq!(changes[0].kind, ChangeKind::FieldChanged);
        assert_eq!(changes[0].key, "amount");
        assert_eq!(changes[0].before, "2");
        assert_eq!(changes[0].after, "3");
    }

    /// A change nested inside an otherwise-identical parent must be found via
    /// the recursive child diff.
    #[test]
    fn nested_child_change_is_detected() {
        let parent = |child_supported| ParsedItem {
            category: ParseCategory::Trigger,
            label: "Attacks".into(),
            source_text: None,
            supported: true,
            details: vec![],
            children: vec![item("Mill", &[("amount", "2")], child_supported)],
        };
        let changes = diff(&[parent(false)], &[parent(true)]);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, ChangeKind::SupportFlip);
        assert_eq!(changes[0].label, "Mill");
    }
}
