//! CR 608.2c + CR 608.2n: the controller follows an instant or sorcery
//! spell's instructions in order, and the spell is put into its owner's
//! graveyard "as the final part of" its resolution — after every instruction
//! has been followed, including an instruction that paused for a player's
//! choice (CR 608.2d). A spell whose instructions pause (a chosen discard, a
//! library search) must therefore stay on the stack until that choice is made,
//! and leave it only afterwards.
//!
//! Observable consequences, each asserted below:
//! * Faithless Looting ("Draw two cards, then discard two cards.") ends up ON
//!   TOP of the two discarded cards (CR 404.2 keeps graveyard order), not
//!   beneath them, and is reported resolved only then.
//! * Doomsday ("Search your library and graveyard for five cards and exile the
//!   rest.") is not in the graveyard while its own search resolves, so "the
//!   rest" never includes Doomsday itself.
//! * A spell with no pause still reaches the graveyard as it resolves, before
//!   anyone receives priority.
//! * Every rule that sends a resolving spell somewhere other than the
//!   graveyard still applies to the deferred final part: flashback (exile),
//!   rebound (exile, with its upkeep cast), an Adventure (exile, with the
//!   creature castable), buyback (its owner's hand), and a CR 616.1 ordering of
//!   two graveyard replacements.
//! * CR 724.1b / CR 724.2b: a spell whose own end procedure exiles it is exiled
//!   at that step, and finishes resolving from exile (CR 608.2m) when a later
//!   instruction pauses.
//! * CR 800.4a: a player who leaves the game while their spell is paused takes
//!   it with them — its owner leaving exiles it (and its controller still
//!   finishes it, CR 608.2m), its controller leaving exiles it when that player
//!   still controls it — rather than the final part putting it into a
//!   graveyard. CR 608.2m: such a spell is reported resolved exactly once when
//!   a surviving player still had one of its instructions to answer; one whose
//!   departed player's question was its last live work is retired with that
//!   player and not reported resolved.
//! * The owed final part is state of the resolution carrier
//!   (`GameState::owed_spell_final_part`), never of the spell's ability, so a
//!   copy of a paused spell (CR 707.10) inherits none of it.
//! * A paused spell that its own instruction moves off the stack ("Exile
//!   Invoke Calamity", after its CR 608.2g free-cast window) is left where it
//!   was put, and is still reported resolved once.
//! * CR 603.3b: whatever triggered while the paused resolution was under way —
//!   its own instructions, the answer that finished it, and its final part's
//!   move off the stack — is put on the stack as one batch the next time a
//!   player would receive priority, above any spell still below it, whatever
//!   answer finished the resolution.
//! * CR 800.4a: a spell whose final part already ran and parked a question of
//!   its own (a CR 616.1 ordering of its own move) is still an object on the
//!   stack, and leaves the game with its departing owner.

use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::create_object;
use engine::types::ability::{CastingPermission, EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastOfferKind, CastPaymentMode, CastingVariant, GameState, OwedSpellFinalPart,
    PendingSpellResolution, PersistedGameState, PersistedRestoreError,
    PersistedRestoreFinalization, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const SHOCK: &str = "Shock deals 2 damage to any target.";

fn db() -> &'static engine::database::card_db::CardDatabase {
    load_db().expect("the committed integration card fixture loads")
}

fn mana(color: ManaType, amount: usize) -> Vec<ManaUnit> {
    (0..amount)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn graveyard(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player must exist")
        .graveyard
        .iter()
        .copied()
        .collect()
}

fn stack_resolved_count(events: &[GameEvent], spell: ObjectId) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::StackResolved { object_id } if *object_id == spell),
        )
        .count()
}

struct LootingBoard {
    runner: GameRunner,
    looting: ObjectId,
    discard_a: ObjectId,
    discard_b: ObjectId,
}

fn stage_faithless_looting() -> LootingBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let looting = scenario.add_real_card(P0, "Faithless Looting", Zone::Hand, db());
    let discard_a = scenario.add_card_to_hand(P0, "Discard A");
    let discard_b = scenario.add_card_to_hand(P0, "Discard B");
    scenario.with_library_top(P0, &["Draw One", "Draw Two", "Pad A", "Pad B"]);
    scenario.add_basic_land(P0, ManaColor::Red);
    let runner = scenario.build();
    LootingBoard {
        runner,
        looting,
        discard_a,
        discard_b,
    }
}

/// CR 608.2n + CR 404.2: the spell reaches the graveyard after the cards its
/// own discard instruction put there, so it is the top card of the graveyard.
#[test]
fn faithless_looting_is_put_into_the_graveyard_on_top_of_the_cards_it_discarded() {
    let LootingBoard {
        mut runner,
        looting,
        discard_a,
        discard_b,
    } = stage_faithless_looting();

    let outcome = runner
        .cast(looting)
        .discard(&[discard_a, discard_b])
        .resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the resolution must complete, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_zone(&[discard_a, discard_b, looting], Zone::Graveyard);
    assert_eq!(
        graveyard(outcome.state(), P0),
        vec![discard_a, discard_b, looting],
        "CR 608.2n: the spell is put into its owner's graveyard as the FINAL part of its \
         resolution, after the discard it instructs — so it sits on top of the discarded cards"
    );
    assert_eq!(
        stack_resolved_count(outcome.events(), looting),
        1,
        "the resolution is reported exactly once"
    );
}

/// CR 608.2c + CR 608.2d + CR 608.2n: while the spell's discard instruction
/// waits for the player's choice, the spell has not finished resolving and is
/// still on the stack — it is not yet a card in the graveyard, and it is not
/// yet reported resolved.
#[test]
fn a_spell_paused_on_its_own_discard_choice_is_still_on_the_stack() {
    let LootingBoard {
        mut runner,
        looting,
        discard_a,
        discard_b,
    } = stage_faithless_looting();

    let mut cast = runner.cast(looting).commit();
    let mut events_until_pause = Vec::new();
    for _ in 0..2 {
        if !matches!(cast.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        let result = cast
            .act(GameAction::PassPriority)
            .expect("passing priority resolves Faithless Looting");
        events_until_pause.extend(result.events);
    }
    assert!(
        matches!(cast.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "reach guard: the discard instruction pauses for the choice, got {:?}",
        cast.state().waiting_for
    );
    assert_eq!(
        cast.state().objects[&looting].zone,
        Zone::Stack,
        "CR 608.2c + CR 608.2n: a spell is put into the graveyard only as the final part of \
         its resolution, not before its discard is chosen"
    );
    assert!(
        !graveyard(cast.state(), P0).contains(&looting),
        "the paused spell is not a card in the graveyard"
    );
    assert_eq!(
        stack_resolved_count(&events_until_pause, looting),
        0,
        "a spell still resolving is not reported resolved"
    );

    let answered = cast
        .act(GameAction::SelectCards {
            cards: vec![discard_a, discard_b],
        })
        .expect("choosing the two discards finishes the resolution");
    assert_eq!(
        stack_resolved_count(&answered.events, looting),
        1,
        "the resolution is reported when its final part runs"
    );

    assert!(
        matches!(cast.state().waiting_for, WaitingFor::Priority { .. }),
        "the resolution completes once the choice is made, got {:?}",
        cast.state().waiting_for
    );
    assert_eq!(
        graveyard(cast.state(), P0),
        vec![discard_a, discard_b, looting],
        "CR 608.2n: the spell follows its discards into the graveyard"
    );
}

/// CR 608.2n + CR 701.23a: Doomsday is still on the stack while its own search
/// resolves, so "exile the rest" of the searched graveyard never exiles
/// Doomsday; it reaches the graveyard afterwards as the final part of its
/// resolution.
#[test]
fn doomsday_is_not_exiled_by_its_own_search_and_ends_in_the_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Library A",
            "Library B",
            "Library C",
            "Library D",
            "Library E",
            "Library F",
            "Library G",
        ],
    );
    scenario.with_graveyard(P0, &["Graveyard A", "Graveyard B", "Graveyard C"]);
    let doomsday = scenario.add_real_card(P0, "Doomsday", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 3));
    let mut runner = scenario.build();
    let searched: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, object)| {
            object.owner == P0 && matches!(object.zone, Zone::Library | Zone::Graveyard)
        })
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        searched.len(),
        10,
        "seven library cards and three graveyard cards"
    );

    let outcome = runner.cast(doomsday).search_first_legal().resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the resolution must complete, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_life_delta(P0, -10);
    assert_eq!(
        outcome.zone_of(doomsday),
        Zone::Graveyard,
        "CR 608.2n: Doomsday is put into its owner's graveyard as the final part of its \
         resolution — it was not among 'the rest' its own search exiled"
    );
    assert_eq!(
        graveyard(outcome.state(), P0),
        vec![doomsday],
        "the searched graveyard was emptied (chosen cards to the library, the rest to exile) \
         before Doomsday itself arrived"
    );
    let exiled: Vec<ObjectId> = searched
        .iter()
        .copied()
        .filter(|id| outcome.zone_of(*id) == Zone::Exile)
        .collect();
    assert_eq!(
        exiled.len(),
        5,
        "exactly the five unchosen searched cards are exiled"
    );
    let on_top: Vec<ObjectId> = outcome.state().players[P0.0 as usize]
        .library
        .iter()
        .take(5)
        .copied()
        .collect();
    assert!(
        on_top.iter().all(|id| searched.contains(id)),
        "the five chosen cards are on top of the library"
    );
}

/// CR 608.2n: a spell whose instructions never pause goes to the graveyard as it
/// resolves, before any player receives priority — the deferral is confined to
/// resolutions that actually paused.
#[test]
fn a_spell_with_no_paused_instruction_reaches_the_graveyard_as_it_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shock = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK)
        .id();
    scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();

    let outcome = runner.cast(shock).target_player(P1).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_life_delta(P1, -2);
    outcome.assert_zone(&[shock], Zone::Graveyard);
    assert_eq!(graveyard(outcome.state(), P0), vec![shock]);
    assert_eq!(stack_resolved_count(outcome.events(), shock), 1);
}

/// Pass priority and answer the resolution's questions until a player holds
/// priority over an empty stack, returning every event. `answer` takes the
/// prompts a test cares about; a discard or a search otherwise takes the
/// first cards offered.
fn run_until_settled(
    runner: &mut GameRunner,
    mut answer: impl FnMut(&GameState) -> Option<GameAction>,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..40 {
        let state = runner.state();
        let action = match answer(state) {
            Some(action) => action,
            None => match &state.waiting_for {
                WaitingFor::Priority { .. } if state.stack.is_empty() => return events,
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DiscardChoice { cards, count, .. }
                | WaitingFor::SearchChoice { cards, count, .. } => GameAction::SelectCards {
                    cards: cards.iter().copied().take(*count).collect(),
                },
                other => panic!("unexpected prompt while resolving: {other:?}"),
            },
        };
        events.extend(
            runner
                .act(action)
                .expect("the engine accepts the answer")
                .events,
        );
    }
    panic!(
        "the resolution did not settle; waiting for {:?}",
        runner.state().waiting_for
    );
}

/// The resolution is over: priority over an empty stack, no carrier left, and
/// no object stranded in the stack zone.
fn assert_settled(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the game is back at a priority window, got {:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "the stack is empty");
    assert!(
        state.resolving_stack_entry.is_none(),
        "the resolution carrier was retired"
    );
    assert!(
        state.owed_spell_final_part().is_none(),
        "the owed final part ends with its carrier"
    );
    let stranded: Vec<&str> = state
        .objects
        .values()
        .filter(|object| object.zone == Zone::Stack)
        .map(|object| object.name.as_str())
        .collect();
    assert!(
        stranded.is_empty(),
        "no object is left in the stack zone: {stranded:?}"
    );
}

/// CR 702.34a + CR 608.2n: a spell cast with flashback is exiled "instead of
/// putting it anywhere else any time it would leave the stack" — including
/// when it leaves as the deferred final part of a paused resolution.
#[test]
fn a_flashback_spell_paused_on_its_discard_is_exiled_as_its_final_part() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let looting = scenario.add_real_card(P0, "Faithless Looting", Zone::Graveyard, db());
    scenario.add_card_to_hand(P0, "Discard A");
    scenario.add_card_to_hand(P0, "Discard B");
    scenario.with_library_top(P0, &["Draw One", "Draw Two", "Pad"]);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 3));
    let mut runner = scenario.build();

    runner
        .cast(looting)
        .casting_variant(CastingVariant::Flashback)
        .commit();
    let events = run_until_settled(&mut runner, |_| None);

    let state = runner.state();
    assert_eq!(
        state.objects[&looting].zone,
        Zone::Exile,
        "CR 702.34a: the flashback spell is exiled as it leaves the stack"
    );
    assert_eq!(
        graveyard(state, P0).len(),
        2,
        "only the two discarded cards are in the graveyard"
    );
    assert_eq!(stack_resolved_count(&events, looting), 1);
    assert_settled(state);
}

/// CR 702.88a + CR 608.2n: a rebound spell cast from hand is exiled "instead
/// of putting it into your graveyard as it resolves", and its upkeep cast is
/// set up — also when its own search paused the resolution.
#[test]
fn a_rebound_spell_paused_on_its_search_is_exiled_with_its_upkeep_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let terramorph = scenario.add_real_card(P0, "Terramorph", Zone::Hand, db());
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db());
    scenario.with_mana_pool(P0, mana(ManaType::Green, 4));
    let mut runner = scenario.build();

    runner.cast(terramorph).commit();
    let events = run_until_settled(&mut runner, |_| None);

    let state = runner.state();
    assert_eq!(
        state.objects[&forest].zone,
        Zone::Battlefield,
        "reach guard: the paused search put the land onto the battlefield"
    );
    assert_eq!(
        state.objects[&terramorph].zone,
        Zone::Exile,
        "CR 702.88a: the rebound spell is exiled instead of going to the graveyard"
    );
    assert!(
        state
            .delayed_triggers
            .iter()
            .any(|trigger| trigger.source_id == terramorph),
        "CR 702.88a: the next-upkeep cast from exile is set up"
    );
    assert_eq!(stack_resolved_count(&events, terramorph), 1);
    assert_settled(state);
}

