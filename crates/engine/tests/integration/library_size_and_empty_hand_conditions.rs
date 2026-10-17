//! Library-size and empty-hand player-quantifier conditions, driven through the
//! real `apply()` pipeline with verbatim Oracle text:
//!
//! - Shelldock Isle: "… if a library has twenty or fewer cards in it." — an
//!   existential over per-player library sizes (CR 400.1 + CR 401.3), checked
//!   when the ability resolves (CR 608.2c), never gating activation (CR 602.5).
//! - Isleback Spawn: the same predicate as a continuous static gate
//!   (CR 611.3a + CR 613.4c).
//! - Howltooth Hollow: "… if each player has no cards in hand." — a universal
//!   over every player's hand (CR 102.1 + CR 402.3), checked on resolution.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::CastingPermission;
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const SHELLDOCK_ISLE: &str = "Hideaway 4 (When this land enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)\nThis land enters tapped.\n{T}: Add {U}.\n{U}, {T}: You may play the exiled card without paying its mana cost if a library has twenty or fewer cards in it.";

const ISLEBACK_SPAWN: &str = "Shroud (This creature can't be the target of spells or abilities.)\nThis creature gets +4/+8 as long as a library has twenty or fewer cards in it.";

const HOWLTOOTH_HOLLOW: &str = "Hideaway 4 (When this land enters, look at the top four cards of your library, exile one face down, then put the rest on the bottom in a random order.)\nThis land enters tapped.\n{T}: Add {B}.\n{B}, {T}: You may play the exiled card without paying its mana cost if each player has no cards in hand.";

const MILL_ONE: &str = "Target player mills a card.";
const DRAW_ONE: &str = "Draw a card.";

fn seed_library(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for i in 0..count {
        scenario.add_card_to_library_top(player, &format!("Library Card {i}"));
    }
}

fn seed_hand(scenario: &mut GameScenario, player: PlayerId, count: usize) {
    for i in 0..count {
        scenario.add_card_to_hand(player, &format!("Held Card {i}"));
    }
}

fn free_instant(scenario: &mut GameScenario, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, name, true, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn library_sizes(runner: &GameRunner) -> Vec<usize> {
    runner
        .state()
        .players
        .iter()
        .map(|p| p.library.len())
        .collect()
}

fn hand_sizes(runner: &GameRunner) -> Vec<usize> {
    runner
        .state()
        .players
        .iter()
        .map(|p| p.hand.len())
        .collect()
}

/// A Hideaway land P0 has played, with the card it hid.
struct HideawayBoard {
    runner: GameRunner,
    land: ObjectId,
    hidden: ObjectId,
}

/// P0 plays the Hideaway land from hand and drives its CR 702.75a trigger,
/// hiding the first offered card. Each player's library is seeded with
/// `libraries[i]` cards — P0's with one extra, because Hideaway exiles one of
/// them — and each hand with `hands[i]` cards besides the land itself.
fn hideaway_land_board(
    name: &str,
    oracle: &str,
    libraries: &[usize],
    hands: &[usize],
    extras: impl FnOnce(&mut GameScenario),
) -> HideawayBoard {
    let player_count = u8::try_from(libraries.len()).expect("small player count");
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario
        .add_land_to_hand(P0, name)
        .from_oracle_text(oracle)
        .id();
    for (index, (&library, &hand)) in libraries.iter().zip(hands).enumerate() {
        let player = PlayerId(u8::try_from(index).expect("small player index"));
        let extra = usize::from(player == P0);
        seed_library(&mut scenario, player, library + extra);
        seed_hand(&mut scenario, player, hand);
    }
    extras(&mut scenario);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing the Hideaway land must be legal");
    let mut hidden = None;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { cards, .. } => {
                hidden = Some(cards[0]);
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("hiding a card");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() && hidden.is_some() => {
                break
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority");
            }
            other => panic!("unexpected prompt while hiding a card: {other:?}"),
        }
    }
    let hidden = hidden.expect("the Hideaway trigger offered a DigChoice");
    assert_eq!(runner.state().priority_player, P0);

    // "This land enters tapped." The scenario stays on this turn so no draw
    // step moves the seeded library and hand sizes; untap directly instead of
    // passing a turn.
    runner
        .state_mut()
        .objects
        .get_mut(&land)
        .expect("land exists")
        .tapped = false;

    HideawayBoard {
        runner,
        land,
        hidden,
    }
}

fn fund(runner: &mut GameRunner, mana: ManaType) {
    runner.state_mut().players[0]
        .mana_pool
        .add(ManaUnit::new(mana, ObjectId(0), false, vec![]));
}

/// Activates the land's "{X}, {T}: You may play the exiled card …" ability
/// (index 1, after the mana ability), passes priority until the stack is empty,
/// accepting the optional offer if it appears. Returns whether it appeared.
fn activate_play_ability(runner: &mut GameRunner, land: ObjectId) -> bool {
    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: 1,
        })
        .expect("CR 602.5: the play ability is activatable whatever the condition");
    settle_stack_accepting_offer(runner)
}

