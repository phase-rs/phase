//! CR 401.4 + CR 730.3 + CR 730.3a: an owner arranges the PHYSICAL ARRIVALS put
//! into one library position at the same time, and only the owner learns the
//! order they chose.
//!
//! Three properties are pinned here:
//!   * a still-merged permanent contributes EVERY card it becomes (CR 730.3) to
//!     the arrangement prompt, so one announced target id plus a same-owner
//!     sibling yields three arriving cards (Riptide Gearhulk);
//!   * the delivery sequence every player can observe does not depend on the
//!     submitted order — the order is applied privately to the hidden library
//!     slots (CR 401.4: a library's owner does not reveal the order of cards
//!     they put into their library; CR 730.3a is the component-arrange
//!     authority);
//!   * a `Shuffle` anywhere later in the chain suppresses the arrangement
//!     (CR 701.24a), including Elixir of Immortality's stage → move graveyard →
//!     shuffle shape whose shuffle is not the immediate child.
//!
//! Every card here is driven through the production path over its verbatim
//! Oracle text, checked against `data/mtgjson/AtomicCards.json`.

use engine::game::merge::{merge_object_onto, MergeSide};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::game::visibility::filter_events_for_viewer;
use engine::types::ability::LibraryPosition;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Riptide Gearhulk's real Oracle text (verified against
// data/mtgjson/AtomicCards.json).
const RIPTIDE_GEARHULK_ORACLE: &str = "Double strike\n\
     Prowess (Whenever you cast a noncreature spell, this creature gets +1/+1 until end of \
     turn.)\n\
     When this creature enters, for each opponent, put up to one target nonland permanent that \
     player controls into its owner's library third from the top.";
// Elixir of Immortality's real Oracle text (verified against
// data/mtgjson/AtomicCards.json).
const ELIXIR: &str = "{2}, {T}: You gain 5 life. Shuffle this artifact and your graveyard into \
     their owner's library.";
// Deem Inferior's real Oracle text (verified against
// data/mtgjson/AtomicCards.json).
const DEEM_INFERIOR: &str = "This spell costs {1} less to cast for each card you've drawn this \
     turn.\nThe owner of target nonland permanent puts it into their library second from the top \
     or on the bottom.";

const P2: PlayerId = PlayerId(2);

/// An unseated viewer: not a seat, so the event filter treats it as an audience.
const SPECTATOR: PlayerId = PlayerId(99);

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn library(runner: &GameRunner, player: PlayerId) -> Vec<ObjectId> {
    runner.state().players[player.0 as usize]
        .library
        .iter()
        .copied()
        .collect()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// The one event of a resolution batch: the events it emitted and the state the
/// viewer would hold afterwards (the transport pair the visibility filter takes).
struct Step {
    events: Vec<GameEvent>,
    state: GameState,
}

fn projected(steps: &[Step], viewer: PlayerId) -> Vec<serde_json::Value> {
    steps
        .iter()
        .flat_map(|step| {
            filter_events_for_viewer(&step.events, &step.state, viewer)
                .into_iter()
                .map(|event| serde_json::to_value(event).expect("game events serialize"))
        })
        .collect()
}

struct RiptideBoard {
    runner: GameRunner,
    riptide: ObjectId,
    /// The survivor of a two-card merged permanent P1 owns and controls.
    pile: ObjectId,
    /// The absorbed component of `pile` (CR 730.3), also owned by P1.
    component: ObjectId,
    /// A permanent P1 owns but P2 controls: Riptide's P2 target, delivered into
    /// P1's library alongside the pile's two cards.
    borrowed: ObjectId,
    p1_library: Vec<ObjectId>,
}

fn riptide_board() -> RiptideBoard {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        [
            mana(ManaType::Colorless, 1),
            mana(ManaType::White, 2),
            mana(ManaType::Blue, 2),
        ]
        .concat(),
    );
    scenario.with_library_top(P1, &["P1 Library 1", "P1 Library 2", "P1 Library 3"]);
    let pile = scenario.add_creature(P1, "P1 Host", 3, 3).id();
    let component = scenario.add_creature(P1, "P1 Component", 2, 2).id();
    let borrowed = scenario
        .add_creature(P1, "Borrowed Bear", 2, 2)
        .controlled_by(P2)
        .id();
    let riptide = scenario
        .add_creature_to_hand_from_oracle(P0, "Riptide Gearhulk", 2, 5, RIPTIDE_GEARHULK_ORACLE)
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        merge_object_onto(state, component, pile, MergeSide::Top, &mut events);
    }
    assert!(
        runner.state().objects[&pile].merged_components.len() == 2,
        "fixture: the pile is two physical cards"
    );
    RiptideBoard {
        p1_library: library(&runner, P1),
        runner,
        riptide,
        pile,
        component,
        borrowed,
    }
}

