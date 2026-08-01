//! The Temporal Anchor: completed-scry bottom predicate, linked exile, and play permission.

use engine::game::casting::{exile_lands_playable_by_permission, spell_objects_available_to_cast};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::parse_oracle_text;
use engine::types::ability::{Comparator, Effect, LibraryPosition, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const ORACLE: &str = "At the beginning of your upkeep, scry 2.\nWhenever you choose to put one or more cards on the bottom of your library while scrying, exile that many cards from the bottom of your library.\nDuring your turn, you may play cards exiled with The Temporal Anchor.";

/// Cast a zero-cost synthetic scry spell through `apply()` until its real
/// `WaitingFor::ScryChoice` is reached. The caller supplies the cards to keep
/// on top; every other offered card goes to the bottom under CR 701.22a.
fn cast_scry_to_choice(
    runner: &mut GameRunner,
    spell: engine::types::ObjectId,
) -> Vec<engine::types::ObjectId> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("scry spell cast must enter the production stack pipeline");

    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ScryChoice { cards, .. } => return cards,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must advance the scry spell");
            }
            other => panic!("expected ScryChoice while resolving scry spell, got {other:?}"),
        }
    }
    panic!("scry spell did not reach WaitingFor::ScryChoice");
}

/// Complete every stack item created by the selected scry, including trigger
/// ordering, through the public `GameAction` reducer.
fn resolve_after_scry_choice(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger order must be accepted");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass must resolve the next stack item");
            }
            other => panic!("unexpected prompt after completing scry: {other:?}"),
        }
    }
    panic!("scry-trigger stack did not settle");
}

#[test]
fn temporal_anchor_parses_completed_scry_predicate_and_bottom_exile() {
    let parsed = parse_oracle_text(
        ORACLE,
        "The Temporal Anchor",
        &[],
        &["Artifact".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::Scry)
        .expect("bottom-selection scry trigger must parse");
    assert_eq!(trigger.scry_bottom_count, Some((Comparator::GE, 1)));

    let execute = trigger
        .execute
        .as_ref()
        .expect("bottom-selection trigger execute");
    assert!(matches!(
        execute.effect.as_ref(),
        Effect::ExileTop {
            player: engine::types::ability::TargetFilter::Controller,
            position: LibraryPosition::Bottom,
            count: QuantityExpr::Ref {
                qty: QuantityRef::TriggeringScryBottomCount
            },
            face_down: false,
        }
    ));
    assert!(
        !parsed
            .triggers
            .iter()
            .any(|trigger| matches!(trigger.mode, TriggerMode::Unknown(_))),
        "reach guard: the completed-scry clause must not be hidden behind an unknown trigger"
    );
}

#[test]
fn temporal_anchor_zero_bottom_keeps_ordinary_scry_watcher_and_skips_anchor_trigger() {
    let Some(db) = load_db() else { return };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let anchor = scenario.add_real_card(P0, "The Temporal Anchor", Zone::Battlefield, db);
    let watcher = scenario
        .add_creature_from_oracle(
            P0,
            "Ordinary Scry Watcher",
            1,
            1,
            "Whenever you scry, you gain 1 life.",
        )
        .id();
    let scry = scenario
        .add_spell_to_hand_from_oracle(P0, "Keep Everything", false, "Scry 2.")
        .id();
    scenario.add_card_to_library_top(P0, "Second Top");
    scenario.add_card_to_library_top(P0, "First Top");
    let mut runner = scenario.build();

    let offered = cast_scry_to_choice(&mut runner, scry);
    assert_eq!(
        offered.len(),
        2,
        "reach guard: the real scry choice must offer both cards"
    );
    runner
        .act(GameAction::SelectCards {
            cards: offered.clone(),
        })
        .expect("keeping every scry card on top must complete the production choice");

    assert!(
        runner.state().stack.iter().any(|entry| matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if source_id == watcher
        )),
        "reach guard: the ordinary 'Whenever you scry' watcher must fire after the real choice"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if source_id == anchor
        )),
        "The Temporal Anchor's GE 1 bottom predicate must not trigger when every card stays on top"
    );

    resolve_after_scry_choice(&mut runner);
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        21,
        "the ordinary scry watcher must resolve; the negative Anchor assertion above reached the completed-scry trigger path"
    );
}

