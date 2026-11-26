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
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
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

// F4 rows (CR 400.7): the blocking rider's "that creature" is an event
// anaphor resolved against incarnation-pinned referents, so a returned or
// blinked attacker is not blocked as its earlier incarnation. The ninja leg
// discriminates behaviorally: the Ninja re-attacks under its original
// storage ID, so a resolver that ignored the captured pin would block it.
// The cloud leg passes pre-fix too (CR 506.4: a zone-changed attacker
// leaves `combat.attackers`, so `place_blocking` no-matches either way):
// its discrimination rests on the folded F4-pins assertions (`[]`
// pre-fix, `[(attacker, incarnation)]` post-fix) plus the F4-unit slot
// matrix in `phase-ast`.

const MISTBLADE_SHINOBI: &str = "Ninjutsu {U} ({U}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nWhenever this creature deals combat damage to a player, you may return target creature that player controls to its owner's hand.";
const NINJA_OF_THE_DEEP_HOURS: &str = "Ninjutsu {1}{U} ({1}{U}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nWhenever this creature deals combat damage to a player, you may draw a card.";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
const MURDER: &str = "Destroy target creature.";

fn archenemy_scenario(raider_count: usize) -> (GameScenario, Vec<ObjectId>) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P0);
    let mut scenario = GameScenario::new_with_format(format, 2, 42);
    scenario.at_phase(Phase::Draw);
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("P0 Library {i}"));
        scenario.add_card_to_library_top(P1, &format!("P1 Library {i}"));
    }
    let mut raiders = Vec::new();
    for i in 0..raider_count {
        raiders.push(
            scenario
                .add_creature(P1, &format!("Grizzly Bears {i}"), 2, 2)
                .id(),
        );
    }
    (scenario, raiders)
}

fn build_with_scheme(scenario: GameScenario) -> GameRunner {
    let mut runner = scenario.build();
    let face = scheme_face("Nature Shields Its Own", NATURE_SHIELDS_ITS_OWN);
    create_scheme_deck_card(runner.state_mut(), &face, P0);
    runner.state_mut().archenemy = Some(P0);
    runner.state_mut().active_player = P0;
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        runner
            .state()
            .command_zone
            .iter()
            .any(|id| runner.state().objects[id].name == "Nature Shields Its Own"),
        "scheme must be face up in the command zone"
    );
    runner.advance_to_combat();
    runner.declare_attackers(&[]).expect("declare no attackers");
    runner
}

fn drive_to_p1_combat(runner: &mut GameRunner) {
    for _ in 0..8 {
        runner.advance_to_phase(Phase::DeclareAttackers);
        if runner.state().active_player == P1
            && matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareAttackers { .. }
            )
        {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt driving to P1 combat: {other:?}"),
        }
    }
    panic!("never reached P1 combat");
}

/// Every (object, incarnation) pin on every stacked ability, sorted.
fn stack_pins(runner: &GameRunner) -> Vec<(ObjectId, u64)> {
    let mut pins = Vec::new();
    for entry in runner.state().stack.iter() {
        if let Some(ability) = entry.ability() {
            for pin in &ability.target_incarnations {
                pins.push((pin.object_id, pin.incarnation));
            }
        }
    }
    pins.sort_by_key(|(id, incarnation)| (id.0, *incarnation));
    pins
}

fn sorted_pins(mut pins: Vec<(ObjectId, u64)>) -> Vec<(ObjectId, u64)> {
    pins.sort_by_key(|(id, incarnation)| (id.0, *incarnation));
    pins
}

/// Resolve to an empty stack at priority, explicitly declining every
/// optional ("you may") offer. Returns the decline count.
fn settle_declining_optionals(runner: &mut GameRunner) -> usize {
    let mut declined = 0;
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
            WaitingFor::OptionalEffectChoice { .. } => {
                declined += 1;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the optional");
            }
            other => panic!("unexpected prompt while settling: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "all triggers must have resolved"
    );
    declined
}

/// Collect the pending unblocked-attacker triggers onto the stack: after
/// `attack_unblocked` the game sits at priority with the triggers pending,
/// and passing surfaces the ordering prompt.
fn stack_unblocked_triggers(runner: &mut GameRunner, expected: usize) {
    for _ in 0..8 {
        if runner.state().stack.len() == expected {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt collecting triggers: {other:?}"),
        }
    }
    panic!(
        "expected {expected} triggers on the stack, got {}",
        runner.state().stack.len()
    );
}

