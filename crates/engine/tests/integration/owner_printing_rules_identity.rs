//! Rules identity of display-only data: the consumed trigger-event occurrence relation.
//!
//! CR 603.2c: an ability triggers once each time its trigger event occurs, and the
//! consumed-occurrence journal is how a second collector is kept from triggering it again
//! for an occurrence the first already collected. An occurrence is a pair (event, ordinal),
//! where the ordinal counts the earlier events of the whole action buffer that are typed
//! `==` to it (`triggers::trigger_event_occurrence`). Every consumer matches claims with
//! that relation, so every producer of claims must number with it. The stack batch paths
//! numbered by serialized string instead; a pair that is typed-equal but serializes apart
//! (a reported life total, a display-only printing; CR 203.1 + CR 206.1: a printing has no
//! effect on game play) was given ordinals the consumers read differently, leaving a
//! collected occurrence collectable.
//!
//! Where typed equality and serialized identity disagree. Typed-equal but serialized apart:
//! the always-equal `LifeTotalReading` in `LifeChanged`, and every `DisplayOnly` printing
//! carrier reachable from a `GameEvent` (the look-back snapshot in `ZoneChangeRecord` and
//! `AbilityActivated`, `LibrarySearchCardView`). Serialized-equal but typed apart:
//! `CounterType::Loyalty` against `CounterType::Generic("loyalty")` in `CounterAdded`.
//! `ResolvedAbility` equality compares the aligned target pins (it does not ignore
//! `target_pins`); its separate divergence is the plural `cost_paid_objects`, compared by
//! object-id sequence only. The producer takes its ordinals from the consumers' relation, so
//! all of these are consumer-consistent by construction.
//!
//! The numbering change is intentional: wherever the two relations disagree, the producer's
//! ordinals now are the consumers'. A non-empty prefix of the caller's buffer that is equal
//! to a claimed event also changes the ordinals a locally numbered producer would write, even
//! when serialization and typed equality agree; the claims are exact for the whole buffer.
//!
//! Persistence and replay. The ledger is serialized with the game state. Persisted
//! reconciliation visits it as a live event carrier and binds records to ledger occurrences
//! by their typed fingerprint (below), which is a separate authority and not a second ordinal
//! allocator. Action replay clones checkpoints and calls the reducer, so it never renumbers a
//! claim, and applying a `ConsumeBeforePriority` collection extends the claims unchanged.
//!
//! Persisted zone-change reconciliation (CR 400.7 + CR 603.2c + CR 203.1 + CR 206.1). A
//! persisted `ZoneChanged` record is bound to its ledger occurrence by the rules projection of
//! the typed record (decoded through `ZoneChangeRecord`'s own compatibility decoding, without
//! its two occurrence stamps), so a printing neither splits one occurrence into two nor selects
//! between two rules-identical ones; the loaded state keeps every original value, printing
//! included. A record that needs a new binding follows the two-source rule: (a) the candidates
//! are the ledger rows with the same rules projection; (b) exactly one candidate binds; (c) two
//! or more fall back to today's raw-JSON comparison, but only when the record and every
//! candidate carry no display data, so a printing never chooses; (d) no candidate is an error.
//! The stamped fast path, the unstamped pass-through and the check that every record naming one
//! occurrence key agrees compare two printing-free records raw, as before, and any pair with
//! display data by the rules projection. An undecodable record is a load error.
//!
//! Cost. On the measured native fixtures, equality numbering cost approximately 0.5% or less
//! of the measured resolver time. The measured single-token runs produced four events per
//! member. WASM timing and other admitted event-volume shapes were not measured.
//!
//! Casting reversal (CR 733.1 + CR 104.4b): the descriptor keeps a restoration list (the exact
//! inverse, printing included, the only list a cancel applies) and a comparison list (the
//! inverse of the rules projection), recorded together at every capture point; two states
//! whose reversals differ only in display-only data are the same state, a descriptor saved
//! before the comparison list existed compares by its restoration list and never equals a new
//! one.
//!
//! Transition-counter completeness: every change of the delivered rules state advances
//! `GameState::state_revision`, including the action boundary's repair on a rejected action
//! and contest driver steps that change the contest but not its visible signature; a state
//! installed in place of the live one continues the replaced counter.
//!
//! Viewer prompt identities bind the delivered, filtered typed prompt after shared private
//! redactions and removal of display-only data. The published revision is equality-only;
//! it intentionally discloses internal repair occurrence/count, never repair contents.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::stack::resolve_next_with_limit;
use engine::game::triggers::ConsumedTriggerEventScope;
use engine::types::ability::{AbilityCost, QuantityExpr, StaticDefinition};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_printing::CardPrinting;
use engine::types::display_only::{rules_projection, DisplayOnly};
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoPassRequest, CastPaymentMode, GameState, PersistedGameState, StackEntryKind,
    StackResolutionAutoPassOverlay, StackResolutionBudget, StackResolutionEntryFence,
    StackResolutionPolicy, StackResolutionSession, TrustedGameStateEnvelope, WaitingFor,
    ZoneChangeRecord,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{AdditionalCostTaxAction, StaticMode};
use engine::types::zones::Zone;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

const BOGWATER_ORACLE: &str =
    "Whenever this creature or another creature you control enters, you gain 1 life.";

/// Synthetic spell: no printed card is claimed, it only produces the five token entries.
const TOKEN_FLOOD_ORACLE: &str = "Create five 1/1 green Insect creature tokens.";

const TOKEN_COUNT: usize = 5;

/// The test-local oracle for the consumers' relation: how many earlier events of the
/// buffer are typed `==` to each event.
fn equality_occurrences(events: &[GameEvent]) -> Vec<usize> {
    (0..events.len())
        .map(|index| {
            events[..index]
                .iter()
                .filter(|earlier| **earlier == events[index])
                .count()
        })
        .collect()
}

/// The committed session Resolve All authorizes, fenced over the whole stack.
fn arm_committed_session(state: &mut GameState) {
    state.stack_resolution_session = Some(StackResolutionSession {
        entries: state
            .stack
            .iter()
            .rev()
            .map(StackResolutionEntryFence::capture)
            .collect(),
        cursor: 0,
        representatives: BTreeSet::from([P0]),
        verified_pass_representatives: BTreeSet::new(),
        budget: StackResolutionBudget::Unlimited,
        policy: StackResolutionPolicy::Committed,
        auto_pass_overlay: StackResolutionAutoPassOverlay {
            baseline: BTreeMap::new(),
        },
    });
}

/// CR 603.2c + CR 119.3: five tokens enter under Bogwater Lumaret, so five identical
/// "you gain 1 life" triggers resolve as one batch. Each gain is one occurrence of the same
/// typed event (the reported totals 21..25 are display-only), so the batch must claim them
/// as ordinals 0..4 and the caller's checkpoint must consume every one.
#[test]
fn bogwater_life_gain_batch_claims_every_gain_at_its_consumer_occurrence() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.add_creature_from_oracle(P0, "Bogwater Lumaret", 2, 2, BOGWATER_ORACLE);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Token Flood", true, TOKEN_FLOOD_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id: CardId(spell.0),
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast token flood");
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("the spell resolves through a committed stack session");
    assert_eq!(
        runner.state().stack.len(),
        TOKEN_COUNT,
        "reach: the spell left exactly its five life-gain triggers"
    );

    // Resolve the five triggers through the production resolver, with the session Resolve
    // All installs, and read the claims before the post-action pipeline takes them.
    let mut state = runner.state().clone();
    arm_committed_session(&mut state);
    let mut events = Vec::new();
    assert_eq!(
        resolve_next_with_limit(&mut state, &mut events, Some(u32::MAX)),
        TOKEN_COUNT as u32
    );

    let totals: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::LifeChanged { new_total, .. } => Some(new_total.0),
            _ => None,
        })
        .collect();
    assert_eq!(totals, (21..=25).map(Some).collect::<Vec<_>>());
    assert_eq!(state.players[0].life, 25);

    let claims = &state.consumed_before_priority_trigger_events;
    assert_eq!(claims.len(), events.len(), "every batch event is claimed");
    assert!(
        claims
            .iter()
            .all(|claim| claim.scope == ConsumedTriggerEventScope::AllCollectors),
        "the batch claims for every collector"
    );
    assert_eq!(
        claims
            .iter()
            .map(|claim| claim.occurrence)
            .collect::<Vec<_>>(),
        equality_occurrences(&events),
        "each ordinal is the one the consumers number the buffer with"
    );
    for (claim, event) in claims.iter().zip(&events) {
        assert_eq!(claim.event, *event);
    }
    assert_eq!(
        claims
            .iter()
            .filter(|claim| matches!(claim.event, GameEvent::LifeChanged { .. }))
            .map(|claim| claim.occurrence)
            .collect::<Vec<_>>(),
        (0..TOKEN_COUNT).collect::<Vec<_>>(),
        "the five gains are five occurrences of one event"
    );
}

// ---------------------------------------------------------------------------
// Casting reversal: restoration list and comparison list
// ---------------------------------------------------------------------------

const BITTER_TRIUMPH_ORACLE: &str = "As an additional cost to cast this spell, discard a card or pay 3 life.\nDestroy target creature or planeswalker.";

fn printing(set_code: &str) -> DisplayOnly<Option<CardPrinting>> {
    DisplayOnly::new(Some(CardPrinting {
        set_code: set_code.to_string(),
        collector_number: "1".to_string(),
    }))
}

fn stamp(state: &mut GameState, id: ObjectId, set_code: &str) {
    let object = state
        .objects
        .get_mut(&id)
        .expect("the stamped object exists");
    object.printing = printing(set_code);
    object.base_printing = printing(set_code);
}

fn set_code(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects[&id]
        .printing
        .get()
        .as_ref()
        .map(|printing| printing.set_code.clone())
}

fn base_set_code(state: &GameState, id: ObjectId) -> Option<String> {
    state.objects[&id]
        .base_printing
        .get()
        .as_ref()
        .map(|printing| printing.set_code.clone())
}

/// Number of nodes whose `set_code` is `code`.
fn sentinels(value: &Value, code: &str) -> usize {
    match value {
        Value::Object(map) => {
            usize::from(map.get("set_code").and_then(Value::as_str) == Some(code))
                + map
                    .values()
                    .map(|child| sentinels(child, code))
                    .sum::<usize>()
        }
        Value::Array(items) => items.iter().map(|child| sentinels(child, code)).sum(),
        _ => 0,
    }
}

/// The descriptor's JSON (the descriptor is crate-private, so tests read its wire form).
fn reversal(state: &GameState) -> Value {
    serde_json::to_value(state).expect("state serializes")["spell_cast_reversal"].clone()
}

