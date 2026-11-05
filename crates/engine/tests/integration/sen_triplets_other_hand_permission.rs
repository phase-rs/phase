//! Sen Triplets — "At the beginning of your upkeep, choose target opponent.
//! This turn, that player can't cast spells or activate abilities and plays
//! with their hand revealed. You may play lands and cast spells from that
//! player's hand this turn." (Oracle text verified against MTGJSON.)
//!
//! Driven through the production upkeep trigger. CR references (verified
//! against docs/MagicCompRules.txt):
//!   - CR 101.2 + CR 602.5: the targeted player can't cast spells or activate
//!     abilities (mana abilities included) for the rest of the turn.
//!   - CR 400.2: the targeted player's hand is revealed while the effect lasts.
//!   - CR 601.2a: the Triplets' controller may cast a spell from that hand,
//!     pays its cost, and becomes the spell's controller.
//!   - CR 116.2a + CR 305.2: a land played from that hand is the controller's
//!     land play and enters under the controller's control (CR 110.2).
//!   - CR 611.2c: the permission reads the hand live, so a card that reaches
//!     it after the trigger resolved is covered.
//!   - CR 514.2: every part ends in the cleanup step.

use engine::ai_support::legal_actions;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, CastingVariant, CastingVariantFace, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SEN_TRIPLETS: &str = "At the beginning of your upkeep, choose target opponent. This turn, that player can't cast spells or activate abilities and plays with their hand revealed. You may play lands and cast spells from that player's hand this turn.";

