//! Unit tests for `policies::removal_lethality` — CR 704.5g removal-target
//! lethality. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! The pure `damage_is_lethal` arithmetic is checked directly; the composed
//! `lethality_bonus` runs against a real `PolicyContext` built over a pending
//! damage cast in `TargetSelection`, mirroring the engine's own
//! `effects_returns_pending_cast_during_target_selection` fixture.

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, PendingCast, TargetSelectionSlot, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::removal_lethality::*;

const AI: PlayerId = PlayerId(0);
const OPP: PlayerId = PlayerId(1);

// ─── damage_is_lethal (pure CR arithmetic) ──────────────────────────────────

/// A tiny stand-in creature carrying only the fields lethality reads.
fn creature(
    toughness: i32,
    damage_marked: u32,
    indestructible: bool,
) -> engine::game::game_object::GameObject {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let id = create_object(&mut state, CardId(9), OPP, "Body".into(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Creature],
        subtypes: Vec::new(),
    };
    obj.power = Some(1);
    obj.toughness = Some(toughness);
    obj.damage_marked = damage_marked;
    if indestructible {
        obj.keywords.push(Keyword::Indestructible);
    }
    state.objects.remove(&id).unwrap()
}

#[test]
fn exact_toughness_is_lethal() {
    // CR 704.5g: 3 damage on an undamaged 3-toughness body destroys it.
    assert!(damage_is_lethal(&creature(3, 0, false), 3));
}

#[test]
fn short_of_toughness_is_not_lethal() {
    // The #6582 misplay: 3 damage on a 7-toughness body.
    assert!(!damage_is_lethal(&creature(7, 0, false), 3));
}

#[test]
fn prior_marked_damage_lowers_the_bar() {
    // CR 120.6: marked damage accumulates — 1 already + 3 new ≥ 4 toughness.
    assert!(damage_is_lethal(&creature(4, 1, false), 3));
}

#[test]
fn indestructible_is_never_lethal() {
    // CR 702.12b: indestructible ignores the lethal-damage SBA.
    assert!(!damage_is_lethal(&creature(1, 0, true), 99));
}

#[test]
fn zero_toughness_is_not_killed_by_the_spell() {
    // Already dying to its own 0-toughness SBA (CR 704.5f), not to this damage.
    assert!(!damage_is_lethal(&creature(0, 0, false), 5));
}

// ─── lethality_bonus (composed over a pending cast) ─────────────────────────

/// Build a pending `DealDamage` (or non-damage) cast in `TargetSelection`
/// aimed at one opponent creature and return the lethality term for choosing it.
fn bonus_for(
    target_toughness: i32,
    target_damage_marked: u32,
    indestructible: bool,
    effect: Effect,
) -> f64 {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let spell = create_object(&mut state, CardId(1), AI, "Removal".into(), Zone::Stack);
    let target = create_object(&mut state, CardId(2), OPP, "Body".into(), Zone::Battlefield);
    {
        let obj = state.objects.get_mut(&target).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        obj.power = Some(1);
        obj.toughness = Some(target_toughness);
        obj.damage_marked = target_damage_marked;
        if indestructible {
            obj.keywords.push(Keyword::Indestructible);
        }
    }

    let ability = ResolvedAbility::new(effect, Vec::new(), spell, AI);
    let pending = PendingCast::new(spell, CardId(1), ability, ManaCost::zero());
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::TargetSelection {
            player: AI,
            pending_cast: Box::new(pending),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: Vec::new(),
                optional: false,
                chooser: None,
            }],
            mode_labels: Vec::new(),
            selection: Default::default(),
        },
        candidates: Vec::new(),
    };
    let candidate = CandidateAction {
        action: GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Target),
    };
    let config = AiConfig::default();
    let aicontext = AiContext::empty(&config.weights);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &aicontext,
        cast_facts: None,
        search_depth: SearchDepth::Root,
    };
    let target_obj = state.objects.get(&target).unwrap();
    lethality_bonus(&ctx, target, target_obj)
}

fn burn(damage: i32) -> Effect {
    Effect::DealDamage {
        amount: QuantityExpr::Fixed { value: damage },
        target: TargetFilter::Any,
        damage_source: None,
        excess: None,
    }
}

#[test]
fn lethal_target_is_rewarded() {
    // 3 damage kills a 3/3 → the clean-kill bonus.
    let b = bonus_for(3, 0, false, burn(3));
    assert!(
        (b - LETHAL_BONUS).abs() < 1e-9,
        "expected +{LETHAL_BONUS} for a clean kill, got {b}"
    );
}

#[test]
fn nonlethal_big_target_is_penalized() {
    // The #6582 misplay: 3 damage on a 7/7. Must score net-negative so a
    // killable smaller target outranks it.
    let b = bonus_for(7, 0, false, burn(3));
    assert!(
        b < 0.0,
        "expected a waste penalty for a survivable target, got {b}"
    );
    // And the penalty must exceed the +2.0 target-quality lure it counteracts.
    assert!(
        b <= -2.0,
        "penalty must overcome the threat-value bonus, got {b}"
    );
}

#[test]
fn indestructible_target_is_penalized_even_when_damage_exceeds_toughness() {
    // CR 702.12b: 5 damage on an indestructible 1/1 still whiffs.
    let b = bonus_for(1, 0, true, burn(5));
    assert!(
        b < 0.0,
        "indestructible target must read as wasted, got {b}"
    );
}

#[test]
fn non_damage_removal_is_inert() {
    // A Destroy spell carries no DealDamage → the term must not perturb its
    // targeting at all.
    let destroy = Effect::Destroy {
        target: TargetFilter::Any,
        cant_regenerate: false,
    };
    assert_eq!(bonus_for(7, 0, false, destroy), 0.0);
}
