use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use draft_core::types::{DeckAddableCardPolicy, DeckAddableCards, DraftCardInstance};
use engine::database::CardDatabase;
use engine::game::deck_validation::card_color_identity;
use engine::types::mana::{ManaColor, ManaType};
use engine::types::CardFace;
use phase_ai::config::AiDifficulty;
use phase_ai::{draft_eval, mana_colors};

/// A suggested Limited deck: drafted-card names + unlimited land distribution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuggestedDeck {
    pub main_deck: Vec<String>,
    pub lands: HashMap<String, u8>,
    /// CR 903.3 + CR 903.5a: the designated commander(s). Every name here is
    /// also a member of `main_deck` -- a designation is a label on a deck card,
    /// never an extra card beside the deck. Empty for the four CR 905.1a kinds.
    pub commander: Vec<String>,
}

/// A standard Limited deck is 40 cards: ~23 spells + ~17 lands.
const DEFAULT_DECK_SIZE: usize = 40;
const DEFAULT_SPELLS: usize = 23;

/// Auto-build a playable 40-card Limited deck from a pool.
///
/// Per D-12: selects ~23 best spells + ~17 lands with curve awareness, always
/// totalling exactly `TARGET_DECK_SIZE` cards.
/// Algorithm:
/// 1. Identify the 2 strongest colors by card count + quality
/// 2. Score every card; pick ~23 on-color spells respecting the mana curve
/// 3. If the on-color pool is too thin to field 23, top up with the best
///    remaining cards regardless of color so the deck still reaches 40
/// 4. Fill the remaining slots with lands distributed by color frequency
pub fn suggest_deck(
    pool: &[DraftCardInstance],
    _difficulty: AiDifficulty,
    card_db: Option<&CardDatabase>,
    min_deck_size: usize,
    commanders_required: u8,
    addable_cards: &DeckAddableCards,
) -> SuggestedDeck {
    // `_difficulty` is intentionally unused: deck suggestion always builds the
    // strongest legal deck. Difficulty governs the *opponents*, not the player's
    // own deck.
    if pool.is_empty() {
        return SuggestedDeck {
            main_deck: Vec::new(),
            lands: HashMap::new(),
            commander: Vec::new(),
        };
    }

    // CR 903.3 + CR 903.5c: a Commander deck is built AROUND its commander, not
    // designated after the fact. Choose the designation from the pool first, then
    // admit only cards whose colour identity the designation covers. Designating
    // after selection produces a deck that is illegal under CR 903.5c -- CR 903.13f
    // routes Commander Draft through CR 903.5 with exactly three exceptions
    // ((1) >=60 cards, (2) any number of same-named cards, (3) the Commander
    // Masters partner grant), and none of them is 903.5c.
    //
    // The `card_db.is_none()` disjunct is NOT the Commander path's behaviour, and
    // must not be re-read as one: `get_bot_deck_inner` refuses
    // `commanders_required > 0` with no database BEFORE this function is called,
    // so from production only the left disjunct is reachable -- the four
    // CR 905.1a kinds, which pass `0`. The right-hand half survives for direct
    // callers (this file's own `#[cfg(test)]` rows) and as a total function's
    // honest answer: eligibility (CR 903.3) and colour identity (CR 903.4) are
    // both read off a `CardFace`, so with no database there is nothing to
    // designate from.
    let (commander, identity): (Vec<String>, Option<HashSet<ManaColor>>) =
        match (commanders_required, card_db) {
            (0, _) | (_, None) => (Vec::new(), None),
            (required, Some(db)) => {
                // Measured over the UNCONSTRAINED pool on purpose: the key's job
                // is to prefer a designation that keeps the pool's two strongest
                // colours playable, which is a property of the pool as drafted.
                let designation_colors = find_best_colors(pool, card_db);
                let mut seen: HashSet<&str> = HashSet::new();
                let mut candidates: Vec<(&DraftCardInstance, DesignationKey<'_>)> = Vec::new();
                for card in pool {
                    let Some(face) = db.get_face_by_name(&card.name) else {
                        continue;
                    };
                    if !engine::game::is_commander_eligible(face) {
                        continue;
                    }
                    if seen.insert(card.name.as_str()) {
                        candidates.push((
                            card,
                            designation_key(card, face, &designation_colors, card_db),
                        ));
                    }
                }
                // Descending by a total key, so two runs over one pool agree.
                candidates.sort_by(|a, b| compare_designation_keys(&b.1, &a.1));
                let commander: Vec<String> = candidates
                    .iter()
                    .take(required as usize)
                    .map(|(card, _)| card.name.clone())
                    .collect();

                if commander.is_empty() {
                    // No eligible card in this pool. Defined, not undefined:
                    // build the deck exactly as today, unconstrained. Surfacing
                    // it to the host is deferred.
                    (Vec::new(), None)
                } else {
                    // CR 702.124c: a rule referring to "your commander's colour
                    // identity" means the COMBINED identities when there are two,
                    // so the constraint is a union and is already correct if
                    // `commanders_required` ever becomes 2.
                    let identity: HashSet<ManaColor> = commander
                        .iter()
                        .filter_map(|name| db.get_face_by_name(name))
                        .flat_map(card_color_identity)
                        .collect();
                    (commander, Some(identity))
                }
            }
        };

    // CR 903.5c: a card can be included only if every colour in its colour
    // identity is also in the commander's. Cards the database cannot resolve are
    // excluded rather than admitted -- the engine must not put a card it cannot
    // judge into a deck whose legality it enforces.
    //
    // This is a single substitution: every selection stage below runs over
    // `pool_in_identity` and keeps its own body. Under `None` the substitution is
    // the identity function, so the four CR 905.1a kinds are bit-identical to
    // today.
    let pool_in_identity: Vec<DraftCardInstance> = match (identity.as_ref(), card_db) {
        (Some(identity), Some(db)) => pool
            .iter()
            .filter(|c| {
                db.get_face_by_name(&c.name)
                    .is_some_and(|face| card_color_identity(face).is_subset(identity))
            })
            .cloned()
            .collect(),
        _ => pool.to_vec(),
    };
    let pool_in_identity = pool_in_identity.as_slice();

    let best_colors = find_best_colors(pool_in_identity, card_db);

    // Spell candidates: every pool card that isn't a land. Lands are added
    // separately as basics in step 4 (a drafted nonbasic land counted here
    // would inflate the deck past 40 once basics are layered on top).
    let mut scored: Vec<(&DraftCardInstance, f64)> = pool_in_identity
        .iter()
        .filter(|c| !is_land(c))
        .map(|c| (c, score_card(c, card_db)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // On-color (or colorless) cards, preserving the global score order.
    let on_color: Vec<(&DraftCardInstance, f64)> = scored
        .iter()
        .filter(|(c, _)| {
            c.colors.is_empty()
                || c.colors
                    .iter()
                    .any(|col| best_colors.contains(&col.as_str()))
        })
        .copied()
        .collect();

    let target_spells = target_spell_count(min_deck_size);
    let mut spells = select_spells_with_curve(&on_color, target_spells);

    // If we couldn't field 23 on-color playables, top up with the best
    // remaining cards from anywhere in the pool so the deck still hits 40.
    if spells.len() < target_spells {
        let chosen: HashSet<&str> = spells.iter().map(|c| c.instance_id.as_str()).collect();
        for entry in &scored {
            if spells.len() >= target_spells {
                break;
            }
            let card = entry.0;
            if !chosen.contains(card.instance_id.as_str()) {
                spells.push(card);
            }
        }
    }

    // `main_deck` includes every selected drafted card, including nonbasic lands.
    // `lands` is exclusively for addable cards, which the deck builder tracks
    // separately from the drafted pool. Keeping drafted lands in `main_deck`
    // makes them leave the pool and appear in the main-deck area instead of being
    // mistaken for unlimited addable lands.
    let spell_names: Vec<String> = spells.iter().map(|c| c.name.clone()).collect();
    let land_total = min_deck_size.saturating_sub(spell_names.len()) as u8;

    // Admit on-color drafted nonbasic fixing lands into the manabase — only under
    // the standard basic-land fill. A custom addable-card policy names the host's
    // own land source, and this phase does not override the host's configured
    // pool with cards the host excluded, so no drafted nonbasic is injected
    // there. Each admitted nonbasic replaces exactly one basic, so the deck total
    // is unchanged.
    //
    // When that source supplies nothing -- `suggest_addable_cards`'s `CustomOnly`
    // arm finds no addable card inside the commander's colour identity
    // (CR 903.5c) -- `lands` is empty and the deck is short of `min_deck_size`.
    // This function still returns it; `get_bot_deck_inner` is what refuses it
    // (CR 903.13f(1)), so no such deck reaches a game.
    let nonbasic_lands = if matches!(
        addable_cards.policy,
        DeckAddableCardPolicy::StandardBasics | DeckAddableCardPolicy::StandardBasicsPlusCustom
    ) {
        select_fixing_lands(pool_in_identity, &best_colors, card_db, land_total)
    } else {
        HashMap::new()
    };
    let nonbasic_count: u8 = nonbasic_lands.values().copied().sum();
    let basics_total = land_total.saturating_sub(nonbasic_count);
    let lands = suggest_addable_cards(
        &spell_names,
        pool_in_identity,
        basics_total,
        addable_cards,
        card_db,
        identity.as_ref(),
    );

    // Preserve drafted-pool order when adding selected nonbasic lands. A count
    // map is used for selection, so decrement it as each matching pool entry is
    // included to handle multiple drafted copies correctly.
    let mut selected_land_counts = nonbasic_lands;
    let mut main_deck = spell_names;
    // The spells occupy `..spell_count`; the drafted nonbasics are appended
    // after them. CR 903.5a's insertion below displaces a SPELL, never one of
    // those lands, so the manabase this function already sized stays intact.
    let spell_count = main_deck.len();
    for card in pool_in_identity {
        if let Some(count) = selected_land_counts.get_mut(&card.name) {
            if *count > 0 {
                main_deck.push(card.name.clone());
                *count -= 1;
            }
        }
    }

    // CR 903.5a: "Each deck must contain exactly 100 cards, INCLUDING its
    // commander." A designation is a label on a deck card, never an extra card
    // beside the deck, so a designated name that the curve selector did not take
    // is inserted here -- displacing one non-designated spell, mirroring the
    // "each admitted nonbasic replaces exactly one basic" discipline above, so
    // the deck total is unchanged. In practice the designation is the pool's
    // highest-scoring eligible card and `select_spells_with_curve` normally takes
    // it, but "normally" is not a guarantee.
    for name in &commander {
        if main_deck.contains(name) {
            continue;
        }
        if let Some(victim) = main_deck[..spell_count]
            .iter()
            .rposition(|existing| !commander.contains(existing))
        {
            main_deck[victim] = name.clone();
        } else {
            main_deck.push(name.clone());
        }
    }

    SuggestedDeck {
        main_deck,
        lands,
        commander,
    }
}

/// On-color drafted nonbasic fixing lands as a `name -> copy-count` map, capped at
/// `cap` lands total. A fixing land is a drafted nonbasic that taps for 2+ colors
/// (via basic land subtypes and/or `Effect::Mana` abilities — shared with draft
/// pick value through [`mana_colors::land_produced_color_types`]) including at
/// least one of the deck's colors. Each admitted copy is a real drafted entry, so
/// copy counts never exceed what the drafter owns. Empty without a card database
/// (produced colors can't be read from the printed type line alone).
fn select_fixing_lands(
    pool: &[DraftCardInstance],
    best_colors: &[&str],
    card_db: Option<&CardDatabase>,
    cap: u8,
) -> HashMap<String, u8> {
    let Some(db) = card_db else {
        return HashMap::new();
    };
    let mut result: HashMap<String, u8> = HashMap::new();
    let mut admitted: u8 = 0;
    for card in pool {
        if admitted >= cap {
            break;
        }
        if !is_land(card) {
            continue;
        }
        let Some(face) = db.get_face_by_name(&card.name) else {
            continue;
        };
        let colors =
            mana_colors::land_produced_color_types(&face.card_type.subtypes, &face.abilities);
        if colors.len() < 2 {
            continue;
        }
        let on_color = colors
            .iter()
            .filter_map(|&t| mana_type_to_color_str(t))
            .any(|s| best_colors.contains(&s));
        if !on_color {
            continue;
        }
        *result.entry(card.name.clone()).or_insert(0) += 1;
        admitted += 1;
    }
    result
}

/// Map a produced `ManaType` to the "W/U/B/R/G" key the color logic uses;
/// colorless has no color key.
fn mana_type_to_color_str(t: ManaType) -> Option<&'static str> {
    match t {
        ManaType::White => Some("W"),
        ManaType::Blue => Some("U"),
        ManaType::Black => Some("B"),
        ManaType::Red => Some("R"),
        ManaType::Green => Some("G"),
        ManaType::Colorless => None,
    }
}

/// Whether a drafted card is a land (so it isn't counted as a spell).
///
/// The engine-truth check is `CardFace.card_type` containing `CoreType::Land`,
/// but this filter runs over the raw `DraftCardInstance` pool before any
/// `CardDatabase` lookup, so the printed type line is the right tool here.
fn is_land(card: &DraftCardInstance) -> bool {
    card.type_line.to_ascii_lowercase().contains("land")
}

/// Find the 2 strongest colors in the pool by card count weighted by quality.
fn find_best_colors<'a>(
    pool: &[DraftCardInstance],
    card_db: Option<&CardDatabase>,
) -> Vec<&'a str> {
    let mut color_scores: HashMap<&str, f64> = HashMap::new();

    for card in pool {
        let card_score = score_card(card, card_db);
        for color in &card.colors {
            let key = match color.as_str() {
                "W" => "W",
                "U" => "U",
                "B" => "B",
                "R" => "R",
                "G" => "G",
                _ => continue,
            };
            *color_scores.entry(key).or_insert(0.0) += card_score;
        }
    }

    let mut sorted: Vec<(&&str, &f64)> = color_scores.iter().collect();
    sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));

    sorted.iter().take(2).map(|(color, _)| **color).collect()
}

