//! Blizzard Brawl (KHM 162) — the conditional "If you control three or more
//! snow permanents, the creature you control gets +1/+0 and gains
//! indestructible until end of turn" bonus.
//!
//! Regression: the snow-permanent gate was satisfied, but the buff landed on the
//! OPPONENT's creature. The buff is a `GenericEffect` reached after a two-target
//! declaration, so its `ParentTargetSlot { index: 0 }` anaphor was indexed
//! against the node's local (most-recent, opponent) propagated targets instead of
//! the whole chain's declared slots.
//!
//! The 2021-02-05 rulings pin the rest of the card: the snow count is checked
//! once, as Blizzard Brawl resolves; an illegal target on either side means
//! nothing fights; and the creature you control keeps the bonus when only the
//! other target is illegal.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::zones::move_to_zone;
use engine::types::ability::EffectKind;
use engine::types::events::GameEvent;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;
use engine::types::Supertype;

use crate::rules::{cast_spell_action, drive_with_response, PriorityResponse};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const BLIZZARD_BRAWL: &str = "Choose target creature you control and target creature you don't control. \
If you control three or more snow permanents, the creature you control gets +1/+0 and gains \
indestructible until end of turn. Then those creatures fight each other. (Each deals damage equal to \
its power to the other.)";

const STEAL: &str = "Gain control of target creature until end of turn.";

const HEXPROOF: &str = "Target creature you control gains hexproof until end of turn.";

/// Cast `spell` and drive the stack empty, answering its target prompts with
/// `targets` in order. When `response` is `Some((instant, target))`, P1 casts
/// that instant at `target` above `spell`, so the response resolves first.
/// Returns every event emitted.
fn drive_cast(
    runner: &mut GameRunner,
    spell: ObjectId,
    targets: &[ObjectId],
    response: Option<(ObjectId, ObjectId)>,
) -> Vec<GameEvent> {
    let cast = cast_spell_action(runner, spell);
    let response = response.map(|(instant, target)| PriorityResponse {
        player: P1,
        instant,
        target,
    });
    drive_with_response(runner, cast, targets, response)
}

/// The board every test casts Blizzard Brawl on: a 2/10 snow creature you
/// control, a 4/10 creature you don't control, `extra_snow` more snow 1/1s you
/// control, a non-snow basic land you control, and — when `response_oracle` is
/// set — an instant in the opponent's hand.
struct BrawlBoard {
    runner: GameRunner,
    spell: ObjectId,
    mine: ObjectId,
    opp: ObjectId,
    other_snow: Vec<ObjectId>,
    land: ObjectId,
    response: Option<ObjectId>,
}

fn brawl_board(extra_snow: usize, response_oracle: Option<&str>) -> BrawlBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 10).id();
    let opp = scenario.add_creature(P1, "Opp", 4, 10).id();
    let other_snow: Vec<ObjectId> = (0..extra_snow)
        .map(|index| {
            scenario
                .add_creature(P0, &format!("Snow {index}"), 1, 1)
                .id()
        })
        .collect();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Blizzard Brawl", false, BLIZZARD_BRAWL)
        .id();
    let response = response_oracle.map(|oracle| {
        scenario
            .add_spell_to_hand_from_oracle(P1, "Response", true, oracle)
            .id()
    });
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    for id in std::iter::once(mine).chain(other_snow.iter().copied()) {
        make_snow(&mut runner, id);
    }
    BrawlBoard {
        runner,
        spell,
        mine,
        opp,
        other_snow,
        land,
        response,
    }
}

fn make_snow(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    if !obj.card_types.supertypes.contains(&Supertype::Snow) {
        obj.card_types.supertypes.push(Supertype::Snow);
    }
    if !obj.base_card_types.supertypes.contains(&Supertype::Snow) {
        obj.base_card_types.supertypes.push(Supertype::Snow);
    }
}

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id]
        .power
        .expect("creature must have a power")
}

fn damage(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].damage_marked as i32
}

fn has_indestructible(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Indestructible)
}

