//! Regression: GitHub issue #524 — Devour Intellect's "instead" rider never
//! fires.
//!
//! Devour Intellect is a black sorcery:
//!   "Target opponent discards a card. If mana from a Treasure was spent to
//!    cast this spell, instead that player reveals their hand, you choose a
//!    nonland card from it, then that player discards that card."
//!
//! The `ConditionInstead` rider is gated by a `QuantityCheck` over
//! `QuantityRef::ManaSpentToCast`. The parser hardcoded `scope:
//! TriggeringSpell`; for a resolving sorcery referring to "this spell" that
//! scope resolves against `state.current_trigger_event`, which is `None`
//! during ordinary spell resolution — so the comparison was always `0 > 0`
//! (false) and the upgraded effect never ran.
//!
//! The fix makes the subject anaphora ("this spell" → `SelfObject`, "that
//! spell" → `TriggeringSpell`) drive the scope at parse time (CR 400.7d).
//!
//! Discriminating signal: the upgraded ("instead") branch runs
//! `Effect::RevealHand`, which sets `WaitingFor::RevealChoice` and marks the
//! opponent's cards as revealed. The base branch runs `Effect::Discard` and
//! never reveals. We assert on `RevealChoice` / `revealed_cards` — a pure
//! shape test that hand-built the condition would not catch the
//! `current_trigger_event == None` runtime failure, so this drives the full
//! cast/resolve pipeline.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{CastOutcome, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P1: PlayerId = PlayerId(1);

use crate::support::shared_card_db as load_db;

/// Create a Treasure artifact token on `owner`'s battlefield. Devour Intellect
/// only cares about the *source snapshot* of the mana that paid for it, so the
/// token need not be a real DB card — only its `Treasure` subtype matters.
fn make_treasure(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
    owner: PlayerId,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        "Treasure".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Treasure".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Create a basic Swamp on `owner`'s battlefield (a non-Treasure mana source).
fn make_swamp(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
    owner: PlayerId,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        "Swamp".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.subtypes.push("Swamp".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

fn add_pool_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: PlayerId,
    units: &[(ManaType, ObjectId)],
) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for (mana, source) in units {
        pool.add(ManaUnit::new(*mana, *source, false, vec![]));
    }
}

/// Cast Devour Intellect, paying its `{1}{B}` cost from a pre-loaded pool whose
/// two mana units carry the source object chosen by `mana_source` (called on
/// the live state so the test can install a Treasure or a Swamp first), and
/// resolve. The cast driver auto-pays from the source-tagged pool, declares the
/// sole legal "target opponent" (P1) for the discard, and resolves — stopping
/// at the upgraded branch's `RevealChoice` boundary when it fires.
fn cast_devour_intellect(
    db: &CardDatabase,
    mana_source: impl FnOnce(&mut engine::types::game_state::GameState) -> ObjectId,
) -> CastOutcome {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell_id = scenario.add_real_card(P0, "Devour Intellect", Zone::Hand, db);
    // Opponent needs cards in hand for the discard / reveal to be observable.
    scenario.add_real_card(P1, "Grizzly Bears", Zone::Hand, db);
    scenario.add_real_card(P1, "Forest", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let source = mana_source(runner.state_mut());
    // Devour Intellect costs {1}{B}; black mana pays the generic {1} too.
    add_pool_mana(
        &mut runner,
        P0,
        &[(ManaType::Black, source), (ManaType::Black, source)],
    );

    runner.cast(spell_id).target_player(P1).resolve()
}

/// Treasure mana spent → the `ConditionInstead` rider evaluates true and the
/// upgraded `RevealHand` effect runs.
#[test]
fn devour_intellect_reveals_hand_when_treasure_mana_spent() {
    let Some(db) = load_db() else {
        return;
    };

    let outcome = cast_devour_intellect(db, |state| {
        // The mana that pays for the spell comes from a Treasure.
        make_treasure(state, 9001, P0)
    });

    // Upgraded branch: RevealHand pauses resolution on a RevealChoice for the
    // caster, and the opponent's hand cards are marked revealed.
    match outcome.final_waiting_for() {
        WaitingFor::RevealChoice { player, .. } => {
            assert_eq!(
                *player, P0,
                "the caster of Devour Intellect chooses the card to discard"
            );
        }
        other => panic!("expected WaitingFor::RevealChoice (instead branch fired), got {other:?}"),
    }
    assert!(
        !outcome.state().revealed_cards.is_empty(),
        "the instead branch must reveal the opponent's hand"
    );
}

/// No Treasure mana spent → the rider evaluates false and the base
/// `Discard` effect runs; the opponent's hand is never revealed.
#[test]
fn devour_intellect_base_discard_when_no_treasure_mana() {
    let Some(db) = load_db() else {
        return;
    };

    let outcome = cast_devour_intellect(db, |state| {
        // Mana sourced from a basic Swamp — not a Treasure.
        make_swamp(state, 9101, P0)
    });

    // Base branch: no RevealHand, so resolution never stops on RevealChoice
    // and no opponent cards are revealed.
    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::RevealChoice { .. }),
        "base branch must not enter a RevealChoice (instead branch must NOT fire), \
         waiting_for={:?}",
        outcome.final_waiting_for(),
    );
    assert!(
        outcome.state().revealed_cards.is_empty(),
        "base Discard branch must not reveal the opponent's hand"
    );
}
