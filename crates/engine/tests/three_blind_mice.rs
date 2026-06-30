//! Regression for issue #2004: Three Blind Mice's Chapter II/III ("II, III —
//! Create a token that's a copy of target token you control.") never occur.
//!
//! CARD TEXT (verified against this engine's `card-data.json`):
//!   (As this Saga enters and after your draw step, add a lore counter.
//!   Sacrifice after IV.)
//!   I — Create a 1/1 white Mouse creature token.
//!   II, III — Create a token that's a copy of target token you control.
//!   IV — Creatures you control get +1/+1 and gain vigilance until end of turn.
//!
//! Root cause: "target token you control" parses to `TargetFilter::Typed {
//! type_filters: [], controller: Some(You), properties: [Token] }` — no card
//! type restriction, since a token can be any permanent type. The legal-target
//! enumerator (`find_legal_targets_with_context` in `targeting.rs`) collapsed
//! any `Typed` filter with empty `type_filters` to "this targets players, not
//! permanents" (the shape of "target opponent"), ignoring `properties`
//! entirely. So Chapter II's target slot enumerated `[Player(controller)]`
//! instead of the tokens on the battlefield, and the ability resolved against
//! the controller as if it were the chosen target rather than pausing for a
//! real token to be selected — `CopyTokenOf` then found no object target to
//! copy and silently no-opped.
//!
//! This test drives the real turn-based-action path that adds lore counters
//! on the controller's precombat main phases (CR 714.3c) and resolves the
//! stack, verifying Chapter II actually creates a copy of the targeted token.

use engine::game::scenario::GameScenario;
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

const THREE_BLIND_MICE: &str = "(As this Saga enters and after your draw step, add a lore counter. Sacrifice after IV.)\nI — Create a 1/1 white Mouse creature token.\nII, III — Create a token that's a copy of target token you control.\nIV — Creatures you control get +1/+1 and gain vigilance until end of turn.";

#[test]
fn three_blind_mice_chapter_ii_copies_targeted_token_on_second_lore_counter() {
    let mut scenario = GameScenario::new();
    let saga_id = scenario
        .add_creature(P0, "Three Blind Mice", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(THREE_BLIND_MICE)
        .id();
    // Both players need a non-empty library so drawing across several turns
    // doesn't decking-out before the Saga reaches its second chapter.
    let plains = ["Plains"; 10];
    scenario.with_library_top(P0, &plains);
    scenario.with_library_top(PlayerId(1), &plains);

    let mut runner = scenario.build();

    // Simulate Chapter I having already resolved: the Saga has one lore
    // counter, and its "Create a 1/1 white Mouse" token is on the
    // battlefield as the only legal target for "target token you control".
    let mouse_id = create_object(
        runner.state_mut(),
        CardId(900),
        P0,
        "Mouse".to_string(),
        Zone::Battlefield,
    );
    {
        let state = runner.state_mut();
        let mouse = state.objects.get_mut(&mouse_id).unwrap();
        mouse.is_token = true;
        mouse.card_types.core_types.push(CoreType::Creature);
        mouse.power = Some(1);
        mouse.toughness = Some(1);

        let saga = state.objects.get_mut(&saga_id).unwrap();
        saga.counters.insert(CounterType::Lore, 1);

        // Park at the end of P0's turn 1 so the next precombat main phase we
        // drive through belongs to P0 again (turn 3), where CR 714.3c adds
        // the Saga's second lore counter.
        state.turn_number = 1;
        state.active_player = P0;
        state.phase = Phase::End;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // Walk through the rest of turn 1, all of P1's turn 2, and into P0's
    // turn 3 precombat main -- the turn-based action that adds the Saga's
    // second lore counter (CR 714.3c) and triggers Chapter II.
    runner.advance_to_phase(Phase::PreCombatMain); // lands on P1's PreCombatMain (turn 2)
    runner.pass_both_players();
    runner.advance_to_phase(Phase::PreCombatMain); // lands on P0's PreCombatMain (turn 3)

    let lore = runner
        .state()
        .objects
        .get(&saga_id)
        .and_then(|obj| obj.counters.get(&CounterType::Lore).copied())
        .unwrap_or(0);
    assert_eq!(
        lore, 2,
        "the turn-based action should have added the Saga's second lore counter"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Chapter II's CopyTokenOf trigger should be on the stack"
    );

    // Let the Chapter II trigger resolve.
    runner.pass_both_players();

    let tokens_named_mouse = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| obj.is_token && obj.name == "Mouse")
        .count();
    assert_eq!(
        tokens_named_mouse, 2,
        "Chapter II should have created a copy of the targeted Mouse token"
    );
}
