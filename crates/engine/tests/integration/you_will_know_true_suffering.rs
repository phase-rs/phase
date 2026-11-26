//! You Will Know True Suffering: commander's-mana-value burn, commanders spared.
//!
//! Oracle: "When you set this scheme in motion, it deals damage equal to your
//! commander's mana value to each noncommander creature your opponents
//! control."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger deals the damage.
//! CR 202.3 + CR 903.3e: the amount is the archenemy's commander's mana
//! value, read in all zones, including library and hand. CR 903.3: "noncommander" excludes
//! every commander — commander is a designation, not a subtype, so the
//! exclusion is `Not(IsCommander)`, never the vacuous `Non(Subtype(...))`.
//!
//! Revert-to-red: drop the possessive quantity arm — the document gaps
//! (scheme_face asserts). Drop the `Not(IsCommander)` exclusion — the
//! opponent's commander dies (exclusion assertions fail). Resolve the amount
//! as 0 — nothing dies (victim assertions fail).

use crate::support::shared_card_db;
use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const YOU_WILL_KNOW_TRUE_SUFFERING: &str = "When you set this scheme in motion, it deals damage equal to your commander's mana value to each noncommander creature your opponents control.";

fn scheme_face(name: &str, oracle: &str) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &["Scheme".to_string()], &[]);
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "{name}: document is gapped"
    );
    let mut face = CardFace {
        name: name.to_string(),
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Scheme);
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

/// Resolve every pending trigger with no choices (mass damage is prompt-free).
fn drain_triggers(runner: &mut GameRunner) {
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while draining triggers: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
}

/// Drive to `target`, declaring no attackers/blockers through combat and
/// passing priority. Panics on any other prompt.
fn drive_to_phase(runner: &mut GameRunner, target: Phase) {
    for _ in 0..64 {
        runner.advance_to_phase(target);
        if runner.state().phase == target {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner.declare_blockers(&[]).expect("declare no blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("unexpected prompt driving to {target:?}: {other:?}"),
        }
    }
    panic!("never reached {target:?}");
}

#[test]
fn you_will_know_true_suffering_burns_commander_mv_sparing_commanders() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    // The archenemy's MV-4 commander sets the burn amount.
    let archenemy_commander = scenario
        .add_creature(P0, "Archenemy Commander", 4, 4)
        .with_mana_cost(ManaCost::generic(4))
        .commander()
        .id();
    // Own noncommander creature: out of scope ("your opponents control").
    let own_bystander = scenario.add_creature(P0, "Own Bystander", 2, 2).id();
    // Opponent's noncommanders bracket the amount: toughness 4 dies, 5 lives.
    let victim = scenario.add_creature(P1, "Doomed Victim", 2, 4).id();
    let survivor = scenario.add_creature(P1, "Tough Survivor", 2, 5).id();
    // Opponent's commander: excluded by "noncommander" (2 toughness would
    // die to any nonzero damage, so survival proves the exclusion).
    let opponent_commander = scenario
        .add_creature(P1, "Opponent Commander", 2, 2)
        .commander()
        .id();
    let mut runner = scenario.build();

    let face = scheme_face("You Will Know True Suffering", YOU_WILL_KNOW_TRUE_SUFFERING);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    drain_triggers(&mut runner);
    runner.advance_until_stack_empty();

    let zone_of = |runner: &GameRunner, id: engine::types::identifiers::ObjectId| {
        runner.state().objects[&id].zone
    };
    assert_eq!(
        zone_of(&runner, victim),
        Zone::Graveyard,
        "toughness 4 must die to the MV-4 burn"
    );
    assert_eq!(
        zone_of(&runner, survivor),
        Zone::Battlefield,
        "toughness 5 must survive the MV-4 burn"
    );
    assert_eq!(
        zone_of(&runner, opponent_commander),
        Zone::Battlefield,
        "the opponent's commander must be spared by noncommander"
    );
    assert_eq!(
        runner.state().objects[&opponent_commander].damage_marked,
        0,
        "the spared commander must take no damage"
    );
    assert_eq!(
        zone_of(&runner, own_bystander),
        Zone::Battlefield,
        "the archenemy's own creatures are out of scope"
    );
    assert_eq!(
        zone_of(&runner, archenemy_commander),
        Zone::Battlefield,
        "the archenemy's commander must survive its own scheme"
    );
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack"
    );
}

