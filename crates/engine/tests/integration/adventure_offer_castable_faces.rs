//! Regression: My Precious // Allure of Power (artifact Equipment // instant
//! Adventure with "As an additional cost to cast this spell, sacrifice a
//! creature") locked the game in an Adventure `CastOffer` with zero legal
//! answers. `CastSpell` was listed in the controller's upkeep, the cast opened
//! the CR 715.3a face prompt, and neither face could be chosen.
//!
//! CR 715.3: the player chooses whether to cast the card as an Adventure — the
//! prompt must expose exactly the faces that are castable now (CR 601.2,
//! CR 301.1 / CR 307.1 timing, CR 601.2b/601.2h additional costs), and it must
//! never be issued with none.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastOfferKind, CastPaymentMode, CastingVariant, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, ObjectId(0), false, vec![]));
    }
}

/// Builds My Precious in P0's hand at `phase`, optionally with a creature P0
/// could sacrifice, and enough mana for either face ({3} or {1}{B}).
fn setup(phase: Phase, with_creature: bool) -> Option<(GameRunner, ObjectId)> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    let precious = scenario.add_real_card(P0, "My Precious", Zone::Hand, db);
    if with_creature {
        scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(
        &mut runner,
        &[ManaType::Black, ManaType::Black, ManaType::Black],
    );
    Some((runner, precious))
}

fn cast_listed(runner: &GameRunner, object_id: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id: id, .. } if *id == object_id),
    )
}

fn cast(runner: &mut GameRunner, object_id: ObjectId) -> WaitingFor {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("listed cast should be accepted")
        .waiting_for
}

fn offered_faces(runner: &GameRunner) -> Vec<bool> {
    legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::ChooseAdventureFace { creature } => Some(creature),
            _ => None,
        })
        .collect()
}

fn stack_variant(runner: &GameRunner, object_id: ObjectId) -> Option<CastingVariant> {
    runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == object_id)
        .and_then(|entry| match entry.kind {
            StackEntryKind::Spell {
                casting_variant, ..
            } => Some(casting_variant),
            _ => None,
        })
}

/// The production lock: upkeep, no creature. The artifact face is
/// sorcery-speed (CR 307.1 / CR 301.1) and the instant face can't pay its
/// sacrifice (CR 601.2h), so nothing is castable and no cast is listed.
#[test]
fn upkeep_without_creature_lists_no_cast() {
    let Some((runner, precious)) = setup(Phase::Upkeep, false) else {
        return;
    };
    assert!(
        !cast_listed(&runner, precious),
        "neither face of My Precious is castable in upkeep without a creature"
    );
}

/// A forged (unlisted) cast must be rejected, never parked in an empty offer.
#[test]
fn upkeep_without_creature_forged_cast_never_parks_in_empty_offer() {
    let Some((mut runner, precious)) = setup(Phase::Upkeep, false) else {
        return;
    };
    let card_id = runner.state().objects[&precious].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: precious,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    if let Ok(result) = result {
        assert!(
            !matches!(result.waiting_for, WaitingFor::CastOffer { .. })
                || !offered_faces(&runner).is_empty(),
            "a cast offer with no legal face is a soft-lock: {:?}",
            result.waiting_for
        );
    }
}

/// Upkeep with a creature: only the instant Adventure face is castable, so the
/// cast goes straight to it (or offers exactly that face).
#[test]
fn upkeep_with_creature_casts_only_the_instant_face() {
    let Some((mut runner, precious)) = setup(Phase::Upkeep, true) else {
        return;
    };
    assert!(cast_listed(&runner, precious));
    let waiting = cast(&mut runner, precious);
    if matches!(
        waiting,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Adventure { .. },
            ..
        }
    ) {
        assert_eq!(offered_faces(&runner), vec![false]);
        runner
            .act(GameAction::ChooseAdventureFace { creature: false })
            .expect("instant face should be castable");
    }
    // Sacrifice-cost selection may precede the stack push.
    finish_pending_cost(&mut runner);
    assert_eq!(
        stack_variant(&runner, precious),
        Some(CastingVariant::Adventure)
    );
}

/// Main phase, no creature to sacrifice: only My Precious itself.
#[test]
fn main_phase_without_creature_casts_only_the_artifact_face() {
    let Some((mut runner, precious)) = setup(Phase::PreCombatMain, false) else {
        return;
    };
    assert!(cast_listed(&runner, precious));
    let waiting = cast(&mut runner, precious);
    if matches!(
        waiting,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Adventure { .. },
            ..
        }
    ) {
        assert_eq!(offered_faces(&runner), vec![true]);
        runner
            .act(GameAction::ChooseAdventureFace { creature: true })
            .expect("artifact face should be castable");
    }
    assert_eq!(
        stack_variant(&runner, precious),
        Some(CastingVariant::Normal)
    );
}

/// Main phase with a creature: both faces are offered (CR 715.3).
#[test]
fn main_phase_with_creature_offers_both_faces() {
    let Some((mut runner, precious)) = setup(Phase::PreCombatMain, true) else {
        return;
    };
    assert!(cast_listed(&runner, precious));
    let waiting = cast(&mut runner, precious);
    assert!(
        matches!(
            waiting,
            WaitingFor::CastOffer {
                kind: CastOfferKind::Adventure { .. },
                ..
            }
        ),
        "expected the Adventure face prompt, got {waiting:?}"
    );
    let mut faces = offered_faces(&runner);
    faces.sort();
    assert_eq!(faces, vec![false, true]);
}

/// A plain Adventure creature still offers both faces in the main phase.
#[test]
fn ordinary_adventure_creature_still_offers_both_faces() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let otter = scenario.add_real_card(P0, "Elusive Otter", Zone::Hand, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_mana(
        &mut runner,
        &[ManaType::Blue, ManaType::Green, ManaType::Green],
    );

    assert!(cast_listed(&runner, otter));
    let waiting = cast(&mut runner, otter);
    assert!(matches!(
        waiting,
        WaitingFor::CastOffer {
            kind: CastOfferKind::Adventure { .. },
            ..
        }
    ));
    let mut faces = offered_faces(&runner);
    faces.sort();
    assert_eq!(faces, vec![false, true]);
    runner
        .act(GameAction::ChooseAdventureFace { creature: true })
        .expect("creature face should be castable");
    assert_eq!(stack_variant(&runner, otter), Some(CastingVariant::Normal));
}

/// Answers any sacrifice-selection prompt with the first legal choice.
fn finish_pending_cost(runner: &mut GameRunner) {
    for _ in 0..4 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            let Some(action) = legal_actions(runner.state()).into_iter().next() else {
                return;
            };
            if runner.act(action).is_err() {
                return;
            }
        }
    }
}
