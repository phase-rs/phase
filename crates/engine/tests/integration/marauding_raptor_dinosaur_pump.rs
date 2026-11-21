//! Marauding Raptor — "Whenever another creature you control enters, this
//! creature deals 2 damage to it. If a Dinosaur is dealt damage this way, this
//! creature gets +2/+0 until end of turn."
//!
//! CR 603.2 + CR 608.2c: the "If a Dinosaur is dealt damage this way" clause is
//! a conditional rider on the same triggered ability — the sub-ability is
//! checked on resolution against the damage the first instruction dealt, and
//! the pump must land whenever a Dinosaur received that damage, even when the
//! Dinosaur survives it (Colossal Dreadmaw, 6/6).
//!
//! Field report (issue #9012): the Raptor dealt its 2 damage to entering
//! Dinosaurs but the +2/+0 never applied.

use engine::database::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Colossal Dreadmaw: a 6/6 Dinosaur, so the 2 damage does not kill it — the
/// damage-dealt rider is tested independently of lethality.
const ENTERING_DINOSAUR: &str = "Colossal Dreadmaw";
/// A Dragon, satisfying the trigger's "another creature you control enters"
/// while missing the "Dinosaur" rider filter — the non-matching control.
const ENTERING_NON_DINOSAUR: &str = "Shivan Dragon";

/// A pool of `colored` mana of one color plus enough colorless to cover a
/// {N}+{C}{C} casting cost.
fn cast_pool(color: ManaType, colored: usize, total: usize) -> Vec<ManaUnit> {
    (0..total)
        .map(|i| {
            if i < colored {
                ManaUnit::new(color, ObjectId(0), false, vec![])
            } else {
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])
            }
        })
        .collect()
}

#[test]
fn raptor_pumps_when_a_surviving_dinosaur_enters() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    scenario.add_real_card(P0, ENTERING_DINOSAUR, Zone::Hand, db);
    scenario.with_mana_pool(P0, cast_pool(ManaType::Green, 2, 7));
    let mut runner = scenario.build();

    let dino = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == ENTERING_DINOSAUR)
        .map(|(id, _)| *id)
        .expect("Dinosaur should be in hand");

    runner.cast(dino).resolve();
    runner.advance_until_stack_empty();

    // Reach guard: the trigger really dealt 2 damage to the Dinosaur
    // (CR 120.3: damage is marked, not subtracted from base power).
    assert_eq!(
        runner.state().objects[&dino].damage_marked,
        2,
        "Raptor's 2 damage must be marked on the entering Dinosaur"
    );
    assert_eq!(runner.state().objects[&dino].zone, Zone::Battlefield);
    // The rider: +2/+0 until end of turn. 2 base -> 4 effective power.
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(4),
        "a Dinosaur took the damage, so Marauding Raptor must be +2/+0"
    );
}

#[test]
fn raptor_does_not_pump_for_a_non_dinosaur() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    scenario.add_real_card(P0, ENTERING_NON_DINOSAUR, Zone::Hand, db);
    scenario.with_mana_pool(P0, cast_pool(ManaType::Red, 2, 7));
    let mut runner = scenario.build();

    let dragon = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == ENTERING_NON_DINOSAUR)
        .map(|(id, _)| *id)
        .expect("Dragon should be in hand");

    runner.cast(dragon).resolve();
    runner.advance_until_stack_empty();

    // Reach guard: the trigger really fired — the Dragon took the 2 damage too.
    assert_eq!(
        runner.state().objects[&dragon].damage_marked,
        2,
        "the Dragon must take the same 2 damage the Dinosaur case deals"
    );
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(2),
        "a non-Dinosaur took the damage, so no pump"
    );
}

/// Pass priority until the stack is empty, appending every event emitted on the
/// way so a test can prove the Raptor's damage instruction actually resolved.
fn drain_stack_collecting_events(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority must be legal while the trigger is on the stack");
        events.extend(result.events);
    }
    assert!(runner.state().stack.is_empty(), "the stack must drain");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at a priority window"
    );
}