/// The CR 903.3 designation's ordering key, most significant component first.
///
/// A tuple rather than a comparator so each candidate's key is computed once,
/// not once per comparison; [`compare_designation_keys`] is the ordering.
type DesignationKey<'a> = (bool, bool, f64, std::cmp::Reverse<&'a str>);

/// Build the ordering key for one commander candidate.
fn designation_key<'a>(
    card: &'a DraftCardInstance,
    face: &CardFace,
    best_colors: &[&str],
    card_db: Option<&CardDatabase>,
) -> DesignationKey<'a> {
    // CR 903.4, via the same function the human deck-builder's CR 903.5c check
    // uses (`deck_validation::color_identity_violations`), so the two paths
    // cannot disagree about a card.
    let identity = card_color_identity(face);
    (
        // Prefer a commander whose identity covers the pool's two strongest
        // colours: that is what makes the CR 903.5c pool filter lose the fewest
        // cards.
        best_colors
            .iter()
            .all(|c| color_key_to_mana_color(c).is_some_and(|color| identity.contains(&color))),
        // Prefer a coloured commander over a colourless one, so the empty-identity
        // manabase (`distribute_lands`'s `Wastes` arm) is reached only when every
        // eligible card in the pool is colourless.
        !identity.is_empty(),
        score_card(card, card_db),
        // A deterministic final tiebreak, so two runs over one pool agree. The
        // key is total: this component always discriminates between two distinct
        // names, and duplicates were already deduplicated by name.
        std::cmp::Reverse(card.name.as_str()),
    )
}

