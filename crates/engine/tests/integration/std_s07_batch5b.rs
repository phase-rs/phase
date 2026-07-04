//! S07 Batch 5b — final Condition_If tranche, increment B (1 card: Avatar Aang).
//!
//! Oracle: "Flying, firebending 2\nWhenever you waterbend, earthbend, firebend,
//!  or airbend, draw a card. Then if you've done all four this turn, transform
//!  Avatar Aang."
//!
//! Before this increment the `ElementalBend` trigger lowered to
//! `Draw{1} -> Transform{SelfRef}` with NO condition — it transformed on EVERY
//! bend — and dropped two swallow warnings (`Condition_If` + `Duration_ThisTurn`).
//! This increment adds `QuantityRef::BendTypesThisTurn` (distinct bend types the
//! controller performed this turn) and a parser arm so the intervening-if
//! "you've done all four this turn" attaches `AbilityCondition::QuantityCheck
//! { BendTypesThisTurn >= 4 }` to the Transform sub-ability (CR 603.4).
//!
//! The runtime tests drive the real resolver (`resolve_ability_chain`) with the
//! ability lowered from the *parsed* Aang definition (`build_resolved_from_def`),
//! so reverting the parser arm drops the condition and the partial-bend test's
//! "did NOT transform" assertion fails — the discriminating guard.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::BackFaceData;
use engine::game::zones::create_object;
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::parser::oracle_ir::diagnostic::OracleDiagnostic;
use engine::types::ability::ResolvedAbility;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::BendingType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

const AVATAR_AANG_ORACLE: &str = "Flying, firebending 2\nWhenever you waterbend, earthbend, \
     firebend, or airbend, draw a card. Then if you've done all four this turn, transform \
     Avatar Aang.";

fn parse_aang() -> ParsedAbilities {
    parse_oracle_text(
        AVATAR_AANG_ORACLE,
        "Avatar Aang",
        &["Flying".to_string(), "firebending".to_string()],
        &["Creature".to_string()],
        &["Avatar".to_string()],
    )
}

fn has_swallow(parsed: &ParsedAbilities, want: &str) -> bool {
    parsed.parse_warnings.iter().any(
        |w| matches!(w, OracleDiagnostic::SwallowedClause { detector, .. } if detector == want),
    )
}

/// Lower Aang's real parsed `ElementalBend` trigger (`Draw -> Transform`) into a
/// resolvable ability. The `Transform` sub-ability carries whatever condition the
/// parser attached — which is exactly what the revert-discrimination hinges on.
fn aang_bend_ability(source_id: ObjectId) -> ResolvedAbility {
    let parsed = parse_aang();
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::ElementalBend)
        .expect("Aang's batched bend trigger lowers to TriggerMode::ElementalBend");
    let def = trigger
        .execute
        .as_ref()
        .expect("bend trigger has an execute body");
    build_resolved_from_def(def, source_id, P0)
}

/// Create a DFC permanent (front face up, back face present) for P0 on the
/// battlefield so `Effect::Transform{SelfRef}` has something to flip.
fn add_aang_dfc(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(1),
        P0,
        "Avatar Aang".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.power = Some(4);
    obj.toughness = Some(4);
    obj.base_power = Some(4);
    obj.base_toughness = Some(4);
    obj.card_types = CardType {
        supertypes: vec![],
        core_types: vec![CoreType::Creature],
        subtypes: vec!["Avatar".to_string()],
    };
    obj.base_card_types = obj.card_types.clone();
    obj.back_face = Some(BackFaceData {
        name: "Avatar Aang, Master of Elements".to_string(),
        power: Some(6),
        toughness: Some(6),
        loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Avatar".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    });
    id
}

fn seed_bends(state: &mut GameState, kinds: &[BendingType]) {
    let p = state.players.iter_mut().find(|p| p.id == P0).unwrap();
    for k in kinds {
        p.bending_types_this_turn.insert(*k);
    }
}

fn hand_len(state: &GameState) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .len()
}

/// Positive reach-guard: with all four distinct bend types performed this turn,
/// the bend trigger both draws AND transforms Aang (intervening-if satisfied).
#[test]
fn avatar_aang_transforms_after_all_four_bends() {
    let mut state = GameState::new_two_player(42);
    let aang = add_aang_dfc(&mut state);
    // A card to draw so the root Draw is non-vacuous.
    create_object(
        &mut state,
        CardId(2),
        P0,
        "Library Card".to_string(),
        Zone::Library,
    );
    // All four distinct bend types already recorded this turn.
    seed_bends(
        &mut state,
        &[
            BendingType::Water,
            BendingType::Earth,
            BendingType::Fire,
            BendingType::Air,
        ],
    );
    assert!(
        !state.objects[&aang].transformed,
        "precondition: front face up"
    );
    let hand_before = hand_len(&state);

    let ability = aang_bend_ability(aang);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("bend trigger resolves");

    assert_eq!(
        hand_len(&state),
        hand_before + 1,
        "the bend trigger always draws a card"
    );
    assert!(
        state.objects[&aang].transformed,
        "all four bend types this turn (>=4) satisfies the intervening-if — Aang transforms"
    );
}

/// Discriminating guard: with only TWO distinct bend types this turn, the trigger
/// draws but the intervening-if fails, so Aang does NOT transform. Reverting the
/// parser arm drops the condition (unconditional Transform) and this assertion
/// fails.
#[test]
fn avatar_aang_no_transform_on_partial_bends() {
    let mut state = GameState::new_two_player(42);
    let aang = add_aang_dfc(&mut state);
    create_object(
        &mut state,
        CardId(2),
        P0,
        "Library Card".to_string(),
        Zone::Library,
    );
    // Only two distinct bend types this turn — the intervening-if (>=4) is false.
    seed_bends(&mut state, &[BendingType::Water, BendingType::Fire]);
    let hand_before = hand_len(&state);

    let ability = aang_bend_ability(aang);
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("bend trigger resolves");

    assert_eq!(
        hand_len(&state),
        hand_before + 1,
        "the bend trigger still draws a card on a partial bend"
    );
    assert!(
        !state.objects[&aang].transformed,
        "only two bend types this turn (<4) fails the intervening-if — Aang must NOT transform. \
         Reverting the parser condition arm makes Transform unconditional and fails here."
    );
}

/// No-swallow gate: with the distinct-bend-count condition attached to Transform,
/// neither the `Condition_If` nor the `Duration_ThisTurn` swallow fires. The
/// keyword names MUST be supplied so the "Flying, firebending 2" line does not
/// parse to `Effect::unimplemented` (which early-returns all swallow detectors and
/// would make this assertion vacuous).
#[test]
fn avatar_aang_no_swallow() {
    let parsed = parse_aang();
    // Non-vacuity: the parse is clean (no Unimplemented), so the detectors run.
    assert!(
        !parsed.parse_warnings.iter().any(
            |w| matches!(w, OracleDiagnostic::SwallowedClause { detector, .. }
                if detector == "Condition_If")
        ),
        "no Condition_If swallow expected — the intervening-if is captured as a \
         QuantityCheck condition. warnings: {:?}",
        parsed.parse_warnings
    );
    assert!(
        !has_swallow(&parsed, "Condition_If"),
        "Condition_If must be cleared. warnings: {:?}",
        parsed.parse_warnings
    );
    assert!(
        !has_swallow(&parsed, "Duration_ThisTurn"),
        "Duration_ThisTurn must be cleared (BendTypesThisTurn marker). warnings: {:?}",
        parsed.parse_warnings
    );
}
