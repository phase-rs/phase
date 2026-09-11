//! Blizzard Brawl (KHM 162) — the conditional "If you control three or more
//! snow permanents, the creature you control gets +1/+0 and gains
//! indestructible until end of turn" bonus.
//!
//! Regression: the snow-permanent gate was satisfied, but the buff landed on the
//! OPPONENT's creature. The buff is a `GenericEffect` reached after a two-target
//! declaration, so its `ParentTargetSlot { index: 0 }` anaphor was indexed
//! against the node's local (most-recent, opponent) propagated targets instead of
//! the whole chain's declared slots.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::ObjectId;
use engine::types::Supertype;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const BLIZZARD_BRAWL: &str = "Choose target creature you control and target creature you don't control. \
If you control three or more snow permanents, the creature you control gets +1/+0 and gains \
indestructible until end of turn. Then those creatures fight each other. (Each deals damage equal to \
its power to the other.)";

fn drive_cast(runner: &mut GameRunner, spell: ObjectId, targets: &[ObjectId]) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast spell");

    let mut next_target = 0usize;
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("mana");
            }
            WaitingFor::TargetSelection { .. } => {
                let t = targets[next_target];
                next_target += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(t)),
                    })
                    .expect("choose target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("resolve");
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }
    panic!("cast did not resolve within window budget");
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
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Mine", 2, 10).id();
    let opp = scenario.add_creature(P1, "Opp", 4, 10).id();
    let snow_a = scenario.add_creature(P0, "SnowA", 1, 1).id();
    let snow_b = scenario.add_creature(P0, "SnowB", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Blizzard Brawl", false, BLIZZARD_BRAWL)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    make_snow(&mut runner, mine);
    match snow_permanents {
        0 => {}
        1 => make_snow(&mut runner, snow_a),
        2 => {
            make_snow(&mut runner, snow_a);
            make_snow(&mut runner, snow_b);
        }
        _ => panic!("unsupported snow count"),
    }

    drive_cast(&mut runner, spell, &[mine, opp]);
    (runner, mine, opp)
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