/// CR 715.3d + CR 608.2n: an Adventure spell is exiled instead of going to the
/// graveyard as it resolves, and its creature may then be cast from exile —
/// also when the Adventure's own search paused the resolution.
#[test]
fn an_adventure_paused_on_its_search_is_exiled_with_its_creature_castable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let giant = scenario.add_real_card(P0, "Beanstalk Giant", Zone::Hand, db());
    let forest = scenario.add_real_card(P0, "Forest", Zone::Library, db());
    scenario.with_mana_pool(P0, mana(ManaType::Green, 3));
    let mut runner = scenario.build();

    runner.cast(giant).adventure_face(false).commit();
    let events = run_until_settled(&mut runner, |_| None);

    let state = runner.state();
    assert_eq!(
        state.objects[&forest].zone,
        Zone::Battlefield,
        "reach guard: Fertile Footsteps' search put the land onto the battlefield"
    );
    assert_eq!(
        state.objects[&giant].zone,
        Zone::Exile,
        "CR 715.3d: the Adventure spell is exiled as it resolves"
    );
    assert!(
        state.objects[&giant]
            .casting_permissions
            .iter()
            .any(|permission| matches!(permission, CastingPermission::AdventureCreature)),
        "CR 715.3d: its controller may cast the creature from exile"
    );
    assert_eq!(stack_resolved_count(&events, giant), 1);
    assert_settled(state);
}

/// CR 702.27a + CR 608.2n: a spell whose buyback cost was paid is put into its
/// owner's hand "instead of into that player's graveyard as it resolves" —
/// also when its own search paused the resolution.
#[test]
fn a_buyback_spell_paused_on_its_search_returns_to_its_owners_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let collusion = scenario.add_real_card(P0, "Demonic Collusion", Zone::Hand, db());
    let discard_a = scenario.add_card_to_hand(P0, "Discard A");
    let discard_b = scenario.add_card_to_hand(P0, "Discard B");
    scenario.with_library_top(P0, &["Library A", "Library B"]);
    scenario.with_mana_pool(P0, mana(ManaType::Black, 5));
    let mut runner = scenario.build();

    runner
        .cast(collusion)
        .accept_optional()
        .pay_cost_with(&[discard_a, discard_b])
        .commit();
    let events = run_until_settled(&mut runner, |_| None);

    let state = runner.state();
    assert_eq!(
        graveyard(state, P0),
        vec![discard_a, discard_b],
        "reach guard: the buyback cost was paid"
    );
    assert_eq!(
        state.objects[&collusion].zone,
        Zone::Hand,
        "CR 702.27a: the buyback spell returns to its owner's hand"
    );
    assert_eq!(stack_resolved_count(&events, collusion), 1);
    assert_settled(state);
}

/// CR 616.1 + CR 608.2n: when two replacement effects would both redirect the
/// spell's deferred move to the graveyard (Rest in Peace and Leyline of the
/// Void), the affected player orders them at that moment — after the paused
/// discard — and the spell then leaves the stack for exile.
#[test]
fn a_replacement_ordering_choice_on_the_deferred_move_is_asked_and_completed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db());
    scenario.add_real_card(P1, "Leyline of the Void", Zone::Battlefield, db());
    let looting = scenario.add_real_card(P0, "Faithless Looting", Zone::Hand, db());
    let discard_a = scenario.add_card_to_hand(P0, "Discard A");
    let discard_b = scenario.add_card_to_hand(P0, "Discard B");
    scenario.with_library_top(P0, &["Draw One", "Draw Two", "Pad"]);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut runner = scenario.build();

    runner.cast(looting).commit();
    let mut ordered_the_spells_own_move = false;
    let events = run_until_settled(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }).then(|| {
            // Once the discards are delivered, the question left is the
            // spell's own final part.
            if state.objects[&looting].zone == Zone::Stack
                && [discard_a, discard_b]
                    .iter()
                    .all(|card| state.objects[card].zone == Zone::Exile)
            {
                ordered_the_spells_own_move = true;
                // The final part has run and parked its own CR 616.1 question:
                // it is no longer owed, but its carrier stays until answered.
                assert!(
                    state.owed_spell_final_part().is_none(),
                    "a final part that parked its own question is no longer owed"
                );
                assert_eq!(
                    state.resolving_stack_entry.as_ref().map(|entry| entry.id),
                    Some(looting),
                    "the carrier stays installed until the parked question is answered"
                );
            }
            GameAction::ChooseReplacement { index: 0 }
        })
    });

    assert!(
        ordered_the_spells_own_move,
        "CR 616.1: the deferred move asks its replacement-ordering question"
    );
    let state = runner.state();
    assert_eq!(state.objects[&looting].zone, Zone::Exile);
    assert!(graveyard(state, P0).is_empty());
    assert_eq!(stack_resolved_count(&events, looting), 1);
    assert_settled(state);
}

/// CR 724.2b + CR 608.2m: "end the combat phase" exiles every object on the
/// stack, including the resolving one — when the procedure ran during combat.
/// The spell is exiled at that step, so when its later discard pauses the
/// resolution after combat has ended it is already in exile and its final part
/// leaves it there.
#[test]
fn an_end_the_combat_phase_spell_paused_after_combat_ended_is_still_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let interrupt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Combat Interrupt",
            true,
            "End the combat phase. Draw two cards, then discard two cards.",
        )
        .id();
    scenario.add_card_to_hand(P0, "Discard A");
    scenario.add_card_to_hand(P0, "Discard B");
    scenario.with_library_top(P0, &["Draw One", "Draw Two", "Pad"]);
    let mut runner = scenario.build();

    runner.cast(interrupt).commit();
    let mut paused_after_combat = false;
    let events = run_until_settled(&mut runner, |state| {
        if matches!(state.waiting_for, WaitingFor::DiscardChoice { .. }) {
            paused_after_combat = !state.phase.is_combat();
            assert_eq!(
                state.objects[&interrupt].zone,
                Zone::Exile,
                "CR 724.2b: exiled by its own procedure before its discard"
            );
        }
        None
    });

    assert!(
        paused_after_combat,
        "reach guard: the discard paused the resolution after combat had ended"
    );
    let state = runner.state();
    assert_eq!(
        state.objects[&interrupt].zone,
        Zone::Exile,
        "CR 724.2b: the resolving object is exiled by the end-of-combat procedure"
    );
    assert_eq!(graveyard(state, P0).len(), 2, "only the discards");
    assert_eq!(stack_resolved_count(&events, interrupt), 1);
    assert_settled(state);
}

/// Faithless Looting cast by P0 in a three-player game, paused on P0's discard.
fn looting_paused_on_its_discard_in_a_three_player_game() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let looting = scenario.add_real_card(P0, "Faithless Looting", Zone::Hand, db());
    scenario.add_card_to_hand(P0, "Discard A");
    scenario.add_card_to_hand(P0, "Discard B");
    scenario.with_library_top(P0, &["Draw One", "Draw Two", "Pad"]);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    let mut runner = scenario.build();

    runner.cast(looting).commit();
    pass_until_a_prompt(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardChoice { player, .. } if player == P0
        ),
        "reach guard: the resolution paused on P0's discard, got {:?}",
        runner.state().waiting_for
    );
    (runner, looting)
}

/// Pass priority around the table until something other than a priority
/// window is asked.
fn pass_until_a_prompt(runner: &mut GameRunner) {
    for _ in 0..12 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is accepted");
    }
}

fn concede(runner: &mut GameRunner, player: PlayerId) -> Vec<GameEvent> {
    engine::game::engine::apply(
        runner.state_mut(),
        player,
        GameAction::Concede { player_id: player },
    )
    .expect("a player may concede at any time")
    .events
}

/// CR 800.4a: when the player who owns and controls a paused spell leaves the
/// game, the spell leaves with every other object they own. Its deferred final
/// part must not later put it into the departed player's graveyard.
#[test]
fn a_paused_spell_leaves_the_game_with_its_departing_owner() {
    let (mut runner, looting) = looting_paused_on_its_discard_in_a_three_player_game();

    let mut events = concede(&mut runner, P0);
    events.extend(run_until_settled(&mut runner, |_| None));

    let state = runner.state();
    assert!(
        state.players[P0.0 as usize].hand.is_empty(),
        "reach guard: the departed player's cards left the game"
    );
    assert_eq!(
        state.objects[&looting].zone,
        Zone::Exile,
        "CR 800.4a: the departed player's spell leaves the game with their other cards"
    );
    assert!(graveyard(state, P0).is_empty());
    assert_eq!(
        stack_resolved_count(&events, looting),
        0,
        "a spell whose departing player's question was its last live work is retired with that \
         player and not reported resolved"
    );
    assert_settled(state);
}

/// Faithless Looting cast by P0 and stolen with Commandeer by P1, in a
/// three-player game, paused on its new controller's discard.
fn stolen_looting_paused_on_its_controllers_discard() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let looting = scenario.add_real_card(P0, "Faithless Looting", Zone::Hand, db());
    let commandeer = scenario.add_real_card(P1, "Commandeer", Zone::Hand, db());
    scenario.add_card_to_hand(P1, "P1 Discard A");
    scenario.add_card_to_hand(P1, "P1 Discard B");
    scenario.with_library_top(P1, &["P1 Draw One", "P1 Draw Two", "P1 Pad"]);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    scenario.with_mana_pool(P1, mana(ManaType::Blue, 7));
    let mut runner = scenario.build();

    runner.cast(looting).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    runner.cast(commandeer).target_object(looting).commit();
    pass_until_a_prompt(&mut runner);
    // "You may choose new targets for it." Faithless Looting has none.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("P1 keeps the spell's targets");
        pass_until_a_prompt(&mut runner);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardChoice { player, .. } if player == P1
        ),
        "reach guard: the stolen Looting paused on its new controller's discard, got {:?}",
        runner.state().waiting_for
    );
    (runner, looting)
}

/// CR 800.4a + CR 608.2m: when a paused spell's OWNER leaves while another
/// player controls it (Commandeer), the spell leaves the game, but the
/// resolution its controller started keeps going: that player still makes the
/// discard, there is nothing left for the final part to move, and the spell is
/// reported resolved once its controller has finished it.
#[test]
fn a_paused_spell_whose_owner_leaves_still_finishes_for_its_controller() {
    let (mut runner, looting) = stolen_looting_paused_on_its_controllers_discard();

    let mut events = concede(&mut runner, P0);
    assert_eq!(
        runner.state().objects[&looting].zone,
        Zone::Exile,
        "CR 800.4a: the spell leaves the game with its departing owner"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DiscardChoice { player, .. } if player == P1
        ),
        "CR 608.2m: the controller's resolution goes on, got {:?}",
        runner.state().waiting_for
    );

    events.extend(run_until_settled(&mut runner, |_| None));
    let state = runner.state();
    assert_eq!(
        graveyard(state, P1).len(),
        3,
        "Commandeer and the two cards its controller discarded"
    );
    assert!(!graveyard(state, P1).contains(&looting));
    assert_eq!(state.objects[&looting].zone, Zone::Exile);
    assert_eq!(stack_resolved_count(&events, looting), 1);
    assert_settled(state);
}

/// CR 800.4a: when the CONTROLLER of a paused spell it stole leaves, the
/// control effect ends and the spell reverts to its owner, so it is not among
/// the objects "still controlled by that player" and is not exiled. The
/// departed player's discard is gone; the resolution ends, and the final part
/// puts the spell into its owner's graveyard (CR 608.2n).
#[test]
fn a_paused_spell_whose_thief_leaves_reverts_to_its_owner() {
    let (mut runner, looting) = stolen_looting_paused_on_its_controllers_discard();

    let mut events = concede(&mut runner, P1);
    events.extend(run_until_settled(&mut runner, |_| None));

    let state = runner.state();
    assert_eq!(
        state.objects[&looting].zone,
        Zone::Graveyard,
        "CR 800.4a: the reverted spell is not exiled; it finishes into its owner's graveyard"
    );
    assert_eq!(graveyard(state, P0), vec![looting]);
    assert_eq!(stack_resolved_count(&events, looting), 1);
    assert_settled(state);
}

/// CR 800.4a: a paused spell whose controller leaves while STILL controlling it
/// — no control effect to end, because the controller cast a card another
/// player owns (Siphon Insight lets P0 cast P1's Faithless Looting from exile)
/// — is exiled, not put into its owner's graveyard by its final part.
#[test]
fn a_paused_spell_its_departing_controller_still_controls_is_exiled() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let siphon = scenario.add_real_card(P0, "Siphon Insight", Zone::Hand, db());
    let looting = scenario.add_real_card(P1, "Faithless Looting", Zone::Library, db());
    scenario.with_library_top(P1, &["P1 Pad"]);
    scenario.add_card_to_hand(P0, "Discard A");
    scenario.add_card_to_hand(P0, "Discard B");
    scenario.with_library_top(P0, &["Draw One", "Draw Two", "Pad"]);
    scenario.with_mana_pool(
        P0,
        [ManaType::Blue, ManaType::Black, ManaType::Red]
            .into_iter()
            .flat_map(|color| mana(color, 1))
            .collect(),
    );
    let mut runner = scenario.build();

    runner.cast(siphon).target_player(P1).commit();
    run_until_settled(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::DigChoice { .. }).then(|| GameAction::SelectCards {
            cards: vec![looting],
        })
    });
    assert_eq!(
        runner.state().objects[&looting].zone,
        Zone::Exile,
        "reach guard: Siphon Insight exiled P1's Faithless Looting for P0 to play"
    );

    runner.cast(looting).commit();
    pass_until_a_prompt(&mut runner);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::DiscardChoice { player, .. } if player == P0),
        "reach guard: P0's Looting paused on P0's discard, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        (
            state.objects[&looting].owner,
            state.objects[&looting].controller
        ),
        (P1, P0),
        "reach guard: P1 owns the paused spell and P0 controls it"
    );

    let mut events = concede(&mut runner, P0);
    events.extend(run_until_settled(&mut runner, |_| None));

    let state = runner.state();
    assert_eq!(
        state.objects[&looting].zone,
        Zone::Exile,
        "CR 800.4a: an object still controlled by the departed player is exiled"
    );
    assert!(
        !graveyard(state, P1).contains(&looting),
        "the final part does not put it into its owner's graveyard"
    );
    assert_eq!(stack_resolved_count(&events, looting), 0);
    assert_settled(state);
}

