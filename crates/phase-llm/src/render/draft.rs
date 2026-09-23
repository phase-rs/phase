//! Text rendering of a draft seat's position for an LLM drafter.
//!
//! The input is a [`DraftPlayerView`] — the same per-seat projection the
//! heuristic bot is handed. That is what enforces the no-cheating property
//! documented in `draft_wasm::bot_ai`: a field this seat may not see is not
//! expressible in the argument, so no amount of prompt text can leak it.

use std::collections::BTreeMap;

use draft_core::types::{DraftCardInstance, DraftKind};
use draft_core::view::{DraftPlayerView, SetLayoutView};
use engine::database::CardDatabase;

use super::text::{clamp_text, one_line};

/// Code -> printed set name, supplied by the caller. Codes are used verbatim
/// when a name is unavailable, so the renderer degrades rather than failing.
pub type SetNames = BTreeMap<String, String>;

/// The format brief: what the player is drafting, in the words a drafter uses.
///
/// Produces "Triple Mirrodin" for a one-set rotation and
/// "Mirrodin / Darksteel / Fifth Dawn" for a block rotation, because the
/// SEQUENCE of sets is the fact a drafter reasons from — which cards can still
/// come, and when the removal gets worse.
pub fn format_context(view: &DraftPlayerView, set_names: &SetNames) -> String {
    let kind = match view.kind {
        DraftKind::Quick => "Booster draft against bots",
        DraftKind::Premier => "Premier booster draft",
        DraftKind::Traditional => "Traditional booster draft",
        DraftKind::Sealed => "Sealed deck",
        DraftKind::CommanderDraft => "Commander draft (CR 903.13)",
        DraftKind::Winston => "Winston draft",
    };

    let mut lines = vec![format!(
        "{kind}, {} seats, {} pack(s) of {} cards.",
        view.seats.len(),
        view.pack_count,
        view.cards_per_pack
    )];

    if let Some(rotation) = source_sentence(view, set_names) {
        lines.push(rotation);
    }
    lines.push(format!(
        "Minimum deck size: {} cards (plus unlimited basic lands).",
        view.min_deck_size
    ));
    lines.join("\n")
}

/// The source sentence: how this draft's boosters are chosen.
///
/// Branches on the layout because the two shapes are different CLAIMS, not
/// different formatting. A uniform draft has a known per-round sequence; a
/// Chaos draft does not have one at all — `pack_generator` randomizes every
/// seat's every round independently, and the view deliberately publishes
/// candidate INTENT rather than assignments. Rendering the candidate pool as an
/// ordered rotation would tell the model which set each future booster will be,
/// which is information no one has.
fn source_sentence(view: &DraftPlayerView, set_names: &SetNames) -> Option<String> {
    // The engine's own per-pack record wins wherever it is published. It is
    // deliberately EMPTY for Chaos (`visible_pack_set_codes`), so this cannot
    // leak an assignment.
    if !view.pack_set_codes.is_empty() {
        return rotation_sentence(&view.pack_set_codes, set_names);
    }
    match &view.source {
        draft_core::view::DraftSourceView::Set { layout } => match layout {
            SetLayoutView::UniformByRound { codes } => rotation_sentence(codes, set_names),
            SetLayoutView::Chaos {
                candidate_codes,
                current_pack_code,
                completed_own_pack_codes,
                ..
            } => chaos_sentence(
                candidate_codes,
                current_pack_code.as_deref(),
                completed_own_pack_codes.as_deref(),
                set_names,
            ),
        },
        draft_core::view::DraftSourceView::Cube { .. } => None,
    }
}

/// What a Chaos drafter actually knows: the pool boosters are drawn from, the
/// set in front of them right now, and — only once the draft is over — which
/// sets they opened. Never a future assignment, because none exists yet.
fn chaos_sentence(
    candidate_codes: &[String],
    current_pack_code: Option<&str>,
    completed_own_pack_codes: Option<&[String]>,
    set_names: &SetNames,
) -> Option<String> {
    if candidate_codes.is_empty() {
        return None;
    }
    let pool: Vec<String> = candidate_codes
        .iter()
        .map(|code| format!("{} ({code})", set_name(code, set_names)))
        .collect();
    let mut sentence = format!(
        "Format: Chaos draft — every booster is drawn at random from this pool,          independently for each seat and each round: {}. Which set a future          booster will be is not knowable.",
        pool.join(", ")
    );
    if let Some(code) = current_pack_code {
        sentence.push_str(&format!(
            " The booster you are holding is {} ({code}).",
            set_name(code, set_names)
        ));
    }
    if let Some(codes) = completed_own_pack_codes.filter(|codes| !codes.is_empty()) {
        let opened: Vec<String> = codes
            .iter()
            .map(|code| format!("{} ({code})", set_name(code, set_names)))
            .collect();
        sentence.push_str(&format!(" You opened, in order: {}.", opened.join(", ")));
    }
    Some(sentence)
}