/// Cast Blizzard Brawl with a 2/10 you-control fighter and a 4/10 opponent
/// fighter, with `snow_permanents` ADDITIONAL snow permanents beyond the
/// you-control fighter (which is always snow itself). Returns `(runner, mine, opp)`.
fn run_blizzard_brawl(snow_permanents: usize) -> (GameRunner, ObjectId, ObjectId) {
    let BrawlBoard {
        mut runner,
        spell,
        mine,
        opp,
        ..
    } = brawl_board(snow_permanents, None);
    drive_cast(&mut runner, spell, &[mine, opp], None);
    (runner, mine, opp)
}

fn controller(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

/// Reach guard: Blizzard Brawl resolved (rather than being removed for having
/// no legal targets) and reached its fight instruction.
fn fight_instruction_resolved(events: &[GameEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Fight,
                ..
            }
        )
    })
}

#[test]
fn blizzard_brawl_buffs_the_you_control_creature_with_three_snow_permanents() {
    let (runner, mine, opp) = run_blizzard_brawl(2);
    assert_eq!(
        power(&runner, mine),
        3,
        "the creature you control gets +1/+0 with three snow permanents"
    );
    assert!(
        has_indestructible(&runner, mine),
        "the creature you control gains indestructible with three snow permanents"
    );
    // CR 608.2c: the buff must NOT land on the opponent's creature — that is the
    // exact mis-binding this regression pins (slot 0 resolved to the most-recent
    // parent target rather than the first declared one).
    assert_eq!(
        power(&runner, opp),
        4,
        "the opponent's creature must not receive the +1/+0"
    );
    assert!(
        !has_indestructible(&runner, opp),
        "the opponent's creature must not gain indestructible"
    );
    // Fight: the buffed 3/10 you-control fighter deals 3; the 4/10 opponent deals 4.
    assert_eq!(
        damage(&runner, opp),
        3,
        "buffed you-control creature deals 3"
    );
    assert_eq!(damage(&runner, mine), 4, "opponent 4/10 deals 4");
}

#[test]
fn blizzard_brawl_no_buff_with_fewer_than_three_snow_permanents() {
    let (runner, mine, opp) = run_blizzard_brawl(1);
    assert_eq!(
        power(&runner, mine),
        2,
        "no +1/+0 with fewer than three snow permanents"
    );
    assert!(
        !has_indestructible(&runner, mine),
        "no indestructible with fewer than three snow permanents"
    );
    // Unbuffed fight: the 2/10 you-control fighter deals 2; the 4/10 opponent deals 4.
    assert_eq!(
        damage(&runner, opp),
        2,
        "unbuffed you-control creature deals 2"
    );
    assert_eq!(damage(&runner, mine), 4, "opponent 4/10 deals 4");
}

/// Ruling: "If either target is an illegal target as Blizzard Brawl tries to
/// resolve, neither creature will fight and no damage will be dealt." The
/// creature you control is stolen in response, so it is no longer "a creature
/// you control": CR 608.2b keeps the bonus off it even though you still
/// control three snow permanents, and CR 701.14b stops the fight.
#[test]
fn blizzard_brawl_stolen_creature_gets_no_bonus_and_nothing_fights() {
    let BrawlBoard {
        mut runner,
        spell,
        mine,
        opp,
        other_snow,
        response,
        ..
    } = brawl_board(3, Some(STEAL));
    let events = drive_cast(
        &mut runner,
        spell,
        &[mine, opp],
        response.map(|steal| (steal, mine)),
    );

    assert_eq!(
        controller(&runner, mine),
        P1,
        "reach guard: the response must have stolen the creature before Blizzard Brawl resolved"
    );
    assert!(
        other_snow.iter().all(|id| controller(&runner, *id) == P0),
        "reach guard: you still control three snow permanents, so only target legality can withhold the bonus"
    );
    assert!(
        fight_instruction_resolved(&events),
        "reach guard: the opponent's creature is still legal, so Blizzard Brawl resolves"
    );
    assert_eq!(
        power(&runner, mine),
        2,
        "an illegal target must not get +1/+0"
    );
    assert!(
        !has_indestructible(&runner, mine),
        "an illegal target must not gain indestructible"
    );
    assert_eq!(damage(&runner, mine), 0, "no fight, so no damage to it");
    assert_eq!(
        damage(&runner, opp),
        0,
        "no fight, so no damage to the other"
    );
}

