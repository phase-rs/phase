//! Draft-pick evaluation: one card-quality heuristic shared by the bot drafter
//! (`draft-wasm::bot_ai`) and the post-draft "Suggest Deck" auto-builder
//! (`draft-wasm::suggest`). Replaces the formerly duplicated keyword-counting
//! heuristics in those two modules.
//!
//! Scoring inspects the engine's parsed card data — effect chains (removal, card
//! draw, token creation, counterspells, tutors via [`EffectProfile`]), creature
//! stat-efficiency normalised by mana value, planeswalkers, and a flat proxy for
//! static abilities. Weights live in [`DraftWeights`] — a tunable struct mirroring
//! [`crate::eval::EvalWeights`] / [`crate::eval::KeywordBonuses`] — not scattered
//! literals.
//!
//! Known limitations:
//! - `CardDatabase::get_face_by_name` returns one face, so MDFCs / split / adventure
//!   cards are scored on their primary face only — under-rating the "never dead"
//!   cards prized in Limited. (Same blind spot as the heuristic this replaces.)
//! - Modal cards ("choose one — destroy / draw / make a token") register every mode's
//!   effect flags; the effect-derived score is scaled by `max_choices / mode_count`
//!   to approximate "you only get some of these".
//! - Mana-dorks / ramp creatures aren't specifically rewarded — only their stats count.
//! - Static-ability value is a flat per-count proxy, not a real anthem evaluation.
//! - Keyword weights are borrowed from board-evaluation [`KeywordBonuses`] defaults
//!   and are not draft-tuned (evasion somewhat under-weighted, vigilance over-weighted
//!   relative to a real pick order). A draft-tuned `DraftWeights::learned()` is future work.

use serde::{Deserialize, Serialize};

use engine::types::ability::PtValue;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;

use crate::cast_facts::EffectProfile;
use crate::eval::{creature_combat_value, KeywordBonuses};

/// Tunable weights for [`evaluate_draft_card`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftWeights {
    /// Keyword bonuses applied to creature stats (reused from board evaluation —
    /// not draft-tuned; see module docs).
    pub keyword: KeywordBonuses,
    /// Flat bonus for a card simply being a creature (the Limited backbone).
    pub creature_flat: f64,
    /// Multiplier on a creature's mana-value-normalised combat value.
    pub creature_efficiency: f64,
    /// Flat bonus for a planeswalker (Limited bomb class).
    pub planeswalker_flat: f64,
    /// Bonus for single-target removal in the effect chain.
    pub removal: f64,
    /// Bonus for mass damage / mass shrink (board wipes).
    pub mass_removal: f64,
    /// Bonus for card draw.
    pub draw: f64,
    /// Bonus for token creation.
    pub token: f64,
    /// Bonus for a counterspell (`Effect::Counter`).
    pub counter: f64,
    /// Bonus for a library tutor.
    pub search: f64,
    /// Bonus per static ability on the face.
    pub static_ability_each: f64,
    /// Cap on the total static-ability bonus.
    pub static_ability_cap: f64,
    /// Flat bonus for a nonbasic fixing land (taps for 2+ colors). A dual/tri
    /// land is playable in any deck running those colors, so it carries real
    /// pick value — but below spells/removal, since fixing is a support piece.
    pub fixing_land: f64,
}

impl Default for DraftWeights {
    fn default() -> Self {
        Self {
            keyword: KeywordBonuses::default(),
            creature_flat: 1.0,
            // Body value = combat_value / mana_value · this. Kept ≤ 1 so a
            // vanilla 2/2-for-2 (~3.5) lands below hard removal (`removal` = 4).
            creature_efficiency: 1.0,
            planeswalker_flat: 5.0,
            removal: 4.0,
            mass_removal: 5.0,
            draw: 2.0,
            token: 1.5,
            counter: 1.0,
            search: 0.5,
            static_ability_each: 0.75,
            static_ability_cap: 2.0,
            // ~card-draw value: above a filler creature, below a 2/2 (~3.5),
            // removal (4), and a bomb — bots take fixing mid-pack, not over playables.
            fixing_land: 2.0,
        }
    }
}

/// A small rarity prior — bombs skew rare/mythic. Weak signal, weighted lightly
/// next to the card-quality terms. Lives here so all draft-scoring constants are
/// in one place, even though rarity is a property of a *printing*
/// (`DraftCardInstance`), not a [`CardFace`]; callers blend it in.
pub fn rarity_prior(rarity: &str) -> f64 {
    match rarity {
        "mythic" => 1.5,
        "rare" => 1.0,
        "uncommon" => 0.4,
        _ => 0.0,
    }
}