/// Cast Riptide Gearhulk targeting the merged pile (P1's target) and the
/// borrowed bear (P2's target), and stop at P1's arrangement prompt.
fn open_arrangement(board: &mut RiptideBoard) -> Vec<ObjectId> {
    let outcome = board
        .runner
        .cast(board.riptide)
        .target_players(&[P1, P2])
        .target_objects(&[board.pile, board.borrowed])
        .resolve();
    match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            count,
            effect_kind,
            library_position,
            ..
        } => {
            assert_eq!(*player, P1, "the cards' owner arranges them (CR 401.4)");
            assert_eq!(
                *effect_kind,
                engine::types::ability::EffectKind::PutAtLibraryPosition
            );
            assert_eq!(*count, cards.len());
            assert_eq!(
                *library_position,
                Some(LibraryPosition::NthFromTop { n: 3 })
            );
            cards.clone()
        }
        other => panic!("expected P1's arrangement prompt, got {other:?}"),
    }
}

/// Finish the resolution after the arrangement answer, draining triggers.
fn resolve_to_priority(runner: &mut GameRunner) {
    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    assert!(
                        runner.state().pending_mass_library_order_choice.is_none(),
                        "the arrangement carrier is consumed by its completion"
                    );
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass finishes the placement");
            }
            other => panic!("unexpected prompt after the arrangement: {other:?}"),
        }
    }
    panic!("the arrangement did not settle within the safety bound");
}

/// CR 730.3 + CR 730.3c + CR 401.4: Riptide's prompt must name the three cards
/// that actually arrive — the merged pile's two physical cards plus the other
/// permanent whose owner is the same — not the two announced target ids.
///
/// REVERT-FAILING: the prompt built from announced ids lists two cards, and the
/// absorbed component is delivered in the merge primitive's fixed order.
#[test]
fn riptide_mixed_merged_pile_and_sibling_prompt_names_every_arrival() {
    let mut board = riptide_board();
    let offered = open_arrangement(&mut board);
    assert_eq!(
        {
            let mut sorted = offered.clone();
            sorted.sort_by_key(|id| id.0);
            sorted
        },
        {
            let mut expected = vec![board.pile, board.component, board.borrowed];
            expected.sort_by_key(|id| id.0);
            expected
        },
        "the prompt lists the pile's physical cards and the sibling permanent"
    );
    assert_eq!(zone(&board.runner, board.pile), Zone::Battlefield);
    assert_eq!(zone(&board.runner, board.borrowed), Zone::Battlefield);

    let chosen = vec![board.borrowed, board.component, board.pile];
    board
        .runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("P1 submits their arrangement");
    resolve_to_priority(&mut board.runner);

    for id in [board.pile, board.component, board.borrowed] {
        assert_eq!(
            zone(&board.runner, id),
            Zone::Library,
            "every physical arrival is placed"
        );
    }
    // CR 401.4: the arrangement UI submits top to bottom, and the fixed-depth
    // block keeps that submitted order from index `n - 1`.
    let f = &board.p1_library;
    assert_eq!(
        library(&board.runner, P1),
        vec![f[0], f[1], chosen[0], chosen[1], chosen[2], f[2]],
        "all three arriving cards take the owner's order at the announced position"
    );
}