/// The path of one serialized cast-undo edit (`{"Value": {"path": [..], ..}}`).
fn edit_path(edit: &Value) -> Vec<String> {
    edit.as_object()
        .and_then(|variants| variants.values().next())
        .and_then(|body| body["path"].as_array())
        .expect("an edit carries its path")
        .iter()
        .map(|part| part.as_str().expect("path parts are strings").to_string())
        .collect()
}

fn edits(list: &Value) -> &Vec<Value> {
    list.as_array().expect("an edit list is an array")
}

fn holds_path(list: &Value, path: &[&str]) -> bool {
    edits(list).iter().any(|edit| edit_path(edit) == path)
}

fn is_printing_edit_of(edit: &Value, id: ObjectId) -> bool {
    let path = edit_path(edit);
    let key = id.0.to_string();
    path.len() == 3
        && path[0] == "objects"
        && path[1] == key
        && (path[2] == "printing" || path[2] == "base_printing")
}

fn without_printing_edits_of(list: &Value, id: ObjectId) -> Vec<Value> {
    edits(list)
        .iter()
        .filter(|edit| !is_printing_edit_of(edit, id))
        .cloned()
        .collect()
}

fn fund(runner: &mut GameRunner, amount: usize) {
    for _ in 0..amount {
        runner
            .state_mut()
            .add_mana_to_pool(
                P0,
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            )
            .expect("the player has a mana pool");
    }
}

fn tap(runner: &mut GameRunner, land: ObjectId) {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&land)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("the engine offers the land's mana ability while paying");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap the land");
}

/// A persisted decode path: the state as written, read back.
type Reload = fn(&GameState) -> GameState;

fn reload_raw(state: &GameState) -> GameState {
    serde_json::from_str(&serde_json::to_string(state).expect("state serializes"))
        .expect("the raw decoder accepts a state it wrote")
}

fn reload_trusted(state: &GameState) -> GameState {
    let text = serde_json::to_string(&TrustedGameStateEnvelope::capture(state.clone()))
        .expect("envelope serializes");
    serde_json::from_str::<TrustedGameStateEnvelope>(&text)
        .expect("the trusted decoder accepts an envelope it wrote")
        .into_game_state()
}

/// The raw decode of `state`'s JSON after `edit` rewrote it.
fn raw_decode_after(state: &GameState, edit: impl FnOnce(&mut Value)) -> GameState {
    let mut json = serde_json::to_value(state).expect("state serializes");
    edit(&mut json);
    serde_json::from_value(json).expect("the edited state decodes")
}

fn without_reversal(state: &GameState) -> GameState {
    raw_decode_after(state, |json| {
        json.as_object_mut()
            .expect("a state is an object")
            .remove("spell_cast_reversal");
    })
}

/// A descriptor saved before the comparison list existed: the same state with that key gone.
fn legacy_of(state: &GameState) -> GameState {
    raw_decode_after(state, |json| {
        let descriptor = json["spell_cast_reversal"]
            .as_object_mut()
            .expect("reach: the state holds a descriptor");
        assert!(
            descriptor.remove("comparison_undo").is_some(),
            "reach: the descriptor held a comparison list"
        );
    })
}

/// A morph card in P0's hand and an opposing synthetic source of an unconditional "cast a
/// spell: pay 2 life" tax (no printed card is claimed), so one action both pays a cost and
/// conceals the printing. Returns the runner paused at mana payment after the face-down
/// choice, the morph card and P0's lands.
fn face_down_under_tax(players: u8, code: &str) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 9771)
    };
    scenario.at_phase(Phase::PreCombatMain);
    let morph = scenario
        .add_creature_to_hand(P0, "Secret Beast", 4, 5)
        .with_keyword(Keyword::Morph(ManaCost::generic(4)))
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let tax = scenario.add_creature(P1, "Tax Source", 1, 1).id();
    let lands: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Green))
        .collect();
    let mut runner = scenario.build();
    stamp(runner.state_mut(), morph, code);
    {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&tax)
            .expect("the tax source exists");
        let definition = StaticDefinition::new(StaticMode::ImposeAdditionalCost {
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            spell_filter: None,
            action: AdditionalCostTaxAction::Cast,
        });
        object.static_definitions.push(definition.clone());
        std::sync::Arc::make_mut(&mut object.base_static_definitions).push(definition);
    }
    engine::game::layers::flush_layers(runner.state_mut());
    fund(&mut runner, 5);
    let card_id = runner.state().objects[&morph].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: morph,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce the morph card");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::AlternativeCastChoice { .. }
        ),
        "reach: the face-down choice is pending, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choose the face-down cost");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the cast waits at mana payment, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[0].life,
        18,
        "reach: the tax was paid by the action that concealed the printing"
    );
    (runner, morph, lands)
}

/// CR 733.1: a cancelled cast returns the card to hand face up, with its real name and the
/// printing it held before the cast, inner values included.
fn assert_cancelled_with_printing(runner: &GameRunner, morph: ObjectId, code: &str) {
    let object = &runner.state().objects[&morph];
    assert_eq!(object.zone, Zone::Hand, "reach: back in hand");
    assert!(!object.face_down, "reach: face up again");
    assert_eq!(object.name, "Secret Beast", "reach: the real name is back");
    assert_eq!(set_code(runner.state(), morph).as_deref(), Some(code));
    assert_eq!(base_set_code(runner.state(), morph).as_deref(), Some(code));
    assert_eq!(
        runner.state().players[0].life,
        20,
        "reach: the tax is refunded"
    );
    assert!(runner.state().pending_cast.is_none());
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// CR 104.4b + CR 203.1 + CR 206.1: two boards whose casting reversals differ only in the
/// stamped printing are the same game state. The restoration list differs (it must return each
/// printing on cancel); the comparison list is the same and holds no printing.
#[test]
fn face_down_casts_whose_reversals_differ_only_in_a_printing_are_one_state() {
    let (a, morph, _) = face_down_under_tax(2, "SAAA");
    let (b, morph_b, _) = face_down_under_tax(2, "SBBB");
    assert_eq!(morph, morph_b, "reach: the boards are the same board");
    let (ra, rb) = (reversal(a.state()), reversal(b.state()));

    assert_eq!(
        ra["cost_mutations"], true,
        "reach: a cost mutation is recorded"
    );
    assert_ne!(
        ra["undo"], rb["undo"],
        "reach: the operands differ in the descriptor"
    );
    assert_eq!(sentinels(&ra["undo"], "SAAA"), 2, "{ra}");
    assert_eq!(sentinels(&rb["undo"], "SBBB"), 2, "{rb}");
    for field in ["printing", "base_printing"] {
        assert!(
            holds_path(&ra["undo"], &["objects", &morph.0.to_string(), field]),
            "reach: the restoration list holds the {field} edit: {ra}"
        );
    }

    let comparison = &ra["comparison_undo"];
    assert!(
        !edits(comparison).is_empty(),
        "reach: the comparison list records the paid cost: {ra}"
    );
    assert_eq!(comparison, &rb["comparison_undo"]);
    for code in ["SAAA", "SBBB"] {
        assert_eq!(
            sentinels(comparison, code),
            0,
            "the comparison list holds {code}"
        );
    }
    assert!(
        a.state() == b.state(),
        "CR 104.4b: printing-only reversals are one state"
    );

    // A live twin with the same printing is equal too: the boards are deterministic, so the
    // descriptor was the only separating field.
    let (twin, _, _) = face_down_under_tax(2, "SAAA");
    assert!(a.state() == twin.state());
}

/// CR 104.4b: a difference in the rules identity still separates states. Both operands go
/// through the same decode, which resets runtime-only fields a live state does not share.
#[test]
fn a_rules_difference_in_the_casting_reversal_still_separates_states() {
    let (a, _, _) = face_down_under_tax(2, "SAAA");
    let plain = reload_raw(a.state());
    assert!(
        plain == reload_raw(a.state()),
        "reach: two decodes of one state are equal"
    );

    let edited = raw_decode_after(a.state(), |json| {
        let list = json["spell_cast_reversal"]["comparison_undo"]
            .as_array_mut()
            .expect("the comparison list is present");
        let edit = list
            .iter_mut()
            .find(|edit| edit_path(edit) == ["players", "0", "life"])
            .expect("reach: the paid life is a comparison edit");
        assert_eq!(edit["Value"]["before"], 20);
        edit["Value"]["before"] = Value::from(19);
    });
    assert!(
        edited != plain,
        "CR 104.4b: a rules difference in the comparison list separates states"
    );
    assert!(
        without_reversal(&edited) == without_reversal(&plain),
        "reach: the descriptor is the only difference"
    );
}

/// CR 733.1 + CR 104.4b: a second payment folds the prior inverse into both lists, so the
/// identity stays printing-free and a cancel still restores the printing exactly.
#[test]
fn a_later_payment_folds_both_lists_and_cancel_restores_the_printing() {
    let (mut a, morph, lands) = face_down_under_tax(2, "SAAA");
    let (mut b, _, _) = face_down_under_tax(2, "SBBB");
    let (undo_before, comparison_before) = {
        let r = reversal(a.state());
        (r["undo"].clone(), r["comparison_undo"].clone())
    };

    tap(&mut a, lands[0]);
    tap(&mut b, lands[0]);
    let (ra, rb) = (reversal(a.state()), reversal(b.state()));
    for runner in [&a, &b] {
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
            "reach: still paying, got {:?}",
            runner.state().waiting_for
        );
    }
    assert_ne!(
        ra["undo"], undo_before,
        "reach: the fold rewrote the restoration list"
    );
    assert_ne!(
        ra["comparison_undo"], comparison_before,
        "reach: the fold rewrote the comparison list"
    );
    assert_eq!(
        sentinels(&ra["undo"], "SAAA"),
        2,
        "the printing is still restorable"
    );
    assert_eq!(sentinels(&ra["comparison_undo"], "SAAA"), 0);
    assert_eq!(ra["comparison_undo"], rb["comparison_undo"]);
    // The fold witness: the folded comparison list is the folded restoration list minus
    // exactly its two printing edits.
    assert_eq!(
        edits(&ra["undo"])
            .iter()
            .filter(|edit| is_printing_edit_of(edit, morph))
            .count(),
        2,
        "the face-down printing is the only one: {ra}"
    );
    assert_eq!(
        edits(&ra["comparison_undo"]),
        &without_printing_edits_of(&ra["undo"], morph),
        "the comparison list was folded with the restoration list"
    );
    assert!(
        a.state() == b.state(),
        "CR 104.4b: printing-only reversals stay one state after a fold"
    );

    a.act(GameAction::CancelCast).expect("cancel A");
    assert_cancelled_with_printing(&a, morph, "SAAA");
    b.act(GameAction::CancelCast).expect("cancel B");
    assert_cancelled_with_printing(&b, morph, "SBBB");
}

