//! `preset-audit` — cross-check a bundled custom-format preset's card lists
//! against an independent published authority.
//!
//! Preset data is hand-transcribed from a paper ruleset's own web page. That is
//! the only source for most of it, and it goes stale silently: Eternal Central
//! revises its B&R lists, and nothing in the build notices. This tool is the
//! one machine-readable check available.
//!
//! **Scope: Old School 93/94 only.** Scryfall publishes an `oldschool`
//! legality, and it is exactly Eternal Central's 93/94 ruleset — verified at
//! implementation time against all three of that preset's lists. There is no
//! `oldschool95` legality, and Swedish Old School is a different ruleset from a
//! different community (25 restricted vs 22, an empty banned list vs 7, plus
//! Summer Magic), so neither of the other two bundled presets can be validated
//! this way. They stay hand-verified against their own primary sources.
//!
//! ## Why this is an audit and not the runtime source
//!
//! Scryfall's `oldschool` legality is per-PRINTING: Black Lotus is
//! `restricted` on LEA/LEB/2ED and `not_legal` on Vintage Masters. That models
//! EC's actual reprint rule (a later printing is legal if it kept the original
//! frame and art), which is a fidelity this engine deliberately does not have —
//! see `docs/proposals/custom-format-engine/RESEARCH.md` §3 and review round
//! 11: no format here checks printing, and `PrintedCardRef` carries no set.
//! The presets' `SetCodeApproximation` disclosure is exactly about that gap.
//!
//! So this compares the two and reports drift. It never rewrites a preset.
//!
//! ## Usage
//!
//! ```text
//! cargo preset-audit
//! ```
//!
//! Exits non-zero if any list drifts, so it can gate a preset change.
//! Requires network access and `curl`.

use std::collections::BTreeSet;
use std::process::Command;

use engine::types::custom_format::{old_school_93_94, CustomFormatDef};

/// Scryfall asks that automated clients identify themselves.
const USER_AGENT: &str = "phase-rs-preset-audit/1.0";

const SEARCH_URL: &str = "https://api.scryfall.com/cards/search";

/// Card names returned by one Scryfall query, following pagination.
///
/// Shells out to `curl` rather than taking an HTTP dependency: this is a
/// manually-run audit, `curl` is what every other Scryfall fetcher in this repo
/// uses (`scripts/lib/scryfall-fetch.sh`), and the engine crate has no business
/// gaining a network client for a tool that never runs in a build.
fn scryfall_names(query: &str) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    let mut page = 1;
    loop {
        let output = Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                // Same retry posture as scripts/lib/scryfall-fetch.sh: Scryfall
                // fronts Cloudflare, which answers throttling with a non-JSON
                // body that a bare `curl -s` would report as success.
                "--retry",
                "5",
                "--retry-all-errors",
                "--retry-delay",
                "2",
                "-A",
                USER_AGENT,
                "--get",
                SEARCH_URL,
                "--data-urlencode",
                &format!("q={query}"),
                "--data-urlencode",
                &format!("page={page}"),
            ])
            .output()
            .map_err(|e| format!("could not run curl: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "curl failed for {query:?}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        let body: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("Scryfall returned non-JSON for {query:?}: {e}"))?;

        // A query matching nothing is a 404 with object=error, not an empty
        // list. That is a legitimate answer for a list we expect to be empty,
        // so it is not an error here.
        if body.get("object").and_then(|o| o.as_str()) == Some("error") {
            if body.get("code").and_then(|c| c.as_str()) == Some("not_found") {
                return Ok(names);
            }
            return Err(format!(
                "Scryfall error for {query:?}: {}",
                body.get("details").and_then(|d| d.as_str()).unwrap_or("?")
            ));
        }

        for card in body["data"].as_array().into_iter().flatten() {
            if let Some(name) = card["name"].as_str() {
                names.insert(name.to_string());
            }
        }

        if body["has_more"].as_bool() != Some(true) {
            return Ok(names);
        }
        page += 1;
    }
}

/// One list compared. Returns a human-readable drift report, or `None` when the
/// two agree.
fn diff(label: &str, ours: &BTreeSet<String>, theirs: &BTreeSet<String>) -> Option<String> {
    if ours == theirs {
        return None;
    }
    let missing: Vec<&str> = theirs.difference(ours).map(String::as_str).collect();
    let extra: Vec<&str> = ours.difference(theirs).map(String::as_str).collect();
    let mut report = format!(
        "  {label}: DRIFT ({} ours / {} theirs)\n",
        ours.len(),
        theirs.len()
    );
    if !missing.is_empty() {
        report.push_str(&format!(
            "    authority has, preset does not: {}\n",
            missing.join(", ")
        ));
    }
    if !extra.is_empty() {
        report.push_str(&format!(
            "    preset has, authority does not: {}\n",
            extra.join(", ")
        ));
    }
    Some(report)
}

fn names_of(list: &[String]) -> BTreeSet<String> {
    list.iter().cloned().collect()
}

/// Compares all three of a preset's card lists.
fn audit_old_school_93_94(preset: &CustomFormatDef) -> Result<Vec<String>, String> {
    let legality = &preset.rules.legality;
    let sets = legality
        .legal_sets
        .as_ref()
        .ok_or("Old School 93/94 must declare legal_sets")?;

    let mut drift = Vec::new();

    if let Some(d) = diff(
        "banned",
        &names_of(&legality.banned),
        &scryfall_names("banned:oldschool")?,
    ) {
        drift.push(d);
    }

    if let Some(d) = diff(
        "restricted",
        &names_of(&legality.restricted),
        &scryfall_names("restricted:oldschool")?,
    ) {
        drift.push(d);
    }

    // The carve-out, derived from the preset's OWN set list rather than from a
    // hardcoded list of promo sets: cards the authority calls legal that have
    // no printing in any set this preset declares. Whatever remains can only be
    // legal by being named, which is what `legal_cards` is for.
    //
    // `in:` ("has a printing in this set"), NOT `set:` ("this printing is from
    // this set") — measured at implementation time: the `set:` form returns 579
    // cards, because a card can satisfy `legal:oldschool` on one printing and
    // dodge a `-set:` exclusion on another. `in:` asks about the card.
    let exclusions: String = sets
        .iter()
        .map(|code| format!(" -in:{}", code.0.to_lowercase()))
        .collect();
    if let Some(d) = diff(
        "legal_cards (named outside legal_sets)",
        &names_of(&legality.legal_cards),
        &scryfall_names(&format!("legal:oldschool{exclusions}"))?,
    ) {
        drift.push(d);
    }

    Ok(drift)
}

fn main() {
    let preset = old_school_93_94();
    println!("=== Preset audit: {} ===", preset.label);
    println!("authority: Scryfall `oldschool` legality (Eternal Central 93/94)\n");

    match audit_old_school_93_94(&preset) {
        Err(e) => {
            eprintln!("audit could not run: {e}");
            std::process::exit(2);
        }
        Ok(drift) if drift.is_empty() => {
            println!("  banned, restricted and legal_cards all match the authority.");
            println!("\nNo drift. (Frame/foil fidelity is out of scope — see this file's header.)");
        }
        Ok(drift) => {
            for report in &drift {
                print!("{report}");
            }
            println!(
                "\n{} list(s) drifted. Re-read the primary ruleset before changing a preset — \
                 Scryfall is a cross-check, not the source of record.",
                drift.len()
            );
            std::process::exit(1);
        }
    }
}