/// Total ordering over [`DesignationKey`].
///
/// The `f64` component uses `partial_cmp(..).unwrap_or(Ordering::Equal)` — the
/// shape this file's existing score sorts already use — so a `NaN` score degrades
/// to the name tiebreak rather than poisoning the sort.
fn compare_designation_keys(a: &DesignationKey<'_>, b: &DesignationKey<'_>) -> std::cmp::Ordering {
    a.0.cmp(&b.0)
        .then_with(|| a.1.cmp(&b.1))
        .then_with(|| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
        .then_with(|| a.3.cmp(&b.3))
}

/// Map the "W/U/B/R/G" key this file's colour logic uses to a [`ManaColor`].
///
/// The inverse direction of [`mana_type_to_color_str`]; `None` for any key that
/// is not one of the five colour letters.
fn color_key_to_mana_color(key: &str) -> Option<ManaColor> {
    match key {
        "W" => Some(ManaColor::White),
        "U" => Some(ManaColor::Blue),
        "B" => Some(ManaColor::Black),
        "R" => Some(ManaColor::Red),
        "G" => Some(ManaColor::Green),
        _ => None,
    }
}

/// The basic land that taps for one colour of mana.
///
/// CR 903.5d: "A card with a basic land type may be included in a Commander deck
/// only if each color of mana it could produce is included in the commander's
/// color identity." Naming exactly one basic per identity colour is what keeps
/// the identity-aware fill inside that rule. Exhaustive over `ManaColor` with no
/// wildcard, so a sixth colour would be a compile error here.
fn mana_color_to_basic(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "Plains",
        ManaColor::Blue => "Island",
        ManaColor::Black => "Swamp",
        ManaColor::Red => "Mountain",
        ManaColor::Green => "Forest",
    }
}

/// Score a card for deck inclusion: the shared engine-data evaluator
/// ([`draft_eval::evaluate_draft_card`]) plus a small rarity prior, falling back
/// to just the rarity prior when no `CardDatabase` is loaded.
fn score_card(card: &DraftCardInstance, card_db: Option<&CardDatabase>) -> f64 {
    let quality = card_db
        .and_then(|db| db.get_face_by_name(&card.name))
        .map(draft_eval::evaluate_draft_card_default)
        .unwrap_or(0.0);
    quality + draft_eval::rarity_prior(&card.rarity)
}

/// Select spells respecting a good mana curve for Limited.
///
/// Target distribution for ~23 spells:
/// - CMC 1: 1-2
/// - CMC 2: 5-6
/// - CMC 3: 5-6
/// - CMC 4: 3-4
/// - CMC 5: 2-3
/// - CMC 6+: 1-2
fn select_spells_with_curve<'a>(
    scored: &[(&'a DraftCardInstance, f64)],
    target: usize,
) -> Vec<&'a DraftCardInstance> {
    // Curve slot targets
    let curve_targets: [(u8, u8, usize); 6] = [
        (0, 1, 2),   // CMC 0-1: up to 2
        (2, 2, 6),   // CMC 2: up to 6
        (3, 3, 6),   // CMC 3: up to 6
        (4, 4, 4),   // CMC 4: up to 4
        (5, 5, 3),   // CMC 5: up to 3
        (6, 255, 2), // CMC 6+: up to 2
    ];

    let mut selected: Vec<&DraftCardInstance> = Vec::new();
    let mut used: Vec<bool> = vec![false; scored.len()];

    // First pass: fill curve slots from highest-scored cards
    for (cmc_low, cmc_high, max_count) in &curve_targets {
        let mut count = 0;
        for (i, (card, _)) in scored.iter().enumerate() {
            if used[i] {
                continue;
            }
            if card.cmc >= *cmc_low && card.cmc <= *cmc_high && count < *max_count {
                selected.push(card);
                used[i] = true;
                count += 1;
            }
        }
    }

    // Second pass: fill remaining slots with best remaining cards
    if selected.len() < target {
        for (i, (card, _)) in scored.iter().enumerate() {
            if selected.len() >= target {
                break;
            }
            if !used[i] {
                selected.push(card);
                used[i] = true;
            }
        }
    }

    // Truncate to target if we overshot
    selected.truncate(target);
    selected
}