/// Cast the entering Dinosaur with `shield` already on the battlefield, then
/// resolve the creature spell and the Raptor's trigger.
fn cast_dinosaur_under_shield(
    db: &CardDatabase,
    shield: &str,
) -> (GameRunner, ObjectId, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    scenario.add_real_card(P0, shield, Zone::Battlefield, db);
    scenario.add_real_card(P0, ENTERING_DINOSAUR, Zone::Hand, db);
    scenario.with_mana_pool(P0, cast_pool(ManaType::Green, 2, 7));
    let mut runner = scenario.build();

    let dino = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == ENTERING_DINOSAUR)
        .map(|(id, _)| *id)
        .expect("Dinosaur should be in hand");

    // The cast harness drives the creature spell and the Raptor's trigger;
    // drain anything it left on the stack so the event log is complete.
    let mut events = runner.cast(dino).resolve().events().to_vec();
    drain_stack_collecting_events(&mut runner, &mut events);
    (runner, raptor, dino, events)
}

/// CR 615.1a + CR 608.2c: prevented damage is damage that is not dealt, so a
/// Dinosaur whose 2 damage is fully prevented (Inner Sanctum: "Prevent all
/// damage that would be dealt to creatures you control.") was not "dealt damage
/// this way" and the Raptor must not get +2/+0. The rider's
/// `PreviousEffectAmount > 0` conjunct reads the post-prevention amount the
/// damage instruction actually dealt, not the entering creature's identity.
#[test]
fn raptor_does_not_pump_when_the_damage_to_the_dinosaur_is_prevented() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, events) = cast_dinosaur_under_shield(db, "Inner Sanctum");

    // Reach guard: the trigger resolved and its 2 damage to the Dinosaur was
    // prevented, not skipped.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: 2 }
                if *source_id == raptor && *target == dino
        )),
        "the Raptor's 2 damage to the Dinosaur must be prevented, got {events:?}"
    );
    assert_eq!(runner.state().objects[&dino].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&dino].damage_marked, 0);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(2),
        "no damage was dealt to the Dinosaur, so Marauding Raptor must not be pumped"
    );
}

/// CR 615.1a + CR 608.2c: a partial prevention still leaves damage dealt.
/// Temple Altisaur ("If a source would deal damage to another Dinosaur you
/// control, prevent all but 1 of that damage.") cuts the 2 damage to 1; the
/// Dinosaur was still dealt damage this way, so the pump applies.
#[test]
fn raptor_pumps_when_the_damage_to_the_dinosaur_is_partially_prevented() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, events) = cast_dinosaur_under_shield(db, "Temple Altisaur");

    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: 1 }
                if *source_id == raptor && *target == dino
        )),
        "Temple Altisaur must prevent all but 1 of the Raptor's damage, got {events:?}"
    );
    assert_eq!(runner.state().objects[&dino].damage_marked, 1);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(4),
        "the Dinosaur was still dealt 1 damage, so Marauding Raptor must be +2/+0"
    );
}

/// P0 resolves Harm's Way ("The next 2 damage that a source of your choice
/// would deal to you and/or permanents you control this turn is dealt to any
/// target instead.") aimed at `recipient`, choosing Marauding Raptor as the
/// source (CR 609.7a), then casts `entering`. The Raptor's 2 damage to the
/// entering creature is redirected to `recipient` (CR 614.9).
fn cast_under_harms_way(
    db: &CardDatabase,
    entering: &str,
    entering_color: ManaType,
    opposing_creature: Option<&str>,
) -> (
    GameRunner,
    ObjectId,
    ObjectId,
    Option<ObjectId>,
    Vec<GameEvent>,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    let harms_way = scenario.add_real_card(P0, "Harm's Way", Zone::Hand, db);
    let entering_id = scenario.add_real_card(P0, entering, Zone::Hand, db);
    let opposing =
        opposing_creature.map(|name| scenario.add_real_card(P1, name, Zone::Battlefield, db));
    let mut pool = vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])];
    pool.extend(cast_pool(entering_color, 2, 6));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    let cast = match opposing {
        Some(creature) => runner.cast(harms_way).target_objects(&[creature]).resolve(),
        None => runner.cast(harms_way).target_player(P1).resolve(),
    };
    let WaitingFor::DamageSourceChoice { options, .. } = cast.final_waiting_for() else {
        panic!(
            "Harm's Way must stop at its source choice, got {:?}",
            cast.final_waiting_for()
        );
    };
    assert!(
        options.contains(&raptor),
        "Marauding Raptor must be offered as the damage source: {options:?}"
    );
    runner
        .act(GameAction::ChooseDamageSource { source: raptor })
        .expect("P0 chooses Marauding Raptor as Harm's Way's source");
    assert_eq!(runner.state().objects[&harms_way].zone, Zone::Graveyard);

    let mut events = runner.cast(entering_id).resolve().events().to_vec();
    drain_stack_collecting_events(&mut runner, &mut events);
    (runner, raptor, entering_id, opposing, events)
}