/// Evaluate a card face's intrinsic draft-pick quality (higher = better). A pure
/// function of card data — no rarity (callers add [`rarity_prior`]), no pick or
/// deck context.
pub fn evaluate_draft_card(face: &CardFace, w: &DraftWeights) -> f64 {
    let core = &face.card_type.core_types;
    let is_creature = core.contains(&CoreType::Creature);
    let is_planeswalker = core.contains(&CoreType::Planeswalker);

    // A pure land carries no spell value, but a fixing land (taps for 2+ colors)
    // is a real pick — duals/tri-lands slot into any deck running those colors.
    // Mono-color nonbasics, colorless utility lands, and basics stay at 0 (not
    // worth a pick over a spell). Checked *after* the creature/planeswalker tests
    // so Land Creatures (Dryad Arbor) and creature/PW manlands score on their
    // bodies. Deck-color matching ("in their colors") is a deck-context concern
    // handled at the call sites, not here — this is the context-free quality.
    if !is_creature && !is_planeswalker && core.contains(&CoreType::Land) {
        return if produced_color_count(face) >= 2 {
            w.fixing_land
        } else {
            0.0
        };
    }

    // ── Effect-chain value ──────────────────────────────────────────────
    let profile = EffectProfile::from_face(face);
    let mut effect_score = 0.0;
    if profile.has_direct_removal_text {
        effect_score += w.removal;
    }
    if profile.has_mass_damage_or_mass_shrink_text {
        effect_score += w.mass_removal;
    }
    if profile.has_draw {
        effect_score += w.draw;
    }
    if profile.has_token_creation {
        effect_score += w.token;
    }
    if profile.has_counter_spell {
        effect_score += w.counter;
    }
    if profile.has_search_library {
        effect_score += w.search;
    }
    // A modal card resolves only some of its modes — scale the (over-counted)
    // effect flags down by the fraction of modes the caster actually picks.
    let modal_factor = match &face.modal {
        Some(m) => (m.max_choices as f64 / m.mode_count.max(1) as f64).min(1.0),
        None => 1.0,
    };
    let mut score = effect_score * modal_factor;

    // ── Body value ──────────────────────────────────────────────────────
    if is_creature {
        let power = fixed_pt(&face.power).unwrap_or(0);
        let toughness = fixed_pt(&face.toughness).unwrap_or(0);
        let mv = face.mana_cost.mana_value().max(1) as f64;
        let combat = creature_combat_value(
            power,
            toughness,
            |kw| face.keywords.contains(kw),
            &w.keyword,
        );
        score += w.creature_flat + combat / mv * w.creature_efficiency;
    }
    if is_planeswalker {
        score += w.planeswalker_flat;
    }

    // ── Static abilities (anthems, "creatures you control get +1/+1", …) ─
    score += (face.static_abilities.len() as f64 * w.static_ability_each).min(w.static_ability_cap);

    score
}

/// [`evaluate_draft_card`] with [`DraftWeights::default`].
pub fn evaluate_draft_card_default(face: &CardFace) -> f64 {
    evaluate_draft_card(face, &DraftWeights::default())
}

/// Count of distinct colors this face can produce — unioning intrinsic mana from
/// its basic land subtypes and its activated `Effect::Mana` abilities. `>= 2`
/// marks a fixing land. Delegates to the shared [`crate::mana_colors`] core so
/// draft pick value and the deck-builder manabase agree on what "fixing" means.
pub fn produced_color_count(face: &CardFace) -> usize {
    crate::mana_colors::land_produced_color_types(&face.card_type.subtypes, &face.abilities).len()
}

