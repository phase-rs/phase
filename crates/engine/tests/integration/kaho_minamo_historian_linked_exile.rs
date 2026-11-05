//! Kaho, Minamo Historian — "cards exiled with Kaho" read by an activation of
//! a Kaho that left the battlefield and came back while the activation waited.
//!
//! Oracle:
//!   "When Kaho enters, search your library for up to three instant cards,
//!    exile them, then shuffle.
//!    {X}, {T}: You may cast a spell with mana value X from among cards exiled
//!    with Kaho without paying its mana cost."
//!
//! CR 113.7a: the activation exists independently of its source, so it keeps
//! acting as the Kaho that activated it after that Kaho is blinked.
//! CR 607.2a: its "cards exiled with Kaho" are the cards exiled by that Kaho's
//! linked triggered ability.
//! CR 400.7: the Kaho that returns is a new object; cards its own enters
//! trigger exiles are not linked to the old activation.

use engine::types::game_state::CastPaymentMode;
use std::collections::VecDeque;

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{CastingPermission, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLink, ExileLinkKind, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KAHO_TEXT: &str = "When Kaho enters, search your library for up to three instant cards, \
     exile them, then shuffle.\n{X}, {T}: You may cast a spell with mana value X from among cards \
     exiled with Kaho without paying its mana cost.";

const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, \
     then return those cards to the battlefield under your control.";

const GRIPTIDE: &str = "Put target creature on top of its owner's library.";

struct KahoBoard {
    runner: GameRunner,
    kaho: ObjectId,
    land: ObjectId,
    /// The instant already exiled with the original Kaho (its enters trigger).
    pile: ObjectId,
    /// Mana-value-0 instants in the library, findable by a re-entry search.
    library_instants: Vec<ObjectId>,
    flickers: Vec<ObjectId>,
    /// A zero-cost Griptide in hand.
    griptide: ObjectId,
}

/// Kaho on the battlefield with one mana-value-0 instant already exiled with
/// it (`TrackedBySource`, the link its enters trigger makes), `flickers`
/// zero-cost Ghostly Flickers and a zero-cost Griptide in hand.
fn kaho_board(flickers: usize) -> KahoBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kaho = scenario
        .add_creature_from_oracle(P0, "Kaho, Minamo Historian", 2, 2, KAHO_TEXT)
        .id();
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let pile = scenario
        .add_spell_to_exile(P0, "Exiled Instant", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let library_instants = (0..3)
        .map(|i| {
            scenario
                .add_spell_to_library_top(P0, &format!("Library Instant {i}"), true)
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    scenario.add_land_to_library_top(P0, "Library Land");
    let flickers = (0..flickers)
        .map(|i| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    &format!("Ghostly Flicker {i}"),
                    true,
                    GHOSTLY_FLICKER,
                )
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    let griptide = scenario
        .add_spell_to_hand_from_oracle(P0, "Griptide", true, GRIPTIDE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().exile_links.push(ExileLink {
        exiled_id: pile,
        source_id: kaho,
        kind: ExileLinkKind::TrackedBySource,
    });
    KahoBoard {
        runner,
        kaho,
        land,
        pile,
        library_instants,
        flickers,
        griptide,
    }
}

/// Pass priority until `done` holds, answering each `SearchChoice` with the
/// next pick in `searches` and accepting every optional effect.
fn pass_until(
    runner: &mut GameRunner,
    searches: &mut VecDeque<Vec<ObjectId>>,
    done: impl Fn(&GameState) -> bool,
) {
    for _ in 0..80 {
        if done(runner.state()) {
            return;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::SearchChoice { .. } => GameAction::SelectCards {
                cards: searches.pop_front().expect("an answer for each search"),
            },
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: true }
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            other => panic!("unexpected prompt {other:?}"),
        };
        runner.act(action).expect("prompt answer accepted");
    }
    panic!("prompt loop did not settle");
}

fn priority_with_stack(len: usize) -> impl Fn(&GameState) -> bool {
    move |state| {
        matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.len() == len
    }
}

/// Activate Kaho's `{X}, {T}` ability with X = 0 and leave it on the stack.
fn activate_kaho(runner: &mut GameRunner, kaho: ObjectId) {
    let ability_index = runner.state().objects[&kaho]
        .abilities
        .iter()
        .position(|def| matches!(&*def.effect, Effect::CastFromZone { .. }))
        .expect("Kaho has a cast-from-exile activated ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: kaho,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Kaho");
    runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("announce X = 0");
    while matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
        runner
            .act(GameAction::PassPriority)
            .expect("finish paying {0}");
    }
    assert!(
        priority_with_stack(1)(runner.state()),
        "the activation waits on the stack; waiting_for = {}",
        runner.waiting_for_kind()
    );
}

/// With the activation waiting, cast a Ghostly Flicker on Kaho and the land
/// and resolve it; the returned Kaho's search takes `pick`.
fn flicker_kaho(board: &mut KahoBoard, flicker: ObjectId, pick: Vec<ObjectId>) {
    board
        .runner
        .cast(flicker)
        .target_objects(&[board.kaho, board.land])
        .commit();
    pass_until(
        &mut board.runner,
        &mut VecDeque::from([pick]),
        priority_with_stack(1),
    );
    assert_eq!(
        board.runner.state().objects[&flicker].zone,
        Zone::Graveyard,
        "the flicker resolved"
    );
}

fn has_free_cast(state: &GameState, card: ObjectId) -> bool {
    state.objects[&card]
        .casting_permissions
        .iter()
        .any(|permission| {
            matches!(
                permission,
                CastingPermission::ExileWithAltCost { cost, .. } if cost.is_without_paying_mana()
            )
        })
}

fn has_live_link(state: &GameState, kaho: ObjectId) -> bool {
    state.exile_links.iter().any(|link| link.source_id == kaho)
}

/// CR 113.7a + CR 607.2a: an activation of Kaho, resolving after Kaho was
/// blinked in response, still casts from the cards that Kaho exiled.
#[test]
fn flickered_kaho_activation_still_casts_from_its_own_exile() {
    let mut board = kaho_board(1);
    let kaho = board.kaho;
    let incarnation_before = board.runner.state().objects[&kaho].incarnation;
    activate_kaho(&mut board.runner, kaho);
    let flicker = board.flickers[0];
    flicker_kaho(&mut board, flicker, vec![]);

    let state = board.runner.state();
    assert_ne!(
        state.objects[&kaho].incarnation, incarnation_before,
        "Kaho came back as a new object"
    );
    assert!(
        state.linked_exile_lki.contains_key(&kaho),
        "the departure recorded Kaho's linked exiles"
    );
    assert!(
        !has_live_link(state, kaho),
        "re-entry pruned the live link, so only the departure record can answer"
    );

    pass_until(
        &mut board.runner,
        &mut VecDeque::new(),
        priority_with_stack(0),
    );
    assert!(
        has_free_cast(board.runner.state(), board.pile),
        "the old activation reads the pile of the Kaho that activated it"
    );
}

/// CR 113.7a + CR 607.2a + CR 400.7: Kaho blinked once while its activation
/// waits, and the returned Kaho's enters trigger exiles a new instant. That
/// instant is exiled with the returned Kaho, a new object, so the activation
/// still casts only from the activating Kaho's pile.
#[test]
fn kaho_activation_ignores_the_returned_kahos_new_exile() {
    let mut board = kaho_board(1);
    let kaho = board.kaho;
    let new_exile = board.library_instants[0];
    activate_kaho(&mut board.runner, kaho);
    let flicker = board.flickers[0];
    flicker_kaho(&mut board, flicker, vec![new_exile]);

    let state = board.runner.state();
    assert_eq!(
        state.objects[&new_exile].zone,
        Zone::Exile,
        "the returned Kaho's enters trigger exiled the new instant"
    );
    assert!(
        state
            .exile_links
            .iter()
            .any(|link| link.exiled_id == new_exile
                && link.source_id == kaho
                && link.kind == ExileLinkKind::TrackedBySource),
        "the new instant is a live link of the returned Kaho"
    );

    pass_until(
        &mut board.runner,
        &mut VecDeque::new(),
        priority_with_stack(0),
    );
    let state = board.runner.state();
    assert!(
        has_free_cast(state, board.pile),
        "the activation casts from the activating Kaho's pile"
    );
    assert!(
        !has_free_cast(state, new_exile),
        "the returned Kaho's exile is not exiled with the activating Kaho"
    );
}

/// CR 113.7a + CR 607.2a + CR 400.7: Kaho blinked twice while its activation
/// waits. The first return exiles a new instant; the second return exiles
/// nothing. The activation still reads only the first Kaho's pile.
#[test]
fn second_blink_does_not_hide_the_first_kahos_exile() {
    let mut board = kaho_board(2);
    let kaho = board.kaho;
    let new_exile = board.library_instants[0];
    activate_kaho(&mut board.runner, kaho);

    let first = board.flickers[0];
    flicker_kaho(&mut board, first, vec![new_exile]);
    let incarnation_after_first = board.runner.state().objects[&kaho].incarnation;
    assert_eq!(
        board.runner.state().objects[&new_exile].zone,
        Zone::Exile,
        "the returned Kaho's enters trigger exiled the new instant"
    );

    let second = board.flickers[1];
    flicker_kaho(&mut board, second, vec![]);
    let state = board.runner.state();
    assert!(
        state.objects[&kaho].incarnation > incarnation_after_first,
        "Kaho came back a second time"
    );
    assert_eq!(state.objects[&new_exile].zone, Zone::Exile);
    assert!(
        !has_live_link(state, kaho),
        "no live link remains, so only the departure records can answer"
    );

    pass_until(
        &mut board.runner,
        &mut VecDeque::new(),
        priority_with_stack(0),
    );
    let state = board.runner.state();
    assert!(
        has_free_cast(state, board.pile),
        "the activation casts from the activating Kaho's pile"
    );
    assert!(
        !has_free_cast(state, new_exile),
        "an instant a later Kaho exiled is not exiled with the activating Kaho"
    );
}

/// CR 113.7a + CR 607.2a: Kaho put on top of its owner's library while its
/// activation waits. The activation still casts from the cards that Kaho
/// exiled.
#[test]
fn kaho_put_into_its_library_in_response_still_casts_from_its_exile() {
    let mut board = kaho_board(0);
    let kaho = board.kaho;
    activate_kaho(&mut board.runner, kaho);
    board
        .runner
        .cast(board.griptide)
        .target_objects(&[kaho])
        .commit();
    pass_until(
        &mut board.runner,
        &mut VecDeque::new(),
        priority_with_stack(1),
    );

    let state = board.runner.state();
    assert_eq!(
        state.objects[&board.griptide].zone,
        Zone::Graveyard,
        "Griptide resolved"
    );
    assert_eq!(
        state.library_of(P0).front(),
        Some(&kaho),
        "Kaho is on top of its owner's library"
    );
    assert!(
        !has_live_link(state, kaho),
        "leaving the battlefield pruned the live link"
    );

    pass_until(
        &mut board.runner,
        &mut VecDeque::new(),
        priority_with_stack(0),
    );
    assert!(
        has_free_cast(board.runner.state(), board.pile),
        "the activation reads the pile of the Kaho that activated it"
    );
}
