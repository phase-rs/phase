//! Regression for GitHub issue #301 — Chaos Wand's free cast of a TARGETED
//! instant or sorcery.
//!
//! Chaos Wand: "{4}, {T}: Target opponent exiles cards from the top of their
//! library until they exile an instant or sorcery card. You may cast that card
//! without paying its mana cost. Then put the exiled cards that weren't cast
//! this way on the bottom of that library in a random order."
//!
//! CR 608.2g + CR 601.2c: the free cast happens as the ability resolves, and
//! choosing the spell's targets is part of casting it. CR 608.2c: "Then put
//! the exiled cards that weren't cast this way on the bottom" is the next
//! instruction, followed only after the cast is finished.
//!
//! The reported bug: accepting the cast opened the cast's target prompt, but
//! the bottom-cleanup ran first, moving the hit back into the library; the
//! advertised target was then rejected and nothing was cast.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const CHAOS_WAND: &str = "{4}, {T}: Target opponent exiles cards from the top of their library \
until they exile an instant or sorcery card. You may cast that card without paying its mana \
cost. Then put the exiled cards that weren't cast this way on the bottom of that library in a \
random order.";

struct Setup {
    runner: GameRunner,
    wand: ObjectId,
    miss: ObjectId,
    bolt: ObjectId,
    unreached: ObjectId,
}

/// P1's library, top to bottom: a land (the miss), Lightning Bolt (the hit),
/// another land (never reached).
fn setup() -> Setup {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let wand = scenario
        .add_artifact_from_oracle(P0, "Chaos Wand", CHAOS_WAND)
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }

    let unreached = scenario
        .add_land_to_library_top(P1, "Unreached Island")
        .id();
    let bolt = scenario
        .add_spell_to_library_top(P1, "Lightning Bolt", true)
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    let miss = scenario.add_land_to_library_top(P1, "Exiled Island").id();

    Setup {
        runner: scenario.build(),
        wand,
        miss,
        bolt,
        unreached,
    }
}

/// Activate the Wand targeting P1, pay its cost, and resolve it up to the
/// "you may cast that card" prompt.
fn activate_to_cast_prompt(runner: &mut GameRunner, wand: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: wand,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Chaos Wand activation must be accepted");
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("target opponent");
            }
            WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected state while activating Chaos Wand: {other:?}"),
        }
    }
    panic!("never reached the optional cast prompt");
}

fn library(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

/// CR 608.2g + CR 601.2c + CR 608.2c (issue #301): accepting the cast of a
/// targeted instant asks for its target while the exiled cards are still in
/// exile; the Bolt is cast at the chosen target, the miss goes to the bottom,
/// and the Bolt deals its damage.
#[test]
fn chaos_wand_casts_a_targeted_instant_before_bottoming_the_rest() {
    let Setup {
        mut runner,
        wand,
        miss,
        bolt,
        unreached,
    } = setup();
    activate_to_cast_prompt(&mut runner, wand);

    assert_eq!(runner.state().objects[&miss].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Exile);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the free cast");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "the free cast must ask for Lightning Bolt's target, found {:?}",
        runner.state().waiting_for
    );
    assert_ne!(
        runner.state().objects[&bolt].zone,
        Zone::Library,
        "the hit must not return to the library while its cast is in progress"
    );
    assert_eq!(
        runner.state().objects[&miss].zone,
        Zone::Exile,
        "CR 608.2c: the bottom instruction waits until the cast is finished"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("the engine-advertised target must be accepted");

    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Stack,
        "Lightning Bolt is cast"
    );
    let p1_library = library(&runner, P1);
    assert_eq!(runner.state().objects[&miss].zone, Zone::Library);
    assert_eq!(
        p1_library.first(),
        Some(&unreached),
        "the unreached card stays on top"
    );
    assert_eq!(
        p1_library.last(),
        Some(&miss),
        "the uncast miss goes to the bottom"
    );
    assert!(!p1_library.contains(&bolt), "the cast Bolt is not bottomed");

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 17, "the free Lightning Bolt deals 3");
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Graveyard);
}

/// CR 608.2c control: declining the cast bottoms the hit with the miss.
#[test]
fn chaos_wand_declined_cast_bottoms_the_hit_with_the_misses() {
    let Setup {
        mut runner,
        wand,
        miss,
        bolt,
        unreached,
    } = setup();
    activate_to_cast_prompt(&mut runner, wand);

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline the free cast");
    runner.advance_until_stack_empty();

    let p1_library = library(&runner, P1);
    assert_eq!(p1_library.first(), Some(&unreached));
    assert_eq!(p1_library.len(), 3);
    assert!(p1_library[1..].contains(&miss));
    assert!(p1_library[1..].contains(&bolt));
    assert_eq!(runner.life(P1), 20);
}

/// CR 601.2a + CR 608.2c: withdrawing the free cast at its target step leaves
/// the Bolt uncast, so the Wand's next instruction bottoms it with the miss.
#[test]
fn chaos_wand_withdrawn_cast_bottoms_the_hit_with_the_misses() {
    let Setup {
        mut runner,
        wand,
        miss,
        bolt,
        unreached,
    } = setup();
    activate_to_cast_prompt(&mut runner, wand);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the free cast");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "reach guard: the free cast is at its target step, found {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::CancelCast)
        .expect("withdraw the free cast");
    runner.advance_until_stack_empty();

    let p1_library = library(&runner, P1);
    assert_eq!(p1_library.first(), Some(&unreached));
    assert_eq!(p1_library.len(), 3);
    assert!(p1_library[1..].contains(&miss));
    assert!(p1_library[1..].contains(&bolt));
    assert_eq!(runner.life(P1), 20);
}

/// CR 709.3a + CR 608.2g + CR 608.2c: a split-card hit with two castable halves
/// first asks which half to cast. The exiled cards stay in exile through that
/// choice, and are bottomed once Death is cast.
#[test]
fn chaos_wand_split_card_hit_waits_through_the_face_choice() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let wand = scenario
        .add_artifact_from_oracle(P0, "Chaos Wand", CHAOS_WAND)
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    // Death's target: a creature card in the caster's graveyard.
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let unreached = scenario
        .add_land_to_library_top(P1, "Unreached Island")
        .id();
    let life_death = scenario.add_real_card(P1, "Life", Zone::Library, db);
    let miss = scenario.add_land_to_library_top(P1, "Exiled Island").id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().players[P1.0 as usize].library =
        engine::im::vector![miss, life_death, unreached];

    activate_to_cast_prompt(&mut runner, wand);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the free cast");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ModalFaceChoice { .. }
        ),
        "both halves are castable, so the free cast asks for a half, found {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&miss].zone,
        Zone::Exile,
        "CR 608.2c: the bottom instruction waits through the face choice"
    );

    runner
        .act(GameAction::ChooseModalFace { back_face: true })
        .expect("cast Death");
    // CR 601.2c: Grizzly Bears is Death's only legal target, so electing the
    // half finishes the cast with no target prompt. That leaves the face
    // choice itself as the answer that must finish the Wand's resolution.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "reach guard: Death is cast straight from the face choice, found {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().stack.last().map(|entry| entry.source_id),
        Some(life_death),
        "Death is on the stack targeting the only creature card in the graveyard"
    );
    assert_eq!(runner.state().objects[&life_death].zone, Zone::Stack);
    let p1_library = library(&runner, P1);
    assert_eq!(p1_library, vec![unreached, miss]);

    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
}