/// CR 733.1 + CR 800.4a: a third player's concession is rebased into both lists, and cancelling
/// the spell neither restores that player nor loses the printing.
#[test]
fn a_concession_rebases_both_lists_and_cancel_keeps_it() {
    let p2 = PlayerId(2);
    let (mut a, morph, _) = face_down_under_tax(3, "SKAA");
    let (mut b, _, _) = face_down_under_tax(3, "SKBB");
    let before = reversal(a.state());

    for runner in [&mut a, &mut b] {
        runner
            .act(GameAction::Concede { player_id: p2 })
            .expect("the third player concedes independently");
        assert!(runner.state().players[2].is_eliminated, "reach: P2 is out");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
    }
    let after = reversal(a.state());
    assert_ne!(
        after["undo"], before["undo"],
        "reach: the rebase rewrote the restoration list"
    );
    assert_eq!(sentinels(&after["undo"], "SKAA"), 2);
    assert!(
        a.state() == b.state(),
        "CR 104.4b: printing-only reversals stay one state after a rebase"
    );

    // The rebase witness: the comparison list is the rebased restoration list minus exactly
    // its two printing edits, and it moved with the rebase.
    let printing_edits = edits(&after["undo"])
        .iter()
        .filter(|edit| is_printing_edit_of(edit, morph))
        .count();
    assert_eq!(
        printing_edits, 2,
        "the face-down printing is the only one: {after}"
    );
    assert_eq!(
        edits(&after["comparison_undo"]),
        &without_printing_edits_of(&after["undo"], morph),
        "the comparison list was rebased with the restoration list"
    );
    assert_ne!(
        after["comparison_undo"], before["comparison_undo"],
        "the pre-concession comparison list is stale after the concession"
    );

    a.act(GameAction::CancelCast).expect("cancel the spell");
    assert!(
        a.state().players[2].is_eliminated,
        "CR 800.4a: the concession is not a casting payment"
    );
    assert_cancelled_with_printing(&a, morph, "SKAA");
}

fn discard_cast(code: &str) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Target", 3, 3);
    let fodder = scenario.add_card_to_hand(P0, "Discard Fodder");
    let spell = scenario
        .add_spell_to_hand(P0, "Bitter Triumph", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .from_oracle_text(BITTER_TRIUMPH_ORACLE)
        .id();
    let mut runner = scenario.build();
    stamp(runner.state_mut(), fodder, code);
    for _ in 0..2 {
        runner
            .state_mut()
            .add_mana_to_pool(
                P0,
                ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]),
            )
            .expect("the player has a mana pool");
    }
    (runner, spell, fodder)
}

fn pay_by_discarding(runner: &mut GameRunner, spell: ObjectId, fodder: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("announce Bitter Triumph");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "reach: the additional cost is offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("choose to discard");
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("discard the fodder");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "reach: the cast waits at mana payment, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 733.1: a cost that moves a printed card (an ordinary move, not a concealment) is
/// restored exactly: the card returns to hand with its printing, its zone-change record is
/// gone, and the hand is as it was.
#[test]
fn cancelling_a_discard_paid_cast_returns_the_printed_card_exactly() {
    let (mut runner, spell, fodder) = discard_cast("STAA");
    let hand_before = runner.state().players[0].hand.clone();
    let records_before =
        serde_json::to_value(&runner.state().zone_changes_this_turn).expect("records serialize");
    assert_eq!(
        edits(&records_before).len(),
        0,
        "reach: no record before the cast"
    );

    pay_by_discarding(&mut runner, spell, fodder);
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "reach: discarded"
    );
    let descriptor = reversal(runner.state());
    assert_eq!(
        descriptor["cost_mutations"], true,
        "reach: the discard is a cost mutation"
    );
    assert_eq!(
        sentinels(&descriptor["undo"], "STAA"),
        0,
        "an ordinary move puts no printing into the restoration list"
    );
    let records =
        serde_json::to_value(&runner.state().zone_changes_this_turn).expect("records serialize");
    assert_eq!(
        edits(&records).len(),
        1,
        "reach: one zone-change record: {records}"
    );
    assert_eq!(
        sentinels(&records, "STAA"),
        1,
        "reach: its look-back snapshot carries the printing"
    );

    // Twins that differ only in the discarded card's printing are one state.
    let (mut twin, twin_spell, twin_fodder) = discard_cast("STBB");
    pay_by_discarding(&mut twin, twin_spell, twin_fodder);
    assert!(
        runner.state() == twin.state(),
        "CR 104.4b: printing-only discard casts are one state"
    );

    runner.act(GameAction::CancelCast).expect("cancel the cast");
    let object = &runner.state().objects[&fodder];
    assert_eq!(
        object.zone,
        Zone::Hand,
        "CR 733.1: the discarded card is back"
    );
    assert_eq!(set_code(runner.state(), fodder).as_deref(), Some("STAA"));
    assert_eq!(
        base_set_code(runner.state(), fodder).as_deref(),
        Some("STAA")
    );
    assert_eq!(
        serde_json::to_value(&runner.state().zone_changes_this_turn).expect("records serialize"),
        records_before,
        "the cancelled cast leaves no zone-change record"
    );
    assert!(runner.state().players[0].hand == hand_before);
    assert!(runner.state().pending_cast.is_none());
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// CR 733.1 + CR 104.4b: a persisted state keeps both lists on the raw and the trusted decode
/// paths, still compares by the identity, and a reloaded cancel restores the printing.
#[test]
fn a_reloaded_reversal_keeps_both_lists_and_compares_the_same() {
    let (a, morph, _) = face_down_under_tax(2, "SAAA");
    let (b, _, _) = face_down_under_tax(2, "SBBB");
    let original = reversal(a.state());
    assert!(
        !edits(&original["comparison_undo"]).is_empty(),
        "reach: a comparison list is saved"
    );
    assert_eq!(sentinels(&original["undo"], "SAAA"), 2);

    let paths: [(&str, Reload); 2] = [("raw", reload_raw), ("trusted", reload_trusted)];
    for (label, reload) in paths {
        let reloaded_a = reload(a.state());
        let reloaded_b = reload(b.state());
        assert_eq!(
            reversal(&reloaded_a),
            original,
            "{label}: the descriptor survives the reload byte for byte"
        );
        assert!(
            reload(a.state()) == reloaded_a,
            "{label}: two decodes of one state are equal"
        );
        assert!(
            without_reversal(&reloaded_a) == without_reversal(&reloaded_b),
            "{label}: reach: the descriptor is the only difference"
        );
        assert!(
            reloaded_a == reloaded_b,
            "{label}: CR 104.4b: printing-only reversals are one state after a reload"
        );

        let mut runner = GameRunner::from_state(reloaded_a);
        runner
            .act(GameAction::CancelCast)
            .expect("cancel after the reload");
        assert_cancelled_with_printing(&runner, morph, "SAAA");
    }
}

/// CR 733.1: a descriptor saved before the comparison list existed compares by its restoration
/// list, never equals a new descriptor, gains no comparison list through a payment or a
/// concession, and still cancels exactly.
#[test]
fn a_legacy_reversal_compares_by_its_restoration_list_and_keeps_no_comparison_list() {
    let (a, morph, lands) = face_down_under_tax(2, "SAAA");
    let (b, _, _) = face_down_under_tax(2, "SBBB");
    let new_descriptor = reload_raw(a.state());
    let legacy_a = legacy_of(a.state());
    let legacy_b = legacy_of(b.state());

    let saved = reversal(&legacy_a);
    assert!(
        saved.get("comparison_undo").is_none(),
        "reach: the legacy shape has no list"
    );
    assert_eq!(
        saved["undo"],
        reversal(a.state())["undo"],
        "reach: same restoration list"
    );
    assert!(
        legacy_a != new_descriptor,
        "a legacy descriptor never equals a new one"
    );
    assert!(new_descriptor != legacy_a, "and the relation is symmetric");
    assert!(legacy_a == legacy_a.clone());
    assert!(
        legacy_a == reload_raw(&legacy_a),
        "a legacy descriptor survives a raw reload"
    );
    assert!(
        reload_trusted(&legacy_a) == reload_trusted(&legacy_a),
        "and a trusted reload"
    );
    assert!(
        reversal(&reload_trusted(&legacy_a))
            .get("comparison_undo")
            .is_none(),
        "a reload never invents a list"
    );
    assert!(
        legacy_a != legacy_b,
        "legacy twins compare by their restoration lists, as before the list existed"
    );

    // A payment folds without gaining a comparison list.
    let mut runner = GameRunner::from_state(legacy_a.clone());
    tap(&mut runner, lands[0]);
    let folded = reversal(runner.state());
    assert_ne!(
        folded["undo"], saved["undo"],
        "reach: the fold rewrote the restoration list"
    );
    assert!(
        folded.get("comparison_undo").is_none(),
        "no list after a fold: {folded}"
    );
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the legacy cast");
    assert_cancelled_with_printing(&runner, morph, "SAAA");

    // A concession rebases without gaining one either.
    let (three, morph3, _) = face_down_under_tax(3, "SKAA");
    let mut runner = GameRunner::from_state(legacy_of(three.state()));
    let before = reversal(runner.state());
    runner
        .act(GameAction::Concede {
            player_id: PlayerId(2),
        })
        .expect("the third player concedes");
    let rebased = reversal(runner.state());
    assert_ne!(
        rebased["undo"], before["undo"],
        "reach: the rebase rewrote the list"
    );
    assert!(
        rebased.get("comparison_undo").is_none(),
        "no list after a rebase: {rebased}"
    );
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the legacy cast");
    assert!(runner.state().players[2].is_eliminated);
    assert_cancelled_with_printing(&runner, morph3, "SKAA");
}

fn load_dump() -> GameState {
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        include_bytes!("../fixtures/dina_conqueror_4p.json.gz").as_slice(),
    )
    .read_to_string(&mut json)
    .expect("fixture .json.gz inflates to UTF-8 JSON");
    let mut envelope: Value = serde_json::from_str(&json).expect("dump envelope parses as JSON");
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    engine::types::game_state::record_captured_onsets(&mut envelope);
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
        .expect("persisted snapshot satisfies the checked restore contract")
}