/// F4-ninja: the round-4 witness — Ninja of the Deep Hours attacks, Mistblade
/// Shinobi's ninjutsu returns it, and its own ninjutsu returns Shinobi,
/// putting the Ninja back attacking under the same storage ID with a new
/// incarnation (CR 400.7). Nature's trigger still grows its Plant, but the
/// Plant must not block the Ninja's new incarnation; the hostile second
/// attacker B is still blocked.
#[test]
fn ninjutsu_returned_attacker_not_blocked_as_earlier_incarnation() {
    let (mut scenario, raiders) = archenemy_scenario(1);
    let ninja = scenario
        .add_creature_from_oracle(P1, "Ninja of the Deep Hours", 2, 2, NINJA_OF_THE_DEEP_HOURS)
        .with_subtypes(vec!["Human", "Ninja"])
        .id();
    let mistblade = scenario
        .add_creature_to_hand_from_oracle(P1, "Mistblade Shinobi", 1, 1, MISTBLADE_SHINOBI)
        .with_subtypes(vec!["Human", "Ninja"])
        .id();
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let b = raiders[0];
    let p0_life = life_of(&runner, P0);

    attack_unblocked(
        &mut runner,
        &[
            (ninja, AttackTarget::Player(P0)),
            (b, AttackTarget::Player(P0)),
        ],
    );
    stack_unblocked_triggers(&mut runner, 2);

    // F4-pins: each firing pins its own attacker at the pre-return incarnation.
    let incarn_ninja = runner.state().objects[&ninja].incarnation;
    let incarn_b = runner.state().objects[&b].incarnation;
    assert_eq!(
        stack_pins(&runner),
        sorted_pins(vec![(ninja, incarn_ninja), (b, incarn_b)]),
        "each stacked trigger pins its attacker"
    );

    // Mana for Mistblade {U} + Ninja {1}{U}.
    for (mana, source) in [
        (ManaType::Blue, mistblade),
        (ManaType::Blue, ninja),
        (ManaType::Colorless, ninja),
    ] {
        runner
            .state_mut()
            .add_mana_to_pool(P1, ManaUnit::new(mana, source, false, vec![]));
    }

    // The witness sequence through the public action: Mistblade returns the
    // Ninja, then the Ninja's own ninjutsu returns Mistblade.
    runner
        .act(GameAction::ActivateNinjutsu {
            ninjutsu_object_id: mistblade,
            creature_to_return: ninja,
        })
        .expect("Mistblade ninjutsu returns the Ninja");
    assert_eq!(runner.state().objects[&ninja].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&mistblade].zone, Zone::Battlefield);
    runner
        .act(GameAction::ActivateNinjutsu {
            ninjutsu_object_id: ninja,
            creature_to_return: mistblade,
        })
        .expect("Ninja ninjutsu returns Mistblade");
    assert_eq!(runner.state().objects[&mistblade].zone, Zone::Hand);
    assert_eq!(runner.state().objects[&ninja].zone, Zone::Battlefield);

    // The witness's core: the original storage ID attacks again as a new
    // object (CR 400.7), with no new attack triggers (a ninjutsu entry is
    // not an attack), and the pins survive the returns unstamped.
    assert_eq!(
        runner.state().objects[&ninja].incarnation,
        incarn_ninja + 2,
        "the hand round-trip is a new object"
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .expect("combat is live")
            .attackers
            .iter()
            .any(|a| a.object_id == ninja),
        "the Ninja is attacking again under its original storage ID"
    );
    assert_eq!(
        runner.state().stack.len(),
        2,
        "reach guard: no new triggers from the ninjutsu entries"
    );
    assert_eq!(
        stack_pins(&runner),
        sorted_pins(vec![(ninja, incarn_ninja), (b, incarn_b)]),
        "pins survive the returns unstamped"
    );

    // Both triggers resolve: the Ninja's Plant cannot block the Ninja's new
    // incarnation, while B's Plant blocks B.
    drain_triggers(&mut runner);
    assert_eq!(
        plants_on_battlefield(&runner),
        2,
        "each trigger still grows its Plant"
    );
    let combat = runner.state().combat.as_ref().expect("combat is live");
    assert!(
        combat
            .blocker_assignments
            .get(&ninja)
            .is_none_or(Vec::is_empty),
        "no Plant blocks the returned Ninja's new incarnation"
    );
    let b_blockers = combat
        .blocker_assignments
        .get(&b)
        .cloned()
        .unwrap_or_default();
    assert_eq!(b_blockers.len(), 1, "the unreturned B is still blocked");
    assert!(
        runner.state().objects[&b_blockers[0]]
            .name
            .contains("Plant"),
        "B's blocker is the Plant"
    );

    // Only the Ninja's 2 damage gets through.
    runner.combat_damage();
    assert_eq!(
        life_of(&runner, P0),
        p0_life - 2,
        "the Ninja deals 2 through the unblocked line"
    );

    // The Ninja's may-draw prompt is explicitly declined to a clean Priority.
    let declined = settle_declining_optionals(&mut runner);
    assert_eq!(
        declined, 1,
        "exactly the Ninja may-draw is offered and declined"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}