/// The rotation sentence for an explicit pack-round set sequence.
pub fn rotation_sentence(codes: &[String], set_names: &SetNames) -> Option<String> {
    if codes.is_empty() {
        return None;
    }
    let named: Vec<String> = codes.iter().map(|code| set_name(code, set_names)).collect();

    // All packs from one set is the "Triple <Set>" idiom a drafter actually
    // uses; a rotation names each set in round order.
    let all_same = named.windows(2).all(|pair| pair[0] == pair[1]);
    Some(if all_same {
        match codes.len() {
            1 => format!("Set: {} ({}).", named[0], codes[0]),
            2 => format!("Format: Double {} ({}).", named[0], codes[0]),
            3 => format!("Format: Triple {} ({}).", named[0], codes[0]),
            count => format!("Format: {count}x {} ({}).", named[0], codes[0]),
        }
    } else {
        format!(
            "Format: {} — one pack per round, in that order ({}).",
            named.join(" / "),
            codes.join(", ")
        )
    })
}

fn set_name(code: &str, set_names: &SetNames) -> String {
    set_names
        .get(&code.to_uppercase())
        .or_else(|| set_names.get(code))
        .cloned()
        .unwrap_or_else(|| code.to_string())
}

/// The seat's current position in the draft: which pack, which pick, and which
/// way packs are moving.
pub fn progress_context(view: &DraftPlayerView) -> String {
    format!(
        "Pack {} of {}, pick {} — packs are passing {:?}.",
        view.current_pack_number + 1,
        view.pack_count,
        view.pick_number + 1,
        view.pass_direction,
    )
}

/// The seat's drafted pool, grouped by colour so colour commitment is legible
/// at a glance rather than something the model has to tally.
pub fn pool_context(pool: &[DraftCardInstance]) -> String {
    if pool.is_empty() {
        return "Your pool is empty — this is your first pick, so take the strongest card."
            .to_string();
    }
    // `BTreeMap` so the same pool always renders identically.
    let mut by_color: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for card in pool {
        let key = if card.colors.is_empty() {
            "Colorless/Land".to_string()
        } else {
            card.colors.join("")
        };
        by_color
            .entry(key)
            .or_default()
            .push(format!("{} ({})", one_line(&card.name), card.cmc));
    }
    let mut out = format!("Your pool ({} cards):\n", pool.len());
    for (color, names) in by_color {
        out.push_str(&format!("  {color}: {}\n", names.join(", ")));
    }
    out
}

