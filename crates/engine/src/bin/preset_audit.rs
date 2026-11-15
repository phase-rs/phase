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
use std::env;
use std::fs::{self, OpenOptions};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use engine::types::custom_format::{old_school_93_94, CustomFormatDef};

/// Scryfall asks that automated clients identify themselves.
const USER_AGENT: &str = "phase-rs-preset-audit/1.0";

const SEARCH_URL: &str = "https://api.scryfall.com/cards/search";

/// What one page of a Scryfall search says.
///
/// Typed rather than `(Vec<String>, bool)` so "is this the last page?" cannot be
/// confused with "did this match anything?" — two different answers that a bare
/// boolean pair would let a caller mix up.
#[derive(Debug, PartialEq, Eq)]
enum Page {
    /// Names on this page, with at least one page after it.
    More(Vec<String>),
    /// Names on this page, which is the last one.
    Last(Vec<String>),
    /// Scryfall's documented answer to a search that matched nothing.
    NoMatch,
}

/// Interpret one Scryfall search response body.
///
/// Split from the transport so the awkward cases are testable without a
/// network: this is a pure function of the bytes curl wrote.
///
/// **A matchless search is HTTP 404 with a JSON error object, not an empty
/// list** — which is why the body of a non-200 response must be read, and why
/// `--fail`, which discards it, cannot be used here. An empty authority list is a real,
/// expected answer for a preset whose carve-out is empty; turning it into a
/// transport error would make that preset unauditable.
fn read_page(body: &[u8], query: &str, page: u32) -> Result<Page, String> {
    let json: serde_json::Value = serde_json::from_slice(body)
        .map_err(|e| format!("Scryfall returned non-JSON for {query:?} (page {page}): {e}"))?;

    let object = json.get("object").and_then(|o| o.as_str());

    if object == Some("error") {
        if json.get("code").and_then(|c| c.as_str()) == Some("not_found") {
            return Ok(Page::NoMatch);
        }
        return Err(format!(
            "Scryfall error for {query:?}: {}",
            json.get("details").and_then(|d| d.as_str()).unwrap_or("?")
        ));
    }

    // Scryfall's search endpoint answers with a `list` object. Anything else
    // that happens to carry `data` and `has_more` is a shape this function was
    // not written to read, and consuming it would produce a confident, wrong
    // verdict about the PRESET when the truth is that the audit could not read
    // the answer.
    if object != Some("list") {
        return Err(format!(
            "Scryfall response for {query:?} has `object` {:?}, not \"list\" (page {page})",
            object.unwrap_or("<missing>")
        ));
    }

    // FAIL CLOSED on a shape we do not recognise. Silently skipping a
    // malformed `data`, a card with no `name`, or an absent `has_more`
    // would report DRIFT — a wrong, actionable-looking verdict about the
    // preset — when the truth is that the audit could not read the answer.
    let Some(data) = json.get("data").and_then(|d| d.as_array()) else {
        return Err(format!(
            "Scryfall response for {query:?} has no `data` array (page {page})"
        ));
    };
    let mut names = Vec::with_capacity(data.len());
    for card in data {
        let Some(name) = card.get("name").and_then(|n| n.as_str()) else {
            return Err(format!(
                "Scryfall returned a card with no `name` for {query:?} (page {page})"
            ));
        };
        names.push(name.to_string());
    }

    match json.get("has_more").and_then(|m| m.as_bool()) {
        Some(true) => Ok(Page::More(names)),
        Some(false) => Ok(Page::Last(names)),
        // Absent/non-bool: the page may or may not be the last, and
        // guessing "last" would silently truncate the authority's list.
        None => Err(format!(
            "Scryfall response for {query:?} has no boolean `has_more` (page {page})"
        )),
    }
}

/// Hard bound on the number of pages one query may consume.
///
/// Every individual request is already bounded by curl's timeouts, but
/// `has_more` is the SERVER's claim that another page exists. A server that
/// keeps answering `true` would keep [`paginate`] running forever, so the
/// audit — which a human runs and waits on — needs a terminating bound as well
/// as a per-request one. Scryfall pages are 175 cards, so this admits ~35,000
/// names: far above any preset carve-out this tool audits, and above the whole
/// Vintage-legal pool, while still terminating.
const MAX_PAGES: u32 = 200;