#[test]
fn temporal_anchor_real_exile_pile_casts_spells_plays_lands_and_isolates_second_source() {
    let Some(db) = load_db() else { return };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let anchor = scenario.add_real_card(P0, "The Temporal Anchor", Zone::Battlefield, db);
    let foreign_anchor = scenario.add_real_card(P1, "The Temporal Anchor", Zone::Battlefield, db);
    let original_top_one = scenario.add_real_card(P0, "Island", Zone::Library, db);
    let original_top_two = scenario.add_real_card(P0, "Mountain", Zone::Library, db);
    let own_spell = scenario.add_real_card(P0, "Ornithopter", Zone::Library, db);
    let own_land = scenario.add_real_card(P0, "Forest", Zone::Library, db);
    let foreign_spell = scenario.add_real_card(P1, "Ornithopter", Zone::Library, db);
    let own_scry = scenario
        .add_spell_to_hand_from_oracle(P0, "Anchor Scry", false, "Scry 4.")
        .id();
    let foreign_scry = scenario
        .add_spell_to_hand_from_oracle(P1, "Foreign Anchor Scry", false, "Scry 1.")
        .id();
    let mut runner = scenario.build();

    // The production scry sees four distinct cards in this explicit order. Keep
    // the original top two and bottom the spell/land pair; the real Anchor
    // trigger must then exile that bottom pair rather than the surviving top
    // pair. This distinguishes `LibraryPosition::Bottom` from a top-only resolver.
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![original_top_one, original_top_two, own_spell, own_land]
    );
    let offered = cast_scry_to_choice(&mut runner, own_scry);
    assert_eq!(
        offered,
        vec![original_top_one, original_top_two, own_spell, own_land],
        "the production scry must offer the entire ordered four-card library"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![original_top_one, original_top_two],
        })
        .expect("keep the original top pair and bottom the spell/land pair through the actual scry choice");
    resolve_after_scry_choice(&mut runner);
    assert_eq!(runner.state().objects[&own_spell].zone, Zone::Exile);
    assert_eq!(runner.state().objects[&own_land].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&original_top_one].zone,
        Zone::Library
    );
    assert_eq!(
        runner.state().objects[&original_top_two].zone,
        Zone::Library
    );
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .library
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![original_top_one, original_top_two],
        "only the original top pair survives in its original order"
    );
    assert!(runner
        .state()
        .exile_links
        .iter()
        .any(|link| { link.source_id == anchor && link.exiled_id == own_spell }));
    assert!(runner
        .state()
        .exile_links
        .iter()
        .any(|link| { link.source_id == anchor && link.exiled_id == own_land }));
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&own_spell),
        "the Anchor-linked nonland must enter the real cast offer on its controller's turn"
    );
    assert!(
        exile_lands_playable_by_permission(runner.state(), P0)
            .iter()
            .any(|(id, source)| *id == own_land && *source == anchor),
        "the Anchor-linked land must enter the real land-play offer with its source identity"
    );

    // CR 601.2a: use the production cast pipeline, not a direct resolver.
    runner
        .cast(own_spell)
        .resolve()
        .assert_zone(&[own_spell], Zone::Battlefield);
    let land_card_id = runner.state().objects[&own_land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: own_land,
            card_id: land_card_id,
        })
        .expect("an Anchor-linked land must be playable through GameAction::PlayLand");
    assert_eq!(runner.state().objects[&own_land].zone, Zone::Battlefield);

    // Give P1 its own completed-scry event and linked pile through the same
    // pipeline. That pile is valid for P1's Anchor only, never for P0's.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        state.lands_played_this_turn = 0;
    }
    assert_eq!(cast_scry_to_choice(&mut runner, foreign_scry).len(), 1);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("bottom P1's foreign card through the actual scry choice");
    resolve_after_scry_choice(&mut runner);
    assert_eq!(runner.state().objects[&foreign_spell].zone, Zone::Exile);
    assert!(runner
        .state()
        .exile_links
        .iter()
        .any(|link| { link.source_id == foreign_anchor && link.exiled_id == foreign_spell }));
    assert!(
        spell_objects_available_to_cast(runner.state(), P1).contains(&foreign_spell),
        "reach guard: the second source's own linked pile must be playable by its controller"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&foreign_spell),
        "a card linked only to P1's second Anchor must not leak into P0's cast offer"
    );
    assert!(
        !runner.state().stack.iter().any(|entry| matches!(
            entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if source_id == anchor
        )),
        "P1's completed scry must not retrigger P0's Anchor"
    );
}
