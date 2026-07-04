//! I Call for Slaughter (scheme) — runtime coverage for the global damage-boost
//! replacement: "If a source you control would deal damage this turn, it deals
//! that much damage plus 1 instead."
//!
//! The parser emits this as an `Effect::AddTargetReplacement` with
//! `target: TargetFilter::None` carrying a `ReplacementDefinition` with
//! `damage_modification: Plus { value: 1 }`, `damage_source_filter: Typed { controller: You }`,
//! and `expiry: EndOfTurn` (verified against `client/public/card-data.json`'s
//! parsed `triggers` for "i call for slaughter").
//!
//! This is the controller-relative class that was silently broken before the
//! `ReplacementDefinition::source_controller` anchor (CR 109.4): the replacement
//! lives in `pending_damage_replacements` under the sentinel `ObjectId(0)`, which
//! has no controller in `state.objects`, so `ControllerRef::You` never resolved
//! and the boost never fired. These tests drive the install path
//! (`add_target_replacement::resolve`) and the apply path (`replace_event`) with
//! the faithfully-constructed definition above; full end-to-end scheme activation
//! is not modeled (the install/apply seam is the unit of behavior the fix
//! touches).

use engine::game::effects::add_target_replacement;
use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::zones::create_object;
use engine::types::ability::{
    ControllerRef, DamageModification, Duration, Effect, QuantityExpr, ReplacementDefinition,
    ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// The replacement the parser emits for the scheme's "plus 1" clause.
fn slaughter_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .damage_modification(DamageModification::Plus {
            value: QuantityExpr::Fixed { value: 1 },
        })
        .damage_source_filter(TargetFilter::Typed(
            TypedFilter::default().controller(ControllerRef::You),
        ))
}

/// Install the global replacement on behalf of `controller` (the player who set
/// the scheme in motion), end-of-turn duration.
fn install(state: &mut GameState, controller: PlayerId) {
    let mut ability = ResolvedAbility::new(
        Effect::AddTargetReplacement {
            replacement: Box::new(slaughter_replacement()),
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

fn damage(source: ObjectId, victim: ObjectId, amount: u32) -> ProposedEvent {
    ProposedEvent::Damage {
        source_id: source,
        target: TargetRef::Object(victim),
        amount,
        is_combat: false,
        applied: Default::default(),
    }
}

/// A source the scheme's controller controls deals damage boosted by +1.
///
/// Discriminating: the `+1` assertion flips if the controller anchor read at
/// `replacement.rs` is reverted — without it the `ControllerRef::You` source
/// filter resolves against a controller-less sentinel and never matches, leaving
/// damage unmodified at 2.
#[test]
fn boosts_a_source_you_control_by_one() {
    let mut state = GameState::new_two_player(42);
    let my_source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "My Source".to_string(),
        Zone::Battlefield,
    );
    let victim = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Victim".to_string(),
        Zone::Battlefield,
    );

    install(&mut state, PlayerId(0));
    assert_eq!(
        state.pending_damage_replacements[0].source_controller,
        Some(PlayerId(0)),
        "install must anchor the scheme controller (CR 109.4)"
    );

    let mut events = Vec::new();
    let result = replace_event(&mut state, damage(my_source, victim, 2), &mut events);
    let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
        panic!("expected modified damage event, got {result:?}");
    };
    assert_eq!(amount, 3, "a source you control deals 2 + 1 = 3");
}

/// An opponent's source is NOT boosted by "a source you control".
#[test]
fn does_not_boost_opponent_source() {
    let mut state = GameState::new_two_player(42);
    let their_source = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Their Source".to_string(),
        Zone::Battlefield,
    );
    let victim = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Victim".to_string(),
        Zone::Battlefield,
    );

    install(&mut state, PlayerId(0));

    let mut events = Vec::new();
    let result = replace_event(&mut state, damage(their_source, victim, 2), &mut events);
    let ReplacementResult::Execute(ProposedEvent::Damage { amount, .. }) = result else {
        panic!("expected unmodified damage event, got {result:?}");
    };
    assert_eq!(amount, 2, "an opponent's source must not be boosted");
}
