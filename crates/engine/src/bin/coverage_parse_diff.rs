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
use std::collections::BTreeSet;
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

/// The six-string identity of a cluster: category, label, change kind, detail
/// key, templated before-value, templated after-value.
type ClusterSig = (String, String, String, String, String, String);

/// Content identity of a snapshot row, total over everything this binary can
/// observe: the displayed name, the Oracle text that drives the errata
/// carve-out, and the canonical multiset of its parse items. Two rows equal
/// under this key are interchangeable in every quantity `compare` emits, so
/// sorting a group by it makes pairing a function of content rather than of
/// `.cards` emission order — which is not determined by the data, because
/// `CardDatabase::face_index` is a `HashMap` and `coverage-report` never sorts
/// before serializing.
///
/// Totality is a checked property, not a hope: of the eight fields on
/// `CardCoverageResult`, this binary reads only `card_name`, `oracle_text`, and
/// `parse_details`. If it is ever changed to read another (`printings`,
/// `supported`, `gap_details`, `gap_count`, `set_code`), that field MUST be
/// added here in the same change, or pairing stops being content-determined.
type RowKey<'a> = (&'a str, Option<&'a str>, Vec<String>);

/// Everything `compare` produces, in the order the renderers consume it.
struct Comparison {
    clusters: Vec<Cluster>,
    /// One per changed ROW, not per distinct name. Rows sharing a name are
    /// distinct cards upstream — `card-data.json` gives them distinct Scryfall
    /// oracle ids — so both count.
    changed_cards: usize,
    oracle_changed: usize,
    added_cards: Vec<String>,
    removed_cards: Vec<String>,
    /// Card names carrying more than one row in EITHER snapshot. Reported on
    /// stderr so a reader can see the comparator's population exceeds its
    /// distinct-name count; every row is still compared.
    duplicate_names: usize,
}

/// The canonical multiset of a row's parse items. `diff_level` cancels items
/// whose `canon` matches as a multiset, so two rows with equal `parse_canon`
/// provably produce no changes. This is `canon` lifted from the item layer to
/// the row layer; a plain `==` is not available because `ParsedItem` does not
/// derive `PartialEq`.
fn parse_canon(details: &[ParsedItem]) -> Vec<String> {
    let mut canons: Vec<String> = details.iter().map(canon).collect();
    canons.sort();
    canons
}

fn row_key(card: &CardCoverageResult) -> RowKey<'_> {
    (
        card.card_name.as_str(),
        card.oracle_text.as_deref(),
        parse_canon(&card.parse_details),
    )
}

/// Group snapshot rows by lowercased card name. `card_name` is NOT unique in
/// `coverage-data.json` (30 names carried two rows apiece as measured on
/// 2026-08-08), so the value is a group, never a single row: collecting into a
/// map keyed on the name is last-wins and silently drops the other rows.
fn group_by_name(cards: &[CardCoverageResult]) -> BTreeMap<String, Vec<&CardCoverageResult>> {
    let mut map: BTreeMap<String, Vec<&CardCoverageResult>> = BTreeMap::new();
    for card in cards {
        map.entry(card.card_name.to_ascii_lowercase())
            .or_default()
            .push(card);
    }
    map
}

/// The card names of a whole name group, in canonical order.
///
/// Used by the two branches that emit a group present on only ONE side, which
/// never reach the `row_key` sort. Without the sort here, those branches would
/// emit in `.cards` input order and `added_cards`/`removed_cards` would not be
/// functions of the input multisets — observable, because both serialize as
/// ordered JSON arrays in `parse-diff.json`. Only `card_name` is emitted from
/// these branches, so sorting names (rather than whole rows) is exactly
/// sufficient.
fn group_names(group: &[&CardCoverageResult]) -> Vec<String> {
    let mut names: Vec<String> = group.iter().map(|c| c.card_name.clone()).collect();
    names.sort();
    names
}