/// Suggest a color-proportional land distribution for a set of spells, sized so
/// that `spells + lands` reaches a standard 40-card deck (clamped to a sane
/// 16–18 land count for hand-built decks). Per D-11.
pub fn suggest_lands(
    spell_names: &[String],
    pool: &[DraftCardInstance],
    min_deck_size: usize,
) -> HashMap<String, u8> {
    let total_lands = min_deck_size
        .saturating_sub(spell_names.len())
        .clamp(16, 18) as u8;
    // `None`: this is the HUMAN deck-builder's land suggestion. The human
    // designates their commander in the deck builder, and the suggester must not
    // constrain a manabase around a commander the human has not chosen. Nothing
    // above `suggest_lands` has an identity to pass, so its own signature does
    // not widen.
    distribute_lands(spell_names, pool, total_lands, None)
}

fn target_spell_count(min_deck_size: usize) -> usize {
    ((min_deck_size * DEFAULT_SPELLS) / DEFAULT_DECK_SIZE).max(1)
}

/// The addable-card fill: the single authority for every card that enters a
/// suggested deck from outside the drafted pool.
///
/// `identity` is the designation's colour identity (CR 903.5c), `None` when the
/// kind designates no commander. `card_db` is needed because an addable card is a
/// *name*, not a pool entry — it never appears in `pool`, so its colour identity
/// can only be read off a `CardFace`. That is the same argument
/// `select_fixing_lands` already makes for its own database parameter.
fn suggest_addable_cards(
    spell_names: &[String],
    pool: &[DraftCardInstance],
    total: u8,
    addable_cards: &DeckAddableCards,
    card_db: Option<&CardDatabase>,
    identity: Option<&HashSet<ManaColor>>,
) -> HashMap<String, u8> {
    // Orthogonal to the policy enum: this admits no card at all, so folding it
    // into the match below would state a relationship that does not exist.
    if total == 0 {
        return HashMap::new();
    }
    // Exhaustive over `DeckAddableCardPolicy`, not a two-variant `matches!`.
    // The CR 903.5c completeness argument for this function is that the two arms
    // cover every policy; a fourth variant added later must be a compile error
    // here, not a silent fall-through into the `CustomOnly` arm that fills every
    // land slot with a host-chosen card under a rule nobody re-checked.
    match addable_cards.policy {
        DeckAddableCardPolicy::StandardBasics | DeckAddableCardPolicy::StandardBasicsPlusCustom => {
            distribute_lands(spell_names, pool, total, identity)
        }
        DeckAddableCardPolicy::CustomOnly => {
            // CR 903.5c: a card can be included in a Commander deck only if every
            // colour in its colour identity is also in the commander's. That is
            // true of an addable card exactly as it is of a drafted one -- the
            // host's addable-card list is a second authority proposing cards, not
            // an exemption from the deck construction rules CR 903.13f routes
            // through CR 903.5. Cards the database cannot resolve are excluded
            // rather than admitted, for the same reason the drafted-pool filter
            // excludes them: the engine must not put a card it cannot judge into
            // a deck whose legality it enforces.
            //
            // `Some(empty)` -- a colourless commander -- needs no special case:
            // only the empty set is a subset of the empty set, so `is_subset`
            // admits exactly the colourless custom cards and rejects the rest.
            // `.find()` preserves `custom`'s order, which `resolve_addable_cards`
            // has already sorted and deduplicated on the only path that can
            // produce this policy, so two runs over one session agree exactly as
            // `.first()` did.
            let mut result = HashMap::new();
            let chosen = match identity {
                None => addable_cards.custom.first(),
                Some(identity) => addable_cards.custom.iter().find(|name| {
                    card_db
                        .and_then(|db| db.get_face_by_name(name.as_str()))
                        .is_some_and(|face| card_color_identity(face).is_subset(identity))
                }),
            };
            if let Some(card) = chosen {
                result.insert(card.clone(), total);
            }
            result
        }
    }
}