/// Ruling: "If the creature you control is still a legal target as Blizzard
/// Brawl tries to resolve but the target creature you don't control isn't, the
/// creature you control will still get the bonuses until end of turn." The
/// opponent's creature gains hexproof in response, so nothing fights (CR
/// 701.14b), while the creature you control is still buffed.
#[test]
fn blizzard_brawl_illegal_opponent_creature_still_buffs_yours_without_a_fight() {
    let BrawlBoard {
        mut runner,
        spell,
        mine,
        opp,
        response,
        ..
    } = brawl_board(2, Some(HEXPROOF));
    let events = drive_cast(
        &mut runner,
        spell,
        &[mine, opp],
        response.map(|hexproof| (hexproof, opp)),
    );

    assert!(
        runner.state().objects[&opp].has_keyword(&Keyword::Hexproof),
        "reach guard: the response must have given the opponent's creature hexproof"
    );
    assert!(
        fight_instruction_resolved(&events),
        "reach guard: the creature you control is still legal, so Blizzard Brawl resolves"
    );
    assert_eq!(
        power(&runner, mine),
        3,
        "the legal creature you control still gets +1/+0"
    );
    assert!(
        has_indestructible(&runner, mine),
        "the legal creature you control still gains indestructible"
    );
    assert_eq!(damage(&runner, mine), 0, "no fight, so no damage to it");
    assert_eq!(
        damage(&runner, opp),
        0,
        "no fight, so no damage to the other"
    );
}

/// Ruling: "Check whether you control three or more snow permanents as
/// Blizzard Brawl is resolving." CR 608.2h: that answer "is determined only
/// once, when the effect is applied", so a snow permanent leaving afterwards
/// does not end the bonus.
#[test]
fn blizzard_brawl_bonus_outlives_a_snow_permanent_leaving_after_resolution() {
    let BrawlBoard {
        mut runner,
        spell,
        mine,
        opp,
        other_snow,
        ..
    } = brawl_board(2, None);
    drive_cast(&mut runner, spell, &[mine, opp], None);
    assert_eq!(
        power(&runner, mine),
        3,
        "reach guard: Blizzard Brawl resolved with three snow permanents"
    );

    move_to_zone(
        runner.state_mut(),
        other_snow[0],
        Zone::Graveyard,
        &mut Vec::new(),
    );
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&other_snow[0]].zone,
        Zone::Graveyard,
        "reach guard: you now control only two snow permanents"
    );
    assert_eq!(
        power(&runner, mine),
        3,
        "the +1/+0 lasts until end of turn once granted"
    );
    assert!(
        has_indestructible(&runner, mine),
        "indestructible lasts until end of turn once granted"
    );
}

/// The inverse of the check above: Blizzard Brawl resolves with two snow
/// permanents, so no bonus is granted, and your land becoming snow afterwards
/// (a third snow permanent) does not grant it late.
#[test]
fn blizzard_brawl_grants_no_bonus_when_a_third_snow_permanent_arrives_after_resolution() {
    let BrawlBoard {
        mut runner,
        spell,
        mine,
        opp,
        land,
        ..
    } = brawl_board(1, None);
    drive_cast(&mut runner, spell, &[mine, opp], None);
    assert_eq!(
        power(&runner, mine),
        2,
        "reach guard: Blizzard Brawl resolved with two snow permanents"
    );

    make_snow(&mut runner, land);
    evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&land]
            .card_types
            .supertypes
            .contains(&Supertype::Snow),
        "reach guard: you now control three snow permanents"
    );
    assert_eq!(
        power(&runner, mine),
        2,
        "a snow permanent arriving after resolution grants no +1/+0"
    );
    assert!(
        !has_indestructible(&runner, mine),
        "a snow permanent arriving after resolution grants no indestructible"
    );
}
