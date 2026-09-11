//! A later instruction that names an earlier declared target by slot
//! (`ParentTargetSlot`) does nothing to that target when it is illegal as the
//! spell or ability resolves. CR 608.2b: "Illegal targets, if any, won't be
//! affected by parts of a resolving spell's effect for which they're illegal."
//!
//! One card per effect that binds a slot this way: Tail Swipe's pump (Pump),
//! Goblin Welder's return (ChangeZone), and Stolen Uniform's gain of control
//! and attach (GainControl, Attach). Each target is made illegal by an
//! opponent's instant cast in response.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{Effect, EffectKind};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::rules::{cast_spell_action, drive_with_response, PriorityResponse};

const TAIL_SWIPE: &str =
    "Choose target creature you control and target creature you don't control. \
If you cast this spell during your main phase, the creature you control gets +1/+1 until end of \
turn. Then those creatures fight each other. (Each deals damage equal to its power to the other.)";

const GOBLIN_WELDER: &str =
    "{T}: Choose target artifact a player controls and target artifact card \
in that player's graveyard. If both targets are still legal as this ability resolves, that player \
simultaneously sacrifices the artifact and returns the artifact card to the battlefield.";

const STOLEN_UNIFORM: &str = "Choose target creature you control and target Equipment. Gain \
control of that Equipment until end of turn. Attach it to the chosen creature. When you lose \
control of that Equipment this turn, if it's attached to a creature you control, unattach it.";

const STEAL: &str = "Gain control of target creature until end of turn.";

const EXILE_FROM_GRAVEYARD: &str = "Exile target card from a graveyard.";

const ARTIFACT_HEXPROOF: &str = "Target artifact you control gains hexproof until end of turn.";

/// Reach guard: `source` resolved an instruction of `kind`, whatever it then
/// affected.
fn resolved(events: &[GameEvent], kind: EffectKind, source: ObjectId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved { kind: resolved_kind, source_id, .. }
                if *resolved_kind == kind && *source_id == source
        )
    })
}

fn controller(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id]
        .power
        .expect("creature must have a power")
}

fn damage(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].damage_marked
}

/// A 2/10 creature you control, a 4/10 creature you don't control, Tail Swipe
/// in your hand, and a steal instant in the opponent's hand, in your main phase.
struct TailSwipeBoard {
    runner: GameRunner,
    spell: ObjectId,
    mine: ObjectId,
    opp: ObjectId,
    steal: ObjectId,
}

fn tail_swipe_board() -> TailSwipeBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 10).id();
    let opp = scenario.add_creature(P1, "Opp", 4, 10).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tail Swipe", true, TAIL_SWIPE)
        .id();
    let steal = scenario
        .add_spell_to_hand_from_oracle(P1, "Steal", true, STEAL)
        .id();
    TailSwipeBoard {
        runner: scenario.build(),
        spell,
        mine,
        opp,
        steal,
    }
}

/// Baseline: cast in your main phase with both targets legal, the creature you
/// control gets +1/+1 and the two creatures fight.
#[test]
fn tail_swipe_pumps_the_creature_you_control_and_they_fight() {
    let TailSwipeBoard {
        mut runner,
        spell,
        mine,
        opp,
        ..
    } = tail_swipe_board();
    let cast = cast_spell_action(&runner, spell);
    drive_with_response(&mut runner, cast, &[mine, opp], None);

    assert_eq!(
        power(&runner, mine),
        3,
        "the creature you control gets +1/+1"
    );
    assert_eq!(damage(&runner, opp), 3, "the pumped 3/11 deals 3");
    assert_eq!(damage(&runner, mine), 4, "the 4/10 deals 4");
}

/// Tail Swipe has Blizzard Brawl's template, with a plain pump on the first
/// declared target. The creature you control is stolen in response, so it is
/// no longer "a creature you control": CR 608.2b keeps the +1/+1 off it, and
/// CR 701.14b stops the fight.
#[test]
fn tail_swipe_stolen_creature_gets_no_pump_and_nothing_fights() {
    let TailSwipeBoard {
        mut runner,
        spell,
        mine,
        opp,
        steal,
    } = tail_swipe_board();
    let cast = cast_spell_action(&runner, spell);
    let events = drive_with_response(
        &mut runner,
        cast,
        &[mine, opp],
        Some(PriorityResponse {
            player: P1,
            instant: steal,
            target: mine,
        }),
    );

    assert_eq!(
        controller(&runner, mine),
        P1,
        "reach guard: the response must have stolen the creature before Tail Swipe resolved"
    );
    assert!(
        resolved(&events, EffectKind::Pump, spell),
        "reach guard: the opponent's creature is still legal, so Tail Swipe resolves and, cast in \
         your main phase, reaches its pump"
    );
    assert!(
        resolved(&events, EffectKind::Fight, spell),
        "reach guard: Tail Swipe reaches its fight instruction"
    );
    assert_eq!(
        power(&runner, mine),
        2,
        "an illegal target must not get +1/+1"
    );
    assert_eq!(damage(&runner, mine), 0, "no fight, so no damage to it");
    assert_eq!(
        damage(&runner, opp),
        0,
        "no fight, so no damage to the other"
    );
}