fn settle_stack_accepting_offer(runner: &mut GameRunner) -> bool {
    let mut offered = false;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting the offer");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority");
            }
            other => panic!("unexpected prompt while resolving the ability: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "the stack must settle after the ability resolves"
    );
    offered
}

fn may_play_from_source(board: &HideawayBoard) -> bool {
    board.runner.state().objects[&board.hidden]
        .casting_permissions
        .iter()
        .any(|p| {
            matches!(
                p,
                CastingPermission::PlayFromExile {
                    source_id: Some(src),
                    ..
                } if *src == board.land
            )
        })
}

/// Reach-guard for every "not offered" row: the activation went through (land
/// tapped, mana spent, stack settled) and the hidden card is still the land's
/// face-down Hideaway card, so the refusal came from the resolution-time
/// condition and not from an upstream short-circuit.
fn assert_resolved_without_offer(board: &HideawayBoard, offered: bool) {
    let state = board.runner.state();
    assert!(state.objects[&board.land].tapped, "the {{T}} cost was paid");
    assert_eq!(
        state.players[0].mana_pool.total(),
        0,
        "the mana cost was paid"
    );
    let hidden = &state.objects[&board.hidden];
    assert_eq!(hidden.zone, Zone::Exile);
    assert!(hidden.face_down);
    assert!(state.exile_links.iter().any(|link| {
        link.exiled_id == board.hidden
            && link.source_id == board.land
            && matches!(link.kind, ExileLinkKind::HideawayLookable { .. })
    }));
    assert!(
        !offered,
        "the condition is false on resolution: no \"you may play\" offer"
    );
    assert!(
        !may_play_from_source(board),
        "the condition is false on resolution: no play permission"
    );
}

fn assert_resolved_with_offer(board: &HideawayBoard, offered: bool) {
    assert!(
        offered,
        "the condition is true on resolution: the offer appears"
    );
    assert!(
        may_play_from_source(board),
        "accepting grants the play permission for the land's hidden card"
    );
}

/// Shelldock Isle with the given per-player library sizes at activation.
fn activate_shelldock(libraries: &[usize]) -> (HideawayBoard, bool) {
    let hands = vec![0; libraries.len()];
    let mut board =
        hideaway_land_board("Shelldock Isle", SHELLDOCK_ISLE, libraries, &hands, |_| {});
    fund(&mut board.runner, ManaType::Blue);
    assert_eq!(library_sizes(&board.runner), libraries, "reach-guard");
    let offered = activate_play_ability(&mut board.runner, board.land);
    (board, offered)
}

/// CR 400.1 + CR 401.3 + CR 608.2c: with every library above twenty cards the
/// condition is false on resolution — the ability resolves and offers nothing.
#[test]
fn shelldock_isle_offers_nothing_when_every_library_has_more_than_twenty_cards() {
    let (board, offered) = activate_shelldock(&[21, 21]);
    assert_resolved_without_offer(&board, offered);
}

/// CR 400.1 + CR 401.3: ANY player's library qualifies — an opponent's library
/// at twenty cards satisfies "a library", though the controller's has 21.
#[test]
fn shelldock_isle_offers_play_when_an_opponents_library_has_twenty_cards() {
    let (board, offered) = activate_shelldock(&[21, 20]);
    assert_resolved_with_offer(&board, offered);
}

/// CR 400.1: each library is checked on its own, never summed — two libraries
/// of fifteen (thirty in total) qualify, as does the controller's own library
/// at exactly twenty.
#[test]
fn shelldock_isle_checks_each_library_separately() {
    for libraries in [[15, 15], [20, 21]] {
        let (board, offered) = activate_shelldock(&libraries);
        assert_resolved_with_offer(&board, offered);
    }
}

/// CR 608.2c + CR 602.5: the condition is read when the ability resolves, not
/// when it is activated — with both libraries at 21 on activation, milling the
/// opponent to twenty in response makes the offer appear.
#[test]
fn shelldock_isle_checks_library_size_on_resolution_not_activation() {
    let mut mill = None;
    let mut board = hideaway_land_board(
        "Shelldock Isle",
        SHELLDOCK_ISLE,
        &[21, 21],
        &[0, 0],
        |scenario| mill = Some(free_instant(scenario, "Mill One", MILL_ONE)),
    );
    let mill = mill.expect("mill helper staged");
    fund(&mut board.runner, ManaType::Blue);
    assert_eq!(library_sizes(&board.runner), [21, 21], "reach-guard");
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.land,
            ability_index: 1,
        })
        .expect("CR 602.5: activation is not gated by the condition");
    assert_eq!(
        board.runner.state().stack.len(),
        1,
        "the ability is on the stack"
    );

    board.runner.cast(mill).target_player(P1).commit();
    assert_eq!(board.runner.state().stack.len(), 2, "the mill responds");
    let offered = settle_stack_accepting_offer(&mut board.runner);

    assert_eq!(
        library_sizes(&board.runner),
        [21, 20],
        "the mill resolved first"
    );
    assert_resolved_with_offer(&board, offered);
}