/// Distribute exactly `total_lands` basics proportional to the colored-mana
/// pips of the selected spells.
///
/// `identity` is the designation's colour identity (CR 903.5c/903.5d), `None`
/// when the kind designates no commander — under `None` every line below is the
/// base behaviour verbatim.
///
/// POSTCONDITION, on every branch: the returned map's values sum to exactly
/// `total_lands`. The `get_bot_deck_inner` seam compares
/// `main_deck.len() + sum(lands)` against `min_deck_size` and refuses a deck
/// under it, so a branch that drops `total_lands % k` cards is not a silent
/// shortfall — it refuses an otherwise CR-legal deck and the pod does not launch.
fn distribute_lands(
    spell_names: &[String],
    pool: &[DraftCardInstance],
    total_lands: u8,
    identity: Option<&HashSet<ManaColor>>,
) -> HashMap<String, u8> {
    // Build name -> card lookup from pool
    let card_by_name: HashMap<&str, &DraftCardInstance> =
        pool.iter().map(|c| (c.name.as_str(), c)).collect();

    // Count color pip occurrences from the selected spells
    let mut color_counts: HashMap<&str, u32> = HashMap::new();
    for name in spell_names {
        if let Some(card) = card_by_name.get(name.as_str()) {
            for color in &card.colors {
                let key = match color.as_str() {
                    "W" => "W",
                    "U" => "U",
                    "B" => "B",
                    "R" => "R",
                    "G" => "G",
                    _ => continue,
                };
                *color_counts.entry(key).or_insert(0) += 1;
            }
        }
    }

    // CR 903.5c/903.5d: the pip counts come from `DraftCardInstance.colors`,
    // while the designation's identity comes from `CardFace::color_identity` —
    // two sources that CAN disagree, which is the whole justification for
    // intersecting them. A design justified that way may not then assume they
    // agree.
    //
    // THE POSITION IS LOAD-BEARING. This must stay strictly UPSTREAM of the
    // `color_counts.is_empty()` test below. An intersection that empties the map
    // here falls into the identity-aware fallback and still produces a full
    // manabase. Placed downstream of that test it would skip both the fallback
    // and the proportional loop (`sorted_colors` would be empty, so that loop's
    // body never runs) and the function would return the empty `lands` map
    // declared just below — a bot deck of spells with no lands at all.
    if let Some(identity) = identity {
        color_counts.retain(|key, _| {
            color_key_to_mana_color(key).is_some_and(|color| identity.contains(&color))
        });
    }

    let mut lands: HashMap<String, u8> = HashMap::new();

    if color_counts.is_empty() {
        // No usable color info. Which basics fill the deck is a CR 903.5d
        // question, so it is answered from the commander's identity where there
        // is one.
        //
        // Every branch here satisfies this function's stated postcondition: the
        // values sum to exactly `total_lands`. The `base`/`extra` remainder term
        // is what does that — `base` inserted k times is
        // `total_lands - (total_lands % k)`, and `+ (i < extra)` returns the last
        // `extra` cards. A literal `total_lands / k` per colour would drop them.
        let basics: Vec<&'static str> = match identity {
            // CR 903.5d: only the basics for the designation's own colours are
            // admissible. Iterated in `ManaColor::ALL`'s declaration order, not
            // in `HashSet` order, so which colour absorbs a remainder card is
            // stable across runs.
            Some(identity) if !identity.is_empty() => ManaColor::ALL
                .into_iter()
                .filter(|color| identity.contains(color))
                .map(mana_color_to_basic)
                .collect(),
            // A colourless commander. `Wastes` has no basic land type and
            // produces only colourless mana, so CR 903.5d does not restrict it,
            // and it is in `STANDARD_BASIC_LANDS` so `DeckAddableCards::is_addable`
            // accepts it — the CR 903.13f(1) floor is still reachable.
            Some(_) => vec!["Wastes"],
            // No designation: the base five-way split, verbatim.
            None => vec!["Plains", "Island", "Swamp", "Mountain", "Forest"],
        };
        let k = basics.len() as u8;
        let base = total_lands / k;
        let extra = total_lands % k;
        for (i, land) in basics.into_iter().enumerate() {
            lands.insert(land.to_string(), base + u8::from((i as u8) < extra));
        }
        return lands;
    }

    let total_pips: u32 = color_counts.values().sum();
    let mut assigned: u8 = 0;

    // Sort colors by count descending for stable assignment
    let mut sorted_colors: Vec<(&&str, &u32)> = color_counts.iter().collect();
    sorted_colors.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

    for (i, (color, count)) in sorted_colors.iter().enumerate() {
        let land_name = color_to_land(color);
        let share = if i == sorted_colors.len() - 1 {
            // Last color gets the remainder so the basics sum to total_lands.
            total_lands - assigned
        } else {
            let remaining_colors = sorted_colors.len() - i - 1;
            let raw = ((**count as f64 / total_pips as f64) * total_lands as f64).round() as u8;
            // Minimum 1 land of any represented color, max leaves room for remaining
            raw.max(1)
                .min(total_lands - assigned - remaining_colors as u8)
        };
        lands.insert(land_name.to_string(), share);
        assigned += share;
    }

    lands
}