/// CR 614.6 + CR 614.9: when Harm's Way redirects all of the Raptor's damage
/// to a player, the entering Dinosaur is dealt no damage, so no Dinosaur was
/// "dealt damage this way" and the Raptor is not pumped — even though the
/// instruction dealt 2 damage and its intended recipient was a Dinosaur.
#[test]
fn raptor_does_not_pump_when_the_damage_is_redirected_to_a_player() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let p1_life = 20;
    let (runner, raptor, dino, _, events) =
        cast_under_harms_way(db, ENTERING_DINOSAUR, ManaType::Green, None);

    // Reach guard: the trigger resolved and its 2 damage reached P1 instead.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 2, .. }
                if *source_id == raptor && *player == P1
        )),
        "the Raptor's 2 damage must be redirected to P1, got {events:?}"
    );
    assert_eq!(runner.life(P1), p1_life - 2);
    assert_eq!(runner.state().objects[&dino].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&dino].damage_marked,
        0,
        "the redirected damage never reached the Dinosaur"
    );
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(2),
        "no Dinosaur was dealt damage this way, so Marauding Raptor must not be pumped"
    );
}

/// CR 614.9: redirected onto an opponent's non-Dinosaur creature, the damage
/// was dealt to that creature, not to the entering Dinosaur — no pump.
#[test]
fn raptor_does_not_pump_when_the_damage_is_redirected_to_a_non_dinosaur() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, bears, _) = cast_under_harms_way(
        db,
        ENTERING_DINOSAUR,
        ManaType::Green,
        Some("Grizzly Bears"),
    );
    let bears = bears.expect("Grizzly Bears is on the battlefield");

    // Reach guard: the redirected 2 damage killed the 2/2 Bears.
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&dino].damage_marked, 0);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(2),
        "a non-Dinosaur was dealt the damage, so Marauding Raptor must not be pumped"
    );
}

/// CR 614.6 + CR 608.2c: the modified event is what the instruction did. A
/// non-Dinosaur enters and Harm's Way redirects the Raptor's damage onto an
/// opponent's Dinosaur — a Dinosaur WAS dealt damage this way, so the Raptor
/// is pumped although the triggering creature is not a Dinosaur.
#[test]
fn raptor_pumps_when_the_damage_is_redirected_onto_a_dinosaur() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dragon, opposing_dino, _) = cast_under_harms_way(
        db,
        ENTERING_NON_DINOSAUR,
        ManaType::Red,
        Some(ENTERING_DINOSAUR),
    );
    let opposing_dino = opposing_dino.expect("P1's Dinosaur is on the battlefield");

    assert_eq!(runner.state().objects[&dragon].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&dragon].damage_marked, 0);
    assert_eq!(
        runner.state().objects[&opposing_dino].damage_marked,
        2,
        "the Raptor's 2 damage must be redirected onto P1's Dinosaur"
    );
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(4),
        "a Dinosaur was dealt damage this way, so Marauding Raptor must be +2/+0"
    );
}

/// Cast the entering Dinosaur with Furnace of Rath ("If a source would deal
/// damage to a permanent or player, it deals double that damage to that
/// permanent or player instead.") and `shield` on the battlefield, then pass
/// priority until the Raptor's damage stops at the controller's replacement
/// ordering prompt (CR 616.1). Answers it with the candidate whose source is
/// `first`, then drains the stack.
fn cast_dinosaur_through_ordering_choice(
    db: &CardDatabase,
    shield: &str,
    first_is_furnace: bool,
) -> (GameRunner, ObjectId, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    let shield_id = scenario.add_real_card(P0, shield, Zone::Battlefield, db);
    let furnace = scenario.add_real_card(P0, "Furnace of Rath", Zone::Battlefield, db);
    let dino = scenario.add_real_card(P0, ENTERING_DINOSAUR, Zone::Hand, db);
    scenario.with_mana_pool(P0, cast_pool(ManaType::Green, 2, 7));
    let mut runner = scenario.build();

    let mut events = runner.cast(dino).commit().events().to_vec();
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "expected priority before the ordering prompt, got {:?}",
            runner.state().waiting_for
        );
        assert!(
            !runner.state().stack.is_empty(),
            "the stack emptied without the Raptor's damage asking for an order"
        );
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must be legal")
                .events,
        );
    }
    // CR 616.1: two replacements apply to the Raptor's damage to the Dinosaur,
    // so its controller chooses one to apply first.
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Raptor's damage must stop at a replacement ordering prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P0,
        "the Dinosaur's controller orders the replacements"
    );
    // Reach guard: the pause is before any damage was dealt.
    assert_eq!(runner.state().objects[&dino].damage_marked, 0);
    let pick = if first_is_furnace { furnace } else { shield_id };
    let index = candidates
        .iter()
        .position(|candidate| candidate.source_id == pick)
        .unwrap_or_else(|| panic!("{pick:?} must be offered, got {candidates:?}"));
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.source_id
                == if first_is_furnace { shield_id } else { furnace }),
        "both replacements must compete, got {candidates:?}"
    );
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("P0 orders the replacements")
            .events,
    );
    drain_stack_collecting_events(&mut runner, &mut events);
    (runner, raptor, dino, events)
}