fn db() -> &'static engine::database::card_db::CardDatabase {
    shared_card_db().expect("the committed integration fixture loads")
}

/// Archenemy game at draw with stocked libraries, before commanders/victims.
fn archenemy_game() -> GameScenario {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..10 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    scenario
}

/// CR 903.3: the designation belongs to the card, so a commander placed outside
/// the command zone is designated directly, with no zone write.
fn designate(runner: &mut GameRunner, commander: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&commander)
        .unwrap()
        .is_commander = true;
}

/// Install the scheme and drive to precombat main, asserting turn 1 sets it in
/// motion (the set-in-motion trigger is left on the stack for the caller to
/// drain).
fn set_scheme_in_motion(runner: &mut GameRunner) {
    let face = scheme_face("You Will Know True Suffering", YOU_WILL_KNOW_TRUE_SUFFERING);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;
    drive_to_phase(runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
}

/// Drain the stack to priority, answering the commander choice with `pick`.
/// The prompt is asserted unconditionally (actor + full eligibility); a second
/// offering fails. Returns every event observed while draining.
fn drain_answering_commander_choice(
    runner: &mut GameRunner,
    expected: &[ObjectId],
    pick: ObjectId,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut answered = false;
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                events.extend(result.events);
            }
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                assert!(
                    !answered,
                    "the commander choice must be offered exactly once"
                );
                assert_eq!(player, P0, "the owner chooses their commander");
                assert_eq!(
                    cards.len(),
                    expected.len(),
                    "eligibility must be exactly the owned commanders: {cards:?}"
                );
                for id in expected {
                    assert!(
                        cards.contains(id),
                        "eligibility must offer {id:?}: {cards:?}"
                    );
                }
                let result = runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("answer the commander choice");
                events.extend(result.events);
                answered = true;
            }
            other => panic!("unexpected prompt while draining triggers: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    assert!(answered, "the commander choice prompt was never offered");
    events
}

/// Drain the stack to priority, failing on ANY prompt (the singleton/zero
/// rows stay silent). Returns every event observed while draining.
fn drain_silently_collecting(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..64 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                events.extend(result.events);
            }
            other => panic!("unexpected prompt while draining triggers: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    events
}

fn assert_priority_and_empty_stack(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the chain must resolve back to priority with an empty stack, got {:?} / {:?}",
        runner.state().stack.len(),
        runner.state().waiting_for
    );
}

/// F2-part: printed partners → choice prompt; the chosen MV burns.
///
/// Tymna the Weaver (MV 3) + Kydele, Chosen of Kruphix (MV 4) — both
/// generic-Partner fixtures. Choosing Tymna burns 3: the toughness-4 victim
/// lives and the toughness-3 victim dies (which also pins "choice", not
/// "greatest" — a Max misread would kill both).
///
/// Revert-to-red: no prompt is offered (the drain's `answered` assertion
/// fails).
#[test]
fn partner_choice_burns_chosen_mv() {
    let mut scenario = archenemy_game();
    let tymna = scenario.add_real_card(P0, "Tymna the Weaver", Zone::Battlefield, db());
    let kydele = scenario.add_real_card(P0, "Kydele, Chosen of Kruphix", Zone::Hand, db());
    scenario.with_commander(kydele);
    let victim = scenario.add_creature(P1, "Doomed Victim", 2, 3).id();
    let survivor = scenario.add_creature(P1, "Tough Survivor", 2, 4).id();
    let mut runner = scenario.build();
    designate(&mut runner, tymna);
    assert_eq!(runner.state().objects[&tymna].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&kydele].zone, Zone::Command);

    set_scheme_in_motion(&mut runner);
    let _events = drain_answering_commander_choice(&mut runner, &[tymna, kydele], tymna);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "toughness 3 must die to the chosen MV-3 burn"
    );
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "toughness 4 must survive the chosen MV-3 burn (not the Max MV 4)"
    );
    assert_priority_and_empty_stack(&runner);
}