const INVOKE_CALAMITY: &str = "You may cast up to two instant and/or sorcery spells with \
     total mana value 6 or less from your graveyard and/or hand without paying their mana costs. \
     If those spells would be put into your graveyard, exile them instead. Exile Invoke Calamity.";

/// CR 608.2m + CR 608.2n: Invoke Calamity pauses on its free-cast window
/// (CR 608.2g), and its last instruction exiles it. Its final part then has no
/// move left to make — the spell stays where its instruction put it — but the
/// spell did resolve, and is reported resolved exactly once.
#[test]
fn a_paused_spell_that_exiles_itself_is_left_in_exile_and_reported_resolved() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let invoke = scenario
        .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", true, INVOKE_CALAMITY)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario
        .add_spell_to_graveyard(P0, "Graveyard Bolt", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.");
    scenario
        .add_spell_to_hand(P0, "Hand Divination", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("Draw a card.");
    scenario.with_mana_pool(P0, mana(ManaType::Colorless, 1));
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&invoke].card_id;
    let mut events = runner
        .act(GameAction::CastSpell {
            object_id: invoke,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Invoke Calamity is accepted")
        .events;
    for _ in 0..2 {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority is accepted")
                .events,
        );
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            }
        ),
        "reach guard: Invoke Calamity paused on its free-cast window, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&invoke].zone, Zone::Stack);
    assert_eq!(stack_resolved_count(&events, invoke), 0);

    events.extend(
        runner
            .act(GameAction::FreeCastWindowChoice { selection: None })
            .expect("declining the free casts is accepted")
            .events,
    );
    events.extend(run_until_settled(&mut runner, |_| None));

    let state = runner.state();
    assert_eq!(
        state.objects[&invoke].zone,
        Zone::Exile,
        "the spell stays where its own instruction put it"
    );
    assert!(!graveyard(state, P0).contains(&invoke));
    assert_eq!(stack_resolved_count(&events, invoke), 1);
    assert_settled(state);
}

/// CR 707.10: a copy copies the spell and the choices made casting it; the
/// state of the original's resolution is not one of those. Chain of Smog's
/// resolution pauses on its target's discard and its copy offer, so its final
/// part is owed by its carrier — and the copy it makes while paused must carry
/// none of that. The copy's own resolution later owes its own final part.
#[test]
fn a_copy_of_a_paused_spell_inherits_no_owed_final_part() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let smog = scenario.add_real_card(P0, "Chain of Smog", Zone::Hand, db());
    for name in ["P1 A", "P1 B", "P1 C"] {
        scenario.add_card_to_hand(P1, name);
    }
    scenario.add_card_to_hand(P0, "P0 A");
    scenario.add_card_to_hand(P0, "P0 B");
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let mut runner = scenario.build();

    runner.cast(smog).target_player(P1).commit();
    let announced = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == smog)
        .and_then(|entry| match &entry.kind {
            StackEntryKind::Spell {
                ability: Some(ability),
                ..
            } => Some(ability.context.clone()),
            _ => None,
        })
        .expect("Chain of Smog is on the stack with its announced ability");
    // CR 707.10: `copy_spell` clears exactly these cast-only fields on a copy;
    // everything else of the announced context is copied as it was.
    let mut expected_copy_context = announced;
    expected_copy_context.cast_from_zone = None;
    expected_copy_context.alt_cost_grant_source = None;
    expected_copy_context.graveyard_permission_authority = None;
    expected_copy_context.graveyard_permission_latch = None;

    let mut events = Vec::new();
    let mut copy: Option<ObjectId> = None;
    let mut smog_owed_at_its_discard = false;
    let mut copy_context_checked = false;
    let mut copy_owed_at_its_offer = false;
    for _ in 0..40 {
        let state = runner.state();
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } if state.stack.is_empty() => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::DiscardChoice {
                player,
                cards,
                count,
                ..
            } => {
                if copy.is_none() {
                    assert_eq!(*player, P1, "Chain of Smog's target discards");
                    assert_eq!(
                        state.owed_spell_final_part(),
                        Some(&OwedSpellFinalPart { spell: smog }),
                        "the paused Chain of Smog's carrier owes its final part"
                    );
                    smog_owed_at_its_discard = true;
                }
                GameAction::SelectCards {
                    cards: cards.iter().copied().take(*count).collect(),
                }
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(*player, P1, "\"that player may copy this spell\"");
                match copy {
                    // Chain of Smog's own offer: P1 copies it.
                    None => GameAction::DecideOptionalEffect { accept: true },
                    // The copy's offer, from inside the copy's own paused
                    // resolution: the copy's carrier owes the copy's final
                    // part, and Chain of Smog has already finished.
                    Some(copy) => {
                        assert_eq!(
                            state.owed_spell_final_part().map(|owed| owed.spell),
                            Some(copy),
                            "the copy's own paused resolution owes the copy's final part"
                        );
                        assert_eq!(
                            state.objects[&smog].zone,
                            Zone::Graveyard,
                            "reach guard: Chain of Smog has finished into its owner's graveyard"
                        );
                        copy_owed_at_its_offer = true;
                        GameAction::DecideOptionalEffect { accept: false }
                    }
                }
            }
            WaitingFor::CopyRetarget { copy_id, .. } => {
                let entry = state
                    .stack
                    .iter()
                    .find(|entry| entry.id == *copy_id)
                    .expect("the copy is on the stack");
                assert_ne!(entry.id, smog);
                assert_eq!(entry.controller, P1, "CR 707.10: P1 controls the copy");
                let StackEntryKind::Spell {
                    ability: Some(ability),
                    ..
                } = &entry.kind
                else {
                    panic!("the copy is a spell with an ability: {:?}", entry.kind);
                };
                assert_eq!(
                    ability.context, expected_copy_context,
                    "CR 707.10: the copy carries the announced context, and nothing of the \
                     original's paused resolution"
                );
                copy_context_checked = true;
                copy = Some(*copy_id);
                GameAction::ChooseTarget { target: None }
            }
            other => panic!("unexpected prompt while resolving: {other:?}"),
        };
        events.extend(
            runner
                .act(action)
                .expect("the engine accepts the answer")
                .events,
        );
    }

    assert!(
        smog_owed_at_its_discard,
        "reach guard: Chain of Smog paused"
    );
    assert!(copy_context_checked, "reach guard: P1 copied Chain of Smog");
    assert!(
        copy_owed_at_its_offer,
        "reach guard: the copy's own resolution paused on its offer"
    );
    let copy = copy.expect("the copy was made");
    let state = runner.state();
    assert!(
        state.players[P1.0 as usize].hand.is_empty(),
        "reach guard: the original made P1 discard two cards and the copy the last one"
    );
    assert_eq!(state.objects[&smog].zone, Zone::Graveyard);
    assert_eq!(stack_resolved_count(&events, smog), 1);
    assert_eq!(stack_resolved_count(&events, copy), 1);
    assert_settled(state);
}

/// CR 608.2m + CR 800.4a: Diabolic Edict ("Target player sacrifices a creature
/// of their choice.") pauses on its target's sacrifice. Its controller leaves
/// the game while that player is still answering: the spell is exiled with its
/// departing controller, but the resolution goes on for the player answering
/// its instruction, and the spell is reported resolved exactly once, when that
/// answer finishes it.
#[test]
fn a_spell_whose_controller_leaves_while_another_player_answers_is_reported_resolved_once() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let edict = scenario.add_real_card(P0, "Diabolic Edict", Zone::Hand, db());
    let first = scenario.add_vanilla(P1, 2, 2);
    let second = scenario.add_vanilla(P1, 2, 2);
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let mut runner = scenario.build();

    runner.cast(edict).target_player(P1).commit();
    pass_until_a_prompt(&mut runner);
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { player, cards, .. }
                if *player == P1 && cards.contains(&first) && cards.contains(&second)
        ),
        "reach guard: the Edict paused on P1's sacrifice, got {:?}",
        runner.state().waiting_for
    );

    let conceded = concede(&mut runner, P0);
    assert_eq!(stack_resolved_count(&conceded, edict), 0);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { player, .. } if player == P1
        ),
        "reach guard: CR 608.2m: P1 still answers the Edict, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&edict].zone,
        Zone::Exile,
        "reach guard: CR 800.4a: the Edict left the game with its departing controller"
    );

    let answered = runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("P1 sacrifices a creature")
        .events;
    assert_eq!(
        stack_resolved_count(&answered, edict),
        1,
        "CR 608.2m: the resolution finished by P1 is reported exactly once"
    );
    let mut events = answered;
    events.extend(run_until_settled(&mut runner, |_| None));
    let state = runner.state();
    assert_eq!(state.objects[&first].zone, Zone::Graveyard);
    assert_eq!(state.objects[&edict].zone, Zone::Exile);
    assert_eq!(stack_resolved_count(&events, edict), 1);
    assert_settled(state);
}

/// CR 608.2n + CR 702.82a: Zombify ("Return target creature card from your
/// graveyard to the battlefield.") returning Mycoloth (Devour 2) pauses on the
/// devour choice inside Zombify's own resolution. The recovery guards that
/// refuse an owing carrier as terminal residue must not stall this live
/// resolution: once the devour choice is answered, Zombify's final part runs.
#[test]
fn zombify_returning_a_devourer_still_finishes_its_final_part() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zombify = scenario.add_real_card(P0, "Zombify", Zone::Hand, db());
    let mycoloth = scenario.add_real_card(P0, "Mycoloth", Zone::Graveyard, db());
    scenario.add_vanilla(P0, 1, 1);
    scenario.add_vanilla(P0, 1, 1);
    scenario.with_mana_pool(P0, mana(ManaType::Black, 4));
    let mut runner = scenario.build();

    runner.cast(zombify).target_object(mycoloth).commit();
    let mut asked_to_devour = false;
    let events = run_until_settled(&mut runner, |state| {
        matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }).then(|| {
            asked_to_devour = true;
            assert_eq!(
                state.objects[&zombify].zone,
                Zone::Stack,
                "Zombify is still resolving while the devour choice is asked"
            );
            GameAction::SelectCards { cards: vec![] }
        })
    });

    assert!(asked_to_devour, "reach guard: Mycoloth's devour was asked");
    let state = runner.state();
    assert_eq!(state.objects[&mycoloth].zone, Zone::Battlefield);
    assert_eq!(state.objects[&zombify].zone, Zone::Graveyard);
    assert_eq!(stack_resolved_count(&events, zombify), 1);
    assert_settled(state);
}

// ---------------------------------------------------------------------------
// CR 603.3b: the paused resolution's triggers, its answer's and its final
// part's go on the stack as one batch, at the next priority.
// ---------------------------------------------------------------------------

const GRAVEYARD_ARCHIVIST: &str =
    "Whenever a card is put into your graveyard from anywhere, you gain 1 life.";
const DEATH_OBSERVER: &str = "Whenever another creature dies, you gain 1 life.";
const SECOND_THOUGHTS: &str = "Draw a card. You may draw a card.";

/// Bloodchief Ascension with three quest counters: "Whenever a card is put
/// into an opponent's graveyard from anywhere, if this enchantment has three
/// or more quest counters on it, you may have that player lose 2 life. If you
/// do, you gain 2 life."
fn bloodchief_ascension(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let id = scenario.add_real_card(player, "Bloodchief Ascension", Zone::Battlefield, db());
    scenario.with_counter(id, CounterType::Generic("quest".to_string()), 3);
    id
}

/// Triggered abilities from `source` on the stack.
fn triggers_from(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                && entry.source_id == source
        })
        .count()
}

/// Controllers of the triggered abilities on the stack, bottom to top.
fn trigger_controllers(state: &GameState) -> Vec<PlayerId> {
    state
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .map(|entry| entry.controller)
        .collect()
}

/// Pass priority until `spell` has left the stack zone.
fn pass_until_resolved(runner: &mut GameRunner, spell: ObjectId) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..12 {
        if runner.state().objects[&spell].zone != Zone::Stack {
            return events;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority is accepted")
                .events,
        );
    }
    panic!(
        "the spell did not resolve; waiting for {:?}",
        runner.state().waiting_for
    );
}

/// Pass priority while it is held over a non-empty stack, returning every
/// event, until something other than a priority window is asked.
fn pass_to_a_prompt_collecting(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..12 {
        let state = runner.state();
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || state.stack.is_empty() {
            return events;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("passing priority is accepted")
                .events,
        );
    }
    events
}

/// Accept the "you may" of Bloodchief Ascension's trigger while settling.
fn accept_optional_effects(state: &GameState) -> Option<GameAction> {
    matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. })
        .then_some(GameAction::DecideOptionalEffect { accept: true })
}

struct EdictBoard {
    runner: GameRunner,
    edict: ObjectId,
    ascension: ObjectId,
    sacrificed: ObjectId,
}

/// Diabolic Edict ("Target player sacrifices a creature of their choice.") cast
/// by P0 at P1, who controls Bloodchief Ascension and two creatures, paused on
/// P1's sacrifice choice.
fn edict_paused_on_a_sacrifice_choice() -> EdictBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let edict = scenario.add_real_card(P0, "Diabolic Edict", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let ascension = bloodchief_ascension(&mut scenario, P1);
    let sacrificed = scenario.add_vanilla(P1, 2, 2);
    scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(edict).target_player(P1).commit();
    pass_until_a_prompt(&mut runner);
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { player, effect_kind: EffectKind::Sacrifice, cards, .. }
                if *player == P1 && cards.contains(&sacrificed)
        ),
        "reach guard: the Edict paused on P1's sacrifice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&edict].zone, Zone::Stack);
    EdictBoard {
        runner,
        edict,
        ascension,
        sacrificed,
    }
}

