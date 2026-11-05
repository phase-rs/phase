//! Vulcan Mind-Meld — "Target opponent looks at the top card of your library,
//! then exiles it face down. You look at the top two cards of their library,
//! then exile them face down. For each card exiled this way, the player who
//! exiled it may play it for as long as it remains exiled. Mana of any type can
//! be spent to cast spells this way." (Oracle text verified against
//! `client/public/card-data.json`.)
//!
//! Covers, through the production cast → resolution path:
//!   * CR 608.2c + CR 115.1: a targeted subject acting on another player's
//!     library performs the exile, and is recorded as the player who exiled
//!     the card (CR 406.3).
//!   * CR 406.3a + CR 611.2a: each exiled card's permission binds to the player
//!     who exiled it — not its owner and not the spell's controller.
//!   * CR 609.4b: "Mana of any type can be spent to cast spells this way" is
//!     scoped to the cards exiled this way.
//!   * CR 406.3: a card exiled face down after a look stays visible to the
//!     player who looked at and exiled it, and hidden from the other player.
//!   * CR 305.1 + CR 701.18b: "play" covers playing a land from exile.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Vulcan Mind-Meld, verbatim (`client/public/card-data.json`).
const MIND_MELD: &str = "Target opponent looks at the top card of your library, then exiles it \
face down. You look at the top two cards of their library, then exile them face down. For each \
card exiled this way, the player who exiled it may play it for as long as it remains exiled. Mana \
of any type can be spent to cast spells this way.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn can_play_land(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::PlayLand { object_id, .. } if *object_id == id))
}

fn can_cast(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id))
}

fn seen_as(runner: &GameRunner, viewer: PlayerId, id: ObjectId) -> String {
    filter_state_for_viewer(runner.state(), viewer).objects[&id]
        .name
        .clone()
}

/// Advance until `player` has priority in their own precombat main phase on a
/// later turn.
fn advance_to_main_of(runner: &mut GameRunner, player: PlayerId) {
    let start_turn = runner.state().turn_number;
    for _ in 0..2000 {
        let state = runner.state();
        if state.turn_number > start_turn
            && state.active_player == player
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { player: p } if p == player)
        {
            return;
        }
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            other => panic!("unexpected prompt while passing turns: {other:?}"),
        };
        runner.act(action).expect("turn passes");
    }
    panic!("did not reach {player:?}'s next main phase");
}

struct Melded {
    runner: GameRunner,
    /// P0's top card, exiled by P1.
    p0_land: ObjectId,
    /// P1's top two cards, exiled by P0: a red sorcery and a land.
    p1_sorcery: ObjectId,
    p1_land: ObjectId,
    /// P1's third card, which the spell does not reach.
    p1_deep: ObjectId,
}

/// P0 casts Vulcan Mind-Meld targeting P1. P0 holds one blue mana and no red,
/// so the red sorcery P0 exiles is castable only through the any-type rider.
fn cast_mind_meld() -> Melded {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Filler 1", "Filler 2", "Filler 3", "Filler 4"]);
    }
    let p0_land = scenario.add_land_to_library_top(P0, "P0 Top Land").id();
    let p1_deep = scenario.add_land_to_library_top(P1, "P1 Deep Land").id();
    let p1_land = scenario.add_land_to_library_top(P1, "P1 Land").id();
    let p1_sorcery = scenario
        .add_spell_to_library_top(P1, "P1 Red Sorcery", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("You gain 3 life.")
        .id();
    let mind_meld = scenario
        .add_spell_to_hand_from_oracle(P0, "Vulcan Mind-Meld", false, MIND_MELD)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![])],
    );
    let mut runner = scenario.build();

    runner.cast(mind_meld).target_player(P1).resolve();
    assert!(runner.state().stack.is_empty(), "Mind-Meld resolved");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "no prompt is left open: {:?}",
        runner.state().waiting_for
    );

    Melded {
        runner,
        p0_land,
        p1_sorcery,
        p1_land,
        p1_deep,
    }
}