/// CR 401.4 + CR 730.3a: the opponent's and the spectator's observations are
/// identical for BOTH submitted orders, while the owner's library holds the
/// order that owner chose.
///
/// REVERT-FAILING: the earlier seam wrote the submitted permutation into
/// `merged_components`, so the public `ZoneChanged` sequence (and thus every
/// viewer's log) followed the owner's private choice.
#[test]
fn merged_pile_arrangement_does_not_reveal_the_owners_order() {
    /// One complete run: cast, answer the prompt, settle, keeping the transport
    /// pair after every action.
    struct Run {
        steps: Vec<Step>,
        offered: Vec<ObjectId>,
        chosen: Vec<ObjectId>,
        library: Vec<ObjectId>,
        state: GameState,
        pile: ObjectId,
        component: ObjectId,
        borrowed: ObjectId,
    }

    fn run(reverse: bool) -> Run {
        let mut board = riptide_board();
        let mut steps = Vec::new();
        let outcome = board
            .runner
            .cast(board.riptide)
            .target_players(&[P1, P2])
            .target_objects(&[board.pile, board.borrowed])
            .resolve();
        let offered = match outcome.final_waiting_for() {
            WaitingFor::EffectZoneChoice { cards, .. } => cards.clone(),
            other => panic!("expected P1's arrangement prompt, got {other:?}"),
        };
        steps.push(Step {
            events: outcome.events().to_vec(),
            state: outcome.state().clone(),
        });
        let mut chosen = offered.clone();
        if reverse {
            chosen.reverse();
        }
        let answer = board
            .runner
            .act(GameAction::SelectCards {
                cards: chosen.clone(),
            })
            .expect("P1 submits their arrangement");
        steps.push(Step {
            events: answer.events,
            state: board.runner.state().clone(),
        });
        for _ in 0..64 {
            match &board.runner.state().waiting_for {
                WaitingFor::OrderTriggers { .. } => {
                    drain_order_triggers_with_identity(board.runner.state_mut());
                }
                WaitingFor::Priority { .. } => {
                    if board.runner.state().stack.is_empty() {
                        break;
                    }
                    let pass = board
                        .runner
                        .act(GameAction::PassPriority)
                        .expect("priority pass settles the trigger");
                    steps.push(Step {
                        events: pass.events,
                        state: board.runner.state().clone(),
                    });
                }
                other => panic!("unexpected prompt while settling: {other:?}"),
            }
        }
        Run {
            steps,
            offered,
            chosen,
            library: library(&board.runner, P1),
            state: board.runner.state().clone(),
            pile: board.pile,
            component: board.component,
            borrowed: board.borrowed,
        }
    }

    let forward = run(false);
    let reverse = run(true);
    assert_eq!(
        forward.offered, reverse.offered,
        "reach guard: both runs offer the same three arrivals"
    );
    assert_eq!(forward.offered.len(), 3);
    assert_ne!(
        forward.chosen, reverse.chosen,
        "reach guard: the two runs submit different orders"
    );
    assert!(
        forward.offered.contains(&forward.pile)
            && forward.offered.contains(&forward.component)
            && forward.offered.contains(&forward.borrowed),
        "reach guard: the merged pile's cards and the sibling are all offered"
    );

    // The submitted order is the owner's private knowledge: every other
    // audience sees exactly the same thing either way.
    let opponent_forward = projected(&forward.steps, P2);
    let opponent_reverse = projected(&reverse.steps, P2);
    assert_eq!(
        opponent_forward, opponent_reverse,
        "an opponent's observations must not depend on the owner's chosen order"
    );
    assert_eq!(
        projected(&forward.steps, SPECTATOR),
        projected(&reverse.steps, SPECTATOR),
        "a spectator's observations must not depend on the owner's chosen order"
    );
    // ... and no viewer is told more than another about the delivery itself.
    // (The cast action is excluded: `SpellCast` carries a pre-existing
    // spectator-only card-id redaction that is unrelated to this ordering.)
    for (name, run) in [("forward", &forward), ("reverse", &reverse)] {
        let delivery = &run.steps[1..];
        assert_eq!(
            projected(delivery, P2),
            projected(delivery, SPECTATOR),
            "{name}: the opponent and the spectator receive the same delivery sequence"
        );
        assert_eq!(
            projected(delivery, P2),
            projected(delivery, P0),
            "{name}: the caster (not the owner) learns no more than an opponent"
        );
    }

    // No other viewer's projected STATE locates the choice either: a library's
    // hidden entries are projected in a canonical order for an audience without
    // private access (CR 401.2), so the serialized view cannot differ between the
    // two permutations even though the ids are public.
    for viewer in [P0, P2, SPECTATOR] {
        assert_eq!(
            serde_json::to_value(engine::game::visibility::filter_state_for_viewer(
                &forward.state,
                viewer
            ))
            .expect("projected state serializes"),
            serde_json::to_value(engine::game::visibility::filter_state_for_viewer(
                &reverse.state,
                viewer
            ))
            .expect("projected state serializes"),
            "the projected state for {viewer:?} must not encode the owner's order"
        );
    }

    // CR 401.2: no serialized view reveals a library's order — including the
    // library owner's and the arranger's. The chooser's knowledge is the
    // submission they made, not a state projection, so opposite permutations are
    // indistinguishable to every viewer.
    for viewer in [P0, P1, P2, SPECTATOR] {
        assert_eq!(
            serde_json::to_value(engine::game::visibility::filter_state_for_viewer(
                &forward.state,
                viewer
            ))
            .expect("projected state serializes"),
            serde_json::to_value(engine::game::visibility::filter_state_for_viewer(
                &reverse.state,
                viewer
            ))
            .expect("projected state serializes"),
            "{viewer:?}: no projection reveals the private library order"
        );
    }

    // The owner's library differs exactly as chosen.
    assert_ne!(
        forward.library, reverse.library,
        "the owner's own library holds the order they submitted"
    );
    assert_eq!(
        &forward.library[2..5],
        &forward.chosen[..3],
        "the submitted order reaches the owner's library"
    );
    assert_eq!(
        &reverse.library[2..5],
        &reverse.chosen[..3],
        "the submitted order reaches the owner's library"
    );
}

