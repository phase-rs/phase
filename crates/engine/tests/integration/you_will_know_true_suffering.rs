//! You Will Know True Suffering: commander's-mana-value burn, commanders spared.
//!
//! Oracle: "When you set this scheme in motion, it deals damage equal to your
//! commander's mana value to each noncommander creature your opponents
//! control."
//!
//! CR 701.32a + CR 904.9: the set-in-motion trigger deals the damage.
//! CR 202.3 + CR 903.3d: the amount is the archenemy's commander's mana
//! value (battlefield or command zone). CR 903.3: "noncommander" excludes
//! every commander — commander is a designation, not a subtype, so the
//! exclusion is `Not(IsCommander)`, never the vacuous `Non(Subtype(...))`.
//!
//! Revert-to-red: drop the possessive quantity arm — the document gaps
//! (scheme_face asserts). Drop the `Not(IsCommander)` exclusion — the
//! opponent's commander dies (exclusion assertions fail). Resolve the amount
//! as 0 — nothing dies (victim assertions fail).

use engine::database::synthesis::synthesize_archenemy;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
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
