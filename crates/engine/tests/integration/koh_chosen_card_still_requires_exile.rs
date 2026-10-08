//! Parse-sourced regression for Koh, the Face Stealer's CR 607.2a exile pin.
//!
//! The parser must lower "the last chosen card" to
//! `And[ChosenCard, Typed[InZone{Exile}]]`: the shared reader stays
//! zone-agnostic (CR 607.2d) and the exile discipline is composed at the
//! emission site (CR 607.2a). The runtime half re-hosts those PARSED statics on
//! a Koh-like permanent and proves the pin's discriminating power:
//!   * the remembered exiled card's ability IS granted (positive reach-guard),
//!   * before any choice nothing is granted,
//!   * the other exiled card's ability is NOT granted (multi-authority
//!     negative), and
//!   * after the remembered card leaves exile the grant drops (CR 400.7 — the
//!     moved card is a new object at the same storage id, so the stored
//!     incarnation pin no longer names it).
//!
//! The stale fixture is deliberately never consulted (1-C5): this test parses
//! the verbatim Oracle text and builds its own objects. Fixture regeneration is
//! `DEFERRED(phase 2)`.

use std::sync::Arc;

use super::koh_face_stealer_grants::pinned_chosen_card_source;
use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::evaluate_layers;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ContinuousModification, Effect, ManaContribution,
    ManaProduction, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// Koh's Oracle text, verbatim (Scryfall).
const KOH_FULL: &str = "When Koh enters, exile up to one other target creature.\nWhenever another nontoken creature dies, you may exile it.\nPay 1 life: Choose a creature card exiled with Koh.\nKoh has all activated and triggered abilities of the last chosen card.";

fn parse_koh() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        KOH_FULL,
        "Koh, the Face Stealer",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Shapeshifter".to_string()],
    )
}

/// CR 607.2a + CR 607.2d: BOTH grant modifications must carry the exile-pinned
/// source shape. Positive reach-guard: exactly two grants present, so the
/// per-modification shape assertions cannot pass vacuously.
#[test]
fn parsed_koh_grants_both_carry_the_exile_pin() {
    let parsed = parse_koh();
    let grants: Vec<&ContinuousModification> = parsed
        .statics
        .iter()
        .flat_map(|s| s.modifications.iter())
        .collect();
    assert_eq!(
        grants.len(),
        2,
        "Koh's static must lower to exactly two grant modifications, got {grants:?}"
    );

    let pinned = pinned_chosen_card_source();
    let activated_source = grants.iter().find_map(|m| match m {
        ContinuousModification::GrantAllActivatedAbilitiesOf { source, .. } => Some(source),
        _ => None,
    });
    let triggered_source = grants.iter().find_map(|m| match m {
        ContinuousModification::GrantAllTriggeredAbilitiesOf { source } => Some(source),
        _ => None,
    });
    assert_eq!(
        activated_source,
        Some(&pinned),
        "the activated grant's parsed source must be the CR 607.2a exile-pinned \
         ChosenCard shape (reader + composed InZone{{Exile}})"
    );
    assert_eq!(
        triggered_source,
        Some(&pinned),
        "the triggered grant's parsed source must be the CR 607.2a exile-pinned \
         ChosenCard shape (reader + composed InZone{{Exile}})"
    );
}

/// A creature card sitting in exile with a distinct, donatable mana ability.
fn exiled_mana_creature(state: &mut GameState, card_id: u64, color: ManaColor) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        format!("Exiled Face {card_id}"),
        Zone::Exile,
    );
    let object = state.objects.get_mut(&id).unwrap();
    object.card_types.core_types = vec![CoreType::Creature];
    object.base_card_types = object.card_types.clone();
    object.abilities = Arc::new(vec![AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![color],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )]);
    id
}

/// The mana colors Koh currently has granted (Layer-6 ability grants).
fn koh_granted_mana_colors(state: &GameState, koh: ObjectId) -> Vec<ManaColor> {
    state.objects[&koh]
        .abilities
        .iter()
        .filter_map(|a| match a.effect.as_ref() {
            Effect::Mana {
                produced: ManaProduction::Fixed { colors, .. },
                ..
            } => Some(colors.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Record `card` as Koh's last chosen card through the real `RememberCard`
/// resolver (targeting the card directly).
fn resolve_remember_card(state: &mut GameState, koh: ObjectId, card: ObjectId) {
    let definition = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::RememberCard {
            target: TargetFilter::SpecificObject { id: card },
        },
    );
    let ability = build_resolved_from_def(&definition, koh, P0);
    let mut events = Vec::new();
    resolve_ability_chain(state, &ability, &mut events, 0).expect("RememberCard must resolve");
}

/// CR 607.2a + CR 607.2d + CR 400.7: the parsed statics grant ONLY the
/// remembered exiled card's abilities, and the grant drops once that card leaves
/// exile — the composed exile pin stops matching AND the moved card is a new
/// object at the same storage id, so the stored incarnation pin no longer names
/// it.
#[test]
fn koh_pin_grants_only_the_remembered_exiled_card_and_drops_when_it_leaves_exile() {
    let parsed = parse_koh();

    let mut state = GameState::new_two_player(7);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let koh = create_object(
        &mut state,
        CardId(1000),
        P0,
        "Koh, the Face Stealer".to_string(),
        Zone::Battlefield,
    );
    {
        let object = state.objects.get_mut(&koh).unwrap();
        object.card_types.core_types = vec![CoreType::Creature];
        object.base_card_types = object.card_types.clone();
        object.static_definitions = parsed.statics.clone().into();
    }

    let remembered = exiled_mana_creature(&mut state, 2000, ManaColor::Green);
    let other = exiled_mana_creature(&mut state, 3000, ManaColor::Red);

    // Negative baseline before the choice: the reader is fail-closed.
    evaluate_layers(&mut state);
    assert!(
        koh_granted_mana_colors(&state, koh).is_empty(),
        "before any card is chosen, Koh must have no granted ability"
    );

    // Real writer: record the Green face as Koh's last chosen card.
    resolve_remember_card(&mut state, koh, remembered);
    evaluate_layers(&mut state);
    assert_eq!(
        koh_granted_mana_colors(&state, koh),
        vec![ManaColor::Green],
        "while the remembered card is in exile, ONLY its ability is granted \
         (positive reach-guard; the Red exiled card must not be granted — \
         multi-authority negative)"
    );

    // CR 400.7: the remembered card leaves exile through the production
    // zone-change pipeline (a new object at the same storage id). The composed
    // CR 607.2a pin stops matching, so the grant drops; the still-exiled Red
    // card is not the remembered object and must not be granted either.
    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            &mut state,
            ZoneMoveRequest::effect(remembered, Zone::Graveyard, remembered),
            &mut events,
        ),
        "the exile -> graveyard move must terminate, not park on a replacement choice"
    );
    evaluate_layers(&mut state);
    assert!(
        koh_granted_mana_colors(&state, koh).is_empty(),
        "once the remembered card leaves exile the exile pin must drop the grant \
         (CR 400.7: the moved card is a new object at the same storage id, so the \
         stored incarnation pin no longer names it); the other exiled card must \
         NOT inherit the grant"
    );
    assert_eq!(
        state.objects[&other].zone,
        Zone::Exile,
        "control: the non-remembered exiled card is still in exile"
    );
}