fn isleback_board(
    libraries: &[usize],
    extras: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId) {
    let player_count = u8::try_from(libraries.len()).expect("small player count");
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spawn = scenario
        .add_creature(P0, "Isleback Spawn", 4, 8)
        .from_oracle_text(ISLEBACK_SPAWN)
        .id();
    for (index, &library) in libraries.iter().enumerate() {
        let player = PlayerId(u8::try_from(index).expect("small player index"));
        seed_library(&mut scenario, player, library);
    }
    extras(&mut scenario);
    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    assert_eq!(library_sizes(&runner), libraries, "reach-guard");
    (runner, spawn)
}

fn power_toughness(runner: &GameRunner, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let obj = &runner.state().objects[&id];
    (obj.power, obj.toughness)
}

/// CR 611.3a + CR 613.4c: Isleback Spawn's bonus is off while every library
/// has more than twenty cards, and turns on as soon as one drops to twenty —
/// the draw's library→hand move re-derives the layer-7c bonus through the
/// engine's own layer flush.
#[test]
fn isleback_spawn_bonus_follows_library_sizes() {
    let mut draw = None;
    let (mut runner, spawn) = isleback_board(&[21, 21], |scenario| {
        draw = Some(free_instant(scenario, "Draw One", DRAW_ONE));
    });
    assert_eq!(power_toughness(&runner, spawn), (Some(4), Some(8)));

    runner.cast(draw.expect("draw helper staged")).resolve();
    assert_eq!(library_sizes(&runner), [20, 21], "reach-guard: P0 drew");
    assert_eq!(
        power_toughness(&runner, spawn),
        (Some(8), Some(16)),
        "a library at twenty cards turns the +4/+8 on"
    );
}

/// CR 400.1 + CR 611.3a: any one library qualifies, each library is checked
/// on its own, and the bonus applies once however many libraries qualify.
#[test]
fn isleback_spawn_bonus_checks_each_library_and_applies_once() {
    for libraries in [[30, 10], [15, 15], [10, 10]] {
        let (runner, spawn) = isleback_board(&libraries, |_| {});
        assert_eq!(
            power_toughness(&runner, spawn),
            (Some(8), Some(16)),
            "libraries {libraries:?}"
        );
    }
}

/// CR 800.4a: a player who left the game takes their library with them — an
/// eliminated player's now-empty library does not count as "a library".
#[test]
fn isleback_spawn_ignores_a_departed_players_library() {
    let mut draw = None;
    let (mut runner, spawn) = isleback_board(&[21, 21, 21], |scenario| {
        draw = Some(free_instant(scenario, "Draw One", DRAW_ONE));
    });
    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("conceding is always legal");
    let departed = &runner.state().players[2];
    assert!(departed.is_eliminated, "reach-guard: P2 left the game");
    assert!(
        departed.library.is_empty(),
        "reach-guard: P2's library left with them"
    );
    evaluate_layers(runner.state_mut());
    assert_eq!(
        power_toughness(&runner, spawn),
        (Some(4), Some(8)),
        "a departed player's empty library is not a library in the game"
    );

    // Positive control in the same game: a remaining player's library at
    // twenty cards still turns the bonus on.
    runner.cast(draw.expect("draw helper staged")).resolve();
    assert_eq!(library_sizes(&runner), [20, 21, 0], "reach-guard: P0 drew");
    assert_eq!(power_toughness(&runner, spawn), (Some(8), Some(16)));
}

/// Howltooth Hollow with the given per-player hand sizes at activation (the
/// land itself has already left P0's hand).
fn activate_howltooth(hands: &[usize]) -> (HideawayBoard, bool) {
    let libraries = vec![8; hands.len()];
    let mut board = hideaway_land_board(
        "Howltooth Hollow",
        HOWLTOOTH_HOLLOW,
        &libraries,
        hands,
        |_| {},
    );
    fund(&mut board.runner, ManaType::Black);
    assert_eq!(hand_sizes(&board.runner), hands, "reach-guard");
    let offered = activate_play_ability(&mut board.runner, board.land);
    (board, offered)
}

/// CR 102.1 + CR 402.3 + CR 608.2c: "each player has no cards in hand" is
/// false while an opponent holds a card.
#[test]
fn howltooth_hollow_offers_nothing_while_an_opponent_holds_a_card() {
    let (board, offered) = activate_howltooth(&[0, 1]);
    assert_resolved_without_offer(&board, offered);
}

/// CR 102.1 + CR 608.2c: with every hand empty on resolution the offer appears.
#[test]
fn howltooth_hollow_offers_play_when_every_hand_is_empty() {
    let (board, offered) = activate_howltooth(&[0, 0]);
    assert_resolved_with_offer(&board, offered);
}

/// CR 102.1: "each player" includes the controller.
#[test]
fn howltooth_hollow_offers_nothing_while_its_controller_holds_a_card() {
    let (board, offered) = activate_howltooth(&[1, 0]);
    assert_resolved_without_offer(&board, offered);
}

/// CR 102.1: "each player" spans every player in a multiplayer game.
#[test]
fn howltooth_hollow_offers_nothing_while_any_one_of_three_players_holds_a_card() {
    let (board, offered) = activate_howltooth(&[0, 0, 1]);
    assert_resolved_without_offer(&board, offered);
}