/// CR 203.1 + CR 206.1: the rules projection of a whole real `GameState` omits every printing.
/// This is the soundness constraint the comparison list rests on: nothing beneath `GameState`
/// serializes a printing through already-erased JSON, so projecting the typed state sees them all.
#[test]
fn whole_game_state_projection_omits_every_printing_on_a_real_board() {
    let mut state = load_dump();
    let objects = state.objects.len();
    assert!(objects > 0, "reach: the dump holds objects");
    assert_eq!(
        rules_projection(&state).expect("projects"),
        serde_json::to_value(&state).expect("serializes"),
        "reach: with no printing the projection is the wire form"
    );

    let ids: Vec<ObjectId> = state.objects.keys().copied().collect();
    for id in ids {
        stamp(&mut state, id, "SENT");
    }
    let wire = serde_json::to_value(&state).expect("serializes");
    assert_eq!(
        sentinels(&wire, "SENT"),
        2 * objects,
        "reach: the wire form holds every object's two printings"
    );
    let projected = rules_projection(&state).expect("projects");
    assert_eq!(
        sentinels(&projected, "SENT"),
        0,
        "the projection holds none"
    );
}

// ---------------------------------------------------------------------------
// Persisted zone-change reconciliation: the typed fingerprint and the two-source rule
// ---------------------------------------------------------------------------

/// Printed card text of Murder (Scryfall, checked 2026-10-24): the board's removal spell.
const MURDER_ORACLE: &str = "Destroy target creature.";

/// Synthetic observer (no printed card is claimed): it only yields a real battlefield-to-
/// graveyard record and a dies trigger that waits on the stack carrying that record.
const DIES_DRAW: &str = "When this creature dies, you draw a card.";

type Fields = serde_json::Map<String, Value>;

/// Board B (stamped) or B0 (`None`): P0's "Doomed Bear" dies to Murder through the production
/// reducer, and the game is left at priority with the bear's dies trigger as the only stack
/// entry, the trigger carrying the bear's ledger record as its `trigger_event`.
fn dies_trigger_board(stamp_code: Option<&str>) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);
    let bear = scenario
        .add_creature(P0, "Doomed Bear", 2, 2)
        .from_oracle_text(DIES_DRAW)
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    if let Some(code) = stamp_code {
        stamp(runner.state_mut(), bear, code);
    }
    drop(runner.cast(murder).target_object(bear).commit());
    for _ in 0..8 {
        let names = runner.stack_names();
        if !names.is_empty() && !names.iter().any(|name| name == "Murder") {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward Murder's resolution");
    }
    assert_eq!(
        runner.stack_names(),
        ["Doomed Bear"],
        "reach: only the dies trigger waits"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    let state = runner.state().clone();
    let record = trigger_record(&state);
    assert_eq!(
        record.object_id, bear,
        "reach: the trigger carries the bear"
    );
    assert_eq!(
        lki_set_code(record).as_deref(),
        stamp_code,
        "reach: the look-back snapshot holds the stamped printing"
    );
    (state, bear)
}

/// The dies trigger's `trigger_event` record, through the typed stack entry.
fn trigger_record(state: &GameState) -> &ZoneChangeRecord {
    state
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                trigger_event: Some(GameEvent::ZoneChanged { record, .. }),
                ..
            } => Some(&**record),
            _ => None,
        })
        .expect("a triggered ability carries a zone-change event")
}

/// The printing inside a record's look-back snapshot (the inner value).
fn lki_set_code(record: &ZoneChangeRecord) -> Option<String> {
    record.trigger_source_context.as_ref().and_then(|context| {
        context
            .lki
            .printing
            .get()
            .as_ref()
            .map(|printing| printing.set_code.clone())
    })
}

fn stamps(record: &ZoneChangeRecord) -> (u32, usize) {
    (record.recorded_turn_number, record.turn_zone_change_index)
}

/// The state as the raw (versioned) and the trusted persisted forms.
fn persisted_forms(state: &GameState) -> [(&'static str, Value); 2] {
    [
        (
            "raw",
            serde_json::to_value(PersistedGameState::Raw(Box::new(state.clone())))
                .expect("raw form serializes"),
        ),
        (
            "trusted",
            serde_json::to_value(PersistedGameState::capture(state.clone()))
                .expect("trusted form serializes"),
        ),
    ]
}

/// The state object of a persisted form: the `state` member of a trusted one, else the value.
fn state_payload(form: &Value) -> &Value {
    form.get("state").unwrap_or(form)
}

fn state_payload_mut(form: &mut Value) -> &mut Value {
    if form.get("state").is_some() {
        form.get_mut("state").expect("trusted form has a state")
    } else {
        form
    }
}

/// Production load: `PersistedGameState` deserialization, then the checked restore.
fn reload(form: Value) -> Result<GameState, String> {
    serde_json::from_value::<PersistedGameState>(form)
        .map_err(|error| error.to_string())?
        .into_game_state()
        .map_err(|error| error.to_string())
}

/// Reload a copy of `form` after `edit` rewrote its state object.
fn reload_edited(form: &Value, edit: impl FnOnce(&mut Value)) -> Result<GameState, String> {
    let mut form = form.clone();
    edit(state_payload_mut(&mut form));
    reload(form)
}

fn at<'a>(root: &'a Value, path: &[String]) -> &'a Value {
    path.iter().fold(root, |node, part| match node {
        Value::Array(items) => &items[part.parse::<usize>().expect("array index")],
        other => &other[part.as_str()],
    })
}

fn at_mut<'a>(root: &'a mut Value, path: &[String]) -> &'a mut Value {
    path.iter().fold(root, |node, part| match node {
        Value::Array(items) => &mut items[part.parse::<usize>().expect("array index")],
        other => other.get_mut(part.as_str()).expect("path key exists"),
    })
}

fn record_at_mut<'a>(payload: &'a mut Value, path: &[String]) -> &'a mut Fields {
    at_mut(payload, path)
        .as_object_mut()
        .expect("a record is an object")
}