/// F4-cloud (additional row): a Cloudshift blink in response to the trigger.
/// The blinked attacker is not blocked as its earlier incarnation; the
/// intervening-if still holds, so the Plant is still created.
#[test]
fn blinked_attacker_not_blocked_as_earlier_incarnation() {
    let (mut scenario, raiders) = archenemy_scenario(1);
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P1, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let a = raiders[0];
    let p0_life = life_of(&runner, P0);

    attack_unblocked(&mut runner, &[(a, AttackTarget::Player(P0))]);
    stack_unblocked_triggers(&mut runner, 1);

    let incarn = runner.state().objects[&a].incarnation;
    assert_eq!(
        stack_pins(&runner),
        vec![(a, incarn)],
        "the stacked trigger pins its attacker"
    );

    // Cloudshift in response: exile + return.
    runner.cast(cloudshift).target_object(a).commit();
    assert_eq!(runner.state().stack.len(), 2);
    runner.act(GameAction::PassPriority).expect("pass");
    runner.act(GameAction::PassPriority).expect("pass");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Cloudshift resolved; the trigger remains"
    );
    assert_eq!(runner.state().objects[&a].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&a].incarnation,
        incarn + 2,
        "exile + return is a new object (CR 400.7)"
    );
    assert_eq!(
        stack_pins(&runner),
        vec![(a, incarn)],
        "the pin is not restamped"
    );

    drain_triggers(&mut runner);
    assert_eq!(
        plants_on_battlefield(&runner),
        1,
        "the intervening-if still holds, so the Plant is created"
    );
    let combat = runner.state().combat.as_ref().expect("combat is live");
    assert!(
        combat.blocker_assignments.get(&a).is_none_or(Vec::is_empty),
        "no Plant blocks the blinked A"
    );

    // Vacuous by construction: nothing attacks, so no damage is possible.
    runner.combat_damage();
    assert_eq!(life_of(&runner, P0), p0_life);
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}

/// F4-ctrl (paired positive): the unblinked attacker is still blocked, and
/// the destroyed-in-response hostile grows its Plant without blocking.
#[test]
fn unblinked_attacker_still_blocked() {
    let (mut scenario, raiders) = archenemy_scenario(2);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let (doomed, steady) = (raiders[0], raiders[1]);
    let p0_life = life_of(&runner, P0);

    attack_unblocked(
        &mut runner,
        &[
            (doomed, AttackTarget::Player(P0)),
            (steady, AttackTarget::Player(P0)),
        ],
    );
    stack_unblocked_triggers(&mut runner, 2);

    // Hostile: the doomed attacker is destroyed with its trigger on the stack.
    runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.cast(murder).target_object(doomed).commit();
    runner.act(GameAction::PassPriority).expect("pass");
    runner.act(GameAction::PassPriority).expect("pass");
    assert_eq!(
        runner.state().objects[&doomed].zone,
        Zone::Graveyard,
        "reach guard: Murder resolved"
    );
    assert_eq!(
        runner.state().stack.len(),
        2,
        "Murder resolved; both triggers remain"
    );

    drain_triggers(&mut runner);
    assert_eq!(
        plants_on_battlefield(&runner),
        2,
        "both triggers still grow their Plants"
    );
    let combat = runner.state().combat.as_ref().expect("combat is live");
    assert!(
        combat
            .blocker_assignments
            .get(&doomed)
            .is_none_or(Vec::is_empty),
        "no Plant blocks the destroyed attacker"
    );
    let steady_blockers = combat
        .blocker_assignments
        .get(&steady)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        steady_blockers.len(),
        1,
        "the unblinked attacker is still blocked"
    );
    assert!(
        runner.state().objects[&steady_blockers[0]]
            .name
            .contains("Plant"),
        "the blocker's Plant"
    );

    runner.combat_damage();
    assert_eq!(
        life_of(&runner, P0),
        p0_life,
        "the Plants block all incoming damage"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}