/// CR 616.1 + CR 608.2c: a replacement ordering prompt pauses the Raptor's
/// damage before it is dealt; the "dealt damage this way" rider is answered by
/// the damage the resumed instruction actually dealt. Furnace of Rath first
/// doubles 2 to 4, then Temple Altisaur prevents all but 1 — the Dinosaur is
/// dealt 1 damage, so the Raptor gets +2/+0.
#[test]
fn raptor_pumps_after_a_replacement_ordering_choice_furnace_first() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, events) =
        cast_dinosaur_through_ordering_choice(db, "Temple Altisaur", true);

    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamageDealt { source_id, target: TargetRef::Object(target), amount: 1, .. }
                if *source_id == raptor && *target == dino
        )),
        "the resumed damage must deal 1 to the Dinosaur, got {events:?}"
    );
    assert_eq!(runner.state().objects[&dino].damage_marked, 1);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(4),
        "the Dinosaur was dealt damage this way, so Marauding Raptor must be +2/+0"
    );
}

/// The other order: Temple Altisaur first cuts 2 to 1, then Furnace of Rath
/// doubles it to 2. The Dinosaur is still dealt damage this way.
#[test]
fn raptor_pumps_after_a_replacement_ordering_choice_shield_first() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, _) =
        cast_dinosaur_through_ordering_choice(db, "Temple Altisaur", false);

    assert_eq!(runner.state().objects[&dino].damage_marked, 2);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(4),
        "the Dinosaur was dealt damage this way, so Marauding Raptor must be +2/+0"
    );
}

/// CR 616.1 + CR 615.1a: the same pause-and-resume path where the answered
/// order still leaves the Dinosaur undamaged — Inner Sanctum ("Prevent all
/// damage that would be dealt to creatures you control.") prevents the
/// doubled damage — so no Dinosaur was dealt damage this way and the resumed
/// rider must not pump.
#[test]
fn raptor_does_not_pump_after_a_replacement_ordering_choice_prevents_all() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, events) =
        cast_dinosaur_through_ordering_choice(db, "Inner Sanctum", true);

    // Reach guard: the resumed damage was prevented, not skipped.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), .. }
                if *source_id == raptor && *target == dino
        )),
        "the resumed damage to the Dinosaur must be prevented, got {events:?}"
    );
    assert_eq!(runner.state().objects[&dino].damage_marked, 0);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(2),
        "no Dinosaur was dealt damage this way, so Marauding Raptor must not be pumped"
    );
}

/// CR 608.2c + CR 616.1 + CR 702.15b: a damage instruction can deal its damage
/// and THEN pause for a replacement ordering choice on its lifelink life gain
/// (`Boon Reflection` + `Alhammarret's Archive`). Because the Dinosaur was
/// already dealt damage before the pause, the resumed rider must still see
/// that damage and grant +2/+0.
#[test]
fn raptor_pumps_when_lifelink_pauses_on_replacement_choice_after_damage_is_dealt() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    let lifelink = scenario.add_real_card(P0, "Lifelink", Zone::Hand, db);
    scenario.add_real_card(P0, "Boon Reflection", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Alhammarret's Archive", Zone::Battlefield, db);
    let dino = scenario.add_real_card(P0, ENTERING_DINOSAUR, Zone::Hand, db);
    let mut pool = vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])];
    pool.extend(cast_pool(ManaType::Green, 2, 7));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    // Enchant Marauding Raptor with Lifelink.
    runner.cast(lifelink).target_object(raptor).resolve();

    let mut events = runner.cast(dino).commit().events().to_vec();
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "expected priority before the lifelink ordering prompt, got {:?}",
            runner.state().waiting_for
        );
        assert!(
            !runner.state().stack.is_empty(),
            "the stack emptied without the lifelink gain asking for an order"
        );
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must be legal")
                .events,
        );
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P0
        ),
        "expected P0's lifelink replacement ordering prompt, got {:?}",
        runner.state().waiting_for
    );
    // Reach guard: the 2 damage to the Dinosaur was ALREADY dealt before the
    // lifelink life-gain replacement paused.
    assert_eq!(runner.state().objects[&dino].damage_marked, 2);
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamageDealt { source_id, target: TargetRef::Object(target), amount: 2, .. }
                if *source_id == raptor && *target == dino
        )),
        "DamageDealt must precede the lifelink replacement pause, got {events:?}"
    );

    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("P0 orders the life-gain replacements")
            .events,
    );
    drain_stack_collecting_events(&mut runner, &mut events);

    assert_eq!(runner.life(P0), 28, "2 lifelink * 2 * 2 = 8 life gained");
    assert_eq!(runner.state().objects[&dino].damage_marked, 2);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(4),
        "a Dinosaur was dealt damage before the lifelink pause, so Marauding Raptor must be +2/+0"
    );
}