/// Every serialized `GameEvent::ZoneChanged` record outside the ledger and the append-only
/// journal, as the path of the record, optionally only those of `object`.
fn live_record_paths(payload: &Value, object: Option<ObjectId>) -> Vec<Vec<String>> {
    fn walk(
        value: &Value,
        path: &mut Vec<String>,
        object: Option<ObjectId>,
        found: &mut Vec<Vec<String>>,
    ) {
        match value {
            Value::Object(map) => {
                let suffix: Option<[&str; 2]> =
                    if map.get("type").and_then(Value::as_str) == Some("ZoneChanged") {
                        Some(["data", "record"])
                    } else if map.contains_key("ZoneChanged") {
                        Some(["ZoneChanged", "record"])
                    } else {
                        None
                    };
                if let Some(suffix) = suffix {
                    let record = &map[suffix[0]][suffix[1]];
                    if object.is_none_or(|object| record["object_id"].as_u64() == Some(object.0)) {
                        let mut record_path = path.clone();
                        record_path.extend(suffix.iter().map(|part| (*part).to_string()));
                        found.push(record_path);
                    }
                    return;
                }
                for (key, child) in map {
                    path.push(key.clone());
                    walk(child, path, object, found);
                    path.pop();
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    path.push(index.to_string());
                    walk(child, path, object, found);
                    path.pop();
                }
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    for (key, child) in payload.as_object().expect("a state is an object") {
        if key == "zone_changes_this_turn" || key == "resolved_rules_journal" {
            continue;
        }
        walk(child, &mut vec![key.clone()], object, &mut found);
    }
    found
}

/// The bear's one live record (the helper asserts the count).
fn bear_live_record_path(payload: &Value, bear: ObjectId) -> Vec<String> {
    let mut paths = live_record_paths(payload, Some(bear));
    assert_eq!(paths.len(), 1, "reach: the bear has one live record");
    paths.remove(0)
}

fn edit_live(payload: &mut Value, bear: ObjectId, edit: impl FnOnce(&mut Fields)) {
    let path = bear_live_record_path(payload, bear);
    edit(record_at_mut(payload, &path));
}

fn bear_row_index(payload: &Value, bear: ObjectId) -> usize {
    payload["zone_changes_this_turn"]
        .as_array()
        .expect("the ledger is an array")
        .iter()
        .position(|row| row["object_id"].as_u64() == Some(bear.0))
        .expect("the ledger holds the bear's row")
}

fn row_path(row: usize) -> Vec<String> {
    vec!["zone_changes_this_turn".to_string(), row.to_string()]
}

fn erase_stamps(record: &mut Fields) {
    record.remove("recorded_turn_number");
    record.remove("turn_zone_change_index");
}

fn set_stamps(record: &mut Fields, turn: u32, index: usize) {
    record.insert("recorded_turn_number".to_string(), Value::from(turn));
    record.insert("turn_zone_change_index".to_string(), Value::from(index));
}

/// Fixture construction only: write or remove the printing in a record's look-back snapshot.
fn set_lki_printing(record: &mut Fields, code: Option<&str>) {
    let lki = record
        .get_mut("trigger_source_context")
        .and_then(|context| context.get_mut("lki"))
        .and_then(Value::as_object_mut)
        .expect("the record carries a look-back snapshot");
    match code {
        Some(code) => {
            lki.insert(
                "printing".to_string(),
                serde_json::json!({"set_code": code, "collector_number": "1"}),
            );
        }
        None => {
            lki.remove("printing");
        }
    }
}

/// Append a copy of ledger row `row`, changed by `edit`, and return its index.
fn append_ledger_twin(payload: &mut Value, row: usize, edit: impl FnOnce(&mut Fields)) -> usize {
    let ledger = payload["zone_changes_this_turn"]
        .as_array_mut()
        .expect("the ledger is an array");
    let mut twin = ledger[row].clone();
    edit(twin.as_object_mut().expect("a row is an object"));
    ledger.push(twin);
    ledger.len() - 1
}

/// Copy the bear's one live event into `deferred_entry_events` (a second live carrier) and
/// return the paths of the original record and of the copy's record.
fn duplicate_live_record_into_deferred_entry_events(
    payload: &mut Value,
    bear: ObjectId,
) -> (Vec<String>, Vec<String>) {
    let original = bear_live_record_path(payload, bear);
    let event_path = &original[..original.len() - 2];
    let event = at(payload, event_path).clone();
    let carrier = payload
        .as_object_mut()
        .expect("a state is an object")
        .entry("deferred_entry_events")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("the carrier is an array");
    carrier.push(event);
    let mut copy = vec![
        "deferred_entry_events".to_string(),
        (carrier.len() - 1).to_string(),
    ];
    copy.extend_from_slice(&original[original.len() - 2..]);
    (original, copy)
}

/// A board and its two persisted forms, with the bear's ledger row.
fn bear_board(
    stamp_code: Option<&str>,
) -> (GameState, ObjectId, usize, [(&'static str, Value); 2]) {
    let (state, bear) = dies_trigger_board(stamp_code);
    let forms = persisted_forms(&state);
    let row = bear_row_index(state_payload(&forms[0].1), bear);
    assert_eq!(
        sentinels(
            &state_payload(&forms[0].1)["zone_changes_this_turn"][row],
            "SPAA"
        ),
        usize::from(stamp_code.is_some()),
        "reach: the ledger row holds the stamped printing"
    );
    (state, bear, row, forms)
}

fn assert_bound(reloaded: &GameState, turn: u32, row: usize, context: &str) {
    assert_eq!(
        stamps(trigger_record(reloaded)),
        (turn, row),
        "{context}: the trigger's record names the bear's ledger occurrence"
    );
}

fn assert_load_error(result: Result<GameState, String>, expected: &str, context: &str) {
    let error = match result {
        Ok(_) => panic!("{context}: the load must fail with '{expected}'"),
        Err(error) => error,
    };
    assert!(
        error.contains(expected),
        "{context}: expected '{expected}', got '{error}'"
    );
}

/// CR 400.7 + CR 603.2c + CR 203.1 + CR 206.1: a record that differs from its ledger row only
/// in a printing is still that row's occurrence, however the printing differs and whether or
/// not it carries its stamps; the loaded state keeps every original value.
#[test]
fn a_printing_only_difference_keeps_the_record_on_its_occurrence() {
    // (board's printing, the live record's printing after the edit, erase the stamps)
    let cases: [(Option<&str>, Option<&str>, bool); 4] = [
        (Some("SPAA"), Some("SQQQ"), true),
        (Some("SPAA"), Some("SQQQ"), false),
        (Some("SPAA"), None, true),
        (None, Some("SQQQ"), true),
    ];
    for (board_stamp, live_printing, erase) in cases {
        let (state, bear, row, forms) = bear_board(board_stamp);
        let turn = state.turn_number;
        for (label, form) in forms {
            let context =
                format!("{label}: board {board_stamp:?}, live {live_printing:?}, erase {erase}");
            // Reach: the unedited record binds the same occurrence once its stamps are gone,
            // so the printing edit is what the rows below test.
            let control = reload_edited(&form, |payload| {
                edit_live(payload, bear, erase_stamps);
            })
            .expect("the unedited board loads");
            assert_bound(&control, turn, row, &context);

            let mut edited = form.clone();
            edit_live(state_payload_mut(&mut edited), bear, |record| {
                set_lki_printing(record, live_printing);
                if erase {
                    erase_stamps(record);
                }
            });
            let payload = state_payload(&edited);
            let live = at(payload, &bear_live_record_path(payload, bear));
            assert_eq!(
                sentinels(live, "SQQQ"),
                usize::from(live_printing.is_some()),
                "{context}: the live record holds the edited printing before the reload"
            );

            let reloaded = reload(edited)
                .unwrap_or_else(|error| panic!("{context}: the load must succeed: {error}"));
            assert_bound(&reloaded, turn, row, &context);
            assert_eq!(
                lki_set_code(trigger_record(&reloaded)).as_deref(),
                live_printing,
                "{context}: the loaded record keeps its own printing"
            );
            assert_eq!(
                lki_set_code(&reloaded.zone_changes_this_turn[row]).as_deref(),
                board_stamp,
                "{context}: the ledger row keeps its own printing"
            );
        }
    }
}

/// CR 400.7: a difference in a rules value still separates a record from its occurrence, with a
/// display-bearing ledger (board B) and a display-free one (board B0): an empty candidate set is
/// the same error whatever the ledger holds.
#[test]
fn a_rules_difference_still_separates_occurrences() {
    for board_stamp in [Some("SPAA"), None] {
        let (state, bear, row, forms) = bear_board(board_stamp);
        let turn = state.turn_number;
        for (label, form) in forms {
            let context = format!("{label}: board {board_stamp:?}");
            // Reach: a printing-only edit on the same board binds.
            let printing_only = reload_edited(&form, |payload| {
                edit_live(payload, bear, |record| {
                    set_lki_printing(record, Some("SQQQ"));
                    erase_stamps(record);
                });
            })
            .expect("a printing-only edit loads");
            assert_bound(&printing_only, turn, row, &context);

            let stronger = |record: &mut Fields| {
                record.insert("power".to_string(), serde_json::json!(9));
            };
            assert_load_error(
                reload_edited(&form, |payload| edit_live(payload, bear, stronger)),
                "no ledger occurrence",
                &format!("{context}: stamped"),
            );
            let unstamped = reload_edited(&form, |payload| {
                edit_live(payload, bear, |record| {
                    stronger(record);
                    erase_stamps(record);
                });
            })
            .expect("an unstamped record without an occurrence passes through");
            assert_eq!(
                stamps(trigger_record(&unstamped)),
                (0, 0),
                "{context}: the unbound record keeps serde's compatibility form"
            );
        }
    }
}

/// CR 603.2c + CR 203.1 + CR 206.1: a printing never selects among rules-identical ledger rows
/// (an unstamped record is ambiguous), a stamped record keeps its own row, and two carriers of
/// the same occurrence stay on the keys they hold.
#[test]
fn a_printing_never_chooses_between_rules_identical_occurrences() {
    let (state, bear, row, forms) = bear_board(Some("SPAA"));
    let turn = state.turn_number;
    let twin_index = state.zone_changes_this_turn.len();
    for (label, form) in forms {
        let with_printing_twin = |payload: &mut Value| {
            let twin = append_ledger_twin(payload, row, |record| {
                set_lki_printing(record, Some("SQQQ"));
            });
            assert_eq!(twin, twin_index, "reach: the twin is appended last");
        };

        // (i) An unstamped record matches both rows by its rules: ambiguous, never chosen.
        assert_load_error(
            reload_edited(&form, |payload| {
                with_printing_twin(payload);
                edit_live(payload, bear, erase_stamps);
            }),
            "ambiguously matches",
            &format!("{label} (i)"),
        );
        // Control: a twin that differs in a rules value is no candidate, so the record binds.
        let rules_twin = reload_edited(&form, |payload| {
            let twin = append_ledger_twin(payload, row, |record| {
                record.insert("power".to_string(), serde_json::json!(9));
            });
            assert_eq!(twin, twin_index);
            edit_live(payload, bear, erase_stamps);
        })
        .expect("a rules-different twin is not a candidate");
        assert_bound(&rules_twin, turn, row, &format!("{label} control"));

        // (ii) A stamped record that carries the other printing stays on its own row.
        let kept = reload_edited(&form, |payload| {
            with_printing_twin(payload);
            edit_live(payload, bear, |record| {
                set_lki_printing(record, Some("SQQQ"));
            });
        })
        .expect("a coherent stamped record loads");
        assert_bound(&kept, turn, row, &format!("{label} (ii)"));
        assert_eq!(
            lki_set_code(trigger_record(&kept)).as_deref(),
            Some("SQQQ"),
            "{label} (ii): the record keeps its printing"
        );
        assert_eq!(
            lki_set_code(&kept.zone_changes_this_turn[twin_index]).as_deref(),
            Some("SQQQ"),
            "{label} (ii): the twin is present with its printing"
        );

        // (iii) Two carriers of the bear's record, each stamped to a different row of the
        // printing-only pair, keep their own stamps.
        let both = reload_edited(&form, |payload| {
            with_printing_twin(payload);
            let (original, copy) = duplicate_live_record_into_deferred_entry_events(payload, bear);
            set_stamps(record_at_mut(payload, &original), turn, twin_index);
            let copy = record_at_mut(payload, &copy);
            set_lki_printing(copy, Some("SQQQ"));
            set_stamps(copy, turn, row);
        })
        .expect("two coherent stamped carriers load");
        assert_eq!(
            stamps(trigger_record(&both)),
            (turn, twin_index),
            "{label} (iii): the stack carrier keeps its stamp"
        );
        let GameEvent::ZoneChanged { record, .. } = &both.deferred_entry_events[0] else {
            panic!("{label} (iii): the copy is a zone-change event");
        };
        assert_eq!(stamps(record), (turn, row), "{label} (iii): the copy too");
    }
}

/// CR 603.2c + CR 203.1 + CR 206.1: a display-bearing candidate disables the raw fallback, so a
/// display-free record is ambiguous between a printing-bearing twin and its raw-identical row;
/// two raw-identical candidates are ambiguous too.
#[test]
fn a_display_bearing_candidate_disables_the_raw_fallback() {
    let (state, bear, row, forms) = bear_board(None);
    let turn = state.turn_number;
    for (label, form) in forms {
        let alone = reload_edited(&form, |payload| edit_live(payload, bear, erase_stamps))
            .expect("the unedited display-free board loads");
        assert_bound(&alone, turn, row, &format!("{label} control"));

        assert_load_error(
            reload_edited(&form, |payload| {
                append_ledger_twin(payload, row, |record| {
                    set_lki_printing(record, Some("SQQQ"));
                });
                edit_live(payload, bear, erase_stamps);
            }),
            "ambiguously matches",
            &format!("{label}: display-bearing twin"),
        );
        assert_load_error(
            reload_edited(&form, |payload| {
                append_ledger_twin(payload, row, |_| {});
                edit_live(payload, bear, erase_stamps);
            }),
            "ambiguously matches",
            &format!("{label}: raw-identical twin"),
        );
    }
}

/// How a legacy alias differs from the original row without differing in any rules value.
type AliasEdit = fn(&mut Fields);

fn alias_base_power_null(record: &mut Fields) {
    record.insert("base_power".to_string(), Value::Null);
}

fn alias_base_power_omitted(record: &mut Fields) {
    record.remove("base_power");
}

fn alias_unknown_field(record: &mut Fields) {
    record.insert("zz_probe_unknown".to_string(), serde_json::json!(1));
}

fn alias_arrival_omitted(record: &mut Fields) {
    record.remove("arrival");
}

fn without_stamps(record: &Value) -> Value {
    let mut record = record.clone();
    erase_stamps(record.as_object_mut().expect("a record is an object"));
    record
}

/// CR 400.7 + CR 603.2c: display-free legacy aliases keep today's unique binding through the raw
/// fallback: of two rules-identical rows, the one that is raw-equal to the record binds.
#[test]
fn legacy_aliases_keep_todays_unique_binding() {
    // (label, the original row's edit, the twin's and the record's edit)
    let aliases: [(&str, AliasEdit, AliasEdit); 3] = [
        (
            "base_power null vs omitted",
            alias_base_power_null,
            alias_base_power_omitted,
        ),
        ("ignored unknown field", |_| {}, alias_unknown_field),
        ("arrival omitted", |_| {}, alias_arrival_omitted),
    ];
    let (state, bear, row, forms) = bear_board(None);
    let turn = state.turn_number;
    let twin_index = state.zone_changes_this_turn.len();
    for (label, form) in &forms {
        for (alias, row_edit, variant_edit) in aliases {
            let context = format!("{label}: {alias}");
            let mut edited = form.clone();
            let payload = state_payload_mut(&mut edited);
            row_edit(record_at_mut(payload, &row_path(row)));
            let twin = append_ledger_twin(payload, row, variant_edit);
            assert_eq!(twin, twin_index);
            edit_live(payload, bear, |record| {
                variant_edit(record);
                erase_stamps(record);
            });
            // Reach: the two rows are distinct in raw JSON, and the record matches only the twin.
            let payload = state_payload(&edited);
            let live = at(payload, &bear_live_record_path(payload, bear));
            let rows = &payload["zone_changes_this_turn"];
            assert_ne!(without_stamps(&rows[row]), without_stamps(&rows[twin]));
            assert_ne!(without_stamps(&rows[row]), without_stamps(live));
            assert_eq!(without_stamps(&rows[twin]), without_stamps(live));

            let reloaded =
                reload(edited).unwrap_or_else(|error| panic!("{context}: must load: {error}"));
            assert_bound(&reloaded, turn, twin, &context);
        }

        // Two raw-identical rows cannot be told apart.
        assert_load_error(
            reload_edited(form, |payload| {
                append_ledger_twin(payload, row, |_| {});
                edit_live(payload, bear, erase_stamps);
            }),
            "ambiguously matches",
            &format!("{label}: raw-identical rows"),
        );
    }

    // On a stamped board the record carries a printing, which disables the fallback.
    let (_, bear, row, forms) = bear_board(Some("SPAA"));
    for (label, form) in forms {
        assert_load_error(
            reload_edited(&form, |payload| {
                alias_base_power_null(record_at_mut(payload, &row_path(row)));
                append_ledger_twin(payload, row, alias_base_power_omitted);
                edit_live(payload, bear, |record| {
                    alias_base_power_omitted(record);
                    erase_stamps(record);
                });
            }),
            "ambiguously matches",
            &format!("{label}: stamped board"),
        );
    }
}

/// CR 400.7 + CR 603.2c: a display-free record with exactly one typed candidate binds (rule
/// (b)), even when it is not raw-equal to it; the unstamped pass-through still compares
/// display-free records raw and leaves such a record unbound.
#[test]
fn a_single_typed_candidate_binds_a_legacy_alias_record() {
    let (state, bear, row, forms) = bear_board(None);
    let turn = state.turn_number;
    for (label, form) in forms {
        let with_alias_record = |stamping: Option<(u32, usize)>| {
            move |payload: &mut Value| {
                alias_base_power_null(record_at_mut(payload, &row_path(row)));
                edit_live(payload, bear, |record| {
                    alias_base_power_omitted(record);
                    match stamping {
                        Some((turn, index)) => set_stamps(record, turn, index),
                        None => erase_stamps(record),
                    }
                });
            }
        };

        // Reach: with the key restored the stamped record is the row's own and binds by the
        // fast path.
        let own = reload_edited(&form, |payload| {
            alias_base_power_null(record_at_mut(payload, &row_path(row)));
            edit_live(payload, bear, |record| {
                alias_base_power_null(record);
                set_stamps(record, turn, row);
            });
        })
        .expect("the unedited stamped record loads");
        assert_bound(&own, turn, row, &format!("{label} control"));

        for (what, stamping) in [("stamped", (turn, row)), ("zero-stamped", (0, 0))] {
            let reloaded = reload_edited(&form, with_alias_record(Some(stamping)))
                .unwrap_or_else(|error| panic!("{label}: {what} alias must load: {error}"));
            assert_bound(&reloaded, turn, row, &format!("{label}: {what}"));
        }
        let unstamped = reload_edited(&form, with_alias_record(None))
            .expect("an unstamped alias passes through");
        assert_eq!(
            stamps(trigger_record(&unstamped)),
            (0, 0),
            "{label}: the pass-through leaves the alias record unbound"
        );
    }
}

/// CR 603.2c: the fast paths keep their structure. A prior-turn stamp is trusted whatever the
/// printing; records that name one occurrence key agree by the pairwise relation (rules always,
/// raw form among display-free records), whatever the visiting order.
#[test]
fn fast_paths_keep_their_structure() {
    // A prior-turn record with a different printing keeps its stamp.
    let (state, bear, row, forms) = bear_board(Some("SPAA"));
    let turn = state.turn_number;
    for (label, form) in forms {
        let prior = reload_edited(&form, |payload| {
            edit_live(payload, bear, |record| {
                set_lki_printing(record, Some("SQQQ"));
                set_stamps(record, turn - 1, 7);
            });
        })
        .expect("a prior-turn record loads");
        assert_eq!(
            stamps(trigger_record(&prior)),
            (turn - 1, 7),
            "{label}: prior turn"
        );
        assert_eq!(
            lki_set_code(trigger_record(&prior)).as_deref(),
            Some("SQQQ")
        );

        // Two carriers of one key that differ only in a printing agree.
        let pair = reload_edited(&form, |payload| {
            let (original, copy) = duplicate_live_record_into_deferred_entry_events(payload, bear);
            set_stamps(record_at_mut(payload, &original), turn, row);
            let copy = record_at_mut(payload, &copy);
            set_lki_printing(copy, Some("SQQQ"));
            set_stamps(copy, turn, row);
        })
        .expect("printing-only carriers of one key agree");
        assert_bound(&pair, turn, row, &format!("{label}: printing-only pair"));
        let GameEvent::ZoneChanged { record, .. } = &pair.deferred_entry_events[0] else {
            panic!("{label}: the copy is a zone-change event");
        };
        assert_eq!(stamps(record), (turn, row));
        assert_eq!(lki_set_code(record).as_deref(), Some("SQQQ"));
    }

    // Two display-free carriers of one key that differ in a legacy alias conflict.
    let (state, bear, row, forms) = bear_board(None);
    let turn = state.turn_number;
    for (label, form) in forms {
        assert_load_error(
            reload_edited(&form, |payload| {
                alias_base_power_null(record_at_mut(payload, &row_path(row)));
                edit_live(payload, bear, alias_base_power_null);
                let (original, copy) =
                    duplicate_live_record_into_deferred_entry_events(payload, bear);
                set_stamps(record_at_mut(payload, &original), turn, row);
                let copy = record_at_mut(payload, &copy);
                alias_base_power_omitted(copy);
                set_stamps(copy, turn, row);
            }),
            "shared by conflicting records",
            &format!("{label}: display-free alias pair"),
        );
    }
}

/// An undecodable record is a load error in the existing style, and the shape guard keeps its
/// message.
#[test]
fn an_undecodable_zone_change_record_fails_the_load() {
    for board_stamp in [Some("SPAA"), None] {
        let (_, bear, _, forms) = bear_board(board_stamp);
        for (label, form) in forms {
            let context = format!("{label}: board {board_stamp:?}");
            reload_edited(&form, |_| {}).expect("the unedited board loads");
            assert_load_error(
                reload_edited(&form, |payload| {
                    edit_live(payload, bear, |record| {
                        record.insert("mana_value".to_string(), serde_json::json!("x"));
                    });
                }),
                "does not decode as a zone-change record",
                &context,
            );
            assert_load_error(
                reload_edited(&form, |payload| {
                    edit_live(payload, bear, |record| {
                        record.remove("to_zone");
                    });
                }),
                "missing its zone-change shape",
                &context,
            );
        }
    }
}

/// An invalid occurrence stamp reads as absent, as it always has: the record is re-bound to its
/// occurrence, and only a record that does not decode is rejected.
#[test]
fn an_invalid_occurrence_stamp_is_repaired_as_today() {
    // (label, the invalid stamp edits)
    let invalid: [(&str, Vec<(&str, Value)>); 3] = [
        ("null index", vec![("turn_zone_change_index", Value::Null)]),
        (
            "string turn and negative index",
            vec![
                ("recorded_turn_number", serde_json::json!("x")),
                ("turn_zone_change_index", serde_json::json!(-1)),
            ],
        ),
        ("null turn", vec![("recorded_turn_number", Value::Null)]),
    ];
    for board_stamp in [Some("SPAA"), None] {
        let (state, bear, row, forms) = bear_board(board_stamp);
        let turn = state.turn_number;
        for (label, form) in forms {
            for (what, edits) in &invalid {
                let context = format!("{label}: board {board_stamp:?}, {what}");
                let apply = |record: &mut Fields| {
                    for (key, value) in edits {
                        record.insert((*key).to_string(), value.clone());
                    }
                };
                let mut edited = form.clone();
                edit_live(state_payload_mut(&mut edited), bear, apply);
                let payload = state_payload(&edited);
                let live = at(payload, &bear_live_record_path(payload, bear));
                for (key, value) in edits {
                    assert_eq!(
                        &live[*key], value,
                        "{context}: the invalid stamp is in place"
                    );
                }
                let reloaded =
                    reload(edited).unwrap_or_else(|error| panic!("{context}: must load: {error}"));
                assert_bound(&reloaded, turn, row, &context);
            }
            // Rejection control: an invalid stamp does not excuse an undecodable record.
            assert_load_error(
                reload_edited(&form, |payload| {
                    edit_live(payload, bear, |record| {
                        record.insert("turn_zone_change_index".to_string(), Value::Null);
                        record.insert("mana_value".to_string(), serde_json::json!("x"));
                    });
                }),
                "does not decode as a zone-change record",
                &format!("{label}: board {board_stamp:?}, rejection control"),
            );
        }
    }
}

fn committed_dump(name: &str) -> &'static [u8] {
    match name {
        "dellian_emblem_conqueror_4p" => {
            include_bytes!("../fixtures/dellian_emblem_conqueror_4p.json.gz").as_slice()
        }
        "tenacity_exquisite_blood_4p" => {
            include_bytes!("../fixtures/tenacity_exquisite_blood_4p.json.gz").as_slice()
        }
        other => panic!("no committed dump named {other}"),
    }
}

/// Every live record's path with its stamps, in a serialized state.
fn live_record_stamps(state: &GameState) -> Vec<(Vec<String>, Value, Value)> {
    let json = serde_json::to_value(state).expect("state serializes");
    live_record_paths(&json, None)
        .into_iter()
        .map(|path| {
            let record = at(&json, &path);
            (
                path.clone(),
                record["recorded_turn_number"].clone(),
                record["turn_zone_change_index"].clone(),
            )
        })
        .collect()
}

/// Real boards reconcile as before: a committed dump loads through the production decoder, and
/// the loaded state, saved on both persisted paths and loaded again, keeps every live record's
/// stamps.
#[test]
fn real_saves_reconcile_as_before() {
    for name in ["dellian_emblem_conqueror_4p", "tenacity_exquisite_blood_4p"] {
        let mut json = String::new();
        flate2::read::GzDecoder::new(committed_dump(name))
            .read_to_string(&mut json)
            .expect("fixture .json.gz inflates to UTF-8 JSON");
        let mut envelope: Value =
            serde_json::from_str(&json).expect("dump envelope parses as JSON");
        // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
        engine::types::game_state::record_captured_onsets(&mut envelope);
        let loaded = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
            .expect("gameState deserializes through the production decoder")
            .into_game_state()
            .expect("persisted snapshot satisfies the checked restore contract");

        let expected = live_record_stamps(&loaded);
        assert!(
            !expected.is_empty(),
            "{name}: reach: the dump holds live zone-change records"
        );
        for (label, form) in persisted_forms(&loaded) {
            let reloaded =
                reload(form).unwrap_or_else(|error| panic!("{name} {label}: reload: {error}"));
            assert_eq!(
                live_record_stamps(&reloaded),
                expected,
                "{name} {label}: every live record keeps its stamps"
            );
        }
    }
}

fn gunzip(gz: &[u8]) -> String {
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

fn projected_capture_snapshot(gz: &[u8]) -> Value {
    let json = gunzip(gz);
    let mut envelope: Value = serde_json::from_str(&json).expect("dump envelope parses as JSON");
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    engine::types::game_state::record_captured_onsets(&mut envelope);
    let mut snapshot = envelope["gameState"].clone();
    let object = snapshot
        .as_object_mut()
        .expect("a captured gameState is a JSON object");
    assert!(
        !object.contains_key("resolution_state_version"),
        "the reporter's capture is an unversioned runtime debug snapshot"
    );
    let stack = object
        .remove("resolution_stack")
        .expect("the wedged capture carries a runtime resolution_stack");
    object.insert("resolution_frames".to_string(), stack);
    object.insert("resolution_state_version".to_string(), Value::from(2));
    snapshot
}

fn load_capture(gz: &[u8]) -> GameState {
    // Project only the runtime envelope onto the supported v2 wire. Do not run the
    // restore finalizer: this witness must reach the live action-boundary repair.
    serde_json::from_value::<engine::types::resolution::ResolutionStateWire>(
        projected_capture_snapshot(gz),
    )
    .expect("the projected snapshot deserializes through the versioned resolution decoder")
    .into_game_state()
}

#[test]
fn rejected_action_repair_advances_revision_but_plain_rejection_does_not() {
    use engine::game::engine::apply;

    let mut state = load_capture(include_bytes!(
        "fixtures/mycoloth_devour_wedge_turn20.json.gz"
    ));
    assert!(
        !state.resolution_stack.is_empty(),
        "reach: the wedge is present"
    );
    let before = state.state_revision;
    assert!(apply(&mut state, P0, GameAction::PassPriority).is_err());
    assert!(
        state.resolution_stack.is_empty(),
        "reach: the entry sweep repaired the wedge"
    );
    assert!(state.state_revision > before);

    let mut healthy = GameState::new_two_player(0);
    healthy.priority_player = P1;
    healthy.waiting_for = WaitingFor::Priority { player: P1 };
    let before = healthy.state_revision;
    assert!(apply(&mut healthy, P0, GameAction::PassPriority).is_err());
    assert_eq!(healthy.state_revision, before);
}

#[test]
fn contest_prompt_mutations_advance_revision_without_publishing() {
    use engine::game::contest::{advance_contest, contest_click, start_contest, ContestStep};
    use engine::types::contest::{ClickDisposition, ClickRejection, ContestConfig};

    const EYE_TO_EYE: &str =
        "You and target creature's controller have a staring contest. If you win, destroy that creature.";
    let config = ContestConfig {
        countdown_ms: 1_000,
        inactivity_timeout_ms: 1_000,
        reconnect_grace_ms: 5_000,
        settle_window_ms: 0,
        max_active_ms: 600_000,
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Island", "Forest"]);
    let victim = scenario.add_vanilla(P1, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Eye to Eye", true, EYE_TO_EYE)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_objects(&[victim]).resolve();
    let mut state = runner.state().clone();
    let mut hidden_order = state.clone();
    hidden_order.players[0].library.swap(0, 1);
    let mut hidden_events = Vec::new();
    let assert_delivery = |a: &GameState, b: &GameState| {
        assert_eq!(a.state_revision, b.state_revision);
        for viewer in [Some(P0), Some(P1), None] {
            let aw = client_envelope(a, viewer);
            let bw = client_envelope(b, viewer);
            assert_eq!(
                aw["state"]["waiting_for"],
                serde_json::to_value(&a.waiting_for).unwrap()
            );
            assert_eq!(aw["state"]["waiting_for"], bw["state"]["waiting_for"]);
            assert_eq!(
                aw["derived"]["waiting_for_identity"],
                bw["derived"]["waiting_for_identity"]
            );
            assert_eq!(
                aw["derived"]["state_revision"],
                bw["derived"]["state_revision"]
            );
        }
    };
    let mut events = Vec::new();
    assert_eq!(
        start_contest(&mut state, config, 0, &mut events),
        ContestStep::Updated
    );
    assert_eq!(
        start_contest(&mut hidden_order, config, 0, &mut hidden_events),
        ContestStep::Updated
    );
    assert_delivery(&state, &hidden_order);
    assert_eq!(
        advance_contest(&mut state, 1_000, &mut events),
        ContestStep::Updated
    );
    assert_eq!(
        advance_contest(&mut hidden_order, 1_000, &mut hidden_events),
        ContestStep::Updated
    );
    assert_delivery(&state, &hidden_order);
    let contest = |state: &GameState| match &state.waiting_for {
        WaitingFor::PhysicalContest { contest, .. } => (**contest).clone(),
        other => panic!("expected PhysicalContest, got {other:?}"),
    };
    let id = contest(&state).id;
    let before = state.state_revision;
    let before_wires = [Some(P0), Some(P1), None].map(|viewer| client_envelope(&state, viewer));
    let (disposition, step) = contest_click(&mut state, id, P0, 1, 1_100, &mut events);
    assert_eq!(disposition, ClickDisposition::Counted);
    assert_eq!(step, ContestStep::Unchanged);
    assert_eq!(contest(&state).contestants[0].clicks, 1);
    assert!(state.state_revision > before);
    assert_eq!(
        contest_click(&mut hidden_order, id, P0, 1, 1_100, &mut hidden_events),
        (disposition, step)
    );
    assert_delivery(&state, &hidden_order);
    for (index, viewer) in [Some(P0), Some(P1), None].into_iter().enumerate() {
        let after = client_envelope(&state, viewer);
        assert_eq!(
            after["state"]["waiting_for"]["data"]["contest"]["contestants"][0]["clicks"],
            1
        );
        assert_ne!(
            before_wires[index]["derived"]["waiting_for_identity"],
            after["derived"]["waiting_for_identity"]
        );
    }

    let before = state.state_revision;
    let prior_contest = contest(&state);
    assert_eq!(
        advance_contest(&mut state, 1_500, &mut events),
        ContestStep::Unchanged
    );
    assert_eq!(contest(&state).phase, prior_contest.phase);
    assert!(contest(&state).clock_ms > prior_contest.clock_ms);
    assert!(state.state_revision > before);
    assert_eq!(
        advance_contest(&mut hidden_order, 1_500, &mut hidden_events),
        ContestStep::Unchanged
    );
    assert_delivery(&state, &hidden_order);

    let before = state.state_revision;
    let (disposition, step) = contest_click(&mut state, id, P0, 1, 1_600, &mut events);
    assert_eq!(
        disposition,
        ClickDisposition::Rejected(ClickRejection::StaleSequence)
    );
    assert_eq!(step, ContestStep::Unchanged);
    assert_eq!(contest(&state).clock_ms, 1_600);
    assert!(state.state_revision > before);
    assert_eq!(
        contest_click(&mut hidden_order, id, P0, 1, 1_600, &mut hidden_events),
        (disposition, step)
    );
    assert_delivery(&state, &hidden_order);

    let before = state.state_revision;
    let clock = contest(&state).clock_ms;
    assert_eq!(
        start_contest(&mut state, config, 1_700, &mut events),
        ContestStep::Unchanged
    );
    assert_eq!(contest(&state).clock_ms, clock);
    assert_eq!(state.state_revision, before);
    assert_eq!(
        start_contest(&mut hidden_order, config, 1_700, &mut hidden_events),
        ContestStep::Unchanged
    );
    assert_delivery(&state, &hidden_order);
    let (disposition, step) =
        contest_click(&mut state, id.wrapping_add(1), P0, 2, 1_800, &mut events);
    assert_eq!(
        disposition,
        ClickDisposition::Rejected(ClickRejection::WrongContest)
    );
    assert_eq!(step, ContestStep::Unchanged);
    assert_eq!(state.state_revision, before);
    assert_eq!(contest(&state).clock_ms, clock);
    assert_eq!(
        contest_click(
            &mut hidden_order,
            id.wrapping_add(1),
            P0,
            2,
            1_800,
            &mut hidden_events
        ),
        (disposition, step)
    );
    assert_delivery(&state, &hidden_order);
    assert_eq!(
        advance_contest(&mut state, clock, &mut events),
        ContestStep::Unchanged
    );
    assert_eq!(state.state_revision, before);
    assert_eq!(
        advance_contest(&mut hidden_order, clock, &mut hidden_events),
        ContestStep::Unchanged
    );
    assert_delivery(&state, &hidden_order);
}

fn client_envelope(state: &GameState, viewer: Option<PlayerId>) -> Value {
    serde_json::to_value(engine::game::derived_views::ClientGameStateRef::wrap(
        state, viewer,
    ))
    .expect("client envelope serializes")
}

#[test]
fn prompt_identity_priority_native_vector() {
    let state = GameState::new_two_player(0);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    for viewer in [Some(P0), Some(P1), None] {
        let wire = client_envelope(&state, viewer);
        assert_eq!(
            engine::types::display_only::canonical_json_bytes(&wire["state"]["waiting_for"]),
            br#"{"data":{"player":0},"type":"Priority"}"#
        );
        assert_eq!(
            wire["derived"]["waiting_for_identity"],
            "ae290362dc64d95b65ecb96ab3bd73cb33c820ddf926c06d687a9631c217bd1d"
        );
        assert_eq!(wire["derived"]["state_revision"], state.state_revision);
    }
}

#[test]
fn prompt_identity_matches_real_delivered_trigger_for_every_viewer() {
    let mut envelope: Value =
        serde_json::from_str(&gunzip(committed_dump("dellian_emblem_conqueror_4p"))).unwrap();
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    engine::types::game_state::record_captured_onsets(&mut envelope);
    let state = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .unwrap()
        .into_game_state()
        .unwrap();
    assert!(matches!(
        state.waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));
    let mut wires = Vec::new();
    for viewer in [
        Some(P0),
        Some(P1),
        Some(PlayerId(2)),
        Some(PlayerId(3)),
        None,
    ] {
        let wire = client_envelope(&state, viewer);
        assert_eq!(
            wire["derived"]["waiting_for_identity"],
            serde_json::to_value(engine::types::display_only::canonical_digest(
                &wire["state"]["waiting_for"]
            ))
            .unwrap(),
            "printing-free delivered bytes define identity for {viewer:?}"
        );
        wires.push(wire);
    }
    assert_ne!(
        wires[0]["state"]["waiting_for"],
        wires[1]["state"]["waiting_for"]
    );
    assert_ne!(
        wires[0]["derived"]["waiting_for_identity"],
        wires[1]["derived"]["waiting_for_identity"]
    );
}

#[test]
fn prompt_identity_ignores_nested_printing_but_not_lki_power() {
    let mut scenario = GameScenario::new();
    let bear = scenario.add_vanilla(P0, 2, 2);
    let original = scenario.build().state().clone();
    let make = |code: Option<&str>, power| {
        let mut state = original.clone();
        if let Some(code) = code {
            stamp(&mut state, bear, code);
        }
        state.objects.get_mut(&bear).unwrap().power = Some(power);
        let record = state.objects[&bear].snapshot_for_zone_change(
            bear,
            Some(Zone::Battlefield),
            Zone::Graveyard,
        );
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: P0,
            // Legacy/shared event context has no private controller. A known controller
            // would correctly remove the event for the other two viewer scopes.
            trigger_controller: None,
            trigger_event: Some(GameEvent::ZoneChanged {
                object_id: bear,
                from: Some(Zone::Battlefield),
                to: Zone::Graveyard,
                record: Box::new(record),
            }),
            trigger_events: Vec::new(),
            target_slots: Vec::new(),
            mode_labels: Vec::new(),
            target_constraints: Vec::new(),
            selection: Default::default(),
            source_id: Some(bear),
            description: None,
        };
        state
    };
    for viewer in [Some(P0), Some(P1), None] {
        let absent = client_envelope(&make(None, 2), viewer);
        let a = client_envelope(&make(Some("SAAA"), 2), viewer);
        let b = client_envelope(&make(Some("SBBB"), 2), viewer);
        let changed = client_envelope(&make(Some("SAAA"), 3), viewer);
        assert!(sentinels(&a["state"]["waiting_for"], "SAAA") > 0);
        assert_ne!(a["state"]["waiting_for"], b["state"]["waiting_for"]);
        assert_eq!(
            absent["derived"]["waiting_for_identity"],
            a["derived"]["waiting_for_identity"]
        );
        assert_eq!(
            a["derived"]["waiting_for_identity"],
            b["derived"]["waiting_for_identity"]
        );
        assert_ne!(
            a["derived"]["waiting_for_identity"],
            changed["derived"]["waiting_for_identity"]
        );
    }
}

#[test]
fn prompt_identity_canonicalizes_both_combat_maps_and_keeps_requirement_sources() {
    use engine::game::combat::{AttackTarget, CombatRequirement};
    use std::collections::HashMap;
    let make = |reverse: bool, source| {
        let mut state = GameState::new_two_player(0);
        let mut targets = vec![
            (ObjectId(1), vec![AttackTarget::Player(P1)]),
            (
                ObjectId(2),
                vec![
                    AttackTarget::Player(P1),
                    AttackTarget::Planeswalker(ObjectId(9)),
                ],
            ),
        ];
        let mut constraints = vec![
            (
                ObjectId(1),
                CombatRequirement::CantAttack {
                    sources: vec![ObjectId(11)],
                },
            ),
            (
                ObjectId(2),
                CombatRequirement::MustAttack {
                    defenders: vec![AttackTarget::Player(P1)],
                    sources: vec![ObjectId(source)],
                },
            ),
        ];
        if reverse {
            targets.reverse();
            constraints.reverse();
        }
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![ObjectId(1), ObjectId(2)],
            valid_attack_targets: vec![AttackTarget::Player(P1)],
            valid_attack_targets_by_attacker: Some(targets.into_iter().collect::<HashMap<_, _>>()),
            attacker_constraints: constraints.into_iter().collect(),
        };
        client_envelope(&state, Some(P0))
    };
    let a = make(false, 22);
    let b = make(true, 22);
    for field in ["valid_attack_targets_by_attacker", "attacker_constraints"] {
        assert_eq!(
            a["state"]["waiting_for"]["data"][field]
                .as_object()
                .unwrap()
                .len(),
            2
        );
    }
    assert_eq!(
        a["derived"]["waiting_for_identity"],
        b["derived"]["waiting_for_identity"]
    );
    assert_ne!(
        a["derived"]["waiting_for_identity"],
        make(false, 23)["derived"]["waiting_for_identity"]
    );
}

/// CR 400.2 + CR 401.2 + CR 701.22a: only the scrying player sees library order.
#[test]
fn prompt_identity_scry_hides_order_from_other_viewers() {
    // Opt, verbatim Oracle text checked by the phase plan.
    const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.";
    let make = |swap: bool| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Island", "Forest"]);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        if swap {
            runner.state_mut().players[0].library.swap(0, 1);
        }
        drop(runner.cast(spell).commit());
        for _ in 0..8 {
            if matches!(runner.state().waiting_for, WaitingFor::ScryChoice { .. }) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ScryChoice { player: P0, .. }
        ));
        runner.state().clone()
    };
    let a = make(false);
    let b = make(true);
    let mut ids_a = a.players[0].library.clone();
    let mut ids_b = b.players[0].library.clone();
    ids_a.sort();
    ids_b.sort();
    assert_eq!(ids_a, ids_b);
    assert_eq!(a.state_revision, b.state_revision);
    for viewer in [Some(P0), Some(P1), None] {
        let aw = client_envelope(&a, viewer);
        let bw = client_envelope(&b, viewer);
        if viewer == Some(P0) {
            assert_ne!(
                aw["state"]["waiting_for"]["data"]["cards"],
                bw["state"]["waiting_for"]["data"]["cards"]
            );
            assert_ne!(
                aw["derived"]["waiting_for_identity"],
                bw["derived"]["waiting_for_identity"]
            );
        } else {
            assert_eq!(
                aw["state"]["waiting_for"]["data"]["cards"],
                serde_json::json!([0])
            );
            assert_eq!(aw["state"]["waiting_for"], bw["state"]["waiting_for"]);
            assert_eq!(
                aw["derived"]["waiting_for_identity"],
                bw["derived"]["waiting_for_identity"]
            );
        }
        assert_eq!(
            aw["state"]["players"][0]["library"]
                .as_array()
                .unwrap()
                .len(),
            bw["state"]["players"][0]["library"]
                .as_array()
                .unwrap()
                .len()
        );
    }
}

/// CR 305.1 + CR 117.3c: playing a land retains priority but changes rules state.
#[test]
fn prompt_identity_priority_stays_but_delivered_revision_changes_after_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_land_to_hand(P0, "Forest").id();
    let mut runner = scenario.build();
    let before = client_envelope(runner.state(), Some(P0));
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .unwrap();
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    let after = client_envelope(runner.state(), Some(P0));
    assert_eq!(
        before["state"]["waiting_for"],
        serde_json::json!({"type":"Priority","data":{"player":0}})
    );
    assert_eq!(
        before["state"]["waiting_for"],
        after["state"]["waiting_for"]
    );
    assert_eq!(
        before["derived"]["waiting_for_identity"],
        after["derived"]["waiting_for_identity"]
    );
    assert_ne!(
        before["derived"]["state_revision"],
        after["derived"]["state_revision"]
    );
}

#[test]
fn prompt_revision_documents_count_only_disclosure_of_private_repair() {
    use engine::game::engine::apply;
    use engine::types::game_state::PostReplacementDrainStack;
    let mut a = GameState::new_two_player(0);
    a.priority_player = P1;
    a.waiting_for = WaitingFor::Priority { player: P1 };
    let mut b = a.clone();
    b.resolution_stack
        .push_post_replacement(PostReplacementDrainStack::default());
    assert!(a.resolution_stack.is_empty());
    assert!(!b.resolution_stack.is_empty());
    assert!(a.resolving_stack_entry.is_none() && b.resolving_stack_entry.is_none());
    assert!(a.pending_cast.is_none() && b.pending_cast.is_none());
    let viewers = [Some(P0), Some(P1), None];
    let before_a = viewers.map(|v| client_envelope(&a, v));
    let before_b = viewers.map(|v| client_envelope(&b, v));
    assert_eq!(before_a, before_b);
    assert!(apply(&mut a, P0, GameAction::PassPriority).is_err());
    assert!(apply(&mut b, P0, GameAction::PassPriority).is_err());
    assert!(a.resolution_stack.is_empty() && b.resolution_stack.is_empty());
    assert!(a.resolving_stack_entry.is_none() && b.resolving_stack_entry.is_none());
    assert!(a.pending_cast.is_none() && b.pending_cast.is_none());
    for (index, viewer) in viewers.into_iter().enumerate() {
        let mut aw = client_envelope(&a, viewer);
        let mut bw = client_envelope(&b, viewer);
        assert_eq!(aw["state"], bw["state"]);
        assert_eq!(
            aw["state"]["waiting_for"],
            before_a[index]["state"]["waiting_for"]
        );
        assert_eq!(
            aw["derived"]["waiting_for_identity"],
            bw["derived"]["waiting_for_identity"]
        );
        assert_eq!(
            aw["derived"]["state_revision"],
            before_a[index]["derived"]["state_revision"]
        );
        assert_eq!(
            bw["derived"]["state_revision"].as_u64().unwrap(),
            before_b[index]["derived"]["state_revision"]
                .as_u64()
                .unwrap()
                + 1
        );
        aw["derived"]
            .as_object_mut()
            .unwrap()
            .remove("state_revision");
        bw["derived"]
            .as_object_mut()
            .unwrap()
            .remove("state_revision");
        assert_eq!(aw, bw);
        assert!(
            aw["state"].get("resolution_stack").is_none(),
            "empty private frames are omitted"
        );
    }
}