// F4-sub rows (CR 614.6): the creating instruction's blocking rider (CR
// 509.4) survives token replacement — full substitution (Divine Visitation
// ruling: "Anything else specified in the effect creating the token ...
// still applies") and additional batches (Chatterfang ruling: "... applies
// to both the original tokens and the Squirrels"). The carried id is the
// propose-time binding against the pinned incarnation (CR 400.7 + CR
// 608.2k); the applier performs no re-resolution (T2/T5 pins).

const DIVINE_VISITATION: &str = "If one or more creature tokens would be created under your control, that many 4/4 white Angel creature tokens with flying and vigilance are created instead.";
const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";

/// Battlefield tokens controlled by P0 with the subtype `subtype`.
fn tokens_of_subtype(runner: &GameRunner, subtype: &str) -> Vec<ObjectId> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| {
            o.is_token && o.controller == P0 && o.card_types.subtypes.iter().any(|s| s == subtype)
        })
        .map(|o| o.id)
        .collect()
}

/// Drive combat damage to end of combat at Priority, answering the
/// attacker's damage-assignment prompt when several blockers share one
/// attacker (CR 510.1c: all damage on the first blocker in order — lethal
/// to a 0/1 Plant or 1/1 Squirrel, so the assignment is legal).
fn settle_combat_damage(runner: &mut GameRunner) {
    runner.combat_damage();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().phase == Phase::EndCombat
                    || runner.state().phase == Phase::PostCombatMain
                {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::AssignCombatDamage {
                total_damage,
                blockers,
                ..
            } => {
                runner
                    .act(GameAction::AssignCombatDamage {
                        mode: Default::default(),
                        assignments: vec![(blockers[0].blocker_id, total_damage)],
                        trample_damage: 0,
                        controller_damage: 0,
                    })
                    .expect("assign all damage to the first blocker");
            }
            other => panic!("unexpected prompt settling combat damage: {other:?}"),
        }
    }
    panic!("combat damage never settled");
}

/// T1 (Seam A): Visitation's substitute Angel enters blocking the unblocked
/// attacker. Revert-fail: no blockers on the Bears.
#[test]
fn visitation_substitute_angel_blocks_the_attacker() {
    let (mut scenario, raiders) = archenemy_scenario(1);
    scenario.add_enchantment_from_oracle(P0, "Divine Visitation", DIVINE_VISITATION);
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let bears = raiders[0];
    let p0_life = life_of(&runner, P0);

    attack_unblocked(&mut runner, &[(bears, AttackTarget::Player(P0))]);
    drain_triggers(&mut runner);

    // Reach guards: substitution demonstrably ran — exactly one Angel (4/4
    // white flyer with vigilance) and no original Plant remains.
    let angels = tokens_of_subtype(&runner, "Angel");
    assert_eq!(angels.len(), 1, "exactly one substitute Angel");
    let angel = &runner.state().objects[&angels[0]];
    assert_eq!(angel.power, Some(4), "substituted token is 4/4");
    assert_eq!(angel.toughness, Some(4), "substituted token is 4/4");
    assert!(
        angel.color.contains(&ManaColor::White),
        "substituted token is white, got {:?}",
        angel.color
    );
    assert!(
        angel.has_keyword(&Keyword::Flying) && angel.has_keyword(&Keyword::Vigilance),
        "substituted Angel has flying and vigilance"
    );
    assert_eq!(
        plants_on_battlefield(&runner),
        0,
        "no original Plant token remains"
    );
    let combat = runner.state().combat.as_ref().expect("combat is live");
    assert_eq!(
        combat
            .blocker_assignments
            .get(&bears)
            .cloned()
            .unwrap_or_default(),
        vec![angels[0]],
        "the substitute Angel blocks the Bears"
    );

    runner.combat_damage();
    assert_eq!(
        life_of(&runner, P0),
        p0_life,
        "the Angel blocks all incoming damage"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}

/// T2 (Seam A hostile pin): a blinked attacker is NOT blocked as its earlier
/// incarnation (CR 400.7 + CR 608.2k), but the Angel is still created (R#50).
/// Passes pre- and post-fix: the propose-time `None` is carried verbatim.
#[test]
fn visitation_substitute_angel_does_not_block_a_blinked_attacker() {
    let (mut scenario, raiders) = archenemy_scenario(1);
    scenario.add_enchantment_from_oracle(P0, "Divine Visitation", DIVINE_VISITATION);
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P1, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let bears = raiders[0];
    let p0_life = life_of(&runner, P0);

    attack_unblocked(&mut runner, &[(bears, AttackTarget::Player(P0))]);
    stack_unblocked_triggers(&mut runner, 1);

    let incarn = runner.state().objects[&bears].incarnation;
    assert_eq!(
        stack_pins(&runner),
        vec![(bears, incarn)],
        "the stacked trigger pins its attacker"
    );

    // Cloudshift in response: exile + return.
    runner.cast(cloudshift).target_object(bears).commit();
    assert_eq!(runner.state().stack.len(), 2);
    runner.act(GameAction::PassPriority).expect("pass");
    runner.act(GameAction::PassPriority).expect("pass");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Cloudshift resolved; the trigger remains"
    );
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&bears].incarnation,
        incarn + 2,
        "exile + return is a new object (CR 400.7)"
    );
    assert_eq!(
        stack_pins(&runner),
        vec![(bears, incarn)],
        "the pin is not restamped"
    );

    drain_triggers(&mut runner);
    let angels = tokens_of_subtype(&runner, "Angel");
    assert_eq!(
        angels.len(),
        1,
        "substitution still applied after the blink"
    );
    let combat = runner.state().combat.as_ref().expect("combat is live");
    assert!(
        combat
            .blocker_assignments
            .get(&bears)
            .is_none_or(Vec::is_empty),
        "no Angel blocks the blinked Bears"
    );

    // Vacuous by construction: nothing attacks, so no damage is possible.
    runner.combat_damage();
    assert_eq!(life_of(&runner, P0), p0_life);
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}