/// CR 701.24a + CR 401.4: a `Shuffle` later in the chain suppresses the owner
/// arrangement, even when intervening instructions separate it from the
/// placement. Elixir of Immortality stages its artifact, moves the graveyard,
/// and only then shuffles, so the staged placement is not a position anyone can
/// order.
///
/// REVERT-FAILING: the immediate-child check misses the two-hop shuffle, so a
/// merged Elixir opened an arrangement prompt that the terminal shuffle then
/// erased.
#[test]
fn merged_elixir_shuffle_chain_opens_no_arrangement() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let elixir = scenario
        .add_artifact_from_oracle(P0, "Elixir of Immortality", ELIXIR)
        .id();
    let component = scenario
        .add_artifact_from_oracle(P0, "Component Relic", "")
        .id();
    let mine_a = scenario.add_creature_to_graveyard(P0, "Mine A", 1, 1).id();
    let mine_b = scenario.add_creature_to_graveyard(P0, "Mine B", 2, 2).id();
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 2));
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        // CR 730.2a: the Elixir stays the topmost component, so the merged
        // artifact still has the activated ability.
        merge_object_onto(state, component, elixir, MergeSide::Bottom, &mut events);
    }
    assert_eq!(
        runner.state().objects[&elixir].merged_components,
        vec![elixir, component],
        "fixture: the source is a two-card merged artifact"
    );

    let outcome = runner.activate(elixir, 0).resolve();
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::EffectZoneChoice { .. }
        ),
        "CR 701.24a: a chained shuffle is not an owner arrangement, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        runner.state().pending_mass_library_order_choice.is_none(),
        "no arrangement carrier survives the chained shuffle"
    );
    for id in [elixir, component, mine_a, mine_b] {
        assert_eq!(
            zone(&runner, id),
            Zone::Library,
            "the staged artifact, its component, and the graveyard end in the library"
        );
    }
    let shuffled = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::Shuffle,
                    ..
                }
            )
        })
        .count();
    assert_eq!(shuffled, 1, "exactly one terminal library shuffle");

    // CR 701.24a: the child's `EffectResolved` is not a shuffle — the library is
    // randomized by the shuffle ACTION. Count that authority, as the Alabaster
    // witness does, and require it after the last physical arrival: the staged
    // artifact, its component and the graveyard cards are all in the library
    // before the owner's one shuffle randomizes it.
    let shuffle_actions: Vec<usize> = outcome
        .events()
        .iter()
        .enumerate()
        .filter_map(|(index, event)| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: engine::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            } if *player_id == P0 => Some(index),
            _ => None,
        })
        .collect();
    let last_arrival = outcome
        .events()
        .iter()
        .rposition(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Library,
                    ..
                } if [elixir, component, mine_a, mine_b].contains(object_id)
            )
        })
        .expect("the staged cards arrive in the library");
    assert_eq!(
        shuffle_actions.len(),
        1,
        "the owner shuffles their library exactly once for the whole chain"
    );
    assert!(
        shuffle_actions[0] > last_arrival,
        "the automatic delivery shuffle must follow the last physical arrival \
         (shuffle at {}, last arrival at {last_arrival})",
        shuffle_actions[0]
    );
    assert!(
        !outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: engine::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            } if *player_id != P0
        )),
        "no other player's library is shuffled"
    );
}