/// CR 616.1 + CR 614.9 + CR 608.2c: competing replacements between Furnace of
/// Rath and Harm's Way pause the Raptor's damage before delivery; choosing Harm's
/// Way first redirects the 2 damage to `opposing` (or P1), and Furnace doubles
/// that redirected damage. Returns `(runner, raptor, entering_id, opposing, events)`.
fn cast_under_furnace_and_harms_way(
    db: &CardDatabase,
    entering: &str,
    entering_color: ManaType,
    opposing_creature: Option<&str>,
) -> (
    GameRunner,
    ObjectId,
    ObjectId,
    Option<ObjectId>,
    Vec<GameEvent>,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    let furnace = scenario.add_real_card(P0, "Furnace of Rath", Zone::Battlefield, db);
    let harms_way = scenario.add_real_card(P0, "Harm's Way", Zone::Hand, db);
    let entering_id = scenario.add_real_card(P0, entering, Zone::Hand, db);
    let opposing =
        opposing_creature.map(|name| scenario.add_real_card(P1, name, Zone::Battlefield, db));
    let mut pool = vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])];
    pool.extend(cast_pool(entering_color, 2, 6));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    let cast = match opposing {
        Some(creature) => runner.cast(harms_way).target_objects(&[creature]).resolve(),
        None => runner.cast(harms_way).target_player(P1).resolve(),
    };
    let WaitingFor::DamageSourceChoice { .. } = cast.final_waiting_for() else {
        panic!("Harm's Way must stop at its source choice");
    };
    runner
        .act(GameAction::ChooseDamageSource { source: raptor })
        .expect("P0 chooses Marauding Raptor as Harm's Way's source");

    let mut events = runner.cast(entering_id).commit().events().to_vec();
    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority must be legal")
                .events,
        );
    }
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Furnace of Rath + Harm's Way must stop at a replacement ordering prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    // Choose Harm's Way (the one-shot shield) first so all 2 damage redirects
    // before Furnace doubles.
    let harms_idx = candidates
        .iter()
        .position(|c| c.source_id != furnace)
        .unwrap_or_else(|| panic!("Harm's Way must be a candidate, got {candidates:?}"));
    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: harms_idx })
            .expect("P0 orders Harm's Way first")
            .events,
    );
    drain_stack_collecting_events(&mut runner, &mut events);
    (runner, raptor, entering_id, opposing, events)
}

#[test]
fn raptor_replacement_ordering_with_harms_way_redirected_to_player_does_not_pump() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, _, events) =
        cast_under_furnace_and_harms_way(db, ENTERING_DINOSAUR, ManaType::Green, None);

    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::DamageDealt { source_id, target: TargetRef::Player(player), amount: 4, .. }
            if *source_id == raptor && *player == P1
    )));
    assert_eq!(runner.life(P1), 16);
    assert_eq!(runner.state().objects[&dino].damage_marked, 0);
    assert_eq!(runner.state().objects[&raptor].power, Some(2));
}

#[test]
fn raptor_replacement_ordering_with_harms_way_redirected_onto_dinosaur_pumps() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dragon, opposing_dino, _) = cast_under_furnace_and_harms_way(
        db,
        ENTERING_NON_DINOSAUR,
        ManaType::Red,
        Some(ENTERING_DINOSAUR),
    );
    let opposing_dino = opposing_dino.expect("P1's Dinosaur is on the battlefield");

    assert_eq!(runner.state().objects[&dragon].damage_marked, 0);
    assert_eq!(runner.state().objects[&opposing_dino].damage_marked, 4);
    assert_eq!(runner.state().objects[&raptor].power, Some(4));
}