/// CR 608.2c + CR 406.3: the target opponent exiles the top card of
/// the caster's library; the caster exiles the top two of the opponent's. All
/// three are face down and attributed to the player who exiled them, and each
/// is visible only to that player.
#[test]
fn mind_meld_each_player_exiles_face_down_and_sees_only_their_own_exiles() {
    let Melded {
        runner,
        p0_land,
        p1_sorcery,
        p1_land,
        p1_deep,
    } = cast_mind_meld();

    for exiled in [p0_land, p1_sorcery, p1_land] {
        assert_eq!(zone_of(&runner, exiled), Zone::Exile, "{exiled:?}");
        assert!(
            runner.state().objects[&exiled].face_down,
            "{exiled:?} face down"
        );
    }
    assert_eq!(zone_of(&runner, p1_deep), Zone::Library, "only the top two");
    assert_eq!(runner.state().objects[&p0_land].exiled_by, Some(P1));
    assert_eq!(runner.state().objects[&p1_sorcery].exiled_by, Some(P0));
    assert_eq!(runner.state().objects[&p1_land].exiled_by, Some(P0));

    // CR 406.3: the player who looked at and exiled a card may keep looking at
    // it; the other player may not — not even the card's owner.
    assert_eq!(seen_as(&runner, P1, p0_land), "P0 Top Land");
    assert_eq!(seen_as(&runner, P0, p0_land), "Hidden Card");
    assert_eq!(seen_as(&runner, P0, p1_land), "P1 Land");
    assert_eq!(seen_as(&runner, P0, p1_sorcery), "P1 Red Sorcery");
    assert_eq!(seen_as(&runner, P1, p1_land), "Hidden Card");
    assert_eq!(seen_as(&runner, P1, p1_sorcery), "Hidden Card");
}

/// CR 406.3a + CR 609.4b + CR 305.1: the caster may play the two cards they
/// exiled — the red sorcery paid with blue mana, and the land as their land
/// drop — but not their own card the opponent exiled.
#[test]
fn mind_meld_caster_plays_the_cards_they_exiled_with_mana_of_any_type() {
    let Melded {
        mut runner,
        p0_land,
        p1_sorcery,
        p1_land,
        ..
    } = cast_mind_meld();

    assert!(can_play_land(&runner, p1_land), "the land P0 exiled");
    assert!(
        !can_play_land(&runner, p0_land),
        "P0's own card was exiled by P1, so P0 may not play it"
    );
    assert!(
        can_cast(&runner, p1_sorcery),
        "CR 609.4b: the red sorcery is castable with blue mana"
    );

    let life_before = runner.life(P0);
    runner.cast(p1_sorcery).resolve();
    assert_eq!(
        runner.life(P0),
        life_before + 3,
        "the sorcery resolved for P0"
    );
    assert_eq!(zone_of(&runner, p1_sorcery), Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&p1_sorcery].owner,
        P1,
        "an owner's card goes to its owner's graveyard"
    );

    runner
        .act(GameAction::PlayLand {
            object_id: p1_land,
            card_id: runner.state().objects[&p1_land].card_id,
        })
        .expect("play the exiled land");
    assert_eq!(zone_of(&runner, p1_land), Zone::Battlefield);
    assert_eq!(runner.state().objects[&p1_land].controller, P0);
}

/// CR 406.3a + CR 611.2a: the opponent may play the card they exiled from the
/// caster's library on their own turn, but never the cards the caster exiled
/// from the opponent's own library.
#[test]
fn mind_meld_opponent_plays_only_the_card_they_exiled() {
    let Melded {
        mut runner,
        p0_land,
        p1_land,
        p1_sorcery,
        ..
    } = cast_mind_meld();

    advance_to_main_of(&mut runner, P1);
    assert_eq!(zone_of(&runner, p0_land), Zone::Exile);
    assert!(
        can_play_land(&runner, p0_land),
        "P1 exiled P0's top card, so P1 may play it"
    );
    assert!(
        !can_play_land(&runner, p1_land),
        "P1 owns this land but P0 exiled it"
    );
    assert!(!can_cast(&runner, p1_sorcery), "P0 exiled P1's sorcery");
    runner
        .act(GameAction::PlayLand {
            object_id: p0_land,
            card_id: runner.state().objects[&p0_land].card_id,
        })
        .expect("P1 plays the land they exiled");
    assert_eq!(zone_of(&runner, p0_land), Zone::Battlefield);
    assert_eq!(runner.state().objects[&p0_land].controller, P1);
}