/// F2-part sibling (rules-fidelity): the gate counts candidates, not values —
/// two MV-3 Tymnas still raise the prompt.
#[test]
fn partner_choice_prompt_offered_for_equal_mvs() {
    let mut scenario = archenemy_game();
    let tymna_a = scenario.add_real_card(P0, "Tymna the Weaver", Zone::Battlefield, db());
    let tymna_b = scenario.add_real_card(P0, "Tymna the Weaver", Zone::Hand, db());
    scenario.with_commander(tymna_b);
    let victim = scenario.add_creature(P1, "Doomed Victim", 2, 3).id();
    let survivor = scenario.add_creature(P1, "Tough Survivor", 2, 4).id();
    let mut runner = scenario.build();
    designate(&mut runner, tymna_a);

    set_scheme_in_motion(&mut runner);
    let _events = drain_answering_commander_choice(&mut runner, &[tymna_a, tymna_b], tymna_a);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "toughness 3 must die to the MV-3 burn"
    );
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "toughness 4 must survive the MV-3 burn"
    );
    assert_priority_and_empty_stack(&runner);
}

/// F2-beacon: a commander in hand counts; the singleton stays silent.
///
/// The commander reaches the hand through a real Command Beacon activation
/// (production move, namesake witness). The burn equals the hand commander's
/// MV with no prompt, and no `CardsRevealed` names it (a read is not a
/// reveal).
///
/// Revert-to-red: the hand is outside the legacy two-zone population, so the
/// burn is 0 and the victim lives.
#[test]
fn commander_in_hand_sets_burn_silently() {
    let mut scenario = archenemy_game();
    let commander = scenario
        .add_creature(P0, "Handbound Commander", 5, 5)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    scenario.with_commander(commander);
    let beacon = scenario.add_real_card(P0, "Command Beacon", Zone::Battlefield, db());
    let victim = scenario.add_creature(P1, "Doomed Victim", 2, 5).id();
    let survivor = scenario.add_creature(P1, "Tough Survivor", 2, 6).id();
    let mut runner = scenario.build();

    let face = scheme_face("You Will Know True Suffering", YOU_WILL_KNOW_TRUE_SUFFERING);
    let scheme = create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    // Real Beacon activation: {T}, sacrifice → commander to hand.
    runner.activate(beacon, 1).resolve();
    // CR 903.9b: the commander would go to hand from the command zone, so its
    // owner is offered the command-zone replacement — decline it (index 1
    // lets the move proceed).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "expected the CR 903.9b replacement prompt, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the command-zone replacement");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&commander].zone,
        Zone::Hand,
        "reach guard: the Beacon must have moved the commander to hand"
    );

    drive_to_phase(&mut runner, Phase::PreCombatMain);
    assert!(
        runner.state().command_zone.contains(&scheme) && !runner.state().objects[&scheme].face_down,
        "reach guard: turn 1 must set the scheme in motion"
    );
    let events = drain_silently_collecting(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "toughness 5 must die to the hand commander's MV-5 burn"
    );
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "toughness 6 must survive the MV-5 burn"
    );
    for event in &events {
        if let GameEvent::CardsRevealed {
            card_ids,
            card_names,
            ..
        } = event
        {
            assert!(
                !card_ids.contains(&commander),
                "a read is not a reveal: {event:?}"
            );
            assert!(
                !card_names.iter().any(|name| name == "Handbound Commander"),
                "a read is not a reveal: {event:?}"
            );
        }
    }
    assert_priority_and_empty_stack(&runner);
}

/// F2-beacon hostile: commanders in hand AND on the battlefield → the prompt
/// offers both (the all-zone census, not just the battlefield).
#[test]
fn commander_in_hand_and_battlefield_prompts_for_both() {
    let mut scenario = archenemy_game();
    let bf_commander = scenario
        .add_creature(P0, "Battlefield Commander", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .commander()
        .id();
    let hand_commander = scenario
        .add_creature_to_hand(P0, "Hand Commander", 6, 6)
        .with_mana_cost(ManaCost::generic(6))
        .commander()
        .id();
    let victim = scenario.add_creature(P1, "Doomed Victim", 2, 6).id();
    let survivor = scenario.add_creature(P1, "Tough Survivor", 2, 7).id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&hand_commander].zone, Zone::Hand);

    set_scheme_in_motion(&mut runner);
    let _events = drain_answering_commander_choice(
        &mut runner,
        &[bf_commander, hand_commander],
        hand_commander,
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "toughness 6 must die to the chosen hand commander's MV-6 burn"
    );
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "toughness 7 must survive the MV-6 burn"
    );
    assert_priority_and_empty_stack(&runner);
}

