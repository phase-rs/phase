//! Runtime regression for the Ad Nauseam repeat-loop batching bug
//! (phase-rs/phase#1032) — "Reveal the top card of your library and put that
//! card into your hand. You lose life equal to its mana value. You may repeat
//! this process any number of times." (CR 107.1c + CR 608.2c).
//!
//! Ad Nauseam parses to a ROOT spell ability whose chain is
//! `RevealTop -> ChangeZone(top card into hand) -> LoseLife(its mana value)`,
//! stamped with `repeat_until: Some(ControllerChoice)`. Each iteration prompts
//! the controller via `WaitingFor::RepeatDecision`; on accept the process runs
//! again, on decline the loop ends.
//!
//! Discriminating assertion (the bug): before the fix, on `accept` the handler
//! re-entered `resolve_ability_chain` WITHOUT first resetting
//! `state.waiting_for` away from the just-answered `RepeatDecision`. That stale
//! value made `waits_for_resolution_choice` wrongly defer each iteration's
//! `ChangeZone`/`LoseLife` sub-chain into `pending_continuation` (accumulating
//! via `append_to_sub_chain`) instead of applying it immediately — so every
//! accepted iteration's card-to-hand and life-loss only landed in one batch
//! when the controller eventually declined. Final totals are identical either
//! way (3 cards, -6 life); only the BETWEEN-accepts checkpoints diverge, so we
//! assert per-iteration state, not just the final aggregate.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AD_NAUSEAM_ORACLE: &str = "Reveal the top card of your library and put that card into your hand. You lose life equal to its mana value. You may repeat this process any number of times.";

/// Parse Ad Nauseam's spell ability, asserting the parsed ROOT ability carries
/// the `ControllerChoice` repeat continuation (the "any number of times" loop).
fn ad_nauseam_def() -> engine::types::ability::AbilityDefinition {
    let parsed = parse_oracle_text(
        AD_NAUSEAM_ORACLE,
        "Ad Nauseam",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let def = parsed
        .abilities
        .first()
        .expect("Ad Nauseam parses to a spell ability")
        .clone();
    assert!(
        matches!(
            def.repeat_until,
            Some(engine::types::ability::RepeatContinuation::ControllerChoice)
        ),
        "precondition: parsed ability carries a ControllerChoice repeat, got {:?}",
        def.repeat_until
    );
    def
}

/// Add the Ad Nauseam spell object to the stack as the ability source.
fn add_source(runner: &mut GameRunner) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Ad Nauseam".to_string(),
        Zone::Stack,
    )
}

/// Add a card with mana value `mv` to the TOP-of-library position that reflects
/// insertion order (`library[0]` = top, `zones.rs` convention). Cards are added
/// top-to-bottom by call order.
fn add_library_card(runner: &mut GameRunner, name: &str, mv: u32) -> ObjectId {
    let card_id = CardId(runner.state().next_object_id);
    let id = create_object(
        runner.state_mut(),
        card_id,
        P0,
        name.to_string(),
        Zone::Library,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.mana_cost = ManaCost::generic(mv);
    id
}

fn hand_size(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

fn life(runner: &GameRunner) -> i32 {
    runner.state().players[P0.0 as usize].life
}

fn hand_contains(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().players[P0.0 as usize].hand.contains(&id)
}

fn awaiting_repeat(runner: &GameRunner) -> bool {
    matches!(
        runner.state().waiting_for,
        WaitingFor::RepeatDecision { .. }
    )
}

#[test]
fn ad_nauseam_applies_hand_and_life_loss_immediately_per_iteration() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    // Top-to-bottom: A(mv 2), B(mv 3), C(mv 1).
    let card_a = add_library_card(&mut runner, "Card A", 2);
    let card_b = add_library_card(&mut runner, "Card B", 3);
    let card_c = add_library_card(&mut runner, "Card C", 1);

    let start_life = life(&runner);
    let start_hand = hand_size(&runner);

    let def = ad_nauseam_def();
    let source = add_source(&mut runner);
    let ability = build_resolved_from_def(&def, source, P0);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    // Initial pass: A into hand, lose 2 life, prompted to repeat.
    assert!(
        awaiting_repeat(&runner),
        "initial pass must prompt to repeat"
    );
    assert_eq!(hand_size(&runner), start_hand + 1, "A revealed into hand");
    assert!(hand_contains(&runner, card_a), "card A is the one in hand");
    assert_eq!(
        life(&runner),
        start_life - 2,
        "lose life = A's mana value 2"
    );

    // First accept reveals B. THE CHECKPOINT THAT FAILS ON UNFIXED CODE:
    // buggy code defers B's move-to-hand + life-loss into pending_continuation,
    // so hand/life would still read start+1 / start-2 here.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("first accept resolves");
    assert!(
        awaiting_repeat(&runner),
        "still prompting after first accept"
    );
    assert_eq!(
        hand_size(&runner),
        start_hand + 2,
        "B must land in hand IMMEDIATELY on accept, not batch-deferred to decline"
    );
    assert!(hand_contains(&runner, card_b), "card B now in hand");
    assert_eq!(
        life(&runner),
        start_life - 5,
        "life must drop by B's mana value 3 IMMEDIATELY on accept (total -5)"
    );

    // Second accept reveals C.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("second accept resolves");
    assert!(
        awaiting_repeat(&runner),
        "still prompting after second accept"
    );
    assert_eq!(hand_size(&runner), start_hand + 3, "C now in hand");
    assert!(hand_contains(&runner, card_c), "card C now in hand");
    assert_eq!(
        life(&runner),
        start_life - 6,
        "life -6 total after C's mv 1"
    );

    // Decline: loop ends, no further change.
    let hand_before_decline = hand_size(&runner);
    let life_before_decline = life(&runner);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline ends the loop");
    assert!(
        !awaiting_repeat(&runner),
        "declining ends the repeat loop — no longer prompting"
    );
    assert_eq!(
        hand_size(&runner),
        hand_before_decline,
        "decline applies nothing new — everything already landed per-iteration"
    );
    assert_eq!(
        life(&runner),
        life_before_decline,
        "decline applies no further life loss — nothing was batch-deferred"
    );
}

#[test]
fn ad_nauseam_decline_immediately_stops_after_mandatory_pass() {
    // CR 107.1c: "any number of times" includes zero repeats — declining right
    // after the mandatory first pass ends the loop with exactly one card in hand
    // and one life-loss applied. Sibling/negative case.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let only_card = add_library_card(&mut runner, "Solo Card", 4);
    let start_life = life(&runner);
    let start_hand = hand_size(&runner);

    let def = ad_nauseam_def();
    let source = add_source(&mut runner);
    let ability = build_resolved_from_def(&def, source, P0);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).unwrap();

    assert!(awaiting_repeat(&runner), "initial pass prompts to repeat");
    assert_eq!(hand_size(&runner), start_hand + 1, "the one card into hand");
    assert!(hand_contains(&runner, only_card), "the card is in hand");
    assert_eq!(
        life(&runner),
        start_life - 4,
        "lose life = its mana value 4"
    );

    // Decline immediately — zero repeats.
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("immediate decline ends the loop");
    assert!(
        !awaiting_repeat(&runner),
        "declining after the mandatory pass ends the loop"
    );
    assert_eq!(
        hand_size(&runner),
        start_hand + 1,
        "no further card enters hand after decline"
    );
    assert_eq!(
        life(&runner),
        start_life - 4,
        "no further life loss after decline"
    );
}