/// CR 401.4 + CR 401.7: the shared placement authority also covers `BeneathTop`.
/// A still-merged permanent targeted by Unexpectedly Absent ("just beneath the
/// top X cards") delivers both of its cards at the resolved depth, and their
/// owner orders them there.
///
/// REVERT-FAILING: `BeneathTop` was excluded from arrangement outright, so the
/// prompt never opened and the pile's cards arrived in the merge stack's order.
#[test]
fn merged_pile_beneath_top_owner_orders_the_physical_cards() {
    use engine::types::ability::QuantityExpr;
    use engine::types::mana::{ManaCost, ManaCostShard};

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P1,
        &[
            "P1 Library 1",
            "P1 Library 2",
            "P1 Library 3",
            "P1 Library 4",
        ],
    );
    let pile = scenario.add_creature(P1, "P1 Host", 3, 3).id();
    let component = scenario.add_creature(P1, "P1 Component", 2, 2).id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Unexpectedly Absent",
        true,
        "Put target nonland permanent into its owner's library just beneath the top X cards of \
         that library.",
    );
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::White, ManaCostShard::White],
        generic: 0,
    });
    let spell = builder.id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        merge_object_onto(state, component, pile, MergeSide::Top, &mut events);
    }
    let library_before = library(&runner, P1);
    let mana_pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    for unit in [mana(ManaType::White, 2), mana(ManaType::Colorless, 2)].concat() {
        mana_pool.add(unit);
    }

    let outcome = runner.cast(spell).x(2).target_object(pile).resolve();
    let offered = match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            library_position,
            ..
        } => {
            assert_eq!(*player, P1, "the cards' owner arranges them (CR 401.4)");
            assert_eq!(
                *library_position,
                Some(LibraryPosition::BeneathTop {
                    depth: QuantityExpr::Fixed { value: 2 }
                }),
                "the resolved depth is carried into the prompt"
            );
            cards.clone()
        }
        other => {
            panic!("expected P1's arrangement prompt beneath the top two cards, got {other:?}")
        }
    };
    assert_eq!(offered.len(), 2);
    assert!(offered.contains(&pile) && offered.contains(&component));

    let mut chosen = offered.clone();
    chosen.reverse();
    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("P1 submits their arrangement");
    resolve_to_priority(&mut runner);

    let mut expected = library_before.clone();
    expected.insert(2, chosen[0]);
    expected.insert(3, chosen[1]);
    assert_eq!(
        library(&runner, P1),
        expected,
        "CR 401.7: both cards land beneath exactly the top two, in the owner's order"
    );
    assert_eq!(zone(&runner, pile), Zone::Library);
    assert_eq!(zone(&runner, component), Zone::Library);
}