/// CR 701.14a + CR 120.10 + CR 616.1: When `Effect::Fight` (The Last Agni Kai)
/// pauses for a replacement ordering choice on the fighting subject's damage
/// (`Furnace of Rath` + `Harm's Way`), `AwaitingDelivery` tracks
/// `fight_fought_creature` so excess dealt to the *fighting subject* (Grizzly
/// Bears taking 8 damage = 6 excess) never leaks into the fought-creature excess
/// channel (Colossal Dreadmaw 6/6 took 4 damage = 0 excess).
#[test]
fn fight_replacement_ordering_ignores_subject_excess_and_reads_fought_creature() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let furnace = scenario.add_real_card(P0, "Furnace of Rath", Zone::Battlefield, db);
    let harms_way = scenario.add_real_card(P0, "Harm's Way", Zone::Hand, db);
    let agni_kai = scenario.add_real_card(P0, "The Last Agni Kai", Zone::Hand, db);
    let big_dino = scenario.add_real_card(P1, ENTERING_DINOSAUR, Zone::Battlefield, db);
    let mut pool = vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])];
    pool.extend(cast_pool(ManaType::Red, 1, 2));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    let cast = runner.cast(harms_way).target_player(P1).resolve();
    let WaitingFor::DamageSourceChoice { .. } = cast.final_waiting_for() else {
        panic!("Harm's Way must stop at its source choice");
    };
    runner
        .act(GameAction::ChooseDamageSource { source: big_dino })
        .expect("P0 chooses opposing Dinosaur as Harm's Way's source");

    let outcome = runner
        .cast(agni_kai)
        .target_objects(&[bears, big_dino])
        .resolve();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!(
            "Fight under Furnace + Harm's Way must stop at ReplacementChoice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(player, P0);
    let harms_idx = candidates
        .iter()
        .position(|c| c.source_id != furnace)
        .unwrap_or_else(|| panic!("Harm's Way must compete, got {candidates:?}"));
    let mut events = runner
        .act(GameAction::ChooseReplacement { index: harms_idx })
        .expect("P0 orders Harm's Way first")
        .events;
    drain_stack_collecting_events(&mut runner, &mut events);

    // Reach guard: Bears took 8 damage (6 excess on the fighting subject).
    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::DamageDealt { target: TargetRef::Object(target), amount: 8, excess: 6, .. }
            if *target == bears
    )));
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        0,
        "fought creature (6/6) took 4 damage (0 excess); fighting subject's 6 excess must not add mana"
    );
}

/// Positive counterpart: when the *fought* creature (2/2) takes excess damage
/// after a replacement ordering choice on the fighting subject (`Furnace` +
/// `Inner Sanctum`), The Last Agni Kai adds exactly that excess red mana.
#[test]
fn fight_replacement_ordering_adds_mana_for_fought_creature_excess() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let big_dino = scenario.add_real_card(P0, ENTERING_DINOSAUR, Zone::Battlefield, db);
    let furnace = scenario.add_real_card(P0, "Furnace of Rath", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Inner Sanctum", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let agni_kai = scenario.add_real_card(P0, "The Last Agni Kai", Zone::Hand, db);
    scenario.with_mana_pool(P0, cast_pool(ManaType::Red, 1, 2));
    let mut runner = scenario.build();

    let outcome = runner
        .cast(agni_kai)
        .target_objects(&[big_dino, bears])
        .resolve();
    let WaitingFor::ReplacementChoice {
        player, candidates, ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!(
            "Fight under Furnace + Inner Sanctum must stop at ReplacementChoice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(player, P0);
    let furnace_idx = candidates
        .iter()
        .position(|c| c.source_id == furnace)
        .unwrap_or_else(|| panic!("Furnace must compete, got {candidates:?}"));
    let mut events = runner
        .act(GameAction::ChooseReplacement { index: furnace_idx })
        .expect("P0 orders Furnace first")
        .events;
    drain_stack_collecting_events(&mut runner, &mut events);

    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamageDealt { target: TargetRef::Object(target), amount: 12, excess: 10, .. }
                if *target == bears
        )),
        "expected Bears to take 12 (10 excess), events: {events:?}"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Red),
        10,
        "fought creature (2/2) took 12 damage = 10 excess, so The Last Agni Kai must add 10 red mana"
    );
}