/// Compare two snapshots' row populations.
///
/// Rows sharing a name are distinct cards, so a group is reconciled the same
/// way `diff_level` reconciles items one layer down: a canonical ordering, then
/// an exact-identity pass, then residual reconciliation, then surplus.
fn compare(base: &[CardCoverageResult], head: &[CardCoverageResult]) -> Comparison {
    let bmap = group_by_name(base);
    let hmap = group_by_name(head);

    // Counted over BOTH snapshots: a group that shrinks to one row still means
    // the comparator's population exceeded its distinct-name count.
    let mut duplicate_name_set: BTreeSet<&str> = BTreeSet::new();
    for (name, group) in bmap.iter().chain(hmap.iter()) {
        if group.len() > 1 {
            duplicate_name_set.insert(name.as_str());
        }
    }

    let mut sig_to_cluster: BTreeMap<ClusterSig, Cluster> = BTreeMap::new();
    let mut changed_cards = 0usize;
    let mut oracle_changed = 0usize;
    let mut added_cards: Vec<String> = Vec::new();
    let mut removed_cards: Vec<String> = Vec::new();

    for (name, head_group) in &hmap {
        let Some(base_group) = bmap.get(name) else {
            // Whole group is new. `head_group` never reaches the sort below, so
            // canonicalize here or the emitted sequence follows input order.
            added_cards.extend(group_names(head_group));
            continue;
        };

        // Canonical order FIRST: pairing must be a function of row content, not
        // of `.cards` emission order, which is not determined by the data.
        let mut base_rows: Vec<(RowKey<'_>, &CardCoverageResult)> =
            base_group.iter().map(|&b| (row_key(b), b)).collect();
        let mut head_rows: Vec<(RowKey<'_>, &CardCoverageResult)> =
            head_group.iter().map(|&h| (row_key(h), h)).collect();
        base_rows.sort_by(|(a_key, _), (b_key, _)| a_key.cmp(b_key));
        head_rows.sort_by(|(a_key, _), (b_key, _)| a_key.cmp(b_key));

        // Pass 1 — exact identity: same Oracle text AND the same canonical
        // parse multiset. `diff_level` emits nothing for such a pair, so they
        // are consumed silently. Mirrors `diff_level`'s own `canon` pass.
        let mut weak_head: Vec<(RowKey<'_>, &CardCoverageResult)> = Vec::new();
        for (h_key, h) in head_rows {
            match base_rows
                .iter()
                .position(|(b_key, _)| b_key.1 == h_key.1 && b_key.2 == h_key.2)
            {
                Some(index) => {
                    base_rows.remove(index);
                }
                None => weak_head.push((h_key, h)),
            }
        }

        // Pass 2 — residual reconciliation by Oracle text alone, so a row whose
        // parse changed still pairs with its counterpart. Mirrors `diff_level`'s
        // `weak_key` pass.
        let mut unpaired_head: Vec<(RowKey<'_>, &CardCoverageResult)> = Vec::new();
        for (h_key, h) in weak_head {
            let Some(index) = base_rows.iter().position(|(b_key, _)| b_key.1 == h_key.1) else {
                unpaired_head.push((h_key, h));
                continue;
            };
            let (_, b) = base_rows.remove(index);
            let mut changes = Vec::new();
            diff_level(&b.parse_details, &h.parse_details, &mut changes);
            if changes.is_empty() {
                continue;
            }
            changed_cards += 1;
            // A single row can emit the same signature more than once (repeated
            // structures). Collapse that HERE, per row, rather than deduping
            // `cluster.cards` globally: a global dedup would also collapse two
            // DISTINCT rows that share a name, undercounting the cluster.
            let mut row_seen: BTreeSet<ClusterSig> = BTreeSet::new();
            for ch in changes {
                let before_t = template(&ch.before, &h.card_name);
                let after_t = template(&ch.after, &h.card_name);
                let sig: ClusterSig = (
                    ch.category.to_string(),
                    ch.label.clone(),
                    ch.kind.label().to_string(),
                    ch.key.clone(),
                    before_t.clone(),
                    after_t.clone(),
                );
                let first_for_this_row = row_seen.insert(sig.clone());
                let cluster = sig_to_cluster.entry(sig).or_insert_with(|| Cluster {
                    category: ch.category,
                    label: ch.label.clone(),
                    kind: ch.kind,
                    key: ch.key.clone(),
                    before: before_t,
                    after: after_t,
                    cards: Vec::new(),
                });
                if first_for_this_row {
                    cluster.cards.push(h.card_name.clone());
                }
            }
        }

        // Pass 3 — rows with no Oracle-text counterpart left: the parse
        // legitimately differs for a non-parser reason (errata/reprint). Carve
        // out; do not attribute to the PR. Surplus beyond that is a real
        // population change. Both lists are already in canonical order.
        let carved = unpaired_head.len().min(base_rows.len());
        oracle_changed += carved;
        added_cards.extend(
            unpaired_head
                .iter()
                .skip(carved)
                .map(|(_, h)| h.card_name.clone()),
        );
        removed_cards.extend(
            base_rows
                .iter()
                .skip(carved)
                .map(|(_, b)| b.card_name.clone()),
        );
    }

    for (name, base_group) in &bmap {
        if !hmap.contains_key(name) {
            // Whole group is gone. Same reasoning as the `added_cards` branch
            // above: this group never reaches the sort.
            removed_cards.extend(group_names(base_group));
        }
    }

    let mut clusters: Vec<Cluster> = sig_to_cluster.into_values().collect();
    // Sort each card list for a stable display order. NO global dedup: repeats
    // within one row are already collapsed above, and two rows sharing a name
    // are two cards that must both be counted.
    for c in &mut clusters {
        c.cards.sort();
    }
    clusters.sort_by(|a, b| {
        b.cards
            .len()
            .cmp(&a.cards.len())
            .then(a.label.cmp(&b.label))
    });

    Comparison {
        clusters,
        changed_cards,
        oracle_changed,
        added_cards,
        removed_cards,
        duplicate_names: duplicate_name_set.len(),
    }
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

/// Parsed CLI arguments.
#[derive(Debug)]
struct Args {
    base_path: String,
    head_path: String,
    base_sha: String,
    head_sha: String,
    markdown_out: Option<String>,
    json_out: Option<String>,
    max_clusters: usize,
}

/// Parse the CLI. `head_sha_default` is CI's `HEAD_SHA` env value, read by the caller so this stays
/// a pure function of its inputs.
///
/// The two provenance flags REJECT a present-but-valueless form: falling back would silently
/// misattribute the whole report to another commit, and a confidently wrong SHA is worse than a
/// missing one. `--markdown` / `--json` / `--max-clusters` stay deliberately lenient — a missing
/// value there omits or degrades output the caller can see, so there is nothing to misattribute.
fn parse_args(
    mut args: impl Iterator<Item = String>,
    head_sha_default: String,
) -> Result<Args, &'static str> {
    let mut positional: Vec<String> = Vec::new();
    let mut markdown_out: Option<String> = None;
    let mut json_out: Option<String> = None;
    let mut base_sha = String::from("unknown");
    let mut head_sha = head_sha_default;
    let mut max_clusters = 25usize;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--markdown" => markdown_out = args.next(),
            "--json" => json_out = args.next(),
            "--base-sha" => {
                base_sha = args
                    .next()
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .ok_or("--base-sha requires a value")?
            }
            "--head-sha" => {
                head_sha = args
                    .next()
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .ok_or("--head-sha requires a value")?
            }
            "--max-clusters" => {
                max_clusters = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(max_clusters)
            }
            other => positional.push(other.to_string()),
        }
    }
    let [base_path, head_path] = <[String; 2]>::try_from(positional)
        .map_err(|_| "expected exactly two positional arguments")?;
    Ok(Args {
        base_path,
        head_path,
        base_sha,
        head_sha,
        markdown_out,
        json_out,
        max_clusters,
    })
}

fn main() {
    // CI exports HEAD_SHA on the `parsediff` step (`ci.yml`) as `pull_request.head.sha`. NOT derived
    // from git: that job checks out the synthetic PR merge commit, so `HEAD` is not the PR head.
    let head_sha_default = std::env::var("HEAD_SHA").unwrap_or_else(|_| String::from("unknown"));
    let args = match parse_args(std::env::args().skip(1), head_sha_default) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("coverage-parse-diff: {msg}");
            eprintln!("usage: coverage-parse-diff <baseline.json> <head.json> [--base-sha SHA] [--head-sha SHA] [--markdown OUT] [--json OUT] [--max-clusters N]");
            process::exit(2);
        }
    };
    let base = load(&args.base_path);
    let head = load(&args.head_path);

    let cmp = compare(&base.cards, &head.cards);
    if cmp.duplicate_names > 0 {
        // Not an error: 30 real cards share a name with another card, and every
        // row is compared. Emitted so a reader — and the receipt pipeline, which
        // captures stderr with SHA256s — can see that the comparator's
        // population exceeds its distinct-name count.
        eprintln!(
            "coverage-parse-diff: {} card name(s) carry more than one row in one or both snapshots; every row was compared (none dropped).",
            cmp.duplicate_names
        );
    }

    let md = render_markdown(
        &args.base_sha,
        &args.head_sha,
        &cmp.clusters,
        args.max_clusters,
        cmp.changed_cards,
        cmp.oracle_changed,
        &cmp.added_cards,
        &cmp.removed_cards,
    );
    match &args.markdown_out {
        Some(p) => {
            if let Err(e) = fs::write(p, &md) {
                eprintln!("coverage-parse-diff: cannot write {p}: {e}");
                process::exit(2);
            }
        }
        None => println!("{md}"),
    }

    if let Some(p) = &args.json_out {
        let json = render_json(
            &args.head_sha,
            &args.base_sha,
            &cmp.clusters,
            &cmp.added_cards,
            &cmp.removed_cards,
            cmp.oracle_changed,
        );
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
    head_sha: &str,
    clusters: &[Cluster],
    max_clusters: usize,
    changed_cards: usize,
    oracle_changed: usize,
    added: &[String],
    removed: &[String],
) -> String {
    let mut s = String::new();
    s.push_str("<!-- coverage-parse-diff -->\n");
    // Provenance: bind this comment to the head it was generated from. The sticky is EDITED in
    // place on every re-push (coverage-parse-diff-comment.yml), so without the head SHA a reader
    // cannot tell a fresh "no changes" from a stale one. Emitted before the branch so the
    // no-changes early return below carries it too, and above the fold so the 60k-char truncation
    // in the comment workflow cannot drop it.
    let _ = writeln!(s, "_Generated for head `{head_sha}`._\n");
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
    /// Same provenance pair the Markdown carries, in the order it presents them (head, then
    /// baseline). The sticky comment sends a reader here when it truncates, so the artifact has to
    /// identify its own commits rather than borrow the comment's.
    head_sha: &'a str,
    base_sha: &'a str,
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
    head_sha: &str,
    base_sha: &str,
    clusters: &[Cluster],
    added: &[String],
    removed: &[String],
    oracle_changed: usize,
) -> String {
    let report = DiffReport {
        head_sha,
        base_sha,
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

    /// Stand-in for CI's `HEAD_SHA`; full 40 chars so the identity check the sticky supports is
    /// exercised at its real width.
    const HEAD_SHA_FIXTURE: &str = "bee984f809e084d2bd0c71c4bbbb3d67ac8d13b4";

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

    /// Build a snapshot row. `set_code` is always empty in real data
    /// (`coverage.rs` writes `String::new()` at the sole construction site), so
    /// the fixture matches.
    fn card(name: &str, oracle: &str, parse_details: &[ParsedItem]) -> CardCoverageResult {
        CardCoverageResult {
            card_name: name.to_string(),
            set_code: String::new(),
            supported: true,
            gap_details: Vec::new(),
            gap_count: 0,
            oracle_text: Some(oracle.to_string()),
            parse_details: parse_details.to_vec(),
            printings: Vec::new(),
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

        let markdown = render_markdown(
            "e085a8d5fa08",
            HEAD_SHA_FIXTURE,
            &clusters,
            4,
            5,
            0,
            &[],
            &[],
        );

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

        let markdown = render_markdown(
            "e085a8d5fa08",
            HEAD_SHA_FIXTURE,
            &clusters,
            1,
            4,
            0,
            &[],
            &[],
        );

        assert!(markdown.contains(
            "<details><summary>… 3 more signature(s) (3 card-changes) — showing first 3;"
        ));
        for marker in ["➕", "➖", "↕️"] {
            assert!(markdown.contains(marker), "missing tail marker: {marker}");
        }
        assert!(!markdown.contains("Affected (first 3): Added Card"));
    }

    /// The sticky is edited in place on every re-push, so a body with no head SHA cannot be told
    /// apart from a stale one. Both render branches must carry it — the no-changes early return is
    /// the one the maintainer hit.
    #[test]
    fn markdown_identifies_the_head_sha_in_both_branches() {
        const HEAD: &str = HEAD_SHA_FIXTURE;

        let empty = render_markdown("e085a8d5fa08", HEAD, &[], 4, 0, 0, &[], &[]);
        assert!(
            empty.contains(HEAD),
            "the no-changes body must identify the head it was generated from: {empty}"
        );
        assert!(
            empty.starts_with("<!-- coverage-parse-diff -->"),
            "scripts/pr_review.py matches the sticky with startswith(MARKER); the marker must stay \
             the first line: {empty}"
        );
        assert!(
            !empty.contains("signature(s)"),
            "scripts/pr_review.py classifies a body containing 'signature(s)' as real_changes; the \
             no-changes body must not: {empty}"
        );

        let clusters = vec![cluster(
            ChangeKind::SupportFlip,
            "Mill",
            "",
            "false",
            "true",
            &["Support Card"],
        )];
        let changed = render_markdown("e085a8d5fa08", HEAD, &clusters, 4, 1, 0, &[], &[]);
        assert!(
            changed.contains(HEAD),
            "the with-changes body must identify the head too: {changed}"
        );
        assert!(
            changed.contains("e085a8d5fa08"),
            "the baseline SHA is still reported alongside the head"
        );
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

    /// The two required positionals plus whatever flags the case is exercising.
    fn argv(flags: &[&str]) -> std::vec::IntoIter<String> {
        let mut v = vec!["base.json".to_string(), "head.json".to_string()];
        v.extend(flags.iter().map(|s| (*s).to_string()));
        v.into_iter()
    }

    /// A missing, empty, or option-token value after a provenance flag is a usage error, not a
    /// silent fallback: the report would otherwise be stamped with a commit the caller never named.
    /// Each arm asserts on its own flag name, so fixing only one of the provenance pair fails the
    /// other.
    #[test]
    fn provenance_flags_reject_missing_empty_and_option_values() {
        let base_err = parse_args(argv(&["--base-sha"]), "env-head".into())
            .expect_err("a valueless --base-sha must not fall back to `unknown`");
        assert!(
            base_err.contains("--base-sha"),
            "the error must name the offending flag: {base_err}"
        );

        let head_err = parse_args(argv(&["--head-sha"]), "env-head".into())
            .expect_err("a valueless --head-sha must not fall back to the env default");
        assert!(
            head_err.contains("--head-sha"),
            "the error must name the offending flag: {head_err}"
        );

        for (flag, invalid_value) in [
            ("--base-sha", ""),
            ("--base-sha", "--markdown"),
            ("--head-sha", ""),
            ("--head-sha", "--markdown"),
        ] {
            let err = parse_args(argv(&[flag, invalid_value]), "env-head".into())
                .expect_err("empty and option-token provenance values must be rejected");
            assert!(
                err.contains(flag),
                "the error must name {flag} for {invalid_value:?}: {err}"
            );
        }

        // Positive control: the same flags WITH values parse, and an explicit --head-sha overrides
        // the env default rather than being ignored.
        let ok = parse_args(
            argv(&["--base-sha", "e085a8d5fa08", "--head-sha", HEAD_SHA_FIXTURE]),
            "env-head".into(),
        )
        .expect("both provenance flags with values must parse");
        assert_eq!(ok.base_sha, "e085a8d5fa08");
        assert_eq!(ok.head_sha, HEAD_SHA_FIXTURE);

        // Omitting them entirely is still legal — that is CI's shape for the head (env-supplied).
        let defaulted = parse_args(argv(&[]), "env-head".into()).expect("positionals alone parse");
        assert_eq!(defaulted.head_sha, "env-head");
        assert_eq!(defaulted.base_sha, "unknown");

        // The positional arity check survives the Vec → [String; 2] rewrite.
        assert!(parse_args(["only-one.json".to_string()].into_iter(), "env-head".into()).is_err());
    }

    /// The asymmetry with the provenance flags is deliberate. A missing `--markdown`/`--json`/
    /// `--max-clusters` value omits or degrades output the caller can see for themselves; there is
    /// no commit to misattribute. Pinned so a later "make every flag strict" sweep is a decision.
    #[test]
    fn output_flags_stay_lenient_on_a_missing_value() {
        let md = parse_args(argv(&["--markdown"]), "env-head".into())
            .expect("a valueless --markdown must not be a usage error");
        assert!(md.markdown_out.is_none(), "output falls back to stdout");

        let js = parse_args(argv(&["--json"]), "env-head".into())
            .expect("a valueless --json must not be a usage error");
        assert!(
            js.json_out.is_none(),
            "the drill-down artifact is simply skipped"
        );

        let mc = parse_args(argv(&["--max-clusters"]), "env-head".into())
            .expect("a valueless --max-clusters must not be a usage error");
        assert_eq!(mc.max_clusters, 25, "the default cluster cap stands");
    }

    /// The sticky comment sends a reader to `parse-diff.json` when its body is truncated, so the
    /// artifact must identify its own commits instead of borrowing the comment's.
    #[test]
    fn json_report_carries_both_shas() {
        const BASE: &str = "e085a8d5fa0817e3a1f6e7c9d40b2a5c3e8f1d62";

        let clusters = vec![cluster(
            ChangeKind::SupportFlip,
            "Mill",
            "",
            "false",
            "true",
            &["Support Card"],
        )];
        let json = render_json(HEAD_SHA_FIXTURE, BASE, &clusters, &[], &[], 0);
        let v: serde_json::Value =
            serde_json::from_str(&json).expect("render_json must emit valid JSON");

        // Distinct fixture values, so a head/base swap fails rather than passing symmetrically.
        assert_eq!(v["head_sha"], HEAD_SHA_FIXTURE);
        assert_eq!(v["base_sha"], BASE);
        assert_eq!(
            v["clusters"][0]["label"], "Mill",
            "the drill-down is unchanged"
        );
    }

    /// T1. Two rows share a name (the real `fast` shape: distinct cards,
    /// distinct Oracle text, distinct parse trees). BOTH must be compared —
    /// keying a map on the name keeps only one and the other's parse change
    /// vanishes.
    #[test]
    fn both_entries_of_a_duplicate_name_are_compared() {
        let base = vec![
            card(
                "Fast",
                "Target creature gains haste until end of turn.",
                &[item("DealDamage", &[("amount", "1")], true)],
            ),
            card(
                "Fast",
                "Discard a card, then draw two cards.",
                &[item("DrawCard", &[("amount", "2")], true)],
            ),
        ];
        let head = vec![
            card(
                "Fast",
                "Target creature gains haste until end of turn.",
                &[item("DealDamage", &[("amount", "3")], true)],
            ),
            card(
                "Fast",
                "Discard a card, then draw two cards.",
                &[item("DrawCard", &[("amount", "4")], true)],
            ),
        ];

        let cmp = compare(&base, &head);

        let labels: Vec<&str> = cmp.clusters.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"DealDamage"),
            "the first duplicate-name row's parse change is invisible: {labels:?}"
        );
        assert!(
            labels.contains(&"DrawCard"),
            "the second duplicate-name row's parse change is invisible: {labels:?}"
        );
        assert_eq!(cmp.clusters.len(), 2);
        assert_eq!(cmp.changed_cards, 2, "two rows changed, not one name");
        assert_eq!(cmp.duplicate_names, 1);
        // Reach-guard: the rows were diffed, not skipped by the Oracle carve-out.
        assert_eq!(cmp.oracle_changed, 0);
        assert!(cmp.added_cards.is_empty());
        assert!(cmp.removed_cards.is_empty());
    }

    /// T2. Positive control: real errata must STILL be carved out and counted.
    /// The fix must stop the carve-out firing on a key collision, not delete it.
    #[test]
    fn genuine_oracle_change_is_still_carved_out() {
        let base = vec![card(
            "Errata Card",
            "Old text.",
            &[item("DealDamage", &[("amount", "1")], true)],
        )];
        let head = vec![card(
            "Errata Card",
            "New text.",
            &[item("DealDamage", &[("amount", "9")], true)],
        )];

        let cmp = compare(&base, &head);

        assert_eq!(cmp.oracle_changed, 1);
        // Non-vacuous: the parse trees DO differ, so a cluster would appear if
        // the carve-out had been removed.
        assert!(cmp.clusters.is_empty());
        assert_eq!(cmp.changed_cards, 0);
        assert_eq!(cmp.duplicate_names, 0);
        assert!(cmp.added_cards.is_empty());
        assert!(cmp.removed_cards.is_empty());
    }

    /// T3. A duplicate-name group that loses a row must report exactly one
    /// removal. Both rows carry identical Oracle text (the real `lightning bolt`
    /// shape), so pairing cannot lean on the text. Also pins that
    /// `duplicate_names` counts a group that is duplicated on the BASE side only.
    #[test]
    fn duplicate_name_group_shrinking_reports_one_removal() {
        let twin =
            |details: &[ParsedItem]| card("Twin", "Twin deals 3 damage to any target.", details);
        let base = vec![
            twin(&[item("DealDamage", &[("amount", "3")], true)]),
            twin(&[item("DealDamage", &[("amount", "3")], true)]),
        ];
        let head = vec![twin(&[item("DealDamage", &[("amount", "3")], true)])];

        let cmp = compare(&base, &head);

        assert_eq!(cmp.removed_cards, vec!["Twin".to_string()]);
        assert!(cmp.added_cards.is_empty());
        assert!(cmp.clusters.is_empty());
        assert_eq!(cmp.changed_cards, 0);
        assert_eq!(cmp.oracle_changed, 0);
        // The group is duplicated only in `base`; a head-side-only counter reads 0.
        assert_eq!(cmp.duplicate_names, 1);
    }

    /// T5. The real `fast`/`replenish` shape: a duplicate-name group in which
    /// ONE row has genuine errata. The errata'd row must be carved out, the
    /// other row's parse change must still cluster, and neither may leak into
    /// added/removed.
    #[test]
    fn errata_on_one_row_of_a_duplicate_group_carves_out_without_hiding_the_other() {
        let base = vec![
            card("Fast", "Old haste text.", &[item("Haste", &[], true)]),
            card(
                "Fast",
                "Discard a card, then draw two cards.",
                &[item("DrawCard", &[("amount", "2")], true)],
            ),
        ];
        let head = vec![
            card("Fast", "New haste text.", &[item("Haste", &[], true)]),
            card(
                "Fast",
                "Discard a card, then draw two cards.",
                &[item("DrawCard", &[("amount", "4")], true)],
            ),
        ];

        let cmp = compare(&base, &head);

        assert_eq!(cmp.oracle_changed, 1);
        // The non-errata row is still fully visible.
        let labels: Vec<&str> = cmp.clusters.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["DrawCard"]);
        assert_eq!(cmp.changed_cards, 1);
        // The carve-out must not degrade into a population change.
        assert!(cmp.added_cards.is_empty());
        assert!(cmp.removed_cards.is_empty());
    }

    /// T6. Two rows sharing a name that change in the SAME way are two changed
    /// cards. Both the headline `changed_cards` and the cluster's card list must
    /// count them twice — deduping either by name reintroduces exactly the
    /// record loss this unit removes, and leaves the two numbers contradicting
    /// each other in the posted comment.
    #[test]
    fn two_rows_sharing_a_name_that_change_identically_count_twice() {
        let bolt = |amount: &str| {
            card(
                "Bolt",
                "Bolt deals 3 damage to any target.",
                &[item("DealDamage", &[("amount", amount)], true)],
            )
        };
        let base = vec![bolt("3"), bolt("3")];
        let head = vec![bolt("4"), bolt("4")];

        let cmp = compare(&base, &head);

        assert_eq!(cmp.changed_cards, 2, "both rows changed");
        assert_eq!(cmp.clusters.len(), 1, "they share one signature");
        assert_eq!(
            cmp.clusters[0].cards,
            vec!["Bolt".to_string(), "Bolt".to_string()],
            "the cluster must count two cards, not one name"
        );
        assert_eq!(cmp.oracle_changed, 0);
        assert!(cmp.added_cards.is_empty());
        assert!(cmp.removed_cards.is_empty());
    }

    /// T7. Pairing inside a duplicate-name group must be decided by row CONTENT,
    /// never by `.cards` emission order — which is not determined by the data
    /// (`CardDatabase::face_index` is a `HashMap` and `coverage-report` never
    /// sorts before serializing). All four input permutations must produce
    /// identical output.
    ///
    /// SCOPE: this fixture pins "at least one of {the canonical sort, the exact
    /// pass}" — it passes if either survives alone. T8 pins the sort
    /// individually; T9 pins the exact pass individually. Do not read this test
    /// as covering either one by itself.
    #[test]
    fn pairing_within_a_group_is_content_determined_not_order_determined() {
        let row = |amount: &str| {
            card(
                "Dup",
                "Same text on both rows.",
                &[item("DealDamage", &[("amount", amount)], true)],
            )
        };
        let base = vec![row("1"), row("2")];
        let head = vec![row("1"), row("3")];
        let base_rev = vec![row("2"), row("1")];
        let head_rev = vec![row("3"), row("1")];

        let forward = compare(&base, &head);

        // Positive control, so the permutation comparison below cannot pass
        // vacuously on four identically-wrong results: the exact-identity pass
        // must match the two `1` rows, leaving exactly one real change (2 → 3).
        assert_eq!(forward.clusters.len(), 1, "exactly one row really changed");
        assert_eq!(forward.clusters[0].before, "2");
        assert_eq!(forward.clusters[0].after, "3");
        assert_eq!(forward.changed_cards, 1);
        assert_eq!(forward.oracle_changed, 0);
        assert!(forward.added_cards.is_empty());
        assert!(forward.removed_cards.is_empty());

        let canonical = render_json(
            "head",
            "base",
            &forward.clusters,
            &forward.added_cards,
            &forward.removed_cards,
            forward.oracle_changed,
        );

        for (b, h) in [
            (&base, &head_rev),
            (&base_rev, &head),
            (&base_rev, &head_rev),
        ] {
            let permuted = compare(b, h);
            assert_eq!(
                render_json(
                    "head",
                    "base",
                    &permuted.clusters,
                    &permuted.added_cards,
                    &permuted.removed_cards,
                    permuted.oracle_changed,
                ),
                canonical,
                "output changed when the input rows were permuted"
            );
            assert_eq!(permuted.changed_cards, forward.changed_cards);
        }
    }

    /// T8. Pins the CANONICAL SORT specifically. Four rows in one name group,
    /// all sharing Oracle text, with pairwise-distinct parse trees, so the exact
    /// pass finds NOTHING and pairing rests entirely on the sort. Without the
    /// sort, `[2, 1]` pairs 2↔3 and 1↔4 instead of 1↔3 and 2↔4, and the
    /// permuted projection differs.
    #[test]
    fn the_canonical_sort_alone_makes_a_no_exact_match_group_order_independent() {
        let row = |amount: &str| {
            card(
                "Dup",
                "Same text on both rows.",
                &[item("DealDamage", &[("amount", amount)], true)],
            )
        };
        let base = vec![row("1"), row("2")];
        let head = vec![row("3"), row("4")];
        let base_rev = vec![row("2"), row("1")];
        let head_rev = vec![row("4"), row("3")];

        let forward = compare(&base, &head);

        // Content control: the sort makes the pairing 1↔3 and 2↔4, in that
        // order (equal `cards.len()` and equal `label`, so the stable sort
        // preserves ascending signature order, and "1" sorts before "2").
        assert_eq!(forward.clusters.len(), 2);
        assert_eq!(forward.clusters[0].before, "1");
        assert_eq!(forward.clusters[0].after, "3");
        assert_eq!(forward.clusters[1].before, "2");
        assert_eq!(forward.clusters[1].after, "4");
        assert_eq!(forward.changed_cards, 2);
        assert_eq!(forward.oracle_changed, 0);
        assert!(forward.added_cards.is_empty());
        assert!(forward.removed_cards.is_empty());

        let canonical = render_json(
            "head",
            "base",
            &forward.clusters,
            &forward.added_cards,
            &forward.removed_cards,
            forward.oracle_changed,
        );

        for (b, h) in [
            (&base, &head_rev),
            (&base_rev, &head),
            (&base_rev, &head_rev),
        ] {
            let permuted = compare(b, h);
            assert_eq!(
                render_json(
                    "head",
                    "base",
                    &permuted.clusters,
                    &permuted.added_cards,
                    &permuted.removed_cards,
                    permuted.oracle_changed,
                ),
                canonical,
                "dropping the canonical sort lets input order pick the pairing"
            );
        }
    }

    /// T9. Pins the EXACT PASS specifically. Its failure mode is semantic, not a
    /// permutation difference: with the sort kept and the exact pass removed,
    /// this fixture is still deterministic but emits TWO spurious clusters
    /// (2→1 and 3→2) where the truth is one exact match plus one real change.
    /// Only a content assertion catches that, so this test asserts content.
    #[test]
    fn the_exact_pass_prevents_spurious_clusters_when_an_identical_counterpart_exists() {
        let row = |amount: &str| {
            card(
                "Dup",
                "Same text on both rows.",
                &[item("DealDamage", &[("amount", amount)], true)],
            )
        };
        let base = vec![row("2"), row("3")];
        let head = vec![row("1"), row("2")];

        let cmp = compare(&base, &head);

        assert_eq!(
            cmp.clusters.len(),
            1,
            "the two `2` rows are identical and must cancel, leaving one change"
        );
        assert_eq!(cmp.clusters[0].before, "3");
        assert_eq!(cmp.clusters[0].after, "1");
        assert_eq!(cmp.changed_cards, 1);
        assert_eq!(cmp.oracle_changed, 0);
        assert!(cmp.added_cards.is_empty());
        assert!(cmp.removed_cards.is_empty());
    }

    /// T10. Pins the two WHOLE-GROUP branches. A name group present on only one
    /// side never reaches the `row_key` sort, so without `group_names`' sort the
    /// emitted name sequence follows `.cards` input order — observable, because
    /// `added_cards`/`removed_cards` serialize as ordered JSON arrays. The two
    /// rows differ in `card_name` BYTES while sharing one lowercased key, which
    /// is the only shape that can expose it.
    #[test]
    fn whole_group_add_and_remove_emit_names_in_canonical_order() {
        let mixed = |name: &str| {
            card(
                name,
                "Same text on both rows.",
                &[item("DealDamage", &[("amount", "1")], true)],
            )
        };
        let empty: Vec<CardCoverageResult> = Vec::new();
        let fwd_rows = vec![mixed("Fast"), mixed("FAST")];
        let rev_rows = vec![mixed("FAST"), mixed("Fast")];

        // Head-only group → the `added_cards` branch.
        let added_fwd = compare(&empty, &fwd_rows);
        let added_rev = compare(&empty, &rev_rows);
        assert_eq!(
            added_fwd.added_cards,
            vec!["FAST".to_string(), "Fast".to_string()],
            "whole-group add must emit names in canonical order"
        );
        assert_eq!(
            added_fwd.added_cards, added_rev.added_cards,
            "whole-group add leaked input order"
        );

        // Base-only group → the `removed_cards` branch (a different code path).
        let removed_fwd = compare(&fwd_rows, &empty);
        let removed_rev = compare(&rev_rows, &empty);
        assert_eq!(
            removed_fwd.removed_cards,
            vec!["FAST".to_string(), "Fast".to_string()],
            "whole-group remove must emit names in canonical order"
        );
        assert_eq!(
            removed_fwd.removed_cards, removed_rev.removed_cards,
            "whole-group remove leaked input order"
        );

        // Reach-guards: both rows really went down the whole-group branches,
        // rather than being paired, carved out, or dropped.
        assert_eq!(added_fwd.duplicate_names, 1);
        assert!(added_fwd.clusters.is_empty());
        assert_eq!(added_fwd.changed_cards, 0);
        assert_eq!(added_fwd.oracle_changed, 0);
        assert!(added_fwd.removed_cards.is_empty());
        assert!(removed_fwd.added_cards.is_empty());
        assert_eq!(removed_fwd.duplicate_names, 1);
    }
}