/// Goblin Welder's return names its second target by slot. The artifact card
/// is exiled from the graveyard in response, so it is an illegal target (CR
/// 608.2b; ruling: "If either one or both has become illegal, nothing gets
/// sacrificed and nothing gets returned to the battlefield") and the ability
/// must not return it from exile.
#[test]
fn goblin_welder_does_not_return_an_artifact_card_exiled_in_response() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let welder = scenario
        .add_creature_from_oracle(P0, "Goblin Welder", 1, 1, GOBLIN_WELDER)
        .id();
    let relic = scenario.add_creature(P1, "Relic", 0, 1).as_artifact().id();
    let scrap = scenario
        .add_creature_to_graveyard(P1, "Scrap", 0, 1)
        .as_artifact()
        .id();
    // The response must NOT be named "Exile": `normalize_card_name_refs` rewrites
    // every occurrence of a card's own name to `~`, and "exile" is not one of the
    // masked keyword actions, so a card named "Exile" parses its own verb away
    // ("~ target card from a graveyard") and resolves as an inert
    // `Effect::Unimplemented` — leaving the target legal and this row vacuous.
    let purge = scenario
        .add_spell_to_hand_from_oracle(P1, "Purge", true, EXILE_FROM_GRAVEYARD)
        .id();
    let mut runner = scenario.build();

    // Reach guard: the return must parse with no origin zone. Only then does a
    // return that skips the legality check move the card out of exile; an
    // origin of the graveyard would refuse that move by itself, and this row
    // would test nothing.
    let return_origin = std::iter::successors(
        runner.state().objects[&welder].abilities.first(),
        |ability| ability.sub_ability.as_deref(),
    )
    .find_map(|ability| match ability.effect.as_ref() {
        Effect::ChangeZone { origin, .. } => Some(*origin),
        _ => None,
    });
    assert_eq!(
        return_origin,
        Some(None),
        "reach guard: Goblin Welder's return parses as a ChangeZone with no origin zone"
    );

    let events = drive_with_response(
        &mut runner,
        GameAction::ActivateAbility {
            source_id: welder,
            ability_index: 0,
        },
        &[relic, scrap],
        Some(PriorityResponse {
            player: P1,
            instant: purge,
            target: scrap,
        }),
    );

    assert!(
        resolved(&events, EffectKind::ChangeZone, purge),
        "reach guard: the response must have exiled the artifact card, or the slot stays legal \
         and this row tests nothing"
    );
    assert!(
        resolved(&events, EffectKind::ChangeZone, welder),
        "reach guard: the artifact is still legal, so the ability resolves and reaches its return"
    );
    assert_eq!(
        runner.state().objects[&scrap].zone,
        Zone::Exile,
        "an artifact card that left the graveyard is an illegal target and is not returned"
    );
    assert!(
        !runner.state().cost_payment_failed_flag,
        "a return that names an illegal slot affects nothing; it must not mark the chain as \
         having failed to do something, as an empty zone scan would"
    );
}

/// A 2/2 creature you control, an Equipment the opponent controls, Stolen
/// Uniform in your hand, and the opponent's `response_oracle` instant.
struct UniformBoard {
    runner: GameRunner,
    spell: ObjectId,
    mine: ObjectId,
    blade: ObjectId,
    response: ObjectId,
}

fn uniform_board(response_oracle: &str) -> UniformBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 2).id();
    let blade = scenario
        .add_creature(P1, "Blade", 0, 1)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Stolen Uniform", true, STOLEN_UNIFORM)
        .id();
    let response = scenario
        .add_spell_to_hand_from_oracle(P1, "Response", true, response_oracle)
        .id();
    UniformBoard {
        runner: scenario.build(),
        spell,
        mine,
        blade,
        response,
    }
}

/// Stolen Uniform names the Equipment by slot for both the gain of control and
/// the attach. The Equipment gains hexproof in response, so it is an illegal
/// target (CR 608.2b): you gain control of nothing and attach nothing.
#[test]
fn stolen_uniform_takes_no_control_of_an_equipment_that_became_illegal() {
    let UniformBoard {
        mut runner,
        spell,
        mine,
        blade,
        response,
    } = uniform_board(ARTIFACT_HEXPROOF);
    let cast = cast_spell_action(&runner, spell);
    let events = drive_with_response(
        &mut runner,
        cast,
        &[mine, blade],
        Some(PriorityResponse {
            player: P1,
            instant: response,
            target: blade,
        }),
    );

    assert!(
        runner.state().objects[&blade].has_keyword(&Keyword::Hexproof),
        "reach guard: the response must have given the Equipment hexproof"
    );
    assert!(
        resolved(&events, EffectKind::GainControl, spell),
        "reach guard: the creature you control is still legal, so Stolen Uniform resolves and \
         reaches its gain of control"
    );
    assert_eq!(
        controller(&runner, blade),
        P1,
        "an illegal Equipment stays under its controller"
    );
    assert!(
        runner.state().objects[&blade].attached_to.is_none(),
        "an illegal Equipment is not attached"
    );
}

/// Ruling: "If the target creature is an illegal target, you'll still gain
/// control of the target Equipment until end of turn, but you won't attach it
/// to the chosen creature." The creature you control is stolen in response.
#[test]
fn stolen_uniform_takes_the_equipment_but_does_not_attach_it_to_a_stolen_creature() {
    let UniformBoard {
        mut runner,
        spell,
        mine,
        blade,
        response,
    } = uniform_board(STEAL);
    let cast = cast_spell_action(&runner, spell);
    drive_with_response(
        &mut runner,
        cast,
        &[mine, blade],
        Some(PriorityResponse {
            player: P1,
            instant: response,
            target: mine,
        }),
    );

    assert_eq!(
        controller(&runner, mine),
        P1,
        "reach guard: the response must have stolen the creature before Stolen Uniform resolved"
    );
    assert_eq!(
        controller(&runner, blade),
        P0,
        "the legal Equipment still comes under your control"
    );
    assert!(
        runner.state().objects[&blade].attached_to.is_none(),
        "the Equipment is not attached to the illegal creature"
    );
}
