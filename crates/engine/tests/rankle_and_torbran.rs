//! Rankle and Torbran — runtime coverage for the third modal mode's global
//! damage-boost replacement: "If a source would deal damage to a player or battle
//! this turn, it deals that much damage plus 2 instead."
//!
//! The parser emits this as an `Effect::AddTargetReplacement` with
//! `target: TargetFilter::None` carrying a `ReplacementDefinition` with
//! `damage_modification: Plus { value: 2 }`, `damage_target_filter: Player { Any }`,
//! NO `damage_source_filter` (the clause is "a source", i.e. *any* source), and
//! `expiry: EndOfTurn` (verified against `client/public/card-data.json`'s parsed
//! `triggers` for "rankle and torbran").
//!
//! Because the source clause is "a source" (not "a source you control"), this
//! mode carries no controller-relative `damage_source_filter` and was therefore
//! NOT broken by the `ObjectId(0)` controller bug the `source_controller` anchor
//! fixes — its match path never consults a source controller. These tests are
//! runtime coverage for the boost itself (boost any source dealing damage to a
//! player; never boost damage dealt to a permanent), confirming the anchor change
//! does not regress the any-source class. They drive the install path
//! (`add_target_replacement::resolve`) and the apply path (`replace_event`).

use engine::game::effects::add_target_replacement;
use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::zones::create_object;
use engine::types::ability::{
    DamageModification, DamageTargetFilter, DamageTargetPlayerScope, Duration, Effect,
    ReplacementDefinition, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// The replacement the parser emits for Rankle and Torbran's third mode.
fn torbran_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .damage_modification(DamageModification::Plus { value: 2 })
        .damage_target_filter(DamageTargetFilter::Player {
            player: DamageTargetPlayerScope::Any,
        })
}

fn install(state: &mut GameState, controller: PlayerId) {
    let mut ability = ResolvedAbility::new(
        Effect::AddTargetReplacement {
            replacement: Box::new(torbran_replacement()),
            target: TargetFilter::None,
        },
        Vec::new(),
        ObjectId(7),
        controller,
    );
    ability.duration = Some(Duration::UntilEndOfTurn);
    let mut events = Vec::new();
    add_target_replacement::resolve(state, &ability, &mut events).unwrap();
}

fn damage(source: ObjectId, target: TargetRef, amount: u32) -> ProposedEvent {
    ProposedEvent::Damage {
        source_id: source,
        target,
        amount,
        is_combat: false,
        applied: Default::default(),
    }
}

/// Any source dealing damage to a player is boosted by +2.
#[test]
fn boosts_any_source_dealing_damage_to_a_player() {
    let mut state = GameState::new_two_player(42);
    // Source controlled by the opponent — still boosted ("a source", any).
    let any_source = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Any Source".to_string(),
        Zone::Battlefield,
    );

    install(&mut state, PlayerId(0));

    let mut events = Vec::new();
    let result = replace_event(
        &mut state,
        damage(any_source, TargetRef::Player(PlayerId(1)), 3),
        &mut events,
    );
    let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
        panic!("expected modified damage event, got {result:?}");
    };
    assert_eq!(
        amount, 5,
        "any source dealing damage to a player deals 3 + 2 = 5"
    );
}

/// Damage dealt to a permanent (not a player) is NOT boosted — the
/// `damage_target_filter` is `Player`.
#[test]
fn does_not_boost_damage_to_a_permanent() {
    let mut state = GameState::new_two_player(42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Source".to_string(),
        Zone::Battlefield,
    );
    let creature = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Creature".to_string(),
        Zone::Battlefield,
    );

    install(&mut state, PlayerId(0));

    let mut events = Vec::new();
    let result = replace_event(
        &mut state,
        damage(source, TargetRef::Object(creature), 3),
        &mut events,
    );
    let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
        panic!("expected unmodified damage event, got {result:?}");
    };
    assert_eq!(
        amount, 3,
        "damage to a permanent is not boosted (target filter is Player)"
    );
}