/// T3 (Seam B): Chatterfang's additional Squirrel enters blocking alongside
/// the original Plant. Revert-fail: only the Plant blocks.
#[test]
fn chatterfang_additional_squirrel_blocks_the_attacker() {
    let (mut scenario, raiders) = archenemy_scenario(1);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let bears = raiders[0];
    let p0_life = life_of(&runner, P0);

    attack_unblocked(&mut runner, &[(bears, AttackTarget::Player(P0))]);
    drain_triggers(&mut runner);

    // Reach guards: the modified event created both batches.
    let plants = tokens_of_subtype(&runner, "Plant");
    let squirrels = tokens_of_subtype(&runner, "Squirrel");
    assert_eq!(plants.len(), 1, "exactly one original Plant");
    assert_eq!(squirrels.len(), 1, "exactly one additional Squirrel");
    let squirrel = &runner.state().objects[&squirrels[0]];
    assert_eq!(squirrel.power, Some(1), "Squirrel is 1/1");
    assert_eq!(squirrel.toughness, Some(1), "Squirrel is 1/1");
    assert!(
        squirrel.color.contains(&ManaColor::Green),
        "Squirrel is green, got {:?}",
        squirrel.color
    );
    let combat = runner.state().combat.as_ref().expect("combat is live");
    let blockers = combat
        .blocker_assignments
        .get(&bears)
        .cloned()
        .unwrap_or_default();
    assert_eq!(blockers.len(), 2, "Plant and Squirrel both block");
    assert!(
        blockers.contains(&plants[0]) && blockers.contains(&squirrels[0]),
        "the blockers are the Plant and the Squirrel"
    );

    settle_combat_damage(&mut runner);
    assert_eq!(
        life_of(&runner, P0),
        p0_life,
        "the blockers stop all incoming damage"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}

/// T4 (doubler pin): Parallel Lives doubles the event to 2 blocking Plants
/// (B#9) — the count path scales the unchanged spec, so no code change is
/// needed. Passes pre- and post-fix.
#[test]
fn parallel_lives_doubled_plants_both_block() {
    let (mut scenario, raiders) = archenemy_scenario(1);
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let bears = raiders[0];
    let p0_life = life_of(&runner, P0);

    attack_unblocked(&mut runner, &[(bears, AttackTarget::Player(P0))]);
    drain_triggers(&mut runner);

    let plants = tokens_of_subtype(&runner, "Plant");
    assert_eq!(plants.len(), 2, "the doubler created two Plants");
    let combat = runner.state().combat.as_ref().expect("combat is live");
    let blockers = combat
        .blocker_assignments
        .get(&bears)
        .cloned()
        .unwrap_or_default();
    assert_eq!(blockers.len(), 2, "both Plants block");
    assert!(
        blockers.contains(&plants[0]) && blockers.contains(&plants[1]),
        "the blockers are both Plants"
    );

    settle_combat_damage(&mut runner);
    assert_eq!(
        life_of(&runner, P0),
        p0_life,
        "the Plants block all incoming damage"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}

/// T5 (Seam B hostile pin): a destroyed attacker is NOT blocked (CR 506.4:
/// removed from combat, no longer attacking), but the Plant and the
/// Squirrel are still created (R#50).
/// Passes pre- and post-fix: the propose-time `None` is carried verbatim
/// through both the own spec and the appended batch.
#[test]
fn chatterfang_tokens_do_not_block_a_destroyed_attacker() {
    let (mut scenario, raiders) = archenemy_scenario(1);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let doomed = raiders[0];
    let p0_life = life_of(&runner, P0);

    attack_unblocked(&mut runner, &[(doomed, AttackTarget::Player(P0))]);
    stack_unblocked_triggers(&mut runner, 1);

    // Hostile: the attacker is destroyed with its trigger on the stack.
    runner.act(GameAction::PassPriority).expect("P1 passes");
    runner.cast(murder).target_object(doomed).commit();
    runner.act(GameAction::PassPriority).expect("pass");
    runner.act(GameAction::PassPriority).expect("pass");
    assert_eq!(
        runner.state().objects[&doomed].zone,
        Zone::Graveyard,
        "reach guard: Murder resolved"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Murder resolved; the trigger remains"
    );

    drain_triggers(&mut runner);
    assert_eq!(
        tokens_of_subtype(&runner, "Plant").len(),
        1,
        "the Plant is still created"
    );
    assert_eq!(
        tokens_of_subtype(&runner, "Squirrel").len(),
        1,
        "the Squirrel is still created"
    );
    let combat = runner.state().combat.as_ref().expect("combat is live");
    assert!(
        combat
            .blocker_assignments
            .get(&doomed)
            .is_none_or(Vec::is_empty),
        "no token blocks the destroyed attacker"
    );

    runner.combat_damage();
    assert_eq!(life_of(&runner, P0), p0_life);
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}

/// T6 (Seams A + B, compound): Visitation + Chatterfang on one event. The
/// pipeline auto-resolves the order without prompting, deterministically
/// Chatterfang-first, so Visitation substitutes BOTH batches: 2 Angels, 0
/// Plants, 0 Squirrels. Both Angels block post-fix. Revert-fail: neither
/// Angel blocks (Seam A drops the rider on the own batch; Seams B then A
/// drop it on the appended batch).
///
/// NOTE (pre-existing, out of scope): the order IS material — Visitation
/// first would yield Angel + Squirrel, not 2 Angels — so CR 616.1 requires
/// an ordering prompt here. `candidate_materiality` classifies the
/// `additional_token_spec` appender `Disjoint`, so
/// `replacement_ordering_is_material` reports immaterial and `pipeline_loop`
/// auto-resolves. `drain_triggers` below pins the current no-prompt
/// behavior (it panics on any prompt); a materiality fix must re-drive this
/// test through the new prompt.
#[test]
fn visitation_and_chatterfang_compound_blocks_with_both_batches() {
    let (mut scenario, raiders) = archenemy_scenario(1);
    scenario.add_enchantment_from_oracle(P0, "Divine Visitation", DIVINE_VISITATION);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    let mut runner = build_with_scheme(scenario);
    drive_to_p1_combat(&mut runner);
    let bears = raiders[0];
    let p0_life = life_of(&runner, P0);

    attack_unblocked(&mut runner, &[(bears, AttackTarget::Player(P0))]);
    drain_triggers(&mut runner);

    // Reach guards: exact batch composition — 2 Angels, 0 Plants, 0 Squirrels.
    let angels = tokens_of_subtype(&runner, "Angel");
    assert_eq!(angels.len(), 2, "substitution applied to both batches");
    assert_eq!(
        plants_on_battlefield(&runner),
        0,
        "no original Plant token remains"
    );
    assert_eq!(
        tokens_of_subtype(&runner, "Squirrel").len(),
        0,
        "the appended Squirrel batch was substituted, not kept"
    );
    let combat = runner.state().combat.as_ref().expect("combat is live");
    let blockers = combat
        .blocker_assignments
        .get(&bears)
        .cloned()
        .unwrap_or_default();
    assert_eq!(blockers.len(), 2, "both Angels block");
    assert!(
        blockers.contains(&angels[0]) && blockers.contains(&angels[1]),
        "the blockers are both Angels"
    );

    settle_combat_damage(&mut runner);
    assert_eq!(
        life_of(&runner, P0),
        p0_life,
        "the Angels block all incoming damage"
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "settled at priority"
    );
}