struct Board {
    runner: GameRunner,
    /// P1's creature card in hand ({1}{G}).
    bear: ObjectId,
    /// P1's instant in hand ({R}).
    shock: ObjectId,
    /// P1's land card in hand.
    opponent_land: ObjectId,
    /// P0's own land card in hand (must stay unplayable once the drop is used).
    own_land: ObjectId,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.add_creature_from_oracle(P0, "Sen Triplets", 3, 3, SEN_TRIPLETS);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    // P1's own mana source: its mana ability is an activated ability.
    scenario.add_basic_land(P1, ManaColor::Red);
    let bear = scenario
        .add_creature_to_hand(P1, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(P1, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let opponent_land = scenario.add_land_to_hand(P1, "Mountain").id();
    let own_land = scenario.add_land_to_hand(P0, "Forest").id();
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Library Card");
        scenario.add_card_to_library_top(P1, "Library Card");
    }
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    Board {
        runner,
        bear,
        shock,
        opponent_land,
        own_land,
    }
}

/// Drive P0's upkeep: the trigger asks for its opponent target, then resolves,
/// and the turn proceeds to P0's precombat main phase.
fn resolve_upkeep_trigger(runner: &mut GameRunner) {
    runner.auto_advance_to_main_phase();
    // CR 603.3d: the only legal target (P1, the sole opponent) is chosen for
    // the trigger as it goes on the stack; a prompt appears only if the engine
    // asks for it.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            })
            .expect("target the opponent");
    }
    runner.advance_until_stack_empty();
    // CR 117.3d: pass the remaining Upkeep and Draw priority windows.
    for _ in 0..8 {
        if runner.state().phase == Phase::PreCombatMain {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass toward the main phase");
    }
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
}

fn hand_visible_to(
    runner: &GameRunner,
    viewer: engine::types::player::PlayerId,
    id: ObjectId,
) -> bool {
    filter_state_for_viewer(runner.state(), viewer)
        .objects
        .get(&id)
        .is_some_and(|object| object.display_visible_to_viewer)
}

#[test]
fn sen_triplets_locks_reveals_and_lends_the_opponents_hand_this_turn() {
    let Board {
        mut runner,
        bear,
        shock,
        opponent_land,
        own_land,
    } = board();

    // Before the trigger: P1's hand is hidden from P0 and P0 cannot reach it.
    assert!(!hand_visible_to(&runner, P0, bear));
    resolve_upkeep_trigger(&mut runner);

    // CR 400.2: P1 plays with their hand revealed this turn.
    for id in [bear, shock, opponent_land] {
        assert!(
            hand_visible_to(&runner, P0, id),
            "P1's hand card {id:?} must be visible to P0 this turn"
        );
    }
    // CR 101.2: P1 can't cast spells this turn (Shock is an instant).
    assert!(
        !spell_objects_available_to_cast(runner.state(), P1).contains(&shock),
        "P1 must not be able to cast Shock this turn"
    );

    // CR 601.2a + CR 116.2a: P0 is offered P1's spell and land.
    let offered = legal_actions(runner.state());
    assert!(
        offered.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == bear
        )),
        "P0 must be offered P1's creature card: {offered:?}"
    );
    assert!(
        offered.iter().any(|action| matches!(
            action,
            GameAction::PlayLand { object_id, .. } if *object_id == opponent_land
        )),
        "P0 must be offered P1's land card: {offered:?}"
    );

    // CR 116.2a + CR 305.2 + CR 110.2: P0 plays P1's Mountain as P0's land drop.
    let land_card = runner.state().objects[&opponent_land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: opponent_land,
            card_id: land_card,
        })
        .expect("P0 plays the land from P1's hand");
    let played = &runner.state().objects[&opponent_land];
    assert_eq!(played.zone, Zone::Battlefield);
    assert_eq!(played.controller, P0, "the played land is P0's");
    assert_eq!(played.owner, P1);
    assert_eq!(runner.state().lands_played_this_turn, 1);
    // CR 305.2b: the land drop is spent — P0's own land is no longer playable.
    let own_card = runner.state().objects[&own_land].card_id;
    assert!(
        runner
            .act(GameAction::PlayLand {
                object_id: own_land,
                card_id: own_card,
            })
            .is_err(),
        "P0 already used the turn's land play"
    );

    // CR 611.2c: a card that reaches P1's hand after resolution is covered.
    let late = {
        let state = runner.state_mut();
        let top = *state.players[1].library.front().expect("P1 library card");
        engine::game::zones::move_to_zone(state, top, Zone::Hand, &mut Vec::new());
        top
    };
    // Reveals are re-synced on every derive pass.
    engine::game::derived::derive_display_state(runner.state_mut());
    assert!(
        hand_visible_to(&runner, P0, late),
        "a card that reaches P1's hand later this turn is revealed too"
    );

    // CR 601.2a: P0 casts P1's Grizzly Bears, paying {1}{G} with P0's Forests,
    // and controls the resulting spell and permanent.
    let bear_card = runner.state().objects[&bear].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bear,
            card_id: bear_card,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the creature from P1's hand");
    assert_eq!(runner.state().objects[&bear].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&bear].controller, P0);

    // CR 602.5: with the spell on the stack, P1 receives priority but can
    // neither cast nor activate anything (not even its land's mana ability).
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    let p1_actions = legal_actions(runner.state());
    assert!(
        p1_actions
            .iter()
            .all(|action| matches!(action, GameAction::PassPriority)),
        "P1 may only pass this turn: {p1_actions:?}"
    );
    runner.advance_until_stack_empty();
    let resolved = &runner.state().objects[&bear];
    assert_eq!(resolved.zone, Zone::Battlefield);
    assert_eq!(resolved.controller, P0, "the cast permanent is P0's");
    assert_eq!(resolved.owner, P1);

    // CR 514.2: everything ends in the cleanup step. Advance into P1's turn.
    for _ in 0..64 {
        let state = runner.state();
        if state.active_player == P1 && state.phase == Phase::PreCombatMain {
            break;
        }
        match &state.waiting_for {
            WaitingFor::Priority { .. } => {}
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: Vec::new(),
                        bands: Vec::new(),
                    })
                    .expect("no attacks");
                continue;
            }
            other => panic!("unexpected prompt while advancing turns: {other:?}"),
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass toward P1's main phase");
    }
    assert_eq!(runner.state().active_player, P1);
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert!(
        !hand_visible_to(&runner, P0, shock),
        "P1's hand is hidden again after the turn"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P1).contains(&shock),
        "P1 can cast spells again on their own turn"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&shock),
        "P0 can no longer cast from P1's hand"
    );
}