/// One numbered pack entry.
pub fn card_line(
    card: &DraftCardInstance,
    db: Option<&CardDatabase>,
    oracle_budget: usize,
) -> String {
    // Name and type line are rendered data and are folded to one line each, so
    // neither can start a line of its own inside the data block.
    let mut parts = vec![one_line(&card.name)];
    if !card.type_line.is_empty() {
        parts.push(one_line(&card.type_line));
    }
    parts.push(format!("mv {}", card.cmc));
    if !card.colors.is_empty() {
        parts.push(card.colors.join(""));
    }
    parts.push(card.rarity.clone());
    let mut line = parts.join(" | ");
    if oracle_budget > 0 {
        if let Some(text) = db
            .and_then(|db| db.get_face_by_name(&card.name))
            .and_then(|face| face.oracle_text.as_deref())
        {
            let collapsed = one_line(text);
            if !collapsed.is_empty() {
                line.push_str(&format!(" | \"{}\"", clamp_text(&collapsed, oracle_budget)));
            }
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> SetNames {
        SetNames::from([
            ("MRD".to_string(), "Mirrodin".to_string()),
            ("DST".to_string(), "Darksteel".to_string()),
            ("5DN".to_string(), "Fifth Dawn".to_string()),
        ])
    }

    fn codes(values: &[&str]) -> Vec<String> {
        values.iter().map(|code| (*code).to_string()).collect()
    }

    #[test]
    fn a_single_set_rotation_reads_as_the_triple_idiom() {
        let sentence = rotation_sentence(&codes(&["MRD", "MRD", "MRD"]), &names()).unwrap();
        assert!(sentence.contains("Triple Mirrodin (MRD)"), "{sentence}");
    }

    #[test]
    fn a_two_pack_single_set_rotation_reads_as_double() {
        let sentence = rotation_sentence(&codes(&["DST", "DST"]), &names()).unwrap();
        assert!(sentence.contains("Double Darksteel"), "{sentence}");
    }

    #[test]
    fn a_block_rotation_names_each_set_in_round_order() {
        let sentence = rotation_sentence(&codes(&["MRD", "DST", "5DN"]), &names()).unwrap();
        assert!(
            sentence.contains("Mirrodin / Darksteel / Fifth Dawn"),
            "{sentence}"
        );
        assert!(sentence.contains("one pack per round"), "{sentence}");
    }

    #[test]
    fn an_unknown_set_code_degrades_to_the_code_itself() {
        let sentence = rotation_sentence(&codes(&["ZZZ", "ZZZ", "ZZZ"]), &names()).unwrap();
        assert!(sentence.contains("Triple ZZZ (ZZZ)"), "{sentence}");
    }

    #[test]
    fn no_sets_yields_no_rotation_sentence() {
        assert_eq!(rotation_sentence(&[], &names()), None);
    }

    // ── Chaos: a candidate pool is not a rotation ────────────────────────

    /// The finding: Chaos candidates were forwarded to the rotation sentence,
    /// which claims "one pack per round, in that order". `pack_generator`
    /// randomizes every seat's every round independently, so that ordering does
    /// not exist and the model was being told something no one knows.
    #[test]
    fn chaos_candidates_are_never_described_as_an_ordered_rotation() {
        let sentence =
            chaos_sentence(&codes(&["MRD", "DST", "5DN"]), None, None, &names()).unwrap();

        assert!(!sentence.contains("in that order"), "{sentence}");
        assert!(!sentence.contains("one pack per round"), "{sentence}");
        assert!(!sentence.contains("Triple"), "{sentence}");
    }

    #[test]
    fn chaos_describes_an_unordered_pool_and_says_the_future_is_unknowable() {
        let sentence =
            chaos_sentence(&codes(&["MRD", "DST", "5DN"]), None, None, &names()).unwrap();

        assert!(sentence.contains("drawn at random"), "{sentence}");
        assert!(
            sentence.contains("independently for each seat"),
            "{sentence}"
        );
        assert!(sentence.contains("not knowable"), "{sentence}");
        for named in ["Mirrodin (MRD)", "Darksteel (DST)", "Fifth Dawn (5DN)"] {
            assert!(sentence.contains(named), "{named} missing from {sentence}");
        }
    }

    /// The two things a Chaos seat legitimately knows: the booster in front of
    /// it, and — only once picking is over — what it opened.
    #[test]
    fn chaos_reports_the_current_booster_when_the_view_publishes_one() {
        let sentence =
            chaos_sentence(&codes(&["MRD", "DST"]), Some("DST"), None, &names()).unwrap();

        assert!(
            sentence.contains("booster you are holding is Darksteel (DST)"),
            "{sentence}"
        );
    }

    #[test]
    fn chaos_reports_its_completed_sequence_only_when_published() {
        let withheld = chaos_sentence(&codes(&["MRD", "DST"]), None, None, &names()).unwrap();
        assert!(!withheld.contains("You opened"), "{withheld}");

        let completed = chaos_sentence(
            &codes(&["MRD", "DST"]),
            None,
            Some(&codes(&["DST", "MRD"])),
            &names(),
        )
        .unwrap();
        assert!(
            completed.contains("You opened, in order: Darksteel (DST), Mirrodin (MRD)"),
            "{completed}"
        );
    }

    #[test]
    fn an_empty_chaos_candidate_pool_yields_no_sentence() {
        assert_eq!(chaos_sentence(&[], None, None, &names()), None);
    }

    #[test]
    fn an_empty_pool_says_so_rather_than_rendering_nothing() {
        assert!(pool_context(&[]).contains("first pick"));
    }

    #[test]
    fn a_pool_groups_by_colour_deterministically() {
        let card = |name: &str, colors: &[&str]| DraftCardInstance {
            instance_id: name.to_string(),
            name: name.to_string(),
            set_code: "MRD".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: colors.iter().map(|c| (*c).to_string()).collect(),
            cmc: 2,
            type_line: "Creature".to_string(),
            draft_effect: None,
        };
        let pool = vec![card("A", &["U"]), card("B", &[]), card("C", &["U"])];
        let rendered = pool_context(&pool);
        assert_eq!(rendered, pool_context(&pool));
        assert!(rendered.contains("U: A (2), C (2)"), "{rendered}");
        assert!(rendered.contains("Colorless/Land: B (2)"), "{rendered}");
    }

    #[test]
    fn a_pack_line_leads_with_the_card_name() {
        let card = DraftCardInstance {
            instance_id: "i1".to_string(),
            name: "Skyshroud Elf".to_string(),
            set_code: "MRD".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: vec!["G".to_string()],
            cmc: 2,
            type_line: "Creature — Elf".to_string(),
            draft_effect: None,
        };
        let line = card_line(&card, None, 0);
        assert!(
            line.starts_with("Skyshroud Elf | Creature — Elf | mv 2 | G | common"),
            "{line}"
        );
    }
}