/// CR 608.2n + CR 603.2: a sacrifice answer that finishes a paused Edict still
/// lets the Edict's own move to P0's graveyard trigger P1's Bloodchief
/// Ascension. The sacrificed creature goes to P1's own graveyard, so exactly
/// one trigger.
#[test]
fn a_sacrifice_answer_that_finishes_a_paused_edict_triggers_on_its_own_graveyard_move() {
    let EdictBoard {
        mut runner,
        edict,
        ascension,
        sacrificed,
    } = edict_paused_on_a_sacrifice_choice();

    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrificed],
        })
        .expect("P1 sacrifices a creature");

    let state = runner.state();
    assert_eq!(state.objects[&edict].zone, Zone::Graveyard);
    assert_eq!(graveyard(state, P0), vec![edict]);
    assert_eq!(state.objects[&sacrificed].zone, Zone::Graveyard);
    assert_eq!(
        triggers_from(state, ascension),
        1,
        "CR 603.2: the Edict's own move into P0's graveyard triggers Bloodchief Ascension"
    );

    run_until_settled(&mut runner, accept_optional_effects);
    let state = runner.state();
    assert_eq!(state.players[P0.0 as usize].life, 18);
    assert_eq!(state.players[P1.0 as usize].life, 22);
    assert_settled(state);
}

/// CR 117.3b: the active player receives priority after the answer that
/// finished the resolution puts its batch on the stack.
#[test]
fn the_active_player_receives_priority_after_a_finishing_answers_batch() {
    let EdictBoard {
        mut runner,
        ascension,
        sacrificed,
        ..
    } = edict_paused_on_a_sacrifice_choice();

    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrificed],
        })
        .expect("P1 sacrifices a creature");

    let state = runner.state();
    assert_eq!(triggers_from(state, ascension), 1);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
}

/// CR 608.2n + CR 603.2: the same move trigger on the player-scope sacrifice
/// route (Innocent Blood: "Each player sacrifices a creature of their
/// choice."), where only P1 has creatures to choose from.
#[test]
fn a_player_scope_sacrifice_answer_triggers_on_the_spells_own_graveyard_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let ascension = bloodchief_ascension(&mut scenario, P1);
    let sacrificed = scenario.add_vanilla(P1, 2, 2);
    scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    assert!(
        runner
            .state()
            .pending_player_scope_sacrifice_choice
            .is_some(),
        "reach guard: the player-scope sacrifice route, got {:?}",
        runner.state().waiting_for
    );
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { player, .. } if *player == P1
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrificed],
        })
        .expect("P1 sacrifices a creature");

    let state = runner.state();
    assert_eq!(state.objects[&blood].zone, Zone::Graveyard);
    assert_eq!(graveyard(state, P0), vec![blood]);
    assert_eq!(triggers_from(state, ascension), 1);
}

/// Positive control of the instrument: an Edict whose target has one creature
/// resolves with no choice, and its own move triggers Bloodchief Ascension.
#[test]
fn an_edict_with_no_choice_triggers_on_its_own_graveyard_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let edict = scenario.add_real_card(P0, "Diabolic Edict", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let ascension = bloodchief_ascension(&mut scenario, P1);
    let only = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(edict).target_player(P1).commit();
    let events = pass_until_resolved(&mut runner, edict);

    assert_eq!(
        stack_resolved_count(&events, edict),
        1,
        "reach guard: the Edict resolved through priority passes"
    );
    let state = runner.state();
    assert!(!matches!(
        state.waiting_for,
        WaitingFor::EffectZoneChoice { .. }
    ));
    assert_eq!(state.objects[&edict].zone, Zone::Graveyard);
    assert_eq!(state.objects[&only].zone, Zone::Graveyard);
    assert_eq!(triggers_from(state, ascension), 1);
}

/// CR 603.3b (preservation): Faithless Looting's two discards and its own move
/// are three cards put into P0's graveyard, triggering P1's Bloodchief
/// Ascension three times in one batch.
#[test]
fn a_discard_answer_puts_the_spells_own_move_trigger_in_its_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let looting = scenario.add_real_card(P0, "Faithless Looting", Zone::Hand, db());
    let discard_a = scenario.add_card_to_hand(P0, "Discard A");
    let discard_b = scenario.add_card_to_hand(P0, "Discard B");
    scenario.with_library_top(P0, &["Draw One", "Draw Two", "Pad"]);
    scenario.with_mana_pool(P0, mana(ManaType::Red, 1));
    let ascension = bloodchief_ascension(&mut scenario, P1);
    let mut runner = scenario.build();

    runner.cast(looting).commit();
    pass_until_a_prompt(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DiscardChoice { .. }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![discard_a, discard_b],
        })
        .expect("P0 discards two cards");

    let state = runner.state();
    let WaitingFor::OrderTriggers { player, triggers } = &state.waiting_for else {
        panic!("P1 orders the batch, got {:?}", state.waiting_for);
    };
    assert_eq!(*player, P1);
    assert_eq!(triggers.len(), 3);
    assert!(trigger_controllers(state).is_empty());
    assert_eq!(graveyard(state, P0), vec![discard_a, discard_b, looting]);

    runner
        .act(GameAction::OrderTriggers {
            order: vec![0, 1, 2],
        })
        .expect("P1 orders the triggers");
    assert_eq!(triggers_from(runner.state(), ascension), 3);
    assert_eq!(trigger_controllers(runner.state()), vec![P1, P1, P1]);
}

/// CR 608.2n + CR 603.2 (preservation): a search answer that finishes
/// Doomsday still lets its own move into P0's graveyard trigger P1's
/// Bloodchief Ascension.
#[test]
fn a_search_answer_triggers_on_the_spells_own_graveyard_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(
        P0,
        &[
            "Library A",
            "Library B",
            "Library C",
            "Library D",
            "Library E",
            "Library F",
            "Library G",
        ],
    );
    scenario.with_graveyard(P0, &["Graveyard A", "Graveyard B", "Graveyard C"]);
    let doomsday = scenario.add_real_card(P0, "Doomsday", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 3));
    let ascension = bloodchief_ascension(&mut scenario, P1);
    let mut runner = scenario.build();
    let life = runner.state().players[P0.0 as usize].life;

    runner.cast(doomsday).commit();
    let mut searched = false;
    for _ in 0..40 {
        let state = runner.state();
        if triggers_from(state, ascension) > 0 {
            break;
        }
        let action = match &state.waiting_for {
            WaitingFor::Priority { .. } if !state.stack.is_empty() => GameAction::PassPriority,
            WaitingFor::SearchChoice { cards, count, .. } => {
                searched = true;
                GameAction::SelectCards {
                    cards: cards.iter().copied().take(*count).collect(),
                }
            }
            other => panic!("unexpected prompt while resolving Doomsday: {other:?}"),
        };
        runner.act(action).expect("the engine accepts the answer");
    }

    assert!(searched, "reach guard: Doomsday's search was answered");
    let state = runner.state();
    assert_eq!(state.players[P0.0 as usize].life, life - 10);
    assert_eq!(state.objects[&doomsday].zone, Zone::Graveyard);
    assert_eq!(triggers_from(state, ascension), 1);
}

struct OptionalDrawBoard {
    runner: GameRunner,
    spell: ObjectId,
    locust_god: ObjectId,
    ascension: ObjectId,
    below: Option<ObjectId>,
}

/// "Second Thoughts" ("Draw a card. You may draw a card."), paused on its
/// optional draw. P0 controls The Locust God ("Whenever you draw a card,
/// create a 1/1 blue and red Insect creature token with flying and haste.");
/// P1 controls Bloodchief Ascension. With `below`, P0 first casts "Below Draw"
/// ("Draw a card."), which stays on the stack beneath it.
fn second_thoughts_paused_on_its_optional_draw(below: bool) -> OptionalDrawBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let below = below.then(|| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Below Draw", false, "Draw a card.")
            .id()
    });
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Second Thoughts", true, SECOND_THOUGHTS)
        .id();
    let locust_god = scenario.add_real_card(P0, "The Locust God", Zone::Battlefield, db());
    scenario.with_library_top(
        P0,
        &["Draw One", "Draw Two", "Draw Three", "Pad A", "Pad B"],
    );
    let ascension = bloodchief_ascension(&mut scenario, P1);
    let mut runner = scenario.build();
    if let Some(below) = below {
        runner.cast(below).commit();
    }
    runner.cast(spell).commit();
    pass_until_a_prompt(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "reach guard: the instant paused on its optional draw, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(
        runner.state().deferred_triggers.len(),
        1,
        "reach guard: the first draw's Locust God trigger waits for the batch"
    );
    OptionalDrawBoard {
        runner,
        spell,
        locust_god,
        ascension,
        below,
    }
}

/// CR 603.3b: Eureka Moment ("Draw two cards. You may put a land card from
/// your hand onto the battlefield.") pauses after its draws triggered The
/// Locust God twice. The answer completes the resolution; the final part's
/// move triggers P1's Bloodchief Ascension; all three go on the stack as one
/// APNAP batch, P0 ordering theirs first.
#[test]
fn triggers_from_the_paused_resolution_and_its_final_part_go_on_the_stack_as_one_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let eureka = scenario.add_real_card(P0, "Eureka Moment", Zone::Hand, db());
    scenario.add_real_card(P0, "The Locust God", Zone::Battlefield, db());
    scenario.with_library_top(P0, &["Draw One", "Draw Two", "Pad"]);
    let mut pool = mana(ManaType::Green, 2);
    pool.extend(mana(ManaType::Blue, 2));
    scenario.with_mana_pool(P0, pool);
    bloodchief_ascension(&mut scenario, P1);
    let mut runner = scenario.build();
    let library_before = runner.state().players[P0.0 as usize].library.len();

    runner.cast(eureka).commit();
    pass_until_a_prompt(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player, .. } if player == P0
    ));
    assert_eq!(runner.state().deferred_triggers.len(), 2);
    assert_eq!(runner.state().objects[&eureka].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before - 2,
        "reach guard: two cards drawn"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P0 declines");
    let state = runner.state();
    assert_eq!(state.objects[&eureka].zone, Zone::Graveyard);
    assert!(
        matches!(state.waiting_for, WaitingFor::OrderTriggers { player, .. } if player == P0),
        "P0 orders their triggers of the batch, got {:?}",
        state.waiting_for
    );
    assert!(
        trigger_controllers(state).is_empty(),
        "no trigger of the batch is on the stack before the batch is ordered"
    );

    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("P0 orders their triggers");
    assert_eq!(trigger_controllers(runner.state()), vec![P0, P0, P1]);
}

/// CR 608.2n + CR 603.3 + CR 614.12a: Zombify returns Clone, which enters as a
/// copy of Flametongue Kavu. Its enters trigger waits for Zombify's final
/// part, and its target is chosen only once Zombify is in the graveyard and
/// its resolution is over.
#[test]
fn a_trigger_the_resolution_creates_waits_for_the_spells_final_part() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zombify = scenario.add_real_card(P0, "Zombify", Zone::Hand, db());
    let clone = scenario.add_real_card(P0, "Clone", Zone::Graveyard, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 4));
    let ftk = scenario.add_real_card(P1, "Flametongue Kavu", Zone::Battlefield, db());
    let bear = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(zombify).target_object(clone).commit();
    let mut events = Vec::new();
    let mut selected_target = false;
    for _ in 0..20 {
        let state = runner.state();
        let action = match &state.waiting_for {
            WaitingFor::ReplacementChoice { candidates, .. } => GameAction::ChooseReplacement {
                index: candidates
                    .iter()
                    .position(|candidate| candidate.source_name == "Clone")
                    .expect("Clone's enter-as-a-copy replacement is offered"),
            },
            WaitingFor::CopyTargetChoice { .. } => GameAction::ChooseTarget {
                target: Some(TargetRef::Object(ftk)),
            },
            WaitingFor::TriggerTargetSelection { .. } => {
                assert_eq!(
                    state.objects[&clone].name, "Flametongue Kavu",
                    "reach guard: Clone entered as a copy of Flametongue Kavu"
                );
                assert_eq!(
                    state.objects[&zombify].zone,
                    Zone::Graveyard,
                    "CR 608.2n: Zombify's final part ran before the trigger's target is chosen"
                );
                assert!(
                    state.resolving_stack_entry.is_none(),
                    "Zombify's resolution is over when the trigger's target is chosen"
                );
                selected_target = true;
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(bear)),
                }
            }
            WaitingFor::Priority { .. } if selected_target => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected prompt: {other:?}"),
        };
        events.extend(runner.act(action).expect("the engine accepts").events);
    }
    assert!(
        selected_target,
        "reach guard: the enters trigger's target was chosen"
    );

    events.extend(run_until_settled(&mut runner, |_| None));
    let state = runner.state();
    assert_eq!(state.objects[&bear].zone, Zone::Graveyard);
    assert_eq!(stack_resolved_count(&events, zombify), 1);
    assert_settled(state);
}

