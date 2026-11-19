//! Nature Shields Its Own: the unblocked-attacker trigger only fires when
//! its controller is the defending player, and four attackers put the
//! scheme away at end of combat.
//!
//! Oracle: "(An ongoing scheme remains face up until it's abandoned.)
//! Whenever a creature attacks and isn't blocked, if you're the defending player, create a 0/1 green Plant creature token that's blocking that creature.
//! When four or more creatures attack you, abandon this scheme at end of combat."
//!
//! CR 508.5 + CR 603.4: "if you're the defending player" is an
//! intervening-if comparing the controller against the defending player of
//! the triggering attacker. CR 508.1 + CR 603.2c: "four or more creatures
//! attack you" counts attackers on the controller through the AttackTarget
//! subject. CR 603.7a: "at end of combat" schedules the abandon past damage.
//!
//! Revert-to-red: drop the intervening-if — the P0T2 negative fails (a
//! Plant appears for the Lion's attack on P1). Drop the defender axis from
//! the count — the abandon still fires but so would attacks elsewhere (the
//! 3-attack negative only guards the threshold; the defender axis is pinned
//! by the parser test). Break the delayed trigger — the scheme stays face up.

use engine::database::synthesis::synthesize_archenemy;
use engine::game::combat::AttackTarget;
use engine::game::deck_loading::create_scheme_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const NATURE_SHIELDS_ITS_OWN: &str = "(An ongoing scheme remains face up until it's abandoned.)\nWhenever a creature attacks and isn't blocked, if you're the defending player, create a 0/1 green Plant creature token that's blocking that creature.\nWhen four or more creatures attack you, abandon this scheme at end of combat.";

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
    face.card_type.supertypes.push(Supertype::Ongoing);
    synthesize_archenemy(&mut face);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(gaps.is_empty(), "{name}: gaps: {gaps:?}");
    face
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn plants_on_battlefield(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield && obj.controller == P0 && obj.name.contains("Plant")
        })
        .count()
}

/// Resolve every pending trigger with no choices (Plant creation and the
/// delayed abandon schedule are prompt-free).
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

/// Declare `attacks`, pass to the declare-blockers step, and decline all
/// blocks — every attacker is unblocked.
fn attack_unblocked(runner: &mut GameRunner, attacks: &[(ObjectId, AttackTarget)]) {
    runner
        .declare_attackers(attacks)
        .expect("declare attackers");
    // CR 509.1a: blockers must be untapped. A defender with no untapped
    // creature has no block to declare, so the engine skips the prompt and
    // the step proceeds with every attacker unblocked — leave the loop once
    // the game has moved past declare attackers.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            runner.declare_blockers(&[]).expect("declare no blockers");
            return;
        }
        if runner.state().phase != Phase::DeclareAttackers {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
    panic!(
        "combat never left declare attackers, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn nature_shields_blocks_for_defender_and_abandons_on_four() {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let lion = scenario.add_creature(P0, "Silvercoat Lion", 2, 2).id();
    let mut raiders = Vec::new();
    for i in 0..4 {
        raiders.push(
            scenario
                .add_creature(P1, &format!("Grizzly Bears {i}"), 2, 2)
                .id(),
        );
    }
    let mut runner = scenario.build();

    let face = scheme_face("Nature Shields Its Own", NATURE_SHIELDS_ITS_OWN);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;

    let p0_life = life_of(&runner, P0);
    let p1_life = life_of(&runner, P1);

    // Turn 1: the scheme is set in motion; no attacks.
    runner.advance_to_phase(Phase::PreCombatMain);
    let scheme = *runner
        .state()
        .command_zone
        .iter()
        .find(|id| runner.state().objects[*id].name == "Nature Shields Its Own")
        .expect("scheme must be face up in the command zone");
    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");

    // Drive to P1's first combat.
    for _ in 0..8 {
        runner.advance_to_phase(Phase::DeclareAttackers);
        if runner.state().active_player == P1
            && matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            )
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt driving to P1T1 combat: {other:?}"),
        }
    }

    // P1T1 POSITIVE (trigger 1) + NEGATIVE (trigger 2 threshold): three
    // attackers, all unblocked — three Plants, but no abandon (3 < 4).
    let three: Vec<(ObjectId, AttackTarget)> = raiders[0..3]
        .iter()
        .map(|id| (*id, AttackTarget::Player(P0)))
        .collect();
    attack_unblocked(&mut runner, &three);
    drain_triggers(&mut runner);
    assert_eq!(
        plants_on_battlefield(&runner),
        3,
        "each unblocked attacker must grow a blocking Plant"
    );
    runner.combat_damage();
    assert_eq!(
        life_of(&runner, P0),
        p0_life,
        "the Plants must block all three attackers"
    );
    assert!(
        !runner.state().objects[&scheme].face_down,
        "three attackers must not abandon the scheme"
    );

    // Drive to P0's second combat.
    for _ in 0..8 {
        runner.advance_to_phase(Phase::DeclareAttackers);
        if runner.state().active_player == P0
            && matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            )
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt driving to P0T2 combat: {other:?}"),
        }
    }

    // P0T2 NEGATIVE (trigger 1 guard): the Lion attacks P1 unblocked — P0
    // is not the defending player, so no Plant appears.
    attack_unblocked(&mut runner, &[(lion, AttackTarget::Player(P1))]);
    drain_triggers(&mut runner);
    assert_eq!(
        plants_on_battlefield(&runner),
        0,
        "attacking P1 must grow no Plant for the archenemy"
    );
    runner.combat_damage();
    assert_eq!(
        life_of(&runner, P1),
        p1_life - 2,
        "reach guard: the Lion attacked unblocked, so the head matched"
    );

    // Drive to P1's second combat.
    for _ in 0..8 {
        runner.advance_to_phase(Phase::DeclareAttackers);
        if runner.state().active_player == P1
            && matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            )
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt driving to P1T2 combat: {other:?}"),
        }
    }

    // P1T2 POSITIVE (trigger 2): four attackers — four Plants plus the
    // delayed abandon at end of combat.
    let four: Vec<(ObjectId, AttackTarget)> = raiders
        .iter()
        .map(|id| (*id, AttackTarget::Player(P0)))
        .collect();
    attack_unblocked(&mut runner, &four);
    drain_triggers(&mut runner);
    assert_eq!(
        plants_on_battlefield(&runner),
        4,
        "each unblocked attacker must grow a blocking Plant"
    );
    runner.combat_damage();
    assert_eq!(
        life_of(&runner, P0),
        p0_life,
        "the Plants must block all four attackers"
    );
    // CR 603.7a: the delayed abandon fires at end of combat.
    for _ in 0..32 {
        if runner.state().objects[&scheme].face_down {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt awaiting the abandon: {other:?}"),
        }
    }
    assert!(
        runner.state().objects[&scheme].face_down,
        "four attackers must abandon the scheme at end of combat"
    );
    assert_eq!(
        runner.state().scheme_deck.back(),
        Some(&scheme),
        "abandoned scheme must be on the bottom of the scheme deck"
    );
}
