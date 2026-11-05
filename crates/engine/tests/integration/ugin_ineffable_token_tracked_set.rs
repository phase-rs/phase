//! Ugin, the Ineffable's +1 — the tracked set its delayed trigger reads must
//! hold the created Spirit token as well as the exiled card (#9225 companion).
//!
//! Oracle (+1): "Exile the top card of your library face down and look at it.
//! Create a 2/2 colorless Spirit creature token. When that token leaves the
//! battlefield, put the exiled card into your hand."
//!
//! The chain is `ExileTop → Token → CreateDelayedTrigger { uses_tracked_set }`.
//! "That token" names the created object, so the token creation must still
//! publish into the chain set. The #9225 rule that keeps a created token out of
//! a set read as CARDS (CR 108.2b: tokens aren't cards — Locke's and Ragavan's
//! cast grants) must not reach this consumer.
//!
//! Driven at `resolve_ability_chain` level on purpose: end to end, Ugin's
//! delayed trigger currently does not fire for an unrelated pre-existing
//! reason, so a full-pipeline assertion could not distinguish this seam.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const UGIN_PLUS_ONE: &str = "Exile the top card of your library face down and look at it. \
     Create a 2/2 colorless Spirit creature token. When that token leaves the \
     battlefield, put the exiled card into your hand.";

/// CR 608.2c: "that token" is the Spirit this ability created, so the set the
/// delayed trigger reads contains it, alongside the exiled card it returns.
#[test]
fn ugin_delayed_trigger_set_contains_the_spirit_token_and_the_exiled_card() {
    let mut state = GameState::new(FormatConfig::standard(), 2, 42);
    let source = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Ugin, the Ineffable".to_string(),
        Zone::Battlefield,
    );
    let top = create_object(
        &mut state,
        CardId(2),
        PlayerId(0),
        "Top Card".to_string(),
        Zone::Library,
    );

    let def = parse_effect_chain(UGIN_PLUS_ONE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, PlayerId(0));
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("Ugin's +1 resolves");

    assert_eq!(
        state.objects[&top].zone,
        Zone::Exile,
        "reach guard: the top card was exiled"
    );
    let spirit = state
        .objects
        .values()
        .find(|object| object.is_token && object.zone == Zone::Battlefield)
        .map(|object| object.id)
        .expect("reach guard: the Spirit token was created");
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "reach guard: the leaves-the-battlefield trigger was created"
    );

    let sets: Vec<&Vec<_>> = state.tracked_object_sets.values().collect();
    assert_eq!(sets.len(), 1, "one chain set for this resolution");
    assert!(
        sets[0].contains(&top),
        "the set holds the exiled card the trigger returns"
    );
    assert!(
        sets[0].contains(&spirit),
        "the set holds the Spirit — \"that token\" names the created object"
    );
}