/// CR 401.7 + CR 401.4: a block of cards put "just beneath the top X cards" of a
/// library holding fewer than X cards goes to the BOTTOM as one intact block.
/// The clamp is measured once, against the pre-insertion library, not per card
/// against a destination that grows with each earlier card.
///
/// REVERT-FAILING: the per-card replay clamped each insertion separately, so two
/// cards beneath depth 3 in a one-card library landed reversed — `[existing, B,
/// A]` instead of the chosen `[existing, A, B]`.
#[test]
fn short_library_beneath_top_order_is_the_chosen_block() {
    use engine::types::ability::QuantityExpr;
    use engine::types::mana::{ManaCost, ManaCostShard};

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 Library 1"]);
    let pile = scenario.add_creature(P1, "P1 Host", 3, 3).id();
    let component = scenario.add_creature(P1, "P1 Component", 2, 2).id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Unexpectedly Absent",
        true,
        "Put target nonland permanent into its owner's library just beneath the top X cards of \
         that library.",
    );
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::White, ManaCostShard::White],
        generic: 0,
    });
    let spell = builder.id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        merge_object_onto(state, component, pile, MergeSide::Top, &mut events);
    }
    let filler = library(&runner, P1)[0];
    let mana_pool = &mut runner.state_mut().players[P0.0 as usize].mana_pool;
    for unit in [mana(ManaType::White, 2), mana(ManaType::Colorless, 3)].concat() {
        mana_pool.add(unit);
    }

    // CR 401.7: X=3 into a one-card library — fewer than N cards means the bottom.
    let outcome = runner.cast(spell).x(3).target_object(pile).resolve();
    let offered = match outcome.final_waiting_for() {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            library_position,
            ..
        } => {
            assert_eq!(*player, P1, "the cards' owner arranges them (CR 401.4)");
            assert_eq!(
                *library_position,
                Some(LibraryPosition::BeneathTop {
                    depth: QuantityExpr::Fixed { value: 3 }
                })
            );
            cards.clone()
        }
        other => panic!("expected P1's arrangement prompt, got {other:?}"),
    };
    assert_eq!(offered.len(), 2);

    let mut chosen = offered.clone();
    chosen.reverse();
    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("P1 submits their arrangement");
    resolve_to_priority(&mut runner);

    assert_eq!(
        library(&runner, P1),
        vec![filler, chosen[0], chosen[1]],
        "CR 401.7: the whole chosen block falls to the bottom, in the submitted order"
    );
    assert_eq!(zone(&runner, pile), Zone::Library);
    assert_eq!(zone(&runner, component), Zone::Library);
}