/// F2-opp: an opponent-controlled commander counts (ownership decides,
/// control is irrelevant). Singleton → silent.
///
/// Revert-to-red: the controller filter misses the stolen commander, so the
/// burn is 0 and the victim lives.
#[test]
fn opponent_controlled_commander_counts() {
    let mut scenario = archenemy_game();
    let stolen = scenario
        .add_creature(P0, "Stolen Commander", 4, 4)
        .with_mana_cost(ManaCost::generic(4))
        .commander()
        .controlled_by(P1)
        .id();
    let victim = scenario.add_creature(P1, "Doomed Victim", 2, 4).id();
    let survivor = scenario.add_creature(P1, "Tough Survivor", 2, 5).id();
    let mut runner = scenario.build();

    set_scheme_in_motion(&mut runner);
    let _events = drain_silently_collecting(&mut runner);
    runner.advance_until_stack_empty();

    // Reach guards: the owner/control split survived setup and resolution.
    assert_eq!(runner.state().objects[&stolen].owner, P0);
    assert_eq!(runner.state().objects[&stolen].controller, P1);
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "toughness 4 must die to the stolen commander's MV-4 burn"
    );
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "toughness 5 must survive the MV-4 burn"
    );
    assert_priority_and_empty_stack(&runner);
}

/// F2-opp hostile: the opponent controls one owned commander and another is
/// owned outright → the ownership census prompts (and the stolen one is a
/// legal pick).
#[test]
fn opponent_controlled_commander_prompts_with_second_owned() {
    let mut scenario = archenemy_game();
    let stolen = scenario
        .add_creature(P0, "Stolen Commander", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .commander()
        .controlled_by(P1)
        .id();
    let loyal = scenario
        .add_creature(P0, "Loyal Commander", 5, 5)
        .with_mana_cost(ManaCost::generic(5))
        .commander()
        .id();
    let victim = scenario.add_creature(P1, "Doomed Victim", 2, 3).id();
    let survivor = scenario.add_creature(P1, "Tough Survivor", 2, 4).id();
    let mut runner = scenario.build();

    set_scheme_in_motion(&mut runner);
    let _events = drain_answering_commander_choice(&mut runner, &[stolen, loyal], stolen);
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&stolen].owner, P0);
    assert_eq!(runner.state().objects[&stolen].controller, P1);
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Graveyard,
        "toughness 3 must die to the chosen stolen commander's MV-3 burn"
    );
    assert_eq!(
        runner.state().objects[&survivor].zone,
        Zone::Battlefield,
        "toughness 4 must survive the MV-3 burn"
    );
    assert_priority_and_empty_stack(&runner);
}

/// F2-zero: no owned commander → 0, no prompt, never stuck. The opponent HAS
/// a commander (ownership hostile): it must not count.
#[test]
fn burns_zero_silently() {
    let mut scenario = archenemy_game();
    let opponent_commander = scenario
        .add_creature(P1, "Opponent Commander", 2, 2)
        .with_mana_cost(ManaCost::generic(4))
        .commander()
        .id();
    let frail = scenario.add_creature(P1, "Frail Bystander", 1, 1).id();
    let mut runner = scenario.build();

    set_scheme_in_motion(&mut runner);
    let _events = drain_silently_collecting(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&opponent_commander].zone,
        Zone::Battlefield,
        "the opponent's commander must not count toward the archenemy's read"
    );
    assert_eq!(
        runner.state().objects[&frail].zone,
        Zone::Battlefield,
        "toughness 1 must survive the zero burn"
    );
    assert_priority_and_empty_stack(&runner);
}