/// CR 603.3b: the final part's trigger (P0's Graveyard Archivist, on the
/// Edict's own move) and the answer's trigger (P1's Death Observer, on the
/// sacrifice) are one batch, put on the stack in APNAP order.
#[test]
fn the_final_parts_trigger_and_the_answers_trigger_are_one_apnap_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let edict = scenario.add_real_card(P0, "Diabolic Edict", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let archivist = scenario
        .add_enchantment_from_oracle(P0, "Graveyard Archivist", GRAVEYARD_ARCHIVIST)
        .id();
    let observer = scenario
        .add_creature_from_oracle(P1, "Death Observer", 1, 1, DEATH_OBSERVER)
        .id();
    let vanilla = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(edict).target_player(P1).commit();
    pass_until_a_prompt(&mut runner);
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { player, cards, .. }
            if *player == P1 && cards.contains(&vanilla) && cards.contains(&observer)
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![vanilla],
        })
        .expect("P1 sacrifices the vanilla");

    let state = runner.state();
    assert_eq!(state.objects[&edict].zone, Zone::Graveyard);
    assert_eq!(triggers_from(state, archivist), 1);
    assert_eq!(triggers_from(state, observer), 1);
    assert_eq!(trigger_controllers(state), vec![P0, P1]);
}

/// CR 603.3b: an answer's own trigger (the optional draw's Locust God
/// trigger) joins the batch of the resolution it finishes, so P0 orders both
/// of their Locust God triggers together.
#[test]
fn an_answers_own_trigger_joins_the_resolutions_batch() {
    let OptionalDrawBoard {
        mut runner, spell, ..
    } = second_thoughts_paused_on_its_optional_draw(false);

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P0 draws");
    let state = runner.state();
    assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
    assert!(
        matches!(
            &state.waiting_for,
            WaitingFor::OrderTriggers { player, triggers } if *player == P0 && triggers.len() == 2
        ),
        "P0 orders both Locust God triggers, got {:?}",
        state.waiting_for
    );
    assert!(trigger_controllers(state).is_empty());

    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("P0 orders their triggers");
    assert_eq!(trigger_controllers(runner.state()), vec![P0, P0, P1]);
}

/// CR 608.2n + CR 117.5 + CR 603.3 + CR 405.5: once the paused instant's
/// final part has run its resolution is over, so its batch goes on the stack
/// above the spell still below it, before that spell begins to resolve,
/// whether the answer accepted the optional draw or declined it.
#[test]
fn a_finished_resolutions_batch_goes_on_the_stack_above_a_spell_still_below_it() {
    for accept in [true, false] {
        let OptionalDrawBoard {
            mut runner,
            spell,
            locust_god,
            ascension,
            below,
        } = second_thoughts_paused_on_its_optional_draw(true);
        let below = below.expect("a spell below");
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![below]
        );

        let mut events = runner
            .act(GameAction::DecideOptionalEffect { accept })
            .expect("P0 answers")
            .events;
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        if accept {
            assert!(
                matches!(
                    &runner.state().waiting_for,
                    WaitingFor::OrderTriggers { player, triggers }
                        if *player == P0 && triggers.len() == 2
                ),
                "accept: P0 orders both Locust God triggers, got {:?}",
                runner.state().waiting_for
            );
            events.extend(
                runner
                    .act(GameAction::OrderTriggers { order: vec![0, 1] })
                    .expect("P0 orders their triggers")
                    .events,
            );
        } else {
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 },
                "decline: the batch is on the stack and P0 holds priority"
            );
            assert!(runner.state().deferred_triggers.is_empty());
        }

        let state = runner.state();
        let sources: Vec<ObjectId> = state.stack.iter().map(|entry| entry.source_id).collect();
        let (expected_sources, expected_controllers) = if accept {
            (
                vec![below, locust_god, locust_god, ascension],
                vec![P0, P0, P1],
            )
        } else {
            (vec![below, locust_god, ascension], vec![P0, P1])
        };
        assert_eq!(
            sources, expected_sources,
            "accept={accept}: the batch sits above the spell below"
        );
        assert_eq!(trigger_controllers(state), expected_controllers);
        assert_eq!(state.objects[&below].zone, Zone::Stack);
        assert_eq!(
            stack_resolved_count(&events, below),
            0,
            "accept={accept}: the spell below has not begun to resolve"
        );
    }
}

/// CR 117.5 + CR 405.5: the Edict's own move trigger goes on the stack above
/// the spell P0 cast before it, and P0 receives priority (CR 117.3b).
#[test]
fn an_edicts_graveyard_move_trigger_goes_above_the_spell_it_was_cast_over() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let below = scenario
        .add_spell_to_hand_from_oracle(P0, "Below Draw", false, "Draw a card.")
        .id();
    scenario.with_library_top(P0, &["Draw One", "Pad"]);
    let edict = scenario.add_real_card(P0, "Diabolic Edict", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let ascension = bloodchief_ascension(&mut scenario, P1);
    let sacrificed = scenario.add_vanilla(P1, 2, 2);
    scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(below).commit();
    runner.cast(edict).target_player(P1).commit();
    pass_until_a_prompt(&mut runner);
    assert!(matches!(
        &runner.state().waiting_for,
        WaitingFor::EffectZoneChoice { player, effect_kind: EffectKind::Sacrifice, .. }
            if *player == P1
    ));
    assert_eq!(runner.state().objects[&edict].zone, Zone::Stack);

    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrificed],
        })
        .expect("P1 sacrifices a creature");

    let state = runner.state();
    assert_eq!(state.objects[&edict].zone, Zone::Graveyard);
    assert_eq!(
        state
            .stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>(),
        vec![below, ascension]
    );
    assert_eq!(state.objects[&below].zone, Zone::Stack);
    assert!(state.deferred_triggers.is_empty());
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
}

/// Footlight Fiend, a 1/1: "When this creature dies, it deals 1 damage to any
/// target."
const FOOTLIGHT_FIEND: &str = "When this creature dies, it deals 1 damage to any target.";

/// Fire Prophecy ("Fire Prophecy deals 3 damage to target creature. You may
/// put a card from your hand on the bottom of your library. If you do, draw a
/// card.") in P0's hand, with a card to put and the mana to cast it.
fn fire_prophecy_in_hand(scenario: &mut GameScenario) -> ObjectId {
    let prophecy = scenario.add_real_card(P0, "Fire Prophecy", Zone::Hand, db());
    scenario.add_card_to_hand(P0, "Spare Card");
    scenario.with_library_top(P0, &["Draw One", "Pad A", "Pad B"]);
    // Enough red that a spell cast before it cannot spend what it needs.
    scenario.with_mana_pool(P0, mana(ManaType::Red, 4));
    prophecy
}

/// Cast Fire Prophecy at `target` and pass until it pauses on its optional
/// put, its damage dealt but no state-based action checked yet.
fn cast_fire_prophecy_to_its_optional_put(
    runner: &mut GameRunner,
    prophecy: ObjectId,
    target: ObjectId,
) {
    runner.cast(prophecy).target_object(target).commit();
    pass_until_a_prompt(runner);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { player, .. } if player == P0),
        "reach guard: Fire Prophecy paused on its optional put, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.objects[&prophecy].zone, Zone::Stack);
    assert_eq!(
        state.objects[&target].zone,
        Zone::Battlefield,
        "reach guard: the lethal damage waits for state-based actions (CR 704.3)"
    );
}

/// CR 117.5 + CR 603.3b + CR 704.5g: P0 declines Fire Prophecy's optional put,
/// which completes its resolution. In the pass that runs its final part,
/// state-based actions destroy P1's Grizzly Bears, triggering P0's Syr Konrad;
/// the final part's move triggers P0's Quest for Ancient Secrets. Both
/// triggered since a player last received priority, so they are one batch and
/// P0 orders them.
#[test]
fn a_state_based_death_in_the_final_parts_pass_joins_the_resolutions_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let prophecy = fire_prophecy_in_hand(&mut scenario);
    let quest = scenario.add_real_card(P0, "Quest for Ancient Secrets", Zone::Battlefield, db());
    let konrad = scenario.add_real_card(P0, "Syr Konrad, the Grim", Zone::Battlefield, db());
    let bears = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    cast_fire_prophecy_to_its_optional_put(&mut runner, prophecy, bears);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P0 declines the optional put");

    let state = runner.state();
    assert_eq!(state.objects[&prophecy].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&bears].zone,
        Zone::Graveyard,
        "reach guard: state-based actions destroyed the bears in this pass"
    );
    assert!(
        matches!(
            &state.waiting_for,
            WaitingFor::OrderTriggers { player, triggers } if *player == P0 && triggers.len() == 2
        ),
        "CR 603.3b: P0 orders Syr Konrad's and the Quest's triggers as one batch, got {:?}",
        state.waiting_for
    );
    assert!(
        trigger_controllers(state).is_empty(),
        "no trigger of the batch is on the stack before the batch is ordered"
    );

    runner
        .act(GameAction::OrderTriggers { order: vec![0, 1] })
        .expect("P0 orders their triggers");
    let state = runner.state();
    assert_eq!(triggers_from(state, quest), 1);
    assert_eq!(triggers_from(state, konrad), 1);
    assert_eq!(trigger_controllers(state), vec![P0, P0]);
}

/// CR 603.7b + CR 603.3b: the same pass with P0's Whippoorwill delayed
/// trigger watching the bears. Its death trigger joins Syr Konrad's and the
/// Quest's batch, so P0 orders all three, and it exiles the bears.
#[test]
fn a_delayed_death_trigger_joins_the_final_parts_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let prophecy = fire_prophecy_in_hand(&mut scenario);
    scenario.with_mana_pool(
        P0,
        [mana(ManaType::Green, 2), mana(ManaType::Red, 4)].concat(),
    );
    let bird = scenario.add_real_card(P0, "Whippoorwill", Zone::Battlefield, db());
    let quest = scenario.add_real_card(P0, "Quest for Ancient Secrets", Zone::Battlefield, db());
    let konrad = scenario.add_real_card(P0, "Syr Konrad, the Grim", Zone::Battlefield, db());
    let bears = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.activate(bird, 0).target_object(bears).resolve();
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "reach guard: Whippoorwill's delayed trigger watches the bears"
    );
    cast_fire_prophecy_to_its_optional_put(&mut runner, prophecy, bears);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P0 declines the optional put");

    let state = runner.state();
    assert_eq!(state.objects[&bears].zone, Zone::Graveyard);
    let WaitingFor::OrderTriggers { player, triggers } = &state.waiting_for else {
        panic!("P0 orders the batch, got {:?}", state.waiting_for);
    };
    assert_eq!(*player, P0);
    let mut sources: Vec<_> = triggers.iter().map(|summary| summary.source_id).collect();
    sources.sort();
    let mut expected = vec![quest, konrad, bird];
    expected.sort();
    assert_eq!(
        sources, expected,
        "CR 603.3b: Whippoorwill's delayed trigger is ordered with the batch, once"
    );
    assert!(trigger_controllers(state).is_empty());

    runner
        .act(GameAction::OrderTriggers {
            order: (0..expected.len()).collect(),
        })
        .expect("P0 orders their triggers");
    run_until_settled(&mut runner, accept_optional_effects);
    let state = runner.state();
    assert_eq!(state.objects[&bears].zone, Zone::Exile);
    assert!(state.delayed_triggers.is_empty());
}

/// CR 117.5 + CR 603.3b + CR 101.4: the same pass with Divination below Fire
/// Prophecy and P1's Footlight Fiend dying to it. Fiend's death trigger (P1)
/// and the Quest's trigger on Fire Prophecy's move (P0) are one batch, put on
/// the stack in APNAP order: P0's below P1's, both above Divination.
#[test]
fn a_state_based_death_trigger_joins_the_final_parts_batch_in_apnap_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let divination = scenario.add_real_card(P0, "Divination", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Blue, 3));
    let prophecy = fire_prophecy_in_hand(&mut scenario);
    let quest = scenario.add_real_card(P0, "Quest for Ancient Secrets", Zone::Battlefield, db());
    let fiend = scenario
        .add_creature_from_oracle(P1, "Footlight Fiend", 1, 1, FOOTLIGHT_FIEND)
        .id();
    let mut runner = scenario.build();

    runner.cast(divination).commit();
    cast_fire_prophecy_to_its_optional_put(&mut runner, prophecy, fiend);
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![divination],
        "reach guard: Divination waits below the paused Fire Prophecy"
    );

    let mut events = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P0 declines the optional put")
        .events;
    let mut chose_target = false;
    for _ in 0..4 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) {
            break;
        }
        chose_target = true;
        events.extend(
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                })
                .expect("P1 targets P0 with Footlight Fiend's trigger")
                .events,
        );
    }
    assert!(
        chose_target,
        "reach guard: Footlight Fiend's death trigger asked for its target"
    );

    let state = runner.state();
    assert_eq!(state.objects[&prophecy].zone, Zone::Graveyard);
    assert_eq!(state.objects[&fiend].zone, Zone::Graveyard);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(
        state
            .stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>(),
        vec![divination, quest, fiend],
        "CR 101.4: P0's trigger goes on the stack first, below P1's"
    );
    assert_eq!(trigger_controllers(state), vec![P0, P1]);
    assert!(state.deferred_triggers.is_empty());
    assert_eq!(stack_resolved_count(&events, divination), 0);
}

/// CR 117.5 + CR 603.3b + CR 610.3: Banisher Priest ("When this creature
/// enters, exile target creature an opponent controls until this creature
/// leaves the battlefield.") has exiled P1's Elvish Visionary ("When this
/// creature enters, draw a card."). Fire Prophecy kills the Priest; in the
/// pass that runs its final part, the Priest's death returns the Visionary,
/// whose enters trigger joins the batch of P0's two Quest triggers (Fire
/// Prophecy's move and the Priest's), put on the stack in APNAP order.
#[test]
fn an_exile_return_in_the_final_parts_pass_joins_the_resolutions_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let priest = scenario.add_real_card(P0, "Banisher Priest", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::White, 3));
    let prophecy = fire_prophecy_in_hand(&mut scenario);
    let quest = scenario.add_real_card(P0, "Quest for Ancient Secrets", Zone::Battlefield, db());
    let visionary = scenario.add_real_card(P1, "Elvish Visionary", Zone::Battlefield, db());
    scenario.with_library_top(P1, &["P1 Draw", "P1 Pad"]);
    let mut runner = scenario.build();

    runner.cast(priest).target_object(visionary).resolve();
    assert_eq!(
        runner.state().objects[&visionary].zone,
        Zone::Exile,
        "reach guard: Banisher Priest exiled the Visionary"
    );
    assert!(runner.state().stack.is_empty());

    cast_fire_prophecy_to_its_optional_put(&mut runner, prophecy, priest);
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P0 declines the optional put");

    let state = runner.state();
    assert_eq!(state.objects[&prophecy].zone, Zone::Graveyard);
    assert_eq!(state.objects[&priest].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&visionary].zone,
        Zone::Battlefield,
        "reach guard: the Visionary returned in this pass"
    );
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(
        state
            .stack
            .iter()
            .map(|entry| entry.source_id)
            .collect::<Vec<_>>(),
        vec![quest, quest, visionary],
        "CR 101.4: P0's Quest triggers go on the stack first, below P1's"
    );
    assert_eq!(trigger_controllers(state), vec![P0, P0, P1]);
    assert!(state.deferred_triggers.is_empty());
}