fn color_to_land(color: &str) -> &'static str {
    match color {
        "W" => "Plains",
        "U" => "Island",
        "B" => "Swamp",
        "R" => "Mountain",
        "G" => "Forest",
        _ => "Wastes",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::card_type::{CardType, CoreType, Supertype};

    fn instance(name: &str, colors: &[&str], cmc: u8, type_line: &str) -> DraftCardInstance {
        DraftCardInstance {
            instance_id: format!("id-{name}"),
            name: name.to_string(),
            set_code: "TST".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: colors.iter().map(|s| s.to_string()).collect(),
            cmc,
            type_line: type_line.to_string(),
            draft_effect: None,
        }
    }

    /// Card DB with four W/U creatures plus an on-color (Plains/Island) and an
    /// off-color (Swamp/Mountain) typed dual. The duals carry no `Effect::Mana`;
    /// their produced colors come from the basic land subtypes (true-dual shape).
    fn fixture_db() -> CardDatabase {
        let creature = |name: &str| {
            format!(
                r#""{name}": {{ "name": "{name}", "mana_cost": {{ "type": "NoCost" }},
                "card_type": {{ "supertypes": [], "core_types": ["Creature"], "subtypes": [] }},
                "power": "2", "toughness": "2", "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [] }}"#
            )
        };
        let dual = |name: &str, a: &str, b: &str| {
            format!(
                r#""{name}": {{ "name": "{name}", "mana_cost": {{ "type": "NoCost" }},
                "card_type": {{ "supertypes": [], "core_types": ["Land"], "subtypes": ["{a}", "{b}"] }},
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [] }}"#
            )
        };
        let json = format!(
            "{{ {}, {}, {}, {}, {}, {} }}",
            creature("White Bear"),
            creature("White Knight"),
            creature("Blue Bird"),
            creature("Blue Wizard"),
            dual("On Color Dual", "Plains", "Island"),
            dual("Off Color Dual", "Swamp", "Mountain"),
        );
        CardDatabase::from_json_str(&json).unwrap()
    }

    fn wu_pool() -> Vec<DraftCardInstance> {
        vec![
            instance("White Bear", &["W"], 2, "Creature — Bear"),
            instance("White Knight", &["W"], 2, "Creature — Knight"),
            instance("Blue Bird", &["U"], 1, "Creature — Bird"),
            instance("Blue Wizard", &["U"], 3, "Creature — Wizard"),
            instance("On Color Dual", &[], 0, "Land — Plains Island"),
            instance("Off Color Dual", &[], 0, "Land — Swamp Mountain"),
        ]
    }

    #[test]
    fn admits_on_color_fixing_land_and_rejects_off_color() {
        let db = fixture_db();
        let deck = suggest_deck(
            &wu_pool(),
            AiDifficulty::Medium,
            Some(&db),
            8,
            0,
            &DeckAddableCards::standard_basics(),
        );
        assert!(
            deck.main_deck.contains(&"On Color Dual".to_string()),
            "on-color (W/U) fixing land should be in the drafted main deck, got {:?}",
            deck.main_deck
        );
        assert!(
            !deck.main_deck.contains(&"Off Color Dual".to_string()),
            "off-color (B/R) fixing land must not be admitted, got {:?}",
            deck.main_deck
        );
        assert!(
            !deck.lands.contains_key("On Color Dual"),
            "drafted lands must not be reported as unlimited addable lands, got {:?}",
            deck.lands
        );
    }

    #[test]
    fn admitting_nonbasics_keeps_deck_total_exact() {
        let db = fixture_db();
        let deck = suggest_deck(
            &wu_pool(),
            AiDifficulty::Medium,
            Some(&db),
            8,
            0,
            &DeckAddableCards::standard_basics(),
        );
        let land_count: u32 = deck.lands.values().map(|&c| c as u32).sum();
        // Each admitted nonbasic replaces one basic — the total never drifts.
        assert_eq!(
            deck.main_deck.len() as u32 + land_count,
            8,
            "spells + lands must equal min_deck_size; lands = {:?}",
            deck.lands
        );
    }

    #[test]
    fn admits_each_copy_of_a_selected_nonbasic_land() {
        let db = fixture_db();
        let mut pool = wu_pool();
        let mut second_dual = instance("On Color Dual", &[], 0, "Land — Plains Island");
        second_dual.instance_id = "id-on-color-dual-2".to_string();
        pool.push(second_dual);

        let deck = suggest_deck(
            &pool,
            AiDifficulty::Medium,
            Some(&db),
            8,
            0,
            &DeckAddableCards::standard_basics(),
        );
        let selected_dual_count = deck
            .main_deck
            .iter()
            .filter(|name| name.as_str() == "On Color Dual")
            .count();
        let basic_land_count: u8 = deck.lands.values().sum();

        assert_eq!(selected_dual_count, 2);
        assert_eq!(basic_land_count, 2, "each selected dual replaces one basic");
        assert_eq!(deck.main_deck.len() + basic_land_count as usize, 8);
    }

    // ── CR 903.3 designation + CR 903.5c containment (U21) ─────────────────
    //
    // Fixture convention, load-bearing (PROBE C''): the fixture database MUST
    // contain the basic lands with their `color_identity` populated. An unknown
    // name has no readable identity, so a containment assertion over a database
    // that omits `Plains` measures the database's gaps rather than the
    // algorithm's output.

    fn creature_face(name: &str, legendary: bool, colors: Vec<ManaColor>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: if legendary {
                    vec![Supertype::Legendary]
                } else {
                    Vec::new()
                },
                core_types: vec![CoreType::Creature],
                subtypes: Vec::new(),
            },
            color_identity: colors,
            ..CardFace::default()
        }
    }

    fn basic_land_face(name: &str, colors: Vec<ManaColor>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                supertypes: vec![Supertype::Basic],
                core_types: vec![CoreType::Land],
                subtypes: vec![name.to_string()],
            },
            color_identity: colors,
            ..CardFace::default()
        }
    }

    /// The `CardFace` -> `serde_json::to_value` -> insert `legalities` ->
    /// `CardDatabase::from_json_str` recipe the landed
    /// `commander_draft_deck_legality.rs` fixture uses.
    fn db_from_faces(faces: Vec<CardFace>) -> CardDatabase {
        let mut entries = serde_json::Map::new();
        for face in faces {
            let mut obj = serde_json::to_value(&face).unwrap();
            obj.as_object_mut().unwrap().insert(
                "legalities".to_string(),
                serde_json::json!({ "commander": "legal" }),
            );
            entries.insert(face.name.to_lowercase(), obj);
        }
        CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string()).unwrap()
    }

    /// Every basic this file's fills can name, so no containment assertion is
    /// ever measuring a missing database row.
    fn basic_faces() -> Vec<CardFace> {
        vec![
            basic_land_face("Plains", vec![ManaColor::White]),
            basic_land_face("Island", vec![ManaColor::Blue]),
            basic_land_face("Swamp", vec![ManaColor::Black]),
            basic_land_face("Mountain", vec![ManaColor::Red]),
            basic_land_face("Forest", vec![ManaColor::Green]),
            // Wastes has no basic land type and produces only colourless mana,
            // so CR 903.5d does not restrict it and its identity is empty.
            basic_land_face("Wastes", Vec::new()),
        ]
    }

    /// PROBE C's fixture: the only commander-eligible card is mono-white, and
    /// the pool also holds six blue playables the unconstrained algorithm takes.
    fn commander_db() -> CardDatabase {
        let mut faces = vec![
            creature_face("Mono W Legend", true, vec![ManaColor::White]),
            creature_face("W Guy", false, vec![ManaColor::White]),
            creature_face("U Guy", false, vec![ManaColor::Blue]),
            creature_face("Blue Addable", false, vec![ManaColor::Blue]),
            creature_face("White Addable", false, vec![ManaColor::White]),
        ];
        faces.extend(basic_faces());
        db_from_faces(faces)
    }

    fn mono_white_legend_pool() -> Vec<DraftCardInstance> {
        let mut pool = vec![instance(
            "Mono W Legend",
            &["W"],
            2,
            "Legendary Creature — Human",
        )];
        for i in 0..6 {
            let mut card = instance("W Guy", &["W"], 2, "Creature — Human");
            card.instance_id = format!("id-w-{i}");
            pool.push(card);
        }
        for i in 0..6 {
            let mut card = instance("U Guy", &["U"], 2, "Creature — Human");
            card.instance_id = format!("id-u-{i}");
            pool.push(card);
        }
        pool
    }

    fn identity_of(db: &CardDatabase, name: &str) -> HashSet<ManaColor> {
        card_color_identity(
            db.get_face_by_name(name)
                .unwrap_or_else(|| panic!("fixture db must resolve {name}")),
        )
    }

    /// VM-4a — CR 903.3: a designation is made, it is commander-eligible, and
    /// CR 903.5a puts it INSIDE the deck rather than beside it.
    #[test]
    fn designates_an_eligible_commander_from_the_pool() {
        let db = commander_db();
        let pool = mono_white_legend_pool();

        // Reach guard: the pool really does hold an eligible card, so an empty
        // designation cannot pass as "nothing to designate".
        assert_eq!(
            pool.iter()
                .filter(|c| db
                    .get_face_by_name(&c.name)
                    .is_some_and(engine::game::is_commander_eligible))
                .count(),
            1,
            "fixture must offer exactly one designation candidate"
        );

        let deck = suggest_deck(
            &pool,
            AiDifficulty::Medium,
            Some(&db),
            60,
            1,
            &DeckAddableCards::standard_basics(),
        );

        // Reach guard before the subject: an empty deck cannot satisfy the
        // membership claim vacuously.
        assert!(!deck.main_deck.is_empty(), "deck must be built");
        assert_eq!(deck.commander.len(), 1, "commander = {:?}", deck.commander);
        assert!(
            engine::game::is_commander_eligible(db.get_face_by_name(&deck.commander[0]).unwrap()),
            "CR 903.3: {} must be commander-eligible",
            deck.commander[0]
        );
        // CR 903.5a: "including its commander".
        assert!(
            deck.main_deck.contains(&deck.commander[0]),
            "CR 903.5a: designation {:?} must be a member of main_deck {:?}",
            deck.commander[0],
            deck.main_deck
        );
    }

    /// VM-4b — CR 903.5c: every card in a designated deck is inside the
    /// commander's colour identity.
    ///
    /// PROBE C ran this exact fixture against base and got 6 `U Guy` + 22
    /// `Island` under a mono-white commander: 28 violations.
    #[test]
    fn designated_deck_is_inside_the_commanders_colour_identity() {
        let db = commander_db();
        let pool = mono_white_legend_pool();

        // Guard (iii): the pool provably contains off-identity playables, so
        // "no violations" cannot pass because nothing off-colour was available.
        assert_eq!(
            pool.iter().filter(|c| c.name == "U Guy").count(),
            6,
            "fixture must offer six off-identity playables"
        );

        let deck = suggest_deck(
            &pool,
            AiDifficulty::Medium,
            Some(&db),
            60,
            1,
            &DeckAddableCards::standard_basics(),
        );

        // Guard (i) and (ii).
        assert_eq!(deck.commander.len(), 1, "commander = {:?}", deck.commander);
        assert!(
            deck.main_deck.len() >= 7,
            "deck must be real, got {:?}",
            deck.main_deck
        );
        // Guard (iv): the `lands` half of the containment claim below is a
        // universal over `deck.lands`'s keys, which is VACUOUSLY TRUE on zero
        // keys, and guards (i)-(iii) all still pass on a landless deck.
        let land_total: usize = deck.lands.values().map(|&n| n as usize).sum();
        assert_eq!(
            deck.main_deck.len() + land_total,
            60,
            "CR 903.13f(1): spells + lands must reach the floor; lands = {:?}",
            deck.lands
        );

        let identity = identity_of(&db, &deck.commander[0]);
        for name in &deck.main_deck {
            assert!(
                identity_of(&db, name).is_subset(&identity),
                "CR 903.5c: {name} is outside {identity:?}"
            );
        }
        for name in deck.lands.keys() {
            assert!(
                identity_of(&db, name).is_subset(&identity),
                "CR 903.5d: basic {name} is outside {identity:?}"
            );
        }
    }

    /// VM-4d — the negative sibling. `commanders_required = 0` designates
    /// nothing AND leaves the deck unconstrained, so the four CR 905.1a kinds
    /// are provably untouched.
    #[test]
    fn no_designation_leaves_the_deck_unconstrained() {
        let db = commander_db();
        let pool = mono_white_legend_pool();

        let deck = suggest_deck(
            &pool,
            AiDifficulty::Medium,
            Some(&db),
            60,
            0,
            &DeckAddableCards::standard_basics(),
        );

        assert!(
            deck.commander.is_empty(),
            "commander = {:?}",
            deck.commander
        );
        // The discriminating half: a globally-applied constraint would strip
        // the blue cards even with no designation.
        assert!(
            deck.main_deck.iter().any(|n| n == "U Guy"),
            "the constraint must be gated on the parameter, got {:?}",
            deck.main_deck
        );
    }

    /// VM-4f — [M5] CR 903.5c over the SECOND card-admitting authority: the
    /// host's `CustomOnly` addable-card list, which never touches the drafted
    /// pool.
    #[test]
    fn custom_only_addable_list_is_filtered_to_the_commanders_identity() {
        let db = commander_db();
        let pool = mono_white_legend_pool();
        let addable = DeckAddableCards {
            policy: DeckAddableCardPolicy::CustomOnly,
            custom: vec!["Blue Addable".to_string(), "White Addable".to_string()],
        };

        // Guard (ii): the off-identity card was offered AND was the
        // unconstrained winner, so the assertion cannot pass because none was
        // available or because ordering made it lose anyway.
        assert_eq!(addable.custom[0], "Blue Addable");
        // Guard (iii): both custom names resolve with their identity set, or
        // the filter measures the database's gaps (PROBE C'').
        assert_eq!(identity_of(&db, "Blue Addable").len(), 1);
        assert_eq!(identity_of(&db, "White Addable").len(), 1);

        let deck = suggest_deck(&pool, AiDifficulty::Medium, Some(&db), 60, 1, &addable);

        assert_eq!(deck.commander.len(), 1, "commander = {:?}", deck.commander);
        let keys: Vec<&str> = deck.lands.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["White Addable"],
            "CR 903.5c: the constrained pick, not the list's first entry"
        );
        // Guard (iv): non-vacuity. This repair's failure mode when nothing
        // qualifies is an EMPTY map, and a universal over zero keys asserts
        // nothing.
        let land_total: usize = deck.lands.values().map(|&n| n as usize).sum();
        assert_eq!(land_total, 60 - deck.main_deck.len());
    }

    /// VM-4g — the shortfall the CR 903.5c filter leaves is reached
    /// DELIBERATELY, by a filter that found no in-identity entry, rather than by
    /// an unhandled path.
    ///
    /// This is the CAUSE, inside `suggest_deck`. The DISPOSITION is one layer
    /// up: `get_bot_deck_inner` refuses this deck (CR 903.13f(1)), so it never
    /// reaches a game. The two must not be folded together — each would still
    /// pass if the other's subject regressed.
    #[test]
    fn custom_only_with_no_in_identity_entry_yields_a_short_deck() {
        let db = commander_db();
        let pool = mono_white_legend_pool();
        let addable = DeckAddableCards {
            policy: DeckAddableCardPolicy::CustomOnly,
            custom: vec!["Blue Addable".to_string()],
        };

        // The list was genuinely exhausted rather than mis-indexed.
        assert_eq!(addable.custom.len(), 1);

        let deck = suggest_deck(&pool, AiDifficulty::Medium, Some(&db), 60, 1, &addable);

        // Reach guards: the empty land map is the CONSTRAINT refusing, not an
        // empty pool or a failed designation.
        assert_eq!(deck.commander.len(), 1, "commander = {:?}", deck.commander);
        assert!(deck.main_deck.len() >= 7, "deck = {:?}", deck.main_deck);

        assert!(deck.lands.is_empty(), "lands = {:?}", deck.lands);
        assert!(deck.main_deck.len() < 60, "deck = {:?}", deck.main_deck);
    }

    /// PROBE D — a designation whose colour identity is EMPTY must produce a
    /// `Wastes` manabase, never the five-colour even split base measured.
    #[test]
    fn colourless_designation_fills_with_wastes_only() {
        let mut faces = vec![
            creature_face("Colorless Legend", true, Vec::new()),
            creature_face("Colorless Guy", false, Vec::new()),
        ];
        faces.extend(basic_faces());
        let db = db_from_faces(faces);

        let mut pool = vec![instance(
            "Colorless Legend",
            &[],
            2,
            "Legendary Creature — Construct",
        )];
        for i in 0..6 {
            let mut card = instance("Colorless Guy", &[], 2, "Creature — Construct");
            card.instance_id = format!("id-c-{i}");
            pool.push(card);
        }

        let deck = suggest_deck(
            &pool,
            AiDifficulty::Medium,
            Some(&db),
            60,
            1,
            &DeckAddableCards::standard_basics(),
        );

        assert_eq!(deck.commander.len(), 1, "commander = {:?}", deck.commander);
        assert!(!deck.lands.is_empty(), "lands must be non-empty");
        let keys: Vec<&str> = deck.lands.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["Wastes"],
            "CR 903.5d: an empty identity admits no coloured basic"
        );
        let land_total: usize = deck.lands.values().map(|&n| n as usize).sum();
        assert_eq!(deck.main_deck.len() + land_total, 60);
    }

    /// VM-4i — item 3d's postcondition on the `Some(identity)` NON-EMPTY branch
    /// of the `color_counts.is_empty()` arm: the returned map's values sum to
    /// exactly `total_lands`.
    ///
    /// FIXTURE SHAPE, and it is synthetic by necessity. Every selected spell's
    /// `CardFace::color_identity` sits INSIDE the two-colour designation (so the
    /// CR 903.5c pool filter admits it) while its `DraftCardInstance.colors` is
    /// NON-EMPTY and DISJOINT from that identity. That is the only route on
    /// which guard (ii) can detect the intersection's POSITION: a fixture whose
    /// selected spells have empty `colors` reaches the same arm, but its
    /// `color_counts` is already empty at the `is_empty()` test, the downstream
    /// intersection is never reached, and the empty-map failure mode is
    /// unreachable. The two routes are NOT interchangeable.
    ///
    /// In production `DraftCardInstance.colors` IS colour identity (both writers
    /// derive it from `CardFace::color_identity`), so no recorded pool has this
    /// shape. The guard exists because item 3d's own design is justified by
    /// "these two sources can disagree", and a design justified that way may not
    /// then assume they agree.
    #[test]
    fn identity_aware_basic_split_distributes_every_land() {
        let mut faces = vec![creature_face(
            "WU Legend",
            true,
            vec![ManaColor::White, ManaColor::Blue],
        )];
        for i in 0..3 {
            faces.push(creature_face(
                &format!("W Spell {i}"),
                false,
                vec![ManaColor::White],
            ));
            faces.push(creature_face(
                &format!("U Spell {i}"),
                false,
                vec![ManaColor::Blue],
            ));
        }
        faces.extend(basic_faces());
        let db = db_from_faces(faces);

        // `colors: ["B"]` on every instance -- non-empty and disjoint from
        // {W,U}. The designation is in `main_deck` (CR 903.5a) and therefore in
        // `spell_names`, so its own instance colours must satisfy the same
        // condition. No nonbasic lands, so `nonbasic_count` is 0 and
        // `total_lands == min_deck_size - main_deck.len()` is computable from
        // the returned deck.
        let mut pool = vec![instance(
            "WU Legend",
            &["B"],
            2,
            "Legendary Creature — Human",
        )];
        for i in 0..3 {
            pool.push(instance(
                &format!("W Spell {i}"),
                &["B"],
                2,
                "Creature — Human",
            ));
            pool.push(instance(
                &format!("U Spell {i}"),
                &["B"],
                2,
                "Creature — Human",
            ));
        }

        let deck = suggest_deck(
            &pool,
            AiDifficulty::Medium,
            Some(&db),
            60,
            1,
            &DeckAddableCards::standard_basics(),
        );

        // Guard (i): a two-colour designation, asserted from the fixture db, so
        // the row cannot pass on a mono-colour identity where `total_lands % k`
        // is 0 by construction.
        assert_eq!(deck.commander.len(), 1, "commander = {:?}", deck.commander);
        assert_eq!(
            identity_of(&db, &deck.commander[0]).len(),
            2,
            "fixture designation must carry exactly two colours"
        );

        // Guard (ii): the fallback reach guard, and this phase's ONLY detector
        // of the [M3] intersection position. A FIVE-key map means the arm was
        // never made identity-aware; an EMPTY map means the intersection landed
        // downstream of the `color_counts.is_empty()` branch, where it skips
        // both the fallback and the proportional loop.
        assert!(
            !deck.lands.is_empty(),
            "the intersection must sit UPSTREAM of the `color_counts.is_empty()` \
             branch, or `distribute_lands` returns no lands at all"
        );
        let mut keys: Vec<&str> = deck.lands.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["Island", "Plains"],
            "CR 903.5d: exactly the two basics for the designation's colours"
        );

        // Guard (iii): non-vacuity / indivisibility, asserted BEFORE the sum.
        // This is what turns the row into an assertion about the REMAINDER
        // rather than about the quotient.
        let total_lands = 60 - deck.main_deck.len();
        assert_eq!(
            total_lands % 2,
            1,
            "fixture must make `total_lands` indivisible by the identity size, \
             or a literal `total_lands / k` passes"
        );

        // Guard (iv): the postcondition itself.
        let land_total: usize = deck.lands.values().map(|&n| n as usize).sum();
        assert_eq!(
            deck.main_deck.len() + land_total,
            60,
            "item 3d's postcondition: every branch sums to `total_lands`; \
             lands = {:?}",
            deck.lands
        );
    }

    #[test]
    fn no_card_db_admits_no_nonbasics() {
        // Without a card DB the produced colors are unknown, so no nonbasic is
        // admitted (the manabase falls back to basics only).
        let deck = suggest_deck(
            &wu_pool(),
            AiDifficulty::Medium,
            None,
            8,
            0,
            &DeckAddableCards::standard_basics(),
        );
        assert!(!deck.main_deck.contains(&"On Color Dual".to_string()));
    }
}