/// CR 730.3 + CR 730.3c + CR 401.4 + CR 608.2d: Deem Inferior's owner chooses
/// "second from the top" or "on the bottom", and a still-merged target then
/// delivers BOTH of its cards into that one position. They arrive at the same
/// time, so their owner orders them there: the choice must reach the owner as an
/// arrangement prompt, and the order they submit must land intact.
///
/// REVERT-FAILING: the answer delivered the single announced target id straight
/// into the library, so the pile's two cards arrived in the merge stack's order
/// and the owner was never offered their permutation.
#[test]
fn deem_inferior_top_branch_offers_the_merged_pile_to_its_owner() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 Library 1", "P1 Library 2", "P1 Library 3"]);
    let pile = scenario.add_creature(P1, "P1 Host", 3, 3).id();
    let component = scenario.add_creature(P1, "P1 Component", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Deem Inferior", false, DEEM_INFERIOR)
        .id();
    scenario.with_mana_pool(
        P0,
        [mana(ManaType::Blue, 1), mana(ManaType::Colorless, 3)].concat(),
    );
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        merge_object_onto(state, component, pile, MergeSide::Top, &mut events);
    }
    let fillers = library(&runner, P1);
    assert_eq!(fillers.len(), 3);

    runner.cast(spell).target_objects(&[pile]).commit();
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TopOrBottomChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Deem Inferior");
    }
    match &runner.state().waiting_for {
        WaitingFor::TopOrBottomChoice {
            player,
            object_id,
            top_position,
            ..
        } => {
            assert_eq!(*player, P1, "CR 608.2d: the permanent's owner chooses");
            assert_eq!(*object_id, pile);
            assert_eq!(*top_position, LibraryPosition::NthFromTop { n: 2 });
        }
        other => panic!("expected Deem Inferior's owner choice, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTopOrBottom { top: true })
        .expect("P1 chooses second from the top");
    let offered = match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            library_position,
            mass_library_order,
            ..
        } => {
            assert_eq!(*player, P1, "CR 401.4: the owner orders the arriving cards");
            assert_eq!(
                *library_position,
                Some(LibraryPosition::NthFromTop { n: 2 })
            );
            assert!(
                mass_library_order.is_some(),
                "the prompt carries the frozen owner batch"
            );
            cards.clone()
        }
        other => panic!("expected P1's arrangement prompt, got {other:?}"),
    };
    assert_eq!(offered.len(), 2, "both physical cards are offered");
    assert!(
        offered.contains(&pile) && offered.contains(&component),
        "the prompt names the pile's two physical cards, not the merged id alone"
    );

    let mut chosen = offered.clone();
    chosen.reverse();
    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("P1 submits their arrangement");
    resolve_to_priority(&mut runner);

    // CR 401.4: the submitted order is top-to-bottom, placed as one intact block
    // at the announced slot (index 1 and 2).
    assert_eq!(
        library(&runner, P1),
        vec![fillers[0], chosen[0], chosen[1], fillers[1], fillers[2]],
        "the owner's permutation lands second and third from the top"
    );
    assert_eq!(zone(&runner, pile), Zone::Library);
    assert_eq!(zone(&runner, component), Zone::Library);
}
/// CR 730.3 + CR 730.3c + CR 401.4 + CR 608.2d: the BOTTOM branch of Deem
/// Inferior's answer reaches the same owner arrangement, and a still-merged target then
/// delivers BOTH of its cards into that one position. They arrive at the same
/// time, so their owner orders them there: the choice must reach the owner as an
/// arrangement prompt, and the order they submit must land intact.
///
/// REVERT-FAILING: the direct bottom answer moved the single announced id and
/// skipped the arrangement, so the pile's order was never the owner's.
#[test]
fn deem_inferior_bottom_branch_offers_the_merged_pile_to_its_owner() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 Library 1", "P1 Library 2", "P1 Library 3"]);
    let pile = scenario.add_creature(P1, "P1 Host", 3, 3).id();
    let component = scenario.add_creature(P1, "P1 Component", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Deem Inferior", false, DEEM_INFERIOR)
        .id();
    scenario.with_mana_pool(
        P0,
        [mana(ManaType::Blue, 1), mana(ManaType::Colorless, 3)].concat(),
    );
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != component);
        let mut events = Vec::new();
        merge_object_onto(state, component, pile, MergeSide::Top, &mut events);
    }
    let fillers = library(&runner, P1);
    assert_eq!(fillers.len(), 3);

    runner.cast(spell).target_objects(&[pile]).commit();
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TopOrBottomChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Deem Inferior");
    }
    match &runner.state().waiting_for {
        WaitingFor::TopOrBottomChoice {
            player,
            object_id,
            top_position,
            ..
        } => {
            assert_eq!(*player, P1, "CR 608.2d: the permanent's owner chooses");
            assert_eq!(*object_id, pile);
            assert_eq!(*top_position, LibraryPosition::NthFromTop { n: 2 });
        }
        other => panic!("expected Deem Inferior's owner choice, got {other:?}"),
    }

    runner
        .act(GameAction::ChooseTopOrBottom { top: false })
        .expect("P1 chooses the bottom");
    let offered = match &runner.state().waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            library_position,
            mass_library_order,
            ..
        } => {
            assert_eq!(*player, P1, "CR 401.4: the owner orders the arriving cards");
            assert_eq!(*library_position, Some(LibraryPosition::Bottom));
            assert!(
                mass_library_order.is_some(),
                "the prompt carries the frozen owner batch"
            );
            cards.clone()
        }
        other => panic!("expected P1's arrangement prompt, got {other:?}"),
    };
    assert_eq!(offered.len(), 2, "both physical cards are offered");
    assert!(
        offered.contains(&pile) && offered.contains(&component),
        "the prompt names the pile's two physical cards, not the merged id alone"
    );

    let mut chosen = offered.clone();
    chosen.reverse();
    runner
        .act(GameAction::SelectCards {
            cards: chosen.clone(),
        })
        .expect("P1 submits their arrangement");
    resolve_to_priority(&mut runner);

    assert_eq!(
        library(&runner, P1),
        vec![fillers[0], fillers[1], fillers[2], chosen[0], chosen[1]],
        "the owner's permutation lands as one block at the bottom"
    );
    assert_eq!(zone(&runner, pile), Zone::Library);
    assert_eq!(zone(&runner, component), Zone::Library);
}