// ---------------------------------------------------------------------------
// CR 800.4a: a spell whose final part ran and parked its own question.
// ---------------------------------------------------------------------------

/// "Plain Draw" ("Draw a card.") cast by P0 in a three-player game where P1
/// controls Rest in Peace and Leyline of the Void: its move off the stack asks
/// P0 to order the two replacements (CR 616.1). The final part has run (the
/// spell is reported resolved), but the spell is still on the stack and its
/// carrier installed.
fn plain_draw_parked_on_its_own_moves_ordering() -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db());
    scenario.add_real_card(P1, "Leyline of the Void", Zone::Battlefield, db());
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Plain Draw", false, "Draw a card.")
        .id();
    scenario.with_library_top(P0, &["Draw One", "Pad"]);
    let mut runner = scenario.build();

    runner.cast(spell).commit();
    let events = pass_to_a_prompt_collecting(&mut runner);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P0),
        "reach guard: the spell's own move asks P0 to order the replacements, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.objects[&spell].zone, Zone::Stack);
    assert!(state.stack.is_empty());
    assert!(
        state.owed_spell_final_part().is_none(),
        "the final part ran and parked its own question"
    );
    assert_eq!(
        state.resolving_stack_entry.as_ref().map(|entry| entry.id),
        Some(spell)
    );
    (runner, spell, events)
}

/// CR 800.4a: the spell is still an object on the stack, owned by P0, so it
/// leaves the game with P0 rather than being stranded in the stack zone. It
/// was reported resolved once, when its final part ran.
#[test]
fn a_spell_whose_final_part_parked_its_own_question_leaves_the_game_with_its_owner() {
    let (mut runner, spell, mut events) = plain_draw_parked_on_its_own_moves_ordering();

    events.extend(concede(&mut runner, P0));
    let state = runner.state();
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Exile,
        "CR 800.4a: the departed owner's spell leaves the game"
    );
    assert!(state.resolving_stack_entry.is_none());

    events.extend(run_until_settled(&mut runner, |_| None));
    assert_settled(runner.state());
    assert_eq!(stack_resolved_count(&events, spell), 1);
}

/// CR 800.4a affects only the departing player's objects: P0's question
/// stands, and answering it exiles the spell through the ordered replacement.
#[test]
fn an_unrelated_departure_leaves_a_parked_final_part_to_its_question() {
    let (mut runner, spell, mut events) = plain_draw_parked_on_its_own_moves_ordering();

    events.extend(concede(&mut runner, PlayerId(2)));
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P0),
        "P0's question is still live, got {:?}",
        state.waiting_for
    );
    assert_eq!(state.objects[&spell].zone, Zone::Stack);

    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("P0 orders the replacements")
            .events,
    );
    events.extend(run_until_settled(&mut runner, |_| None));
    let state = runner.state();
    assert_eq!(state.objects[&spell].zone, Zone::Exile);
    assert!(state.resolving_stack_entry.is_none());
    assert_eq!(stack_resolved_count(&events, spell), 1);
    assert_settled(state);
}

/// Quest for Ancient Secrets: "Whenever a card is put into your graveyard from
/// anywhere, you may put a quest counter on this enchantment." (its sacrifice
/// ability is not under test).
///
/// CR 608.2n + CR 603.2 + CR 603.2c: Diabolic Edict ("Target player sacrifices a
/// creature of their choice.") cast by P0 at P1 pauses on P1's choice; the
/// answer finishes the resolution, and the Edict's own move into P0's graveyard
/// triggers P0's Quest exactly once. The sacrificed creature goes to P1's
/// graveyard, which "your graveyard" does not see.
#[test]
fn quest_for_ancient_secrets_sees_a_paused_edicts_own_graveyard_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let edict = scenario.add_real_card(P0, "Diabolic Edict", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let quest = scenario.add_real_card(P0, "Quest for Ancient Secrets", Zone::Battlefield, db());
    let sacrificed = scenario.add_vanilla(P1, 2, 2);
    scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(edict).target_player(P1).commit();
    pass_until_a_prompt(&mut runner);
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { player, effect_kind: EffectKind::Sacrifice, cards, .. }
                if *player == P1 && cards.contains(&sacrificed)
        ),
        "reach guard: the Edict paused on P1's sacrifice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&edict].zone, Zone::Stack);
    assert_eq!(triggers_from(runner.state(), quest), 0);

    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrificed],
        })
        .expect("P1 sacrifices a creature");

    let state = runner.state();
    assert_eq!(state.objects[&edict].zone, Zone::Graveyard);
    assert_eq!(graveyard(state, P0), vec![edict]);
    assert_eq!(state.objects[&sacrificed].zone, Zone::Graveyard);
    assert_eq!(
        triggers_from(state, quest),
        1,
        "CR 603.2c: one card went into P0's graveyard, the Edict itself"
    );
}

const PLANAR_VOID: &str =
    "Whenever another card is put into a graveyard from anywhere, exile that card.";

/// CR 608.2n + CR 603.3b: Innocent Blood ("Each player sacrifices a creature of
/// their choice.") with a Planar Void on each side. Three cards go into
/// graveyards (P0's creature, P1's creature, Innocent Blood itself), so each
/// Void triggers three times. Both players' triggers are one batch: P0 orders
/// theirs, then P1 orders theirs (APNAP), before either group resolves.
#[test]
fn innocent_blood_with_planar_voids_orders_the_sacrifices_and_its_own_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let void_0 = scenario
        .add_enchantment_from_oracle(P0, "Planar Void", PLANAR_VOID)
        .id();
    let void_1 = scenario
        .add_enchantment_from_oracle(P1, "Planar Void", PLANAR_VOID)
        .id();
    let own_creature = scenario.add_vanilla(P0, 2, 2);
    let chosen = scenario.add_vanilla(P1, 2, 2);
    let other = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    assert!(
        runner
            .state()
            .pending_player_scope_sacrifice_choice
            .is_some(),
        "reach guard: the player-scope sacrifice route, got {:?}",
        runner.state().waiting_for
    );
    let WaitingFor::EffectZoneChoice { player, cards, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a sacrifice choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "only P1 has a creature to choose between");
    assert!(cards.contains(&chosen) && cards.contains(&other));

    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("P1 sacrifices a creature");

    let mut ordering_players = Vec::new();
    for _ in 0..4 {
        let WaitingFor::OrderTriggers { player, triggers } = runner.state().waiting_for.clone()
        else {
            break;
        };
        assert_eq!(
            triggers.len(),
            3,
            "one Void trigger per card put into a graveyard, the Blood's own move included"
        );
        ordering_players.push(player);
        runner
            .act(GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            })
            .expect("the player orders their triggers");
    }
    assert_eq!(
        ordering_players,
        vec![P0, P1],
        "CR 603.3b: APNAP, before either group resolves"
    );

    let state = runner.state();
    assert_eq!(state.objects[&blood].zone, Zone::Graveyard);
    assert_eq!(triggers_from(state, void_0), 3);
    assert_eq!(triggers_from(state, void_1), 3);

    run_until_settled(&mut runner, |_| None);
    let state = runner.state();
    for card in [blood, own_creature, chosen] {
        assert_eq!(
            state.objects[&card].zone,
            Zone::Exile,
            "each card put into a graveyard was exiled by a Void"
        );
    }
    assert_eq!(state.objects[&other].zone, Zone::Battlefield);
    assert_settled(state);
}

/// What a Reanimate-on-Clone run leaves, for the W2 rows.
struct ReanimatedClone {
    runner: GameRunner,
    reanimate: ObjectId,
    clone: ObjectId,
    bolt: Option<ObjectId>,
    /// The legal targets of the copied trigger's prompt, empty when it chose
    /// its only target without one.
    trigger_choices: Vec<TargetRef>,
}

/// Reanimate ("Put target creature card from a graveyard onto the battlefield
/// under your control. You lose life equal to that card's mana value.") on a
/// Clone ("You may have this creature enter as a copy of any creature on the
/// battlefield.") that copies Skullwinder ("When this creature enters, return
/// target card from your graveyard to your hand, then choose an opponent. That
/// player returns a card from their graveyard to their hand."). Drives the
/// copy-entry replacement choice, the copy choice and the copied trigger's
/// target choice.
///
/// Life is deliberately not read: Reanimate whose target enters through a
/// copy-entry replacement pause loses no life, at the base too; that defect is
/// tracked separately.
fn reanimate_a_clone_as_skullwinder(other_graveyard_card: bool) -> ReanimatedClone {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reanimate = scenario.add_real_card(P0, "Reanimate", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let clone = scenario.add_real_card(P0, "Clone", Zone::Graveyard, db());
    let bolt = other_graveyard_card
        .then(|| scenario.add_real_card(P0, "Lightning Bolt", Zone::Graveyard, db()));
    let skullwinder = scenario.add_real_card(P1, "Skullwinder", Zone::Battlefield, db());
    let mut runner = scenario.build();

    runner.cast(reanimate).target_object(clone).commit();
    let mut trigger_choices: Vec<TargetRef> = Vec::new();
    let mut copied = false;
    for _ in 0..30 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 0 },
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                assert!(valid_targets.contains(&skullwinder));
                copied = true;
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(skullwinder)),
                }
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                trigger_choices = target_slots[selection.current_slot].legal_targets.clone();
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(reanimate)),
                }
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                GameAction::PassPriority
            }
            _ => break,
        };
        runner.act(action).expect("the engine accepts the answer");
    }
    assert!(copied, "reach guard: the Clone was asked what to copy");
    ReanimatedClone {
        runner,
        reanimate,
        clone,
        bolt,
        trigger_choices,
    }
}

/// CR 608.2n + CR 603.3 + CR 603.3d: the copied "when this creature enters"
/// trigger chooses its targets when it is put on the stack, after the spell
/// finished, so Reanimate (the only card in P0's graveyard) is already there to
/// be chosen: the trigger is not dropped for want of a target, and returns it.
#[test]
fn a_copy_entry_trigger_sees_the_parent_spell_in_the_graveyard_when_the_graveyard_was_empty() {
    let ReanimatedClone {
        runner,
        reanimate,
        clone,
        trigger_choices,
        ..
    } = reanimate_a_clone_as_skullwinder(false);
    let state = runner.state();

    assert_eq!(state.objects[&clone].zone, Zone::Battlefield);
    assert_eq!(
        state.objects[&clone].name, "Skullwinder",
        "the Clone entered as a copy of Skullwinder"
    );
    assert!(
        trigger_choices.is_empty(),
        "a single legal target is chosen without a prompt, got {trigger_choices:?}"
    );
    assert_eq!(
        state.objects[&reanimate].zone,
        Zone::Hand,
        "the copied trigger was not dropped: it targeted Reanimate and returned it"
    );
}

/// The same with another card in P0's graveyard: Reanimate is a legal target
/// beside it, so the copied trigger prompts for the choice.
#[test]
fn a_copy_entry_trigger_sees_the_parent_spell_in_the_graveyard_among_other_graveyard_cards() {
    let ReanimatedClone {
        runner,
        reanimate,
        clone,
        bolt,
        trigger_choices,
    } = reanimate_a_clone_as_skullwinder(true);
    let state = runner.state();
    let bolt = bolt.expect("the variant has a second graveyard card");

    assert_eq!(state.objects[&clone].zone, Zone::Battlefield);
    assert_eq!(state.objects[&clone].name, "Skullwinder");
    let mut offered = trigger_choices;
    offered.sort_by_key(|target| format!("{target:?}"));
    let mut expected = vec![TargetRef::Object(bolt), TargetRef::Object(reanimate)];
    expected.sort_by_key(|target| format!("{target:?}"));
    assert_eq!(
        offered, expected,
        "reach guard: the trigger asked for a target, and Reanimate is selectable"
    );
    assert_eq!(state.objects[&reanimate].zone, Zone::Hand);
    assert_eq!(state.objects[&bolt].zone, Zone::Graveyard);
}

/// Sibling: a Clone copying a creature with no enters trigger has nothing to
/// target, and Reanimate still ends in the graveyard.
#[test]
fn a_clone_copying_a_vanilla_creature_leaves_reanimate_in_the_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reanimate = scenario.add_real_card(P0, "Reanimate", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let clone = scenario.add_real_card(P0, "Clone", Zone::Graveyard, db());
    let bears = scenario.add_vanilla(P1, 2, 2);
    let mut runner = scenario.build();

    runner.cast(reanimate).target_object(clone).commit();
    let mut copied = false;
    for _ in 0..30 {
        let action = match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => GameAction::ChooseReplacement { index: 0 },
            WaitingFor::CopyTargetChoice { .. } => {
                copied = true;
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(bears)),
                }
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                GameAction::PassPriority
            }
            _ => break,
        };
        runner.act(action).expect("the engine accepts the answer");
    }

    assert!(copied, "reach guard: the Clone was asked what to copy");
    let state = runner.state();
    assert_eq!(state.objects[&clone].zone, Zone::Battlefield);
    assert_eq!(state.objects[&reanimate].zone, Zone::Graveyard);
    assert_settled(state);
}