/// Extract a fixed power/toughness value, ignoring `*` / variable / derived stats.
fn fixed_pt(pt: &Option<PtValue>) -> Option<i32> {
    match pt {
        Some(PtValue::Fixed(v)) => Some(*v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction, QuantityExpr,
        TargetFilter, TriggerDefinition,
    };
    use engine::types::card_type::CardType;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost};
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;

    fn face(core: Vec<CoreType>) -> CardFace {
        CardFace {
            card_type: CardType {
                core_types: core,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn vanilla_creature(power: i32, toughness: i32, mv: u32) -> CardFace {
        CardFace {
            power: Some(PtValue::Fixed(power)),
            toughness: Some(PtValue::Fixed(toughness)),
            mana_cost: ManaCost::generic(mv),
            ..face(vec![CoreType::Creature])
        }
    }

    fn destroy_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        )
    }

    #[test]
    fn removal_spell_outscores_a_vanilla_two_two() {
        // Invariant under any non-degenerate weights: `removal` (4.0) dwarfs a
        // 2/2's body value (~3.x with default `creature_efficiency`).
        let mut bolt = face(vec![CoreType::Instant]);
        bolt.abilities = vec![destroy_ability()];
        let bear = vanilla_creature(2, 2, 2);

        let w = DraftWeights::default();
        assert!(evaluate_draft_card(&bolt, &w) > evaluate_draft_card(&bear, &w));
    }

    #[test]
    fn etb_removal_creature_lands_in_the_removal_tier() {
        // Removal on a *triggered* ability (Ravenous Chupacabra shape) must be
        // picked up — `collect_face_effects` scans `face.triggers`, not just
        // `face.abilities`. This is the central reason for the rewrite.
        let mut chupacabra = vanilla_creature(2, 2, 4);
        chupacabra.triggers = vec![TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .execute(destroy_ability())];
        let plain_fatty = vanilla_creature(3, 3, 4);

        let w = DraftWeights::default();
        let chup = evaluate_draft_card(&chupacabra, &w);
        let giant = evaluate_draft_card(&plain_fatty, &w);
        assert!(
            chup > giant + w.removal * 0.5,
            "ETB-removal creature ({chup}) should clear a vanilla body ({giant}) by roughly a removal's worth"
        );
    }

    #[test]
    fn cheap_flyer_beats_overcosted_fatty_under_default_weights() {
        // Tuning-dependent (relies on `creature_efficiency` / `flying_mult`
        // defaults), NOT an invariant — a future weight retune that flips this is
        // a tuning change, not a regression.
        let mut flyer = vanilla_creature(3, 3, 3);
        flyer.keywords = vec![Keyword::Flying];
        let fatty = vanilla_creature(7, 7, 6);

        let w = DraftWeights::default();
        assert!(evaluate_draft_card(&flyer, &w) > evaluate_draft_card(&fatty, &w));
    }

    #[test]
    fn planeswalker_outscores_a_comparable_creature() {
        let pw = CardFace {
            mana_cost: ManaCost::generic(4),
            ..face(vec![CoreType::Planeswalker])
        };
        let creature = vanilla_creature(4, 4, 4);

        let w = DraftWeights::default();
        assert!(evaluate_draft_card(&pw, &w) > evaluate_draft_card(&creature, &w));
    }

    #[test]
    fn variable_power_creature_does_not_panic() {
        let mut star = vanilla_creature(0, 0, 2);
        star.power = Some(PtValue::Variable("*".to_string()));
        star.toughness = Some(PtValue::Variable("1+*".to_string()));
        let _ = evaluate_draft_card_default(&star); // must not panic
    }

    #[test]
    fn bare_land_scores_zero_but_land_creature_does_not() {
        let land = face(vec![CoreType::Land]);
        assert_eq!(evaluate_draft_card_default(&land), 0.0);

        let mut dryad_arbor = vanilla_creature(1, 1, 0);
        dryad_arbor.card_type.core_types = vec![CoreType::Land, CoreType::Creature];
        assert!(evaluate_draft_card_default(&dryad_arbor) > 0.0);
    }

    fn land_with_subtypes(subtypes: &[&str]) -> CardFace {
        let mut f = face(vec![CoreType::Land]);
        f.card_type.subtypes = subtypes.iter().map(|s| s.to_string()).collect();
        f
    }

    fn tap_for(colors: Vec<ManaColor>) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors,
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
    }

    #[test]
    fn fixing_land_via_basic_subtypes_scores_positive() {
        // A true dual (Land — Plains Island) makes W+U via its types, no Effect::Mana.
        let dual = land_with_subtypes(&["Plains", "Island"]);
        assert_eq!(produced_color_count(&dual), 2);
        assert_eq!(
            evaluate_draft_card_default(&dual),
            DraftWeights::default().fixing_land
        );
    }

    #[test]
    fn fixing_land_via_mana_abilities_scores_positive() {
        // A painland-style land with two colored tap abilities.
        let mut painland = face(vec![CoreType::Land]);
        painland.abilities = vec![
            tap_for(vec![ManaColor::White]),
            tap_for(vec![ManaColor::Blue]),
        ];
        assert_eq!(produced_color_count(&painland), 2);
        assert_eq!(
            evaluate_draft_card_default(&painland),
            DraftWeights::default().fixing_land
        );
    }

    #[test]
    fn mono_color_nonbasic_land_scores_zero() {
        let mono = land_with_subtypes(&["Forest"]);
        assert_eq!(produced_color_count(&mono), 1);
        assert_eq!(evaluate_draft_card_default(&mono), 0.0);
    }

    #[test]
    fn colorless_only_land_scores_zero() {
        // Produces only {C} → no colored sources → not a fixing pick.
        let mut utility = face(vec![CoreType::Land]);
        utility.abilities = vec![AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )];
        assert_eq!(produced_color_count(&utility), 0);
        assert_eq!(evaluate_draft_card_default(&utility), 0.0);
    }
}