/// Follow `has_more` across pages, up to [`MAX_PAGES`], collecting every name.
///
/// Split from the transport for the same reason [`read_page`] is: the awkward
/// cases — here, a server that never stops claiming another page — are then
/// testable without a network. `fetch` performs one request and interprets it.
fn paginate(
    query: &str,
    mut fetch: impl FnMut(u32) -> Result<Page, String>,
) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    for page in 1..=MAX_PAGES {
        match fetch(page)? {
            Page::NoMatch if page == 1 => return Ok(names),
            // After a page that reported `has_more`, "nothing matched"
            // contradicts it, and stopping here would silently truncate the
            // authority's list.
            Page::NoMatch => {
                return Err(format!(
                    "Scryfall said {query:?} matched nothing on page {page}, after a page \
                     reporting `has_more`"
                ))
            }
            Page::Last(found) => {
                names.extend(found);
                return Ok(names);
            }
            Page::More(found) => names.extend(found),
        }
    }
    // Fail closed, exactly as an unreadable page does: a bounded audit that
    // reports it could not finish is right, and a short list that would read as
    // preset DRIFT is wrong.
    Err(format!(
        "Scryfall still reported `has_more` for {query:?} after {MAX_PAGES} pages; \
         refusing to paginate further"
    ))
}

/// Card names returned by one Scryfall query, following pagination.
///
/// Shells out to `curl` rather than taking an HTTP dependency: this is a
/// manually-run audit, `curl` is what every other Scryfall fetcher in this repo
/// uses (`scripts/lib/scryfall-fetch.sh`), and the engine crate has no business
/// gaining a network client for a tool that never runs in a build.
fn scryfall_names(query: &str) -> Result<BTreeSet<String>, String> {
    // Unique per run (pid + clock), and created fresh with `create_new`
    // (O_CREAT|O_EXCL) before each request: an existing path, a planted symlink
    // included, is refused rather than followed, so curl only ever writes a
    // file this process just created.
    let run_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let body_path = env::temp_dir().join(format!(
        "phase-preset-audit-{}-{run_nanos}.json",
        std::process::id()
    ));
    paginate(query, |page| {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&body_path)
            .map_err(|e| format!("could not create {}: {e}", body_path.display()))?;
        let output = Command::new("curl")
            .args([
                // NO `--fail`. It exits before the body can be read, and the
                // body is where Scryfall says whether a non-200 is "nothing
                // matched" (fine) or a real error (not). `read_page` decides.
                "--silent",
                "--show-error",
                // Retry posture of scripts/lib/scryfall-fetch.sh, minus its
                // `--fail`. `--retry` retries timeouts and the transient
                // statuses (408, 429, 5xx); `--retry-all-errors` adds transport
                // failures such as a refused or reset connection, which bare
                // `--retry` does not. Without `--fail` an HTTP 404 is not an
                // error to curl, so neither flag retries the 404 that means
                // "nothing matched" — measured: one request.
                "--retry",
                "5",
                "--retry-all-errors",
                "--retry-delay",
                "2",
                // BOUNDED. An audit that hangs is worse than one that fails:
                // it blocks a preset change with no verdict and no error. These
                // cap a single attempt and the whole retry sequence, so the
                // tool always terminates with an actionable result.
                "--connect-timeout",
                "10",
                "--max-time",
                "30",
                "--retry-max-time",
                "120",
                "-A",
                USER_AGENT,
                "--get",
                SEARCH_URL,
                "--data-urlencode",
                &format!("q={query}"),
                "--data-urlencode",
                &format!("page={page}"),
                // The body goes to a file, not stdout: curl truncates an output
                // file before each retry, whereas stdout keeps every attempt's
                // body. A retried 503 followed by a 200 would read as two
                // concatenated documents, which no JSON parser accepts, so no
                // retry could ever recover.
                "--output",
            ])
            .arg(&body_path)
            .output();
        // Read and removed before any check, so the file never outlives this
        // request — even when curl could not be run — and the next page's
        // `create_new` succeeds.
        let body = fs::read(&body_path).unwrap_or_default();
        let _ = fs::remove_file(&body_path);
        let output = output.map_err(|e| format!("could not run curl: {e}"))?;

        // curl's exit status FIRST. Without `--fail`, curl exits 0 for every
        // complete HTTP response, a 404 included, so a nonzero exit means the
        // transfer did not complete — and the file holds at most an earlier
        // attempt's or a partial response, not this request's answer, however
        // well-formed it looks.
        if !output.status.success() {
            return Err(format!(
                "curl failed for {query:?} (page {page}): {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }

        read_page(&body, query, page)
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Scryfall's real 404 body for a search that matched nothing.
    /// This is the case `--fail` used to swallow: curl exits nonzero, and
    /// without reading the body the audit reported a transport failure for what
    /// is actually a well-formed "the answer is the empty set".
    const NO_MATCH_404: &[u8] = br#"{
      "object": "error",
      "code": "not_found",
      "status": 404,
      "details": "Your query didn't match any cards. Adjust your search terms or refer to the syntax guide at https://scryfall.com/docs/reference"
    }"#;

    #[test]
    fn a_matchless_search_reads_as_the_empty_set_not_a_failure() {
        assert_eq!(
            read_page(NO_MATCH_404, "banned:oldschool", 1),
            Ok(Page::NoMatch)
        );
    }

    /// The paired control: a DIFFERENT Scryfall error is still an error. Were
    /// the fix "treat any error object as empty", a malformed query would report
    /// the preset's whole list as drift instead of saying the audit broke.
    #[test]
    fn any_other_scryfall_error_is_still_an_error() {
        let bad_syntax = br#"{"object":"error","code":"bad_request","status":400,
          "details":"Expected a value after ':'"}"#;
        let err =
            read_page(bad_syntax, "legal:", 1).expect_err("a bad query must not read as empty");
        assert!(err.contains("Expected a value"), "{err}");
    }

    #[test]
    fn pagination_is_driven_by_has_more() {
        let page_one = br#"{"object":"list","has_more":true,
          "data":[{"name":"Black Lotus"},{"name":"Ancestral Recall"}]}"#;
        assert_eq!(
            read_page(page_one, "restricted:oldschool", 1),
            Ok(Page::More(vec![
                "Black Lotus".to_string(),
                "Ancestral Recall".to_string()
            ]))
        );
        let page_two = br#"{"object":"list","has_more":false,"data":[{"name":"Timetwister"}]}"#;
        assert_eq!(
            read_page(page_two, "restricted:oldschool", 2),
            Ok(Page::Last(vec!["Timetwister".to_string()]))
        );
    }

    /// Fail closed: each unreadable shape must produce a verdict about the
    /// AUDIT, never a silently short list that would read as preset drift.
    #[test]
    fn unreadable_shapes_fail_closed() {
        for (body, expected) in [
            (&br#"<html>Just a moment...</html>"#[..], "non-JSON"),
            (&br#"{"object":"list","has_more":false}"#[..], "`data`"),
            (
                &br#"{"object":"list","has_more":false,"data":[{"id":"x"}]}"#[..],
                "no `name`",
            ),
            (
                &br#"{"object":"list","data":[{"name":"Shivan Dragon"}]}"#[..],
                "`has_more`",
            ),
        ] {
            let err = read_page(body, "legal:oldschool", 1)
                .expect_err("an unreadable page must not report names");
            assert!(err.contains(expected), "expected {expected:?} in: {err}");
        }
    }

    /// A payload that is shaped like a page but is not one. Every field
    /// `read_page` consumes is present and well-formed — only `object` says this
    /// is a different kind of document — so reading it would report ONE name as
    /// the authority's entire list, and the preset's real entries as drift.
    #[test]
    fn a_plausible_non_list_payload_is_refused() {
        let a_single_card = br#"{"object":"card","name":"Black Lotus","has_more":false,
          "data":[{"name":"Black Lotus"}]}"#;
        let err = read_page(a_single_card, "banned:oldschool", 1)
            .expect_err("a non-list object must not be read as a page of results");
        assert!(err.contains("\"card\""), "{err}");
        assert!(err.contains("list"), "{err}");
    }

    /// A server that never stops claiming another page must end the audit, not
    /// run it forever. The closure is deterministic and always `More`, so this
    /// pins both halves: the call count stops at the bound, and the verdict is
    /// an error rather than the names collected so far.
    #[test]
    fn endless_has_more_stops_at_the_bound() {
        let mut calls = 0;
        let result = paginate("banned:oldschool", |page| {
            calls += 1;
            Ok(Page::More(vec![format!("Card {page}")]))
        });
        let err = result.expect_err("an unbounded server must not produce a verdict");
        assert!(err.contains("refusing to paginate"), "{err}");
        assert_eq!(calls, MAX_PAGES, "the bound must be what stopped it");
    }

    /// The paired control: a server that DOES finish is unaffected by the bound,
    /// and every page's names survive into the union.
    #[test]
    fn a_terminating_server_collects_every_page() {
        let mut calls = 0;
        let names = paginate("banned:oldschool", |page| {
            calls += 1;
            Ok(match page {
                1 => Page::More(vec!["Black Lotus".to_string()]),
                _ => Page::Last(vec!["Timetwister".to_string()]),
            })
        })
        .expect("a well-behaved server must produce a verdict");
        assert_eq!(calls, 2);
        assert_eq!(
            names.iter().map(String::as_str).collect::<Vec<_>>(),
            ["Black Lotus", "Timetwister"]
        );
    }
}