/// Omnath, Locus of Mana ("You don't lose unspent green mana as steps and phases
/// end.") and Horizon Stone ("If you would lose unspent mana, that mana becomes
/// colorless instead.") both apply to the mana emptying of the cleanup step, so
/// CR 616.1 has P0 order them. Casts the spell `add_spell` puts in P0's hand
/// and passes until that ordering is asked.
fn cast_into_the_cleanup_mana_order(
    add_spell: impl FnOnce(&mut GameScenario) -> ObjectId,
    pool: Vec<ManaUnit>,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_spell(&mut scenario);
    let omnath = scenario.add_real_card(P0, "Omnath, Locus of Mana", Zone::Battlefield, db());
    let stone = scenario
        .add_artifact_from_oracle(
            P0,
            "Horizon Stone",
            "If you would lose unspent mana, that mana becomes colorless instead.",
        )
        .id();
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    runner.cast(spell).commit();
    pass_until_a_prompt(&mut runner);

    let state = runner.state();
    let WaitingFor::ReplacementChoice { candidates, .. } = &state.waiting_for else {
        panic!(
            "reach guard: the cleanup mana emptying asks for an order, got {:?}",
            state.waiting_for
        );
    };
    let mut sources: Vec<ObjectId> = candidates.iter().map(|c| c.source_id).collect();
    sources.sort();
    let mut expected = vec![omnath, stone];
    expected.sort();
    assert_eq!(sources, expected, "reach guard: both replacements compete");
    assert_eq!(state.phase, Phase::Cleanup);
    (runner, spell)
}

/// CR 724.1b + CR 724.1d: Time Stop ("End the turn.") exiles every object on
/// the stack, including the resolving Time Stop, before the turn skips to the
/// cleanup step; the ordering prompt belongs to that transition, so Time Stop
/// is already in exile when it is asked, not on the stack beside it.
#[test]
fn time_stop_is_exiled_before_the_cleanup_mana_order_prompt() {
    let (mut runner, stop) = cast_into_the_cleanup_mana_order(
        |scenario| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Time Stop", true, "End the turn.")
                .id()
        },
        mana(ManaType::Green, 2),
    );
    assert_eq!(
        runner.state().objects[&stop].zone,
        Zone::Exile,
        "CR 724.1b: Time Stop was exiled by its own procedure before the prompt"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 orders the replacements");
    run_until_settled(&mut runner, |_| None);
    let state = runner.state();
    assert_eq!(state.objects[&stop].zone, Zone::Exile);
    assert_settled(state);
}

const GLORIOUS_END: &str = "End the turn. (Exile all spells and abilities from the stack, \
including this card. The player whose turn it is discards down to their maximum hand size. \
Damage wears off, and \"this turn\" and \"until end of turn\" effects end.)\n\
At the beginning of your next end step, you lose the game.";

/// CR 724.1b + CR 608.2m: Glorious End ("End the turn. ... At the beginning of
/// your next end step, you lose the game.") is exiled by its own end procedure
/// before the cleanup ordering prompt, even though an instruction follows it;
/// it then finishes resolving from exile and creates its delayed trigger once.
#[test]
fn glorious_end_is_exiled_before_the_cleanup_mana_order_prompt() {
    let (mut runner, glorious) = cast_into_the_cleanup_mana_order(
        |scenario| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Glorious End", true, GLORIOUS_END)
                .id()
        },
        mana(ManaType::Green, 2),
    );
    let state = runner.state();
    assert_eq!(
        state.objects[&glorious].zone,
        Zone::Exile,
        "CR 724.1b: Glorious End was exiled by its own procedure before the prompt"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 orders the replacements");
    run_until_settled(&mut runner, |_| None);
    let state = runner.state();
    assert_eq!(state.objects[&glorious].zone, Zone::Exile);
    assert_eq!(
        state
            .delayed_triggers
            .iter()
            .filter(|trigger| trigger.source_id == glorious)
            .count(),
        1,
        "CR 608.2m: the rider after the end procedure still ran, once"
    );
    assert_settled(state);
}

const DAYS_UNDOING: &str = "Each player shuffles their hand and graveyard into their library, \
then draws seven cards. If it's your turn, end the turn. (Exile all spells and abilities from \
the stack, including this card. Discard down to your maximum hand size. Damage wears off, and \
\"this turn\" and \"until end of turn\" effects end.)";
const HURKYLS_FINAL_MEDITATION: &str = "During turns other than yours, this spell costs {3} more \
to cast.\nReturn all nonland permanents to their owners' hands. End the turn. (Exile all spells \
and abilities from the stack, including this card. The player whose turn it is discards down to \
their maximum hand size. Damage wears off, and \"this turn\" and \"until end of turn\" effects \
end.)";
const ULTIMA: &str = "Destroy all artifacts and creatures. End the turn. (Exile all spells and \
abilities from the stack, including this card. The player whose turn it is discards down to \
their maximum hand size. Damage wears off, and \"this turn\" and \"until end of turn\" effects \
end.)";
const MANDATE_OF_PEACE: &str = "Cast this spell only during combat.\nYour opponents can't cast \
spells this turn.\nEnd the combat phase. (Remove all attackers and blockers from combat. Exile \
all spells and abilities from the stack, including this spell.)";
const LEYLINE_OF_ANTICIPATION: &str = "If this card is in your opening hand, you may begin the \
game with it on the battlefield.\nYou may cast spells as though they had flash.";
const LIBRARY: [&str; 9] = ["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9"];

/// CR 724.1b + CR 608.2m: Day's Undoing's end procedure, reached only after its
/// shuffle-and-draw instruction, exiles the resolving Day's Undoing before the
/// cleanup ordering prompt, and it finishes resolving from exile.
#[test]
fn days_undoing_on_its_controllers_turn_is_exiled_by_its_own_end_procedure() {
    let (mut runner, undoing) = cast_into_the_cleanup_mana_order(
        |scenario| {
            scenario.with_library_top(P0, &LIBRARY);
            scenario.with_library_top(P1, &LIBRARY);
            scenario
                .add_spell_to_hand_from_oracle(P0, "Day's Undoing", false, DAYS_UNDOING)
                .id()
        },
        mana(ManaType::Green, 2),
    );
    assert_eq!(
        runner.state().objects[&undoing].zone,
        Zone::Exile,
        "CR 724.1b: Day's Undoing was exiled by its own procedure before the prompt"
    );

    let mut events = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 orders the replacements")
        .events;
    events.extend(run_until_settled(&mut runner, |_| None));
    let state = runner.state();
    assert_eq!(state.objects[&undoing].zone, Zone::Exile);
    assert!(!graveyard(state, P0).contains(&undoing));
    assert_eq!(stack_resolved_count(&events, undoing), 1);
    assert_eq!(state.players[0].hand.len(), 7, "reach: the draw ran");
    assert_settled(state);
}

/// CR 724.1b: an "end the turn" spell whose resolution never pauses is exiled
/// by its own procedure, not put into the graveyard.
#[test]
fn an_end_the_turn_spell_with_no_pause_is_exiled() {
    for (name, is_instant, text, bear_zone) in [
        (
            "Hurkyl's Final Meditation",
            true,
            HURKYLS_FINAL_MEDITATION,
            Zone::Hand,
        ),
        ("Ultima", false, ULTIMA, Zone::Graveyard),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &LIBRARY);
        scenario.with_library_top(P1, &LIBRARY);
        let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, name, is_instant, text)
            .id();
        let mut runner = scenario.build();

        runner.cast(spell).commit();
        let events = run_until_settled(&mut runner, |_| None);
        let state = runner.state();
        assert_eq!(
            state.objects[&bear].zone, bear_zone,
            "reach: {name} resolved"
        );
        assert_eq!(state.phase, Phase::Cleanup, "reach: {name} ended the turn");
        assert_eq!(
            state.objects[&spell].zone,
            Zone::Exile,
            "CR 724.1b: {name} is exiled by its own procedure"
        );
        assert_eq!(stack_resolved_count(&events, spell), 1);
        assert_settled(state);
    }
}

/// CR 724.2b: Mandate of Peace, cast during combat, is exiled by its own
/// "end the combat phase" procedure.
#[test]
fn mandate_of_peace_is_exiled_by_ending_the_combat_phase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let mandate = scenario
        .add_spell_to_hand_from_oracle(P0, "Mandate of Peace", true, MANDATE_OF_PEACE)
        .id();
    let mut runner = scenario.build();

    runner.cast(mandate).commit();
    run_until_settled(&mut runner, |_| None);
    let state = runner.state();
    assert_eq!(state.phase, Phase::PostCombatMain, "reach: combat ended");
    assert_eq!(state.objects[&mandate].zone, Zone::Exile);
    assert_settled(state);
}

/// Day's Undoing cast on an opponent's turn runs no end procedure ("If it's
/// your turn"), so it goes to its owner's graveyard and the spell below it
/// stays on the stack.
#[test]
fn days_undoing_on_an_opponents_turn_is_not_exiled() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &LIBRARY);
    scenario.with_library_top(P1, &LIBRARY);
    scenario.add_enchantment_from_oracle(P1, "Leyline of Anticipation", LEYLINE_OF_ANTICIPATION);
    let below = scenario
        .add_spell_to_hand_from_oracle(P0, "Below", true, "Draw a card.")
        .id();
    let undoing = scenario
        .add_spell_to_hand_from_oracle(P1, "Day's Undoing", false, DAYS_UNDOING)
        .id();
    let mut runner = scenario.build();

    runner.cast(below).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes with Below on the stack");
    runner.cast(undoing).commit();
    for _ in 0..2 {
        runner
            .act(GameAction::PassPriority)
            .expect("both players pass");
    }
    let state = runner.state();
    assert_eq!(
        state.objects[&below].zone,
        Zone::Stack,
        "reach: no end procedure exiled the spell below"
    );
    assert_eq!(state.phase, Phase::PreCombatMain);
    assert!(graveyard(state, P1).contains(&undoing));
}

/// CR 724.2g: "end the combat phase" outside combat does nothing, so the
/// resolving spell is not exiled and goes to the graveyard.
#[test]
fn ending_the_combat_phase_outside_combat_does_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &LIBRARY);
    let interrupt = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Combat Interrupt",
            true,
            "End the combat phase. Draw a card.",
        )
        .id();
    let mut runner = scenario.build();

    runner.cast(interrupt).commit();
    run_until_settled(&mut runner, |_| None);
    let state = runner.state();
    assert_eq!(state.players[0].hand.len(), 1, "reach: the draw ran");
    assert_eq!(state.phase, Phase::PreCombatMain);
    assert_eq!(state.objects[&interrupt].zone, Zone::Graveyard);
    assert_settled(state);
}

/// A decoded game state at a priority window holding one bare spell-resolution
/// frame: an empty stack, the resolving carrier naming a spell object of the
/// given type in the given zone, and no other continuation. Returns the state
/// and the object's id.
fn bare_spell_resolution_frame(core_type: CoreType, zone: Zone) -> (GameState, ObjectId) {
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 0x57AC);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    let spell = create_object(&mut state, CardId(77), P0, "Rested Spell".to_string(), zone);
    {
        let object = state.objects.get_mut(&spell).expect("the object exists");
        object.card_types.core_types.push(core_type);
        object.base_card_types = object.card_types.clone();
    }
    state.resolving_stack_entry = Some(StackEntry {
        id: spell,
        source_id: spell,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    state.push_spell_resolution(PendingSpellResolution {
        object_id: spell,
        controller: P0,
        casting_variant: CastingVariant::Normal,
        cast_from_zone: None,
        cast_controller: Some(P0),
        cast_timing_permission: None,
        spell_targets: vec![],
        actual_mana_spent: 0,
        kickers_paid: vec![],
        additional_cost_payment_count: 0,
        additional_cost_payments: vec![],
        convoked_creatures: vec![],
    });
    (state, spell)
}

/// What a stale pass over a decoded state leaves: it is accepted, and an
/// instant or sorcery's carrier is not settled out from under it.
fn assert_stale_pass_keeps_the_carrier_and_the_spell(mut state: GameState, spell: ObjectId) {
    apply(&mut state, P0, GameAction::PassPriority)
        .expect("the priority holder may submit a stale pass");
    assert_eq!(
        state.resolving_stack_entry.as_ref().map(|entry| entry.id),
        Some(spell),
        "the carrier is kept"
    );
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Stack,
        "the spell is not stranded by a settled carrier"
    );
}

/// CR 608.2n: a bare spell-resolution frame beside an instant or sorcery that
/// is still in the stack zone is live final-delivery debt, not a terminal rest:
/// the spell has not made its move to the graveyard, and settling the carrier
/// would strand it there. Restore fails closed with the existing error, with no
/// owed marker needed to say so.
#[test]
fn a_persisted_bare_spell_rest_naming_a_stacked_instant_or_sorcery_fails_closed() {
    for core_type in [CoreType::Sorcery, CoreType::Instant] {
        let (state, spell) = bare_spell_resolution_frame(core_type, Zone::Stack);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. })
                && state.resolution_stack.len() == 1
                && state.owed_spell_final_part().is_none()
                && state.resolving_stack_entry.as_ref().map(|entry| entry.id) == Some(spell),
            "reach guard: a bare markerless frame naming the carrier ({core_type:?})"
        );

        let restore = PersistedGameState::Raw(Box::new(state.clone()))
            .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated);
        assert_eq!(
            restore.err(),
            Some(PersistedRestoreError::UnsettledPriorityResolution),
            "{core_type:?} in the stack zone"
        );
        assert_stale_pass_keeps_the_carrier_and_the_spell(state, spell);
    }
}