/// Real printed cards for the borrowed-hand casting-method boundaries, reparsed
/// from a small MTGJSON fixture:
///   - Zoetic Cavern — "{T}: Add {C}. Morph {2}" (a Land with morph).
fn borrowed_hand_db() -> &'static engine::database::card_db::CardDatabase {
    static DB: std::sync::OnceLock<engine::database::card_db::CardDatabase> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sen_triplets_borrowed_hand_atomic.json");
        engine::database::card_db::CardDatabase::from_mtgjson(&path)
            .expect("parse the borrowed-hand fixture cards")
    })
}

/// Sen Triplets on P0's battlefield at the start of P0's turn, with `setup`
/// adding the rest of the board; the card database is installed on the state.
fn borrowed_hand_board<T>(setup: impl FnOnce(&mut GameScenario) -> T) -> (GameRunner, T) {
    let mut scenario = GameScenario::new();
    scenario.add_creature_from_oracle(P0, "Sen Triplets", 3, 3, SEN_TRIPLETS);
    for _ in 0..3 {
        scenario.add_card_to_library_top(P0, "Library Card");
        scenario.add_card_to_library_top(P1, "Library Card");
    }
    let ids = setup(&mut scenario);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), borrowed_hand_db());
    let state = runner.state_mut();
    state.turn_number = 2;
    state.phase = Phase::Untap;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    (runner, ids)
}

fn cast_spell(runner: &mut GameRunner, object_id: ObjectId) -> Result<(), String> {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

/// CR 702.37a + CR 702.37c + CR 305.9: a land with morph in the borrowed hand is
/// offered both as P0's land play and as a {3} face-down creature spell, and the
/// face-down cast resolves into a 2/2 under P0's control.
#[test]
fn sen_triplets_offers_and_casts_a_borrowed_morph_land_face_down() {
    let (mut runner, cavern) = borrowed_hand_board(|scenario| {
        for _ in 0..3 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        scenario.add_real_card(P1, "Zoetic Cavern", Zone::Hand, borrowed_hand_db())
    });
    assert!(runner.state().objects[&cavern]
        .card_types
        .core_types
        .contains(&engine::types::card_type::CoreType::Land));
    resolve_upkeep_trigger(&mut runner);

    let offered = legal_actions(runner.state());
    assert!(
        offered.iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == cavern
        )),
        "P0 must be offered the face-down cast of P1's Zoetic Cavern: {offered:?}"
    );
    assert!(
        offered.iter().any(|action| matches!(
            action,
            GameAction::PlayLand { object_id, .. } if *object_id == cavern
        )),
        "P0 must still be offered P1's Zoetic Cavern as a land play: {offered:?}"
    );

    cast_spell(&mut runner, cavern).expect("P0 casts P1's Zoetic Cavern face down");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: engine::types::actions::AlternativeCastDecision::Alternative,
            })
            .expect("choose the face-down cast");
    }
    assert_eq!(runner.state().objects[&cavern].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&cavern].controller, P0);
    runner.advance_until_stack_empty();

    let cast = &runner.state().objects[&cavern];
    assert_eq!(cast.zone, Zone::Battlefield);
    assert!(cast.face_down, "Zoetic Cavern enters face down");
    assert_eq!((cast.power, cast.toughness), (Some(2), Some(2)));
    assert_eq!(cast.controller, P0, "the face-down creature is P0's");
    assert_eq!(cast.owner, P1);
    assert_eq!(
        runner.state().lands_played_this_turn,
        0,
        "casting the morph face down is not a land play"
    );
}
