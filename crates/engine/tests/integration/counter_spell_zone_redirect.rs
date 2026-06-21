//! GitHub issue #3980 — "Counter target spell. If that spell is countered this
//! way, put it <zone> instead of into that player's graveyard." must redirect
//! the countered spell to the named zone rather than the graveyard.
//!
//! CR 701.6a + CR 614.1a: the countered spell's default graveyard destination
//! is replaced by the named zone (Memory Lapse top of library, Spell Crumple
//! bottom of library, Remand owner's hand). A plain counter (Cancel) keeps the
//! default graveyard rule (regression).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Put an opponent instant on the stack whose owner is `controller`, mirroring
/// the helper in `issue_3300_counter_spell.rs`.
fn put_spell_on_stack(runner: &mut GameRunner, controller: PlayerId) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(701),
        controller,
        "Shock".to_string(),
        Zone::Stack,
    );
    if let Some(obj) = runner.state_mut().objects.get_mut(&spell) {
        obj.card_types.core_types = vec![CoreType::Instant];
    }
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(701),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

fn build_counter_caster(oracle: &str, name: &str) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut cs = scenario.add_spell_to_hand_from_oracle(P0, name, true, oracle);
    cs.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Blue],
    });
    let counter = cs.id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let runner = scenario.build();
    (runner, counter)
}

/// CR 701.6a + CR 614.1a: Memory Lapse puts the countered spell on TOP of its
/// owner's library, not the graveyard. FAILS if the redirect is dropped (the
/// pre-fix behavior puts it in the graveyard).
#[test]
fn memory_lapse_redirects_countered_spell_to_library_top() {
    const MEMORY_LAPSE: &str = "Counter target spell. If that spell is countered this way, put it on top of its owner's library instead of into that player's graveyard.";
    let (mut runner, counter) = build_counter_caster(MEMORY_LAPSE, "Memory Lapse");
    let opponent_spell = put_spell_on_stack(&mut runner, P1);

    runner
        .cast(counter)
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        runner.state().stack.is_empty(),
        "the spell must be countered (off the stack)"
    );
    assert_eq!(
        runner.state().objects[&opponent_spell].zone,
        Zone::Library,
        "Memory Lapse must redirect the countered spell to its owner's library"
    );
    assert!(
        !runner.state().players[P1.0 as usize]
            .graveyard
            .contains(&opponent_spell),
        "the countered spell must NOT reach the graveyard"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize]
            .library
            .front()
            .copied(),
        Some(opponent_spell),
        "the countered spell must be on TOP of its owner's library"
    );
}

/// CR 701.6a + CR 614.1a: Spell Crumple puts the countered spell on the BOTTOM
/// of its owner's library.
#[test]
fn spell_crumple_redirects_countered_spell_to_library_bottom() {
    const SPELL_CRUMPLE: &str = "Counter target spell. If that spell is countered this way, put it on the bottom of its owner's library instead of into that player's graveyard.";
    let (mut runner, counter) = build_counter_caster(SPELL_CRUMPLE, "Spell Crumple");
    let opponent_spell = put_spell_on_stack(&mut runner, P1);

    runner
        .cast(counter)
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        runner.state().stack.is_empty(),
        "the spell must be countered"
    );
    assert_eq!(
        runner.state().objects[&opponent_spell].zone,
        Zone::Library,
        "Spell Crumple must redirect the countered spell to its owner's library"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize]
            .library
            .back()
            .copied(),
        Some(opponent_spell),
        "the countered spell must be on the BOTTOM of its owner's library"
    );
}

/// CR 701.6a + CR 614.1a: Remand returns the countered spell to its owner's
/// HAND, and its controller draws a card from the trailing "Draw a card."
#[test]
fn remand_redirects_countered_spell_to_hand_and_draws() {
    const REMAND: &str = "Counter target spell. If that spell is countered this way, put it into its owner's hand instead of into that player's graveyard.\nDraw a card.";
    // Give P0 a library card to draw.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut cs = scenario.add_spell_to_hand_from_oracle(P0, "Remand", true, REMAND);
    cs.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Blue],
    });
    let counter = cs.id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_card_to_library_top(P0, "Island");
    let mut runner = scenario.build();
    let opponent_spell = put_spell_on_stack(&mut runner, P1);

    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner
        .cast(counter)
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        runner.state().stack.is_empty(),
        "the spell must be countered"
    );
    assert_eq!(
        runner.state().objects[&opponent_spell].zone,
        Zone::Hand,
        "Remand must redirect the countered spell to its owner's hand"
    );
    assert!(
        runner.state().players[P1.0 as usize]
            .hand
            .contains(&opponent_spell),
        "the countered spell must be in its owner's hand"
    );
    // The Remand caster (P0) drew a card; net hand change is +1 (Remand itself
    // left the hand to the stack, but the draw lands a new card).
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand_before, // -1 for Remand cast, +1 for the draw == net 0
        "Remand's controller must draw a card (net hand: -1 cast +1 draw)"
    );
}

/// REGRESSION: a plain "Counter target spell." (Cancel) keeps the default CR
/// 701.6a graveyard destination — no redirect.
#[test]
fn plain_counter_still_sends_countered_spell_to_graveyard() {
    const CANCEL: &str = "Counter target spell.";
    let (mut runner, counter) = build_counter_caster(CANCEL, "Cancel");
    let opponent_spell = put_spell_on_stack(&mut runner, P1);

    runner
        .cast(counter)
        .target_objects(&[opponent_spell])
        .resolve();

    assert!(
        runner.state().stack.is_empty(),
        "the spell must be countered"
    );
    assert_eq!(
        runner.state().objects[&opponent_spell].zone,
        Zone::Graveyard,
        "a plain counter must send the countered spell to the graveyard"
    );
    assert!(
        runner.state().players[P1.0 as usize]
            .graveyard
            .contains(&opponent_spell),
        "the countered spell must be in its owner's graveyard"
    );
}