/// Discriminating controls for the refusal above, each markerless and each
/// restoring: a permanent spell (in the stack zone, or already on the
/// battlefield, the shape the permanent-entry epilogue leaves) and an instant or
/// sorcery already in the graveyard are genuine terminal rests the restore
/// settles. A refusal of every typed carrier, or of every object in the stack
/// zone, would fail one of these.
#[test]
fn a_persisted_bare_spell_rest_naming_a_spell_that_has_left_the_stack_still_settles() {
    for (core_type, zone) in [
        (CoreType::Creature, Zone::Battlefield),
        (CoreType::Creature, Zone::Stack),
        (CoreType::Sorcery, Zone::Graveyard),
    ] {
        let (state, _spell) = bare_spell_resolution_frame(core_type, zone);
        let restored = PersistedGameState::Raw(Box::new(state))
            .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
            .unwrap_or_else(|error| panic!("{core_type:?} in {zone:?} must restore: {error:?}"))
            .finalize_after_rehydration(|_| Ok(()))
            .expect("the settled state is publishable");

        assert!(
            restored.resolving_stack_entry.is_none() && restored.resolution_stack.is_empty(),
            "{core_type:?} in {zone:?}: the carrier is settled"
        );
        assert_eq!(restored.waiting_for, WaitingFor::Priority { player: P0 });
    }
}

/// CR 608.2n: the same refusal when the snapshot also carries the owed
/// final-part marker (the shape the engine itself writes): restore fails closed
/// and a stale pass leaves the carrier, the spell and the marker alone.
#[test]
fn a_persisted_bare_spell_rest_owing_a_final_part_fails_closed() {
    let (state, spell) = bare_spell_resolution_frame(CoreType::Sorcery, Zone::Stack);
    let mut raw = serde_json::to_value(&state).expect("the state serializes");
    raw["resolving_spell_final_part"] =
        serde_json::to_value(OwedSpellFinalPart { spell }).expect("the marker serializes");
    let persisted: PersistedGameState =
        serde_json::from_value(raw.clone()).expect("the snapshot decodes");
    assert!(
        serde_json::to_value(&persisted)
            .expect("the decoded snapshot reserializes")
            .get("resolving_spell_final_part")
            .is_some(),
        "reach guard: the decoded snapshot carries the owed final part"
    );

    assert_eq!(
        persisted
            .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
            .err(),
        Some(PersistedRestoreError::UnsettledPriorityResolution)
    );

    let mut decoded: GameState = serde_json::from_value(raw).expect("the state decodes");
    assert!(decoded.owed_spell_final_part().is_some());
    apply(&mut decoded, P0, GameAction::PassPriority)
        .expect("the priority holder may submit a stale pass");
    assert_eq!(
        decoded.resolving_stack_entry.as_ref().map(|entry| entry.id),
        Some(spell)
    );
    assert_eq!(decoded.objects[&spell].zone, Zone::Stack);
    assert!(
        decoded.owed_spell_final_part().is_some(),
        "the marker is kept"
    );
}

/// The residue a markerless terminal-rest fixture carries beside its carrier.
#[derive(Clone, Copy, Debug)]
enum TerminalRest {
    Devour,
    OwnerlessSweep,
}

fn markerless_rest(rest: TerminalRest, core_type: CoreType, zone: Zone) -> (GameState, ObjectId) {
    use engine::types::game_state::{
        DrainStatus, PostReplacementDrain, PostReplacementDrainStack, ResidentDrainPolicy,
    };
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 0x57AC);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    let spell = create_object(&mut state, CardId(77), P0, "Rested Spell".to_string(), zone);
    {
        let object = state.objects.get_mut(&spell).expect("the object exists");
        object.card_types.core_types.push(core_type);
        object.base_card_types = object.card_types.clone();
    }
    state.resolving_stack_entry = Some(StackEntry {
        id: spell,
        source_id: spell,
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    let mut drains = PostReplacementDrainStack::default();
    if matches!(rest, TerminalRest::OwnerlessSweep) {
        drains.install(
            PostReplacementDrain {
                status: DrainStatus::Dispatching,
                source: None,
                applied: Default::default(),
                event_source: None,
                event_target: None,
                controller: None,
            },
            ResidentDrainPolicy::Replace,
        );
    }
    state.resolution_stack.push_post_replacement(drains);
    if matches!(rest, TerminalRest::Devour) {
        state.push_devour_change_zone_snapshot([ObjectId(27)].into_iter().collect());
    }
    (state, spell)
}

/// CR 608.2n: the two terminal-rest recoveries that settle a resolving carrier
/// beside residue other than a bare spell-resolution frame (a Devour-only
/// snapshot under an empty replacement parent, and an ownerless `Dispatching`
/// post-replacement drain) are guarded the way the bare-frame recovery is: a
/// markerless instant or sorcery still in the stack zone has not made its move
/// to the graveyard, and settling its carrier would strand it there.
fn assert_restore_refuses(rest: TerminalRest, expected: PersistedRestoreError) {
    for core_type in [CoreType::Sorcery, CoreType::Instant] {
        let (state, spell) = markerless_rest(rest, core_type, Zone::Stack);
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. })
                && state.owed_spell_final_part().is_none()
                && state.stack.is_empty()
                && !state.resolution_stack.is_empty()
                && state.resolving_stack_entry.as_ref().map(|entry| entry.id) == Some(spell)
                && state.objects[&spell].zone == Zone::Stack,
            "reach guard: a markerless {core_type:?} carrier in the stack zone beside the {rest:?} rest"
        );
        assert_eq!(
            PersistedGameState::Raw(Box::new(state))
                .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
                .err(),
            Some(expected.clone()),
            "{core_type:?} in the stack zone beside the {rest:?} rest"
        );
    }
}

#[test]
fn a_persisted_devour_rest_naming_a_stacked_instant_or_sorcery_fails_closed() {
    assert_restore_refuses(
        TerminalRest::Devour,
        PersistedRestoreError::TerminalRestRecoveryExhausted,
    );
}

#[test]
fn a_stale_pass_does_not_recover_a_devour_rest_naming_a_stacked_instant_or_sorcery() {
    for core_type in [CoreType::Sorcery, CoreType::Instant] {
        let (mut state, spell) = markerless_rest(TerminalRest::Devour, core_type, Zone::Stack);
        apply(&mut state, P0, GameAction::PassPriority)
            .expect("the priority holder may submit a stale pass");
        assert_eq!(
            state.resolving_stack_entry.as_ref().map(|entry| entry.id),
            Some(spell),
            "{core_type:?}: the carrier is kept"
        );
        assert_eq!(
            state.objects[&spell].zone,
            Zone::Stack,
            "{core_type:?}: the spell is not stranded by a settled carrier"
        );
    }
}

#[test]
fn an_ownerless_sweep_rest_naming_a_stacked_instant_or_sorcery_fails_closed() {
    assert_restore_refuses(
        TerminalRest::OwnerlessSweep,
        PersistedRestoreError::UnsettledPriorityResolution,
    );
}

/// Discriminating controls for the refusals above, over both rests: a
/// permanent spell in the stack zone and an instant or sorcery already in the
/// graveyard are genuine terminal rests the restore settles. A refusal of every
/// carrier, or of every object in the stack zone, would fail one of these.
#[test]
fn a_terminal_rest_naming_a_spell_that_has_left_the_stack_still_settles() {
    for rest in [TerminalRest::Devour, TerminalRest::OwnerlessSweep] {
        for (core_type, zone) in [
            (CoreType::Creature, Zone::Stack),
            (CoreType::Sorcery, Zone::Graveyard),
        ] {
            let (state, _spell) = markerless_rest(rest, core_type, zone);
            let restored = PersistedGameState::Raw(Box::new(state))
                .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
                .unwrap_or_else(|error| {
                    panic!("{core_type:?} in {zone:?} beside {rest:?} must restore: {error:?}")
                })
                .finalize_after_rehydration(|_| Ok(()))
                .expect("the settled state is publishable");
            assert!(
                restored.resolving_stack_entry.is_none() && restored.resolution_stack.is_empty(),
                "{core_type:?} in {zone:?} beside {rest:?}: the carrier is settled"
            );
            assert_eq!(restored.waiting_for, WaitingFor::Priority { player: P0 });
        }
    }
}

const HARSH_MERCY: &str =
    "Each player chooses a creature type. Destroy all creatures that aren't of \
    a type chosen this way. They can't be regenerated.";

/// Harsh Mercy cast by P0 in a three-player game, paused on a creature-type
/// choice owned by `owner`. Returns the runner and the spell.
fn harsh_mercy_paused_on_a_choice_of(owner: PlayerId) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mercy = scenario
        .add_spell_to_hand_from_oracle(P0, "Harsh Mercy", false, HARSH_MERCY)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(P0, mana(ManaType::White, 1));
    let mut runner = scenario.build();

    runner.cast(mercy).commit();
    // Each player chooses in turn order: P0 first, then P1, then P2.
    for _ in 0..12 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is accepted");
            }
            WaitingFor::NamedChoice {
                player, options, ..
            } if *player != owner => {
                let choice = options.first().expect("a creature type is offered").clone();
                runner
                    .act(GameAction::ChooseOption { choice })
                    .expect("the earlier player names a creature type");
            }
            _ => break,
        }
    }
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::NamedChoice { player, source: Some(source), .. }
                if *player == owner && source.prompt.controller == P0
        ),
        "reach guard: the resolution paused on {owner:?}'s creature-type choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner
            .state()
            .owed_spell_final_part()
            .map(|owed| owed.spell),
        Some(mercy),
        "reach guard: the paused sorcery owes its final part"
    );
    assert_eq!(
        runner.state().objects[&mercy].zone,
        Zone::Stack,
        "reach guard: the paused sorcery is still on the stack"
    );
    (runner, mercy)
}

/// CR 608.2n + CR 800.4a: the sorcery's controller is not the player who
/// leaves. P1, who owns one of Harsh Mercy's creature-type choices, concedes;
/// the choice is abandoned with its resolution, and the spell, which is
/// P0's and stays in the game, is put into its owner's graveyard as its final
/// part, exactly once, instead of being stranded in the stack zone.
#[test]
fn a_spell_whose_non_controller_prompt_owner_leaves_finishes_into_its_owners_graveyard() {
    let (mut runner, mercy) = harsh_mercy_paused_on_a_choice_of(P1);

    let mut events = concede(&mut runner, P1);
    events.extend(run_until_settled(&mut runner, |_| None));

    let state = runner.state();
    assert_eq!(
        state.objects[&mercy].zone,
        Zone::Graveyard,
        "CR 608.2n: the spell is put into its owner's graveyard"
    );
    assert_eq!(graveyard(state, P0), vec![mercy]);
    assert_eq!(
        stack_resolved_count(&events, mercy),
        1,
        "the resolution is reported exactly once"
    );
    assert_settled(state);
}

/// Control: the leaver is the sorcery's controller (and owner), the existing
/// CR 800.4a path. The spell leaves the game with them and is never reported
/// resolved; nothing is stranded.
#[test]
fn a_spell_whose_controller_owns_the_prompt_and_leaves_goes_with_them() {
    let (mut runner, mercy) = harsh_mercy_paused_on_a_choice_of(P0);

    let mut events = concede(&mut runner, P0);
    events.extend(run_until_settled(&mut runner, |_| None));

    let state = runner.state();
    assert_eq!(
        state.objects[&mercy].zone,
        Zone::Exile,
        "CR 800.4a: the departed player's spell leaves the game with them"
    );
    assert!(graveyard(state, P0).is_empty());
    assert_eq!(stack_resolved_count(&events, mercy), 0);
    assert_settled(state);
}

/// Harsh Mercy cast by P0 and stolen with Commandeer by P1 in a three-player
/// game, paused on P2's creature-type choice while P1 controls the spell.
fn commandeered_harsh_mercy_paused_on_p2s_choice() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mercy = scenario
        .add_spell_to_hand_from_oracle(P0, "Harsh Mercy", false, HARSH_MERCY)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let commandeer = scenario.add_real_card(P1, "Commandeer", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::White, 1));
    scenario.with_mana_pool(P1, mana(ManaType::Blue, 7));
    let mut runner = scenario.build();

    runner.cast(mercy).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    runner.cast(commandeer).target_object(mercy).commit();
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is accepted");
            }
            // "You may choose new targets for it." Harsh Mercy has none.
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("P1 keeps the spell's targets");
            }
            WaitingFor::NamedChoice {
                player, options, ..
            } if *player != PlayerId(2) => {
                let choice = options.first().expect("a creature type is offered").clone();
                runner
                    .act(GameAction::ChooseOption { choice })
                    .expect("the earlier player names a creature type");
            }
            _ => break,
        }
    }
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::NamedChoice { player, source: Some(source), .. }
                if *player == PlayerId(2) && source.prompt.controller == P1
        ),
        "reach guard: the stolen resolution paused on P2's creature-type choice, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&mercy].controller,
        P1,
        "reach guard: the thief controls the spell at the moment of concession"
    );
    assert_eq!(
        runner
            .state()
            .owed_spell_final_part()
            .map(|owed| owed.spell),
        Some(mercy),
        "reach guard: the paused sorcery owes its final part"
    );
    assert_eq!(runner.state().objects[&mercy].zone, Zone::Stack);
    (runner, mercy)
}

/// CR 800.4a: the thief of a paused spell leaves while another player is
/// answering. The control effect ends first and the spell reverts to its
/// surviving owner, so it is not exiled with the thief: it finishes into its
/// owner's graveyard, reported resolved once.
#[test]
fn a_commandeered_paused_spell_whose_thief_leaves_reverts_to_its_owner_and_finishes() {
    let (mut runner, mercy) = commandeered_harsh_mercy_paused_on_p2s_choice();

    let mut events = concede(&mut runner, P1);
    events.extend(run_until_settled(&mut runner, |_| None));

    let state = runner.state();
    assert_eq!(
        state.objects[&mercy].zone,
        Zone::Graveyard,
        "CR 800.4a: the reverted spell is not exiled; it finishes into its owner's graveyard"
    );
    assert_eq!(graveyard(state, P0), vec![mercy]);
    assert_eq!(stack_resolved_count(&events, mercy), 1);
    assert_settled(state);
}
