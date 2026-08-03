//! CR 603.6a + CR 603.2c + CR 400.7 — a token entering the battlefield must be recorded through
//! `restrictions::record_zone_change`, so its `ZoneChanged` event carries this turn's real
//! zone-change index.
//!
//! DEFECT: `GameObject::snapshot_for_zone_change` leaves `turn_zone_change_index` at its `0`
//! placeholder for the recorder to overwrite (`zones.rs` does exactly that for ordinary moves).
//! The two token emit sites built the record and emitted it WITHOUT ever reaching the recorder, so
//! every token entry shipped index `0`. The batched zone-change replay guard
//! (`triggers.rs::batched_zone_change_already_collected`) dedups on
//! `(definition_ref, turn_zone_change_index)` — CR 603.2c, "an ability triggers only once each
//! time its trigger event occurs" — so a SECOND same-turn token batch collided with the first on
//! `(def, 0)` and its fire was silently swallowed.

use engine::game::effects::{incubate, token};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, ResolvedAbility, TargetFilter,
    TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// The batched enters-the-battlefield class (CR 603.6a + CR 603.2c): "Whenever one or more
/// creatures you control enter, you gain 1 life." Built directly rather than loaded from a card
/// because the behaviour under test is the ENGINE's batched-dedup KEY, which is card-agnostic —
/// and no card in `integration_cards.json` carries a batched ETB trigger that admits tokens
/// without an additional "only once each turn" clause that would mask the second fire.
fn batched_etb_life_trigger() -> TriggerDefinition {
    let mut def = TriggerDefinition::new(TriggerMode::ChangesZone);
    def.batched = true;
    def.destination = Some(Zone::Battlefield);
    def.trigger_zones = vec![Zone::Battlefield];
    def.execute = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )));
    def.description =
        Some("Whenever one or more creatures you control enter, you gain 1 life.".to_string());
    def
}

/// Resolve one `Effect::Token` batch of `count` tokens through the production token resolver, then
/// run the real trigger pipeline over the emitted events. Returns the emitted events so the test
/// can read the `turn_zone_change_index` the entries actually shipped.
fn mint_token_batch(state: &mut GameState, source: ObjectId, count: i32) -> Vec<GameEvent> {
    let ability = ResolvedAbility::new(
        Effect::Token {
            name: "Saproling".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Fixed(1),
            types: vec!["Creature".to_string()],
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count: QuantityExpr::Fixed { value: count },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        },
        Vec::new(),
        source,
        P0,
    );
    let mut events = Vec::new();
    token::resolve(state, &ability, &mut events).expect("the token batch resolves");
    process_triggers(state, &events);
    drain_order_triggers_with_identity(state);
    events
}

/// Resolve one `Effect::Incubate` through the production incubate resolver, then run the real
/// trigger pipeline over the emitted events — the "other mechanism" half of the mixed-group case.
///
/// `incubate.rs` was one of SEVEN battlefield-entry emit sites that built a `ZoneChanged` record
/// with `snapshot_for_zone_change` and emitted it without ever reaching the recorder, so it shipped
/// the index-`0` placeholder. It is routed through `record_zone_change` by this change because
/// these very tests drive it; the six that remain (`conjure.rs`, `counters.rs` ×2 — the `:526`
/// inline emit and `push_token_entry_events` — `token_copy.rs` ×2, `gift_delivery.rs`) are the
/// class-wide follow-up.
fn incubate_batch(state: &mut GameState, source: ObjectId, count: i32) -> Vec<GameEvent> {
    let ability = ResolvedAbility::new(
        Effect::Incubate {
            count: QuantityExpr::Fixed { value: count },
        },
        Vec::new(),
        source,
        P0,
    );
    let mut events = Vec::new();
    incubate::resolve(state, &ability, &mut events).expect("the incubate resolves");
    process_triggers(state, &events);
    drain_order_triggers_with_identity(state);
    events
}

fn zone_change_indices(events: &[GameEvent]) -> Vec<usize> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::ZoneChanged { record, .. } => Some(record.turn_zone_change_index),
            _ => None,
        })
        .collect()
}

fn life_of_p0(state: &GameState) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 is seated")
        .life
}

fn token_ids(state: &GameState) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.is_token))
        .collect()
}

/// R4 (CR 603.6a + CR 603.2c): TWO token batches in ONE turn, from ONE `batched: true`
/// `ChangesZone` trigger, must fire the trigger TWICE — once per batch — because each batch is a
/// distinct trigger event.
///
/// REVERT-PROBE (discriminating, RUN): restore the direct
/// `snapshot_for_zone_change` emit in `push_committed_token_entry_events` (index left at the `0`
/// placeholder) ⇒ both batches key on `(def, 0)`, the second is dropped by
/// `batched_zone_change_already_collected`, and P0 gains 1 life instead of 2.
#[test]
fn second_same_turn_token_batch_still_triggers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .expect("host permanent")
        .trigger_definitions
        .push(batched_etb_life_trigger());

    let life_start = life_of_p0(runner.state());
    let turn_start = runner.state().turn_number;

    // ── BATCH 1 ──
    let first = mint_token_batch(runner.state_mut(), host, 2);
    runner.advance_until_stack_empty();
    let after_first = life_of_p0(runner.state());
    // POSITIVE reach-guard: the batched trigger really fires (a fixture that never triggers would
    // make the second-batch assertion below vacuously "unchanged").
    assert_eq!(
        after_first - life_start,
        1,
        "one batch of 2 tokens fires the batched trigger exactly ONCE (CR 603.2c)"
    );

    // ── BATCH 2, SAME TURN ──
    let second = mint_token_batch(runner.state_mut(), host, 2);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().turn_number,
        turn_start,
        "both batches are in the SAME turn (the dedup ledger is per-turn)"
    );

    // (1) DISCRIMINATOR: the second batch is a distinct trigger event and fires again.
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        2,
        "a SECOND same-turn token batch fires the batched trigger again (index 0 ⇒ swallowed ⇒ 1)"
    );

    // (2) MECHANISM: the two batches carry DISJOINT zone-change indices — the dedup key that
    //     makes (1) possible. Under the defect every index is the `0` placeholder.
    let first_ix = zone_change_indices(&first);
    let second_ix = zone_change_indices(&second);
    assert_eq!(first_ix.len(), 2, "batch 1 emits one ZoneChanged per token");
    assert_eq!(
        second_ix.len(),
        2,
        "batch 2 emits one ZoneChanged per token"
    );
    assert!(
        first_ix.iter().all(|a| second_ix.iter().all(|b| a != b)),
        "the two batches must not share a zone-change index, got {first_ix:?} vs {second_ix:?}"
    );
    let mut all = [first_ix, second_ix].concat();
    all.sort_unstable();
    all.dedup();
    assert_eq!(
        all.len(),
        4,
        "each of the 4 token entries gets its OWN index (all-0 placeholder ⇒ 1)"
    );
}

/// MIXED-GROUP (CR 603.2c): a SIBLING mechanism's entry and a token entry in the SAME turn are two
/// distinct trigger events, so one `batched: true` `ChangesZone` trigger must fire for EACH.
///
/// This is the case where routing entries through `record_zone_change` makes the engine dedup
/// LESS, not more: an emit site that never reaches the recorder ships the index-`0` placeholder, so
/// before this change a token entry collided with the Incubator's `0` at `(def, 0)` and the second
/// mechanism's fire was swallowed. CR 603.2c bounds an ability to one fire per *occurrence* of its
/// trigger event — two permanents entering are two occurrences, so the suppressed fire was never
/// rules-correct.
///
/// Both mechanisms are routed now (`token.rs` and `incubate.rs`), so this passes in either order;
/// `mixed_group_sibling_last_also_fires` is the reversed-order twin.
///
/// REVERT-PROBE (discriminating, RUN): restore the direct `snapshot_for_zone_change` emit in
/// `push_committed_token_entry_events` ⇒ the token batch ships index `0`, collides with the
/// Incubator's `0`, and P0 gains 1 life instead of 2.
#[test]
fn mixed_group_sibling_then_token_each_fire_the_batched_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .expect("host permanent")
        .trigger_definitions
        .push(batched_etb_life_trigger());

    // The index arithmetic below is only legible if the per-turn ledger starts empty.
    assert_eq!(
        runner.state().zone_changes_this_turn.len(),
        0,
        "the CR 400.7 per-turn zone-change ledger starts empty"
    );
    let life_start = life_of_p0(runner.state());
    let turn_start = runner.state().turn_number;

    // ── SIBLING MECHANISM FIRST: Incubate (index-0 placeholder, pushed to the ledger directly) ──
    let incubator = incubate_batch(runner.state_mut(), host, 2);
    runner.advance_until_stack_empty();
    // POSITIVE reach-guard: the sibling entry really reaches the batched trigger. Without this the
    // token assertion below would pass vacuously for a fixture that never triggered at all.
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        1,
        "the Incubator entry fires the batched trigger once (CR 603.6a)"
    );
    let sibling_ix = zone_change_indices(&incubator);
    assert_eq!(
        sibling_ix,
        vec![0],
        "the first entry of an empty-ledger turn takes index 0 (placeholder and real agree here)"
    );

    // ── TOKEN ENTRY, SAME TURN ──
    let tokens = mint_token_batch(runner.state_mut(), host, 2);
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().turn_number,
        turn_start,
        "both mechanisms are in the SAME turn (the dedup ledger is per-turn)"
    );

    // (1) DISCRIMINATOR: two mechanisms, two trigger events, two fires.
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        2,
        "a token entry after a sibling-mechanism entry fires the batched trigger AGAIN \
         (token shipping index 0 ⇒ collides with the sibling ⇒ 1)"
    );

    // (2) MECHANISM: the token entries carry real, nonzero indices assigned past the sibling's.
    let token_ix = zone_change_indices(&tokens);
    assert_eq!(
        token_ix,
        vec![1, 2],
        "token entries are indexed past the sibling's ledger entry (placeholder ⇒ [0, 0])"
    );
}

/// REVERSED ORDER (CR 603.2c): the sibling mechanism enters SECOND. This is the half a
/// token-only fix cannot reach — `record_zone_change` assigns `zone_changes_this_turn.len()`, so
/// the first token of an empty-ledger turn legitimately takes index `0` and an unrouted sibling's
/// placeholder `0` collides with it. Routing `incubate.rs` through the recorder is what makes the
/// sibling's index real (`2`, past the two token entries) and its fire survive.
///
/// REVERT-PROBE (discriminating, RUN): restore the direct
/// `zone_changes_this_turn.push_back(..)` + `record_battlefield_entry` emit in `incubate.rs`
/// (index left at the `0` placeholder) ⇒ the Incubator collides with the token batch's index `0`,
/// its fire is swallowed, and P0's delta stays 1 instead of 2.
#[test]
fn mixed_group_sibling_last_also_fires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .expect("host permanent")
        .trigger_definitions
        .push(batched_etb_life_trigger());

    let life_start = life_of_p0(runner.state());

    let tokens = mint_token_batch(runner.state_mut(), host, 2);
    runner.advance_until_stack_empty();
    // POSITIVE reach-guard: the token batch fires, so the unchanged total below is a genuine
    // suppression and not a fixture that never triggered.
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        1,
        "the token batch fires the batched trigger once"
    );
    assert_eq!(
        zone_change_indices(&tokens),
        vec![0, 1],
        "the first token of an empty-ledger turn legitimately takes index 0"
    );

    let incubator = incubate_batch(runner.state_mut(), host, 2);
    runner.advance_until_stack_empty();
    // (1) DISCRIMINATOR: two mechanisms, two occurrences, two fires (CR 603.2c).
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        2,
        "the sibling entry after a token batch fires the batched trigger AGAIN \
         (sibling shipping index 0 ⇒ collides with the token's legitimate 0 ⇒ 1)"
    );
    // (2) MECHANISM: the sibling's index is assigned past the two token entries already on the
    //     ledger, so it can no longer alias onto the token batch's legitimate `0`.
    assert_eq!(
        zone_change_indices(&incubator),
        vec![2],
        "the sibling entry is indexed past the token batch (unrouted placeholder ⇒ [0])"
    );
}

/// MUST-NOT-FLIP for the paired deletion: routing token entries through `record_zone_change`
/// (which performs the CR 403.3 battlefield-entry bookkeeping itself) means the emit sites must
/// NOT also call `record_battlefield_entry`.
///
/// REVERT-PROBE (discriminating, RUN): re-add the deleted
/// `crate::game::restrictions::record_battlefield_entry` call in
/// `apply_create_token_after_replacement_with_created_ids` ⇒ every token appears TWICE in
/// `battlefield_entries_this_turn` and the per-id count assertion fails with 2.
#[test]
fn battlefield_entries_this_turn_counts_each_token_exactly_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Token Source", 1, 1).id();
    let mut runner = scenario.build();

    let before: Vec<ObjectId> = token_ids(runner.state());
    mint_token_batch(runner.state_mut(), host, 3);
    let minted: Vec<ObjectId> = token_ids(runner.state())
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();

    // POSITIVE reach-guard: tokens were actually created, so the counts below are non-vacuous.
    assert_eq!(minted.len(), 3, "the batch minted 3 tokens");

    for id in &minted {
        let entries = runner
            .state()
            .battlefield_entries_this_turn
            .iter()
            .filter(|r| r.object_id == *id)
            .count();
        assert_eq!(
            entries, 1,
            "token {id:?} is recorded in battlefield_entries_this_turn exactly once \
             (re-adding the deleted record_battlefield_entry ⇒ 2)"
        );
    }

    // The same entries are also visible on the CR 400.7 zone-change ledger — the recorder that
    // assigns the index. Before the fix, tokens never appeared here at all.
    for id in &minted {
        assert_eq!(
            runner
                .state()
                .zone_changes_this_turn
                .iter()
                .filter(|r| r.object_id == *id && r.to_zone == Zone::Battlefield)
                .count(),
            1,
            "token {id:?} is recorded on the zone-change ledger exactly once"
        );
    }
}

// ───────── the SUPPRESS route (CR 403.3 + CR 603.6a) ─────────
//
// `finalize_committed_liminal_token_entry_from_action` records AND emits inline only on the
// `TokenEntryEventEmission::Emit` route. On `Suppress` — reached solely from the liminal branch of
// `engine_replacement.rs::handle_copy_target_choice` — the token is on the battlefield before it is
// the thing that entered (`BecomeCopy` has not run), so the whole entry, record and events
// together, is PARKED on `GameState::pending_token_battlefield_entry` and realized later by
// `token::flush_pending_token_battlefield_entry`. This test pins that the realization happens
// exactly once on both per-turn ledgers, describing the copy rather than the pre-copy Shapeshifter.
//
// HONEST SCOPE — the PAUSED sub-route (a liminal entry carrying counters, so the commit consults
// `add_counter_with_replacement` and may suspend mid-loop) is NOT covered here, and is deliberately
// NOT claimed unreachable.
//
// Half of it is structural. The commit concatenates two vectors into `counters_to_apply`
// (`token.rs`): the `LiminalEntry`'s and the `ProposedEvent::TokenEntry`'s. The entry's is empty by
// construction — `token_copy.rs` takes the liminal branch only when `etb_counters.is_empty()` and
// builds the entry with `Vec::new()`.
//
// The other half is not. The event's vector also starts `Vec::new()`, but it is passed through
// `replace_event` before the commit sees it, and `apply_single_replacement` appends
// `modifiers.etb_counters` to a `TokenEntry`'s vector — which `replacement_event_keys_for_event`
// matches under BOTH `ChangeZone` and `Moved`. So a non-`SelfRef` ETB-counter replacement is not
// structurally excluded from this route.
//
// MEASURED instead of argued: driving both liminal routes (this Embalm/copy-target one and a plain
// `CopyTokenOf`) with the only two external `Moved` ETB-counter grants in `data/card-data.json`
// that admit tokens at all (Spider-Punk's and Tesak's granted Riot/Unleash — every other one is
// either `SelfRef` or `NonToken`-guarded) left `counters_to_apply` empty in every arm; on the
// copy-target route the grant is not even offered, because the token has not yet chosen what to
// copy when the replacement pass runs and both grants are subtype-scoped.
//
// So: unreached by the current card pool, not impossible. The post-finalize realization handed to
// the commit for the paused case (`PendingCounterPostAction::EmitCommittedCopyTokenEntry`,
// convergence point (b) below) is kept for that reason — it keeps the realization inside the action
// that answers the counter-ordering choice rather than resting on a `liminal_immediate ⇒ no
// counters` argument that spans two files and holds only as long as the card-pool measurement
// above does.

/// Verbatim Oracle text (Amonkhet). The Embalm line is a keyword hint so the scenario's parse
/// pipeline synthesizes the graveyard-activated token-copy ability, exactly as
/// `vizier_of_many_faces_embalm_copy_panic_5278.rs` does — the token it creates is a copy of
/// Vizier, so it carries Vizier's own "enter as a copy" replacement and pauses for a copy target,
/// which is the only production route to `TokenEntryEventEmission::Suppress`.
const VIZIER_ORACLE: &str = "You may have this creature enter as a copy of any creature on the battlefield, except if this creature was embalmed, the token has no mana cost, it's white, and it's a Zombie in addition to its other types.\nEmbalm {3}{U}{U}";

/// CR 403.3 + CR 603.6a: a liminal copy-token entry committed on the `Suppress` route must land on
/// both per-turn ledgers exactly once, describing the REALIZED copy, and must emit its entry pair
/// exactly once — all of it from the single realization the flush performs, never half of it.
///
/// Record and events are one owned value (`GameState::pending_token_battlefield_entry`) consumed by
/// one function (`token::flush_pending_token_battlefield_entry`), so "recorded but never emitted"
/// and "emitted but never recorded" are both unrepresentable rather than guarded. This test pins
/// that on the production Embalm/copy-target drive; the unpaused route realizes at convergence
/// point (a), inside `engine_replacement::finish_copy_target_choice_entry`.
///
/// REVERT-PROBE (discriminating, RUN): replace the `Suppress` park in
/// `token::finalize_committed_liminal_token_entry_from_action` with the pre-lifecycle
/// `record_committed_token_entry(state, object_id);` ⇒ the row is written from the pre-copy
/// Shapeshifter (assertion (2b) reads `name: "Vizier of Many Faces"`, `power: Some(0)`) and nothing
/// is ever parked for the flush to realize, so assertion (3)'s emit count is 0.
#[test]
fn suppressed_liminal_copy_token_entry_is_recorded_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let vizier = scenario
        .add_creature_to_graveyard(P0, "Vizier of Many Faces", 0, 0)
        .with_mana_cost(engine::types::mana::ManaCost::Cost {
            generic: 3,
            shards: vec![engine::types::mana::ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["Embalm"], VIZIER_ORACLE)
        .id();
    // The creature the Embalm token is asked to copy.
    scenario.add_creature(P0, "Grizzly Bears", 3, 3);

    let mut runner = scenario.build();
    {
        let dummy = ObjectId(0);
        let pool = &mut runner.state_mut().players[0].mana_pool;
        for m in [
            engine::types::mana::ManaType::Blue,
            engine::types::mana::ManaType::Blue,
            engine::types::mana::ManaType::Colorless,
            engine::types::mana::ManaType::Colorless,
            engine::types::mana::ManaType::Colorless,
        ] {
            pool.add(engine::types::mana::ManaUnit::new(m, dummy, false, vec![]));
        }
    }

    let embalm_index = runner.state().objects[&vizier]
        .abilities
        .iter()
        .position(|a| matches!(&*a.effect, Effect::CopyTokenOf { .. }))
        .expect("the synthesized Embalm ability is on the graveyard Vizier");
    runner
        .act(engine::types::actions::GameAction::ActivateAbility {
            source_id: vizier,
            ability_index: embalm_index,
        })
        .expect("activate Embalm");

    // Drive the entry prompts: accept the enter-as-copy replacement, then pick the copy target.
    // Answering the copy target is what routes the commit through the `Suppress` branch.
    let mut token = None;
    let mut prompts: Vec<String> = Vec::new();
    let mut entry_events: Vec<usize> = Vec::new();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            engine::types::game_state::WaitingFor::ManaPayment { .. }
            | engine::types::game_state::WaitingFor::Priority { .. } => {
                if token.is_some() && runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(engine::types::actions::GameAction::PassPriority)
                    .expect("pass priority");
            }
            engine::types::game_state::WaitingFor::ReplacementChoice { candidates, .. } => {
                prompts.push(format!("ReplacementChoice({})", candidates.len()));
                runner
                    .act(engine::types::actions::GameAction::ChooseReplacement { index: 0 })
                    .expect("accept the enter-as-copy replacement");
            }
            engine::types::game_state::WaitingFor::CopyTargetChoice {
                source_id,
                valid_targets,
                ..
            } => {
                prompts.push("CopyTargetChoice".to_string());
                let target = *valid_targets
                    .iter()
                    .find(|id| {
                        runner
                            .state()
                            .objects
                            .get(id)
                            .is_some_and(|o| o.name == "Grizzly Bears")
                    })
                    .expect("the Bear is a legal copy target");
                token.get_or_insert(source_id);
                let result = runner
                    .act(engine::types::actions::GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(target)),
                    })
                    .expect("choose the copy target");
                entry_events.extend(result.events.iter().filter_map(|e| match e {
                    GameEvent::ZoneChanged { record, to, .. }
                        if record.object_id == source_id && *to == Zone::Battlefield =>
                    {
                        Some(record.turn_zone_change_index)
                    }
                    _ => None,
                }));
            }
            other => {
                prompts.push(format!("{other:?}"));
                break;
            }
        }
    }
    // POSITIVE reach-guard: the copy-target prompt is the ONLY production entrance to the
    // `Suppress` commit, so without it every assertion below would be about a different route.
    let token = token.unwrap_or_else(|| {
        panic!("the Embalm token must reach its copy-target prompt; prompts seen = {prompts:?}")
    });
    runner.advance_until_stack_empty();

    // (1) DISCRIMINATOR: the suppressed-emission entry is recorded exactly once (CR 403.3).
    assert_eq!(
        runner
            .state()
            .battlefield_entries_this_turn
            .iter()
            .filter(|r| r.object_id == token)
            .count(),
        1,
        "the Suppress-route copy token is recorded in battlefield_entries_this_turn exactly once"
    );
    // (2) …through the CR 400.7 recorder, so it also carries a real zone-change index.
    assert_eq!(
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|r| r.object_id == token && r.to_zone == Zone::Battlefield)
            .count(),
        1,
        "the Suppress-route copy token reaches the zone-change ledger exactly once"
    );
    // (2b) CR 400.7: the row must describe the state at the moment of the move — the REALIZED
    //      copy. The `Suppress` commit does NOT record: it PARKS the entry on
    //      `GameState::pending_token_battlefield_entry`, and
    //      `token::flush_pending_token_battlefield_entry` writes the row ONCE, post-`BecomeCopy`,
    //      from a snapshot taken at flush. Recording at commit instead would describe a 0/0
    //      Shapeshifter, and the look-back consumers that read this ledger directly
    //      (`game/quantity.rs` zone-change scans, the `SuppressTriggers` ETB filters in
    //      `game/triggers.rs`) would see the pre-copy object, so "each Bear that entered the
    //      battlefield this turn" would miss a token that is by then a Bear.
    //
    //      REVERT-PROBE (discriminating, RUN): move the flush call in
    //      `engine_replacement.rs::finish_copy_target_choice_entry` to BEFORE the `BecomeCopy`
    //      chain resolves ⇒ `name` reads "Vizier of Many Faces" and `power` reads `Some(0)`,
    //      failing here, while the count assertions (1) and (2) above stay green — isolating the
    //      flip to row CONTENT, not row COUNT.
    let entry_row = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|r| r.object_id == token && r.to_zone == Zone::Battlefield)
        .expect("the Suppress-route copy token has a zone-change row")
        .clone();
    assert_eq!(
        entry_row.name, "Grizzly Bears",
        "the recorded entry names the copied creature, not the pre-copy Shapeshifter"
    );
    assert_eq!(
        entry_row.power,
        Some(3),
        "the recorded entry carries the copied power, not the 0/0 the token had before \
         `BecomeCopy` resolved"
    );
    // (2c) …and the TWO ledgers agree. `record_zone_change` writes the zone-change row and
    //      calls `record_battlefield_entry`, so both describe this one entry; they back
    //      different typed predicates (`ZoneChangeCountThisTurn` vs `BattlefieldEntriesThisTurn`,
    //      the latter via `battlefield_entry_matches_filter`, which reads these very fields).
    //      Refreshing one without the other makes "how many Bears entered this turn" answer 1
    //      on one ledger and 0 on the other.
    //
    //      Both rows come from the SINGLE `record_zone_change` call the flush makes, so they
    //      cannot disagree by construction — that structural agreement is what this pins.
    //
    //      REVERT-PROBE (discriminating, RUN): delete the `record_zone_change` call inside
    //      `token::record_committed_token_entry` and push the row onto `zone_changes_this_turn`
    //      directly ⇒ `battlefield_entries_this_turn` never gets its row and the
    //      `.expect("...has a battlefield-entry row")` below panics, while (2b) above stays
    //      green — isolating the flip to the SECOND ledger.
    let battlefield_row = runner
        .state()
        .battlefield_entries_this_turn
        .iter()
        .find(|r| r.object_id == token)
        .expect("the Suppress-route copy token has a battlefield-entry row")
        .clone();
    assert_eq!(
        battlefield_row.name, entry_row.name,
        "both CR 403.3 ledgers describe the same entry, so they must name the same creature"
    );
    // The measured subtypes here are ["Zombie"], and that is the FIXTURE, not a copy rule:
    // `GameScenario::add_creature` (game/scenario.rs:357) sets only `CoreType::Creature` and
    // P/T, so this "Grizzly Bears" has no subtypes to copy and Zombie is all that remains.
    // Embalm adds it — `VIZIER_ORACLE` above says "a Zombie IN ADDITION TO its other types" —
    // so nothing here is evidence about whether copy exceptions replace subtypes. Do not read
    // it as such.
    //
    // These two therefore assert ledger AGREEMENT rather than a concrete subtype; `name` above
    // is what carries the discrimination, since (2b) already pins it to a concrete post-copy
    // value. Both rows snapshot the same live object, so a typed query cannot get one answer
    // from `battlefield_entry_matches_filter` and a different one from a zone-change scan.
    assert_eq!(
        battlefield_row.subtypes, entry_row.subtypes,
        "both CR 403.3 ledgers snapshot the same object, so their subtypes agree"
    );
    assert_eq!(
        battlefield_row.core_types, entry_row.core_types,
        "both CR 403.3 ledgers snapshot the same object, so their core types agree"
    );
    // (3) The deferred emit really happened, carrying the recorder-assigned index (CR 603.6a +
    //     CR 400.7). Read off the `ActionResult` of the copy-target submission itself, which is
    //     the action that runs the whole Suppress tail.
    assert_eq!(
        entry_events.len(),
        1,
        "the copy-target submission emits exactly one battlefield ZoneChanged for the token"
    );
    let ledger_index = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .position(|r| r.object_id == token && r.to_zone == Zone::Battlefield)
        .expect("the entry is on the ledger");
    assert_eq!(
        entry_events[0], ledger_index,
        "the emitted event carries the index the recorder assigned (placeholder ⇒ 0 ≠ ledger slot)"
    );
    // NOT asserted here, and deliberately: no board ETB trigger fires for this route. MEASURED —
    // a ChangesZone→Battlefield trigger grafted onto a live permanent (layers flushed) gained 0
    // life, with `batched: true` AND with `batched: false`. That is a PRE-EXISTING gap in the
    // copy-target-choice resume path, and the non-batched arm is what makes it independent of the
    // batched dedup this change touches: the event IS emitted (assertion 3), it just fires nothing.
    // The CAUSE is deliberately not named — an earlier draft blamed `state.deferred_entry_events`
    // filtering the emit out at the priority boundary, which cannot be it
    // (`replay_deferred_entry_events` takes that vector EMPTY before this emit happens). Recorded
    // as a follow-up with the symptom only, not fixed here.
}

// ───────── the POSTPONED entry lifecycle (CR 400.7 + CR 403.3 + CR 614.12a) ─────────
//
// A `Suppress`-route token is committed to the battlefield BEFORE it is the thing that entered:
// `BecomeCopy` has not run, and the copied card's own mandatory as-enters choice (CR 614.12a) is
// unanswered. Its CR 400.7 record and its CR 603.6a entry events are therefore PARKED on
// `GameState::pending_token_battlefield_entry` and realized as one indivisible operation by
// `token::flush_pending_token_battlefield_entry` at the first instant the object IS that thing.
//
// Three convergence points call that one flush, and a fourth defensive call in the `Suppress` arm
// itself (`token.rs`) exists only so parking over a live entry cannot lose it. (a) is pinned
// INDEPENDENTLY — deleting it alone flips its test. (b) and the in-`apply_action` half of (c) are
// EXERCISED, not isolated: on every route the card pool reaches, the action-boundary half of (c)
// converges the same work, so deleting either one alone flips nothing (measured). They are kept for
// CR 704.3 ordering — the CR 400.7 row must be written before the settling action's SBA pass so
// CR 704.5f cannot bury a 0-toughness copy first — and, for (b), for a drain that does not settle
// in its own action. Their tests still discriminate the flush lifecycle as a whole (delete the park
// and every route test fails).
//   (a) `engine_replacement::finish_copy_target_choice_entry` — the unpaused route
//       (`suppressed_liminal_copy_token_entry_is_recorded_once`, above).
//   (b) `PendingCounterPostAction::EmitCommittedCopyTokenEntry` — the CR 616.1 ETB-counter
//       ordering pause (`..._realizes_through_an_etb_counter_ordering_pause`).
//   (c) `token::realize_settled_token_battlefield_entry` — every other pause shape, however many
//       round trips it takes (`..._through_a_mandatory_as_enters_choice`,
//       `..._that_raises_a_second_pause`). One gate (settled `Priority` + token still on the
//       battlefield) called from two places in `engine.rs`: inside `apply_action` before
//       `run_post_action_pipeline`, and at the action boundary, where a realization now also runs
//       `run_post_action_pipeline_from` over the slice it appended so the handlers that return an
//       `ActionResult` straight out of the reducer match (`handle_tribute_choice`) get the same
//       CR 603.6a check. Both tests measure +1 life; what distinguishes them is WHERE the pipeline
//       runs, pinned by the presence of `OrderTriggers(2)` on the Fanatic route.

/// Verbatim Oracle text from `data/card-data.json` (paraphrases can take a different parser
/// branch, so the fixtures below must use the real strings).
const PAINTERS_SERVANT_ORACLE: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";
const FANATIC_OF_XENAGOS_ORACLE: &str = "Trample\nTribute 1 (As this creature enters, an opponent of your choice may put a +1/+1 counter on it.)\nWhen this creature enters, if tribute wasn't paid, it gets +1/+1 and gains haste until end of turn.";
const FAITHFUL_WATCHDOG_ORACLE: &str =
    "Vigilance\nThis creature enters with three +1/+1 counters on it.";
const HARDENED_SCALES_ORACLE: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION_ORACLE: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";
const SOUL_WARDEN_ORACLE: &str = "Whenever another creature enters, you gain 1 life.";

/// What one answered prompt did to the token's entry: the events its `ActionResult` carried and
/// both per-turn ledgers as of immediately after it returned.
#[derive(Debug, Clone)]
struct CopyEntryStep {
    /// The prompt label this step answered (mirrors `CopyEntryDrive::prompts` positionally).
    answered: String,
    /// `turn_zone_change_index` of every battlefield `ZoneChanged` this action emitted FOR THE
    /// TOKEN.
    zone_changed_indices: Vec<usize>,
    /// How many `TokenCreated` events this action emitted for the token.
    tokens_created: usize,
    /// CR 400.7 rows for the token on `zone_changes_this_turn` after this action.
    zone_rows: usize,
    /// CR 403.3 rows for the token on `battlefield_entries_this_turn` after this action.
    entry_rows: usize,
    /// Whether an entry is still parked awaiting realization after this action.
    parked: bool,
}

#[derive(Debug)]
struct CopyEntryDrive {
    prompts: Vec<String>,
    steps: Vec<CopyEntryStep>,
    token: Option<ObjectId>,
}

impl CopyEntryDrive {
    fn token(&self) -> ObjectId {
        self.token.unwrap_or_else(|| {
            panic!(
                "the Embalm token must reach its copy-target prompt; prompts seen = {:?}",
                self.prompts
            )
        })
    }
}

/// Put a graveyard Vizier of Many Faces with its synthesized Embalm ability in play, and stage the
/// {3}{U}{U} it costs into P0's pool.
fn stage_embalm_vizier(scenario: &mut GameScenario) -> ObjectId {
    let vizier = scenario
        .add_creature_to_graveyard(P0, "Vizier of Many Faces", 0, 0)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        })
        .from_oracle_text_with_keywords(&["Embalm"], VIZIER_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        [
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ]
        .into_iter()
        .map(|m| ManaUnit::new(m, ObjectId(0), false, vec![]))
        .collect(),
    );
    vizier
}

fn token_entry_step(
    runner: &GameRunner,
    token: Option<ObjectId>,
    answered: String,
    events: &[GameEvent],
) -> CopyEntryStep {
    let matches_token = |id: ObjectId| token == Some(id);
    CopyEntryStep {
        answered,
        zone_changed_indices: events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged { record, to, .. }
                    if matches_token(record.object_id) && *to == Zone::Battlefield =>
                {
                    Some(record.turn_zone_change_index)
                }
                _ => None,
            })
            .collect(),
        tokens_created: events
            .iter()
            .filter(|event| {
                matches!(event, GameEvent::TokenCreated { object_id, .. } if matches_token(*object_id))
            })
            .count(),
        zone_rows: runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|record| matches_token(record.object_id) && record.to_zone == Zone::Battlefield)
            .count(),
        entry_rows: runner
            .state()
            .battlefield_entries_this_turn
            .iter()
            .filter(|record| matches_token(record.object_id))
            .count(),
        parked: runner.state().pending_token_battlefield_entry.is_some(),
    }
}

/// Activate the graveyard Vizier's Embalm ability and answer every prompt the resulting token
/// entry raises, recording each answer's effect on the two CR 400.7 / CR 403.3 ledgers.
///
/// `copy_target` names the battlefield creature the copy-target prompt must pick; `None` DECLINES
/// the "enter as a copy" replacement, which routes the entry through `TokenEntryEventEmission::Emit`
/// instead (the positive control). Later `ReplacementChoice` prompts are the CR 616.1 ETB-counter
/// ordering choice and always take the first ordering.
fn drive_embalm_copy(
    runner: &mut GameRunner,
    vizier: ObjectId,
    copy_target: Option<&str>,
) -> CopyEntryDrive {
    let embalm_index = runner.state().objects[&vizier]
        .abilities
        .iter()
        .position(|ability| matches!(&*ability.effect, Effect::CopyTokenOf { .. }))
        .expect("the synthesized Embalm ability is on the graveyard Vizier");
    runner
        .act(GameAction::ActivateAbility {
            source_id: vizier,
            ability_index: embalm_index,
        })
        .expect("activate Embalm");

    let mut drive = CopyEntryDrive {
        prompts: Vec::new(),
        steps: Vec::new(),
        token: None,
    };
    let mut replacements_answered = 0_usize;
    for _ in 0..64 {
        let (label, action) = match runner.state().waiting_for.clone() {
            WaitingFor::ManaPayment { .. } | WaitingFor::Priority { .. } => {
                // Settled: the entry finished (the copy route knows its token id; the declined
                // route never gets one) and nothing is left resolving. Anything further would be
                // the turn advancing, which clears the per-turn ledgers under the assertions.
                let entry_done = drive.token.is_some() || copy_target.is_none();
                if entry_done && runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
                continue;
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                // The FIRST replacement choice is Vizier's own optional "enter as a copy"
                // (index 1 declines it); any later one is the CR 616.1 ordering between two
                // ETB-counter replacements, where either ordering reaches this seam.
                let index = usize::from(replacements_answered == 0 && copy_target.is_none());
                replacements_answered += 1;
                (
                    format!("ReplacementChoice({})", candidates.len()),
                    GameAction::ChooseReplacement { index },
                )
            }
            WaitingFor::CopyTargetChoice {
                source_id,
                valid_targets,
                ..
            } => {
                let wanted = copy_target.expect("declining must not raise a copy-target prompt");
                let target = *valid_targets
                    .iter()
                    .find(|id| {
                        runner
                            .state()
                            .objects
                            .get(id)
                            .is_some_and(|object| object.name == wanted)
                    })
                    .unwrap_or_else(|| panic!("{wanted} must be a legal copy target"));
                drive.token = Some(source_id);
                (
                    "CopyTargetChoice".to_string(),
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    },
                )
            }
            WaitingFor::NamedChoice { options, .. } => (
                format!("NamedChoice({})", options.len()),
                GameAction::ChooseOption {
                    choice: options
                        .first()
                        .expect("a mandatory named choice offers at least one option")
                        .clone(),
                },
            ),
            // CR 702.104a: decline the tribute so the companion "if tribute wasn't paid" trigger
            // also runs — the longest continuation this class produces.
            WaitingFor::TributeChoice { .. } => (
                "TributeChoice".to_string(),
                GameAction::DecideOptionalEffect { accept: false },
            ),
            // CR 603.3b: a realized entry can trigger two same-controller abilities at once (the
            // copy's own ETB plus a battlefield observer), which surfaces an ordering prompt.
            WaitingFor::OrderTriggers { triggers, .. } => (
                format!("OrderTriggers({})", triggers.len()),
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                },
            ),
            other => {
                drive.prompts.push(format!("{other:?}"));
                break;
            }
        };
        let result = runner
            .act(action)
            .unwrap_or_else(|err| panic!("answering {label} failed: {err:?}"));
        drive.prompts.push(label.clone());
        let step = token_entry_step(runner, drive.token, label, &result.events);
        drive.steps.push(step);
    }
    runner.advance_until_stack_empty();
    drive
}

/// Both per-turn ledgers' single row for `token`, panicking (with the drive's prompt trace) when
/// either is missing.
fn entry_rows(
    runner: &GameRunner,
    token: ObjectId,
    drive: &CopyEntryDrive,
) -> (String, Option<i32>, String) {
    let zone_row = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == token && record.to_zone == Zone::Battlefield)
        .unwrap_or_else(|| {
            panic!(
                "the realized copy token must have a CR 400.7 zone-change row; prompts = {:?}",
                drive.prompts
            )
        });
    let battlefield_row = runner
        .state()
        .battlefield_entries_this_turn
        .iter()
        .find(|record| record.object_id == token)
        .unwrap_or_else(|| {
            panic!(
                "the realized copy token must have a CR 403.3 battlefield-entry row; prompts = {:?}",
                drive.prompts
            )
        });
    (
        zone_row.name.clone(),
        zone_row.power,
        battlefield_row.name.clone(),
    )
}

fn ledger_index(runner: &GameRunner, token: ObjectId) -> usize {
    runner
        .state()
        .zone_changes_this_turn
        .iter()
        .position(|record| record.object_id == token && record.to_zone == Zone::Battlefield)
        .expect("the entry is on the CR 400.7 ledger")
}

/// CR 400.7 + CR 403.3 + CR 614.12a — the maintainer's named failure path. Embalm Vizier of Many
/// Faces copying Painter's Servant: the copy carries Painter's MANDATORY "as this creature enters,
/// choose a color" replacement, so the entry pauses on a `NamedChoice` that spans a client round
/// trip. Both ledgers must describe the REALIZED copy exactly once, and the entry pair must be
/// emitted exactly once, on the action that finally settles.
///
/// REVERT-PROBE (discriminating, RUN): delete the
/// `token::realize_settled_token_battlefield_entry` call in `engine::apply_action_boundary_core`
/// AND the one in `engine::apply_action` ⇒ the `ChooseOption` step carries no entry events and both
/// ledgers stay at 0 rows, failing the four post-flush assertions, while
/// `suppressed_liminal_copy_token_entry_is_recorded_once` (convergence point (a)) and
/// `..._realizes_through_an_etb_counter_ordering_pause` (convergence point (b)) stay green.
///
/// SECOND REVERT-PROBE, isolating the CONVERGENCE as a whole (discriminating, RUN): deleting only
/// the `apply_action` call now flips NOTHING — the action-boundary call realizes the entry and runs
/// `run_post_action_pipeline_from` over the slice it appended, so the observer still fires. What
/// still flips this test's Soul Warden assertion 1 → 0 is deleting the boundary block's pipeline
/// call as well; the two placements now differ only in ordering against this action's CR 704.3 SBA
/// pass, which no fixture on this route discriminates.
#[test]
fn suppressed_liminal_copy_token_entry_realizes_through_a_mandatory_as_enters_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = stage_embalm_vizier(&mut scenario);
    scenario.add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT_ORACLE);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN_ORACLE);
    let mut runner = scenario.build();
    let life_start = life_of_p0(runner.state());

    let drive = drive_embalm_copy(&mut runner, vizier, Some("Painter's Servant"));
    // POSITIVE reach-guard: the mandatory as-enters pause was actually reached. Without it every
    // assertion below could be about a route that never postponed anything.
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string(),
            "NamedChoice(5)".to_string(),
        ],
        "the copy's own CR 614.12a colour choice must pause the entry"
    );
    let token = drive.token();

    // (1) PRE-FLUSH NEGATIVE, paired with the reach-guard above: at the `NamedChoice` pause the
    //     entry is postponed — no row on either ledger, no event emitted, and the entry is parked.
    let copy_step = &drive.steps[1];
    assert_eq!(
        (copy_step.zone_rows, copy_step.entry_rows),
        (0, 0),
        "the entry is postponed until the copy IS the thing that entered (CR 614.12a)"
    );
    assert_eq!(
        (
            copy_step.zone_changed_indices.len(),
            copy_step.tokens_created
        ),
        (0, 0),
        "nothing is emitted for the token while its as-enters choice is unanswered"
    );
    assert!(
        copy_step.parked,
        "the postponed entry is parked on GameState so it survives the round trip"
    );

    // (2) DISCRIMINATOR: the realizing action writes ONE row on EACH ledger, describing the
    //     copied creature — not the 0/0 pre-copy Shapeshifter the head recorded here.
    let settled = &drive.steps[2];
    assert_eq!(
        (settled.zone_rows, settled.entry_rows),
        (1, 1),
        "the realized entry lands on both CR 400.7 / CR 403.3 ledgers exactly once"
    );
    assert!(
        !settled.parked,
        "the parked entry is consumed by its realization"
    );
    let (zone_name, zone_power, battlefield_name) = entry_rows(&runner, token, &drive);
    assert_eq!(
        zone_name, "Painter's Servant",
        "the recorded entry names the copied creature, not the pre-copy Shapeshifter"
    );
    assert_eq!(
        zone_power,
        Some(1),
        "the recorded entry carries the copied power, not the 0/0 the token had before BecomeCopy"
    );
    assert_eq!(
        battlefield_name, zone_name,
        "both CR 403.3 ledgers are written by the one record_zone_change call, so they agree"
    );

    // (3) The emit rides the SAME action that realized the entry, exactly once, carrying the
    //     recorder-assigned `turn_zone_change_index`. That index is the engine's own key — the CR
    //     does not name it — and the batched zone-change replay guard dedups on it to hold the
    //     CR 603.2c once-per-occurrence bound (same framing as this file's module header).
    assert_eq!(
        settled.tokens_created, 1,
        "the entry pair is emitted exactly once, on the realizing action"
    );
    assert_eq!(
        settled.zone_changed_indices,
        vec![ledger_index(&runner, token)],
        "the emitted ZoneChanged carries the index the recorder assigned"
    );

    // (4) CR 603.2 + CR 603.6a: the pair is emitted from inside `apply_action`, ahead of
    //     `run_post_action_pipeline`, so this action's trigger scan sees the token enter and the
    //     board's ETB observers fire. The action-boundary convergence would also produce +1 here
    //     (it runs the same pipeline over the slice it appends), so this assertion pins THAT the
    //     observer fires, not WHERE the realization happened; the Fanatic test's `OrderTriggers(2)`
    //     is what pins the boundary route specifically.
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        1,
        "Soul Warden observes the realized copy token entering; prompts = {:?}",
        drive.prompts
    );
}

/// CR 400.7 + CR 614.12a + CR 702.104a — the SECOND-PAUSE class. Fanatic of Xenagos's as-enters
/// `Choose(Opponent)` continuation raises a `TributeChoice`, so the entry spans TWO client round
/// trips of two different prompt shapes. This is the shape a fix hung off any single prompt
/// variant's resume arm cannot see.
///
/// REVERT-PROBE (discriminating, RUN): delete the `run_post_action_pipeline_from` block in
/// `engine::apply_action_boundary_core` (leaving the bare realize call) ⇒ the reach-guard below
/// loses its `"OrderTriggers(2)"` element and fails first, and the Soul Warden assertion goes
/// 1 → 0. No other test in this file moves.
///
/// CR 603.6a (`docs/MagicCompRules.txt:2599`): this class settles through `handle_tribute_choice`,
/// which builds its `ActionResult` directly in the reducer match and never reaches
/// `run_post_action_pipeline`, so the action-boundary convergence is what runs the ETB check for
/// it. TWO abilities trigger — Soul Warden's observer and the copy's own CR 603.4 "if tribute
/// wasn't paid" ETB — same controller, so CR 603.3b makes their order the controller's choice and
/// the ordering prompt is REQUIRED here, not an artifact of the harness.
#[test]
fn suppressed_liminal_copy_token_entry_realizes_through_an_as_enters_choice_with_a_second_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = stage_embalm_vizier(&mut scenario);
    scenario
        .add_creature(P0, "Fanatic of Xenagos", 3, 3)
        .from_oracle_text_with_keywords(&["Trample", "Tribute"], FANATIC_OF_XENAGOS_ORACLE);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN_ORACLE);
    let mut runner = scenario.build();
    let life_start = life_of_p0(runner.state());

    let drive = drive_embalm_copy(&mut runner, vizier, Some("Fanatic of Xenagos"));
    // POSITIVE reach-guard: the SECOND pause was reached. A fixture that stopped at the
    // `NamedChoice` would exercise the same route as the Painter test.
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string(),
            "NamedChoice(1)".to_string(),
            "TributeChoice".to_string(),
            "OrderTriggers(2)".to_string(),
        ],
        "the tribute continuation raises a SECOND pause, and the realized entry then raises the \
         CR 603.3b ordering prompt for its two ETB triggers"
    );
    let token = drive.token();

    // (1) PRE-FLUSH NEGATIVE at BOTH intermediate pauses, paired with the reach-guard above.
    for step in &drive.steps[1..3] {
        assert_eq!(
            (step.zone_rows, step.entry_rows),
            (0, 0),
            "nothing is recorded at the {:?} pause",
            step.answered
        );
        assert_eq!(
            (step.zone_changed_indices.len(), step.tokens_created),
            (0, 0),
            "nothing is emitted at the {:?} pause",
            step.answered
        );
        assert!(
            step.parked,
            "the entry stays parked across the {:?} pause",
            step.answered
        );
    }

    // (2) DISCRIMINATOR: post-copy identity survives TWO round trips, once per ledger.
    let settled = &drive.steps[3];
    assert_eq!(
        (settled.zone_rows, settled.entry_rows),
        (1, 1),
        "the realized entry lands on both ledgers exactly once after two pauses"
    );
    assert!(
        !settled.parked,
        "the parked entry is consumed by its realization"
    );
    let (zone_name, zone_power, battlefield_name) = entry_rows(&runner, token, &drive);
    assert_eq!(zone_name, "Fanatic of Xenagos");
    assert_eq!(zone_power, Some(3));
    assert_eq!(battlefield_name, zone_name);

    // (3) The emit rides the action that finally settled.
    assert_eq!(
        settled.tokens_created, 1,
        "the entry pair is emitted exactly once, on the action that settled"
    );
    assert_eq!(
        settled.zone_changed_indices,
        vec![ledger_index(&runner, token)],
        "the emitted ZoneChanged carries the index the recorder assigned"
    );

    // (4) CR 603.6a (`MagicCompRules.txt:2599`): the realized entry is the event that put a
    //     permanent onto the battlefield, so every permanent is checked for matching ETB triggers.
    //     `handle_tribute_choice` builds its `ActionResult` straight out of the reducer match, so
    //     the action-boundary convergence in `apply_action_boundary_core` is what runs that check
    //     for this class. TWO triggers fire (Soul Warden's observer and Fanatic's own CR 603.4
    //     "if tribute wasn't paid" ETB) — the `OrderTriggers(2)` element of the reach-guard above
    //     pins that, and this assertion pins that the observer actually resolved.
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        1,
        "Soul Warden observes the realized copy token entering through the direct-return handler; \
         prompts = {:?}",
        drive.prompts
    );
}

/// CR 400.7 + CR 616.1 — convergence point (b). Copying Faithful Watchdog ("enters with three
/// +1/+1 counters") while Hardened Scales and Branching Evolution both want to modify that counter
/// event forces the CR 616.1 ordering choice, which pauses the entry INSIDE the counter pipeline.
/// Realizing there puts the entry pair into `events` before this action's trigger scan AND before
/// its CR 704.3 SBA pass. The action-boundary convergence would also make the observers fire on
/// this fixture (it runs the same pipeline over the slice it appends); what (b) and the
/// in-`apply_action` call own, and the boundary does not, is that SBA ordering — (b) additionally
/// owns a drain that does NOT settle in its own action.
///
/// REVERT-PROBE (discriminating, RUN): delete the park itself (`token.rs`'s `Suppress` arm stores
/// nothing) ⇒ every ledger, emit and observer assertion in this test fails. Deleting the two
/// IN-ACTION realization points — the flush call in
/// `counters::apply_pending_counter_post_action`'s `EmitCommittedCopyTokenEntry` arm AND
/// `token::realize_settled_token_battlefield_entry` inside `engine::apply_action` — no longer flips
/// anything here: this fixture's counter-order answer settles to `Priority`, so the action-boundary
/// convergence realizes the entry and runs `run_post_action_pipeline_from` over it in the same
/// action. What those two still own is CR 704.3 ordering (row before the SBA pass), which this
/// fixture does not discriminate.
#[test]
fn suppressed_liminal_copy_token_entry_realizes_through_an_etb_counter_ordering_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = stage_embalm_vizier(&mut scenario);
    scenario
        .add_creature(P0, "Faithful Watchdog", 0, 0)
        .with_plus_counters(3)
        .from_oracle_text_with_keywords(&["Vigilance"], FAITHFUL_WATCHDOG_ORACLE);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_ORACLE);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION_ORACLE);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN_ORACLE);
    let mut runner = scenario.build();
    let life_start = life_of_p0(runner.state());

    let drive = drive_embalm_copy(&mut runner, vizier, Some("Faithful Watchdog"));
    // POSITIVE reach-guard: the SECOND `ReplacementChoice` is the CR 616.1 ordering pause. Without
    // it this fixture would be the unpaused route the (a) test already covers.
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string(),
            "ReplacementChoice(2)".to_string(),
        ],
        "two competing +1/+1 counter replacements must raise the CR 616.1 ordering choice"
    );
    let token = drive.token();

    // (1) The entry is postponed across the counter pause, exactly as across a named choice.
    let copy_step = &drive.steps[1];
    assert_eq!(
        (copy_step.zone_rows, copy_step.entry_rows),
        (0, 0),
        "nothing is recorded while the CR 616.1 ordering choice is open"
    );
    assert!(
        copy_step.parked,
        "the entry is parked across the counter pause"
    );

    // (2) The counter-order answer realizes it, once per ledger, post-copy.
    let settled = &drive.steps[2];
    assert_eq!(
        (settled.zone_rows, settled.entry_rows),
        (1, 1),
        "the realized entry lands on both ledgers exactly once"
    );
    assert_eq!(
        settled.tokens_created, 1,
        "the entry pair rides the counter-order answer"
    );
    let (zone_name, _zone_power, battlefield_name) = entry_rows(&runner, token, &drive);
    assert_eq!(zone_name, "Faithful Watchdog");
    assert_eq!(battlefield_name, zone_name);

    // (3) The pair is emitted BEFORE this action's trigger scan, so a board ETB observer sees the
    //     token enter (CR 603.2).
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        1,
        "Soul Warden observes the copy token entering (CR 603.6a); deleting the park entirely is \
         what takes this to 0"
    );
}

/// POSITIVE CONTROL (CR 603.6a): declining the "enter as a copy" replacement routes the same
/// fixture through `TokenEntryEventEmission::Emit`, which records and emits inline at the finalize
/// tail and never parks anything. Proves the instrument the tests above use is not blind — the
/// same drive, the same assertions, a different lifecycle half.
#[test]
fn declined_copy_replacement_records_the_token_entry_without_parking_it() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = stage_embalm_vizier(&mut scenario);
    scenario.add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT_ORACLE);
    scenario.add_creature_from_oracle(P0, "Soul Warden", 1, 1, SOUL_WARDEN_ORACLE);
    let mut runner = scenario.build();
    let life_start = life_of_p0(runner.state());

    let drive = drive_embalm_copy(&mut runner, vizier, None);
    // POSITIVE reach-guard: the enter-as-a-copy replacement really was offered and declined.
    assert_eq!(
        drive.prompts,
        vec!["ReplacementChoice(2)".to_string()],
        "declining the copy replacement raises no copy-target prompt"
    );
    assert!(
        drive.steps.iter().all(|step| !step.parked),
        "the Emit route never parks an entry"
    );
    assert!(
        runner.state().pending_token_battlefield_entry.is_none(),
        "no entry is left parked once the drive settles"
    );

    // The Embalm token entered under its OWN identity, once per ledger. It is a 0/0 Shapeshifter
    // copy of Vizier with no copy target chosen, so CR 704.5f puts it into the graveyard right
    // after — the ENTRY still happened and is still recorded, which is the point.
    let entry = runner.state().battlefield_entries_this_turn.to_vec();
    assert_eq!(
        entry.len(),
        1,
        "the declined route records exactly one battlefield entry (the Embalm token's)"
    );
    let token = entry[0].object_id;
    assert_eq!(
        entry[0].name, "Vizier of Many Faces",
        "the Emit route records the token's OWN identity"
    );
    assert_eq!(
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|record| record.object_id == token && record.to_zone == Zone::Battlefield)
            .count(),
        1,
        "the Emit-route token is recorded on the CR 400.7 ledger exactly once"
    );
    assert_eq!(
        runner
            .state()
            .battlefield_entries_this_turn
            .iter()
            .filter(|record| record.object_id == token)
            .count(),
        1,
        "the Emit-route token is recorded on the CR 403.3 ledger exactly once"
    );
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        1,
        "Soul Warden observes the plain Embalm token entering — the instrument is not blind"
    );
}

/// CR 603.2c — a postponed entry must not collide with a normally-recorded one. The realized copy
/// token and a plain `Effect::Token` batch minted in the SAME turn (the `Emit` path, through
/// `push_committed_token_entry_events` → `record_committed_token_entry` → `record_zone_change`)
/// must occupy DISTINCT `turn_zone_change_index` values, because the batched zone-change replay
/// guard dedups on that index.
///
/// The second producer is deliberately NOT `token_copy.rs`'s `record_battlefield_entry` sites:
/// those never reach `record_zone_change`, so they have no `zone_changes_this_turn` row to compare
/// against and the assertion would be vacuous.
///
/// REVERT-PROBE (discriminating, RUN): delete the `record_zone_change` call inside
/// `token::record_committed_token_entry` (push onto `zone_changes_this_turn` directly, leaving the
/// snapshot's `0` placeholder) ⇒ the copy token and the minted tokens all report index `0` and the
/// distinctness assertion fails.
#[test]
fn a_realized_copy_token_entry_and_a_same_turn_token_batch_take_distinct_indices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = stage_embalm_vizier(&mut scenario);
    let painter = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT_ORACLE)
        .id();
    let mut runner = scenario.build();

    let drive = drive_embalm_copy(&mut runner, vizier, Some("Painter's Servant"));
    // POSITIVE reach-guard: the postponed route ran, so the index below is a REALIZED entry's.
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string(),
            "NamedChoice(5)".to_string(),
        ],
    );
    let token = drive.token();
    let copy_index = ledger_index(&runner, token);
    let turn_start = runner.state().turn_number;

    let minted = mint_token_batch(runner.state_mut(), painter, 2);
    assert_eq!(
        runner.state().turn_number,
        turn_start,
        "both producers are in the SAME turn (the dedup ledger is per-turn)"
    );
    let minted_indices = zone_change_indices(&minted);
    assert_eq!(
        minted_indices.len(),
        2,
        "the Emit-path batch emits one ZoneChanged per token"
    );
    assert!(
        minted_indices.iter().all(|index| *index != copy_index),
        "the realized copy entry ({copy_index}) must not share an index with the same-turn \
         token batch ({minted_indices:?})"
    );
}

/// CR 400.7 + CR 603.6a — convergence point (a). On the UNPAUSED copy route the entry is realized
/// inside `finish_copy_target_choice_entry`, i.e. during the action that answers the copy-target
/// prompt. The settled-`Priority` backstop cannot substitute for it: this action does not settle
/// (a stale second `CopyTargetChoice` is a known pre-existing defect on this route), so the
/// backstop would slip the row and the emit into a LATER action — one client round trip late, with
/// an empty CR 400.7 look-back in between.
///
/// REVERT-PROBE (discriminating, RUN): delete the flush call in
/// `engine_replacement::finish_copy_target_choice_entry` ⇒ the FIRST copy-target answer emits
/// nothing and both ledgers are still empty after it, failing here, while
/// `..._through_a_mandatory_as_enters_choice`, `..._with_a_second_pause` and
/// `..._an_etb_counter_ordering_pause` stay green (they realize at (c) / (b)).
#[test]
fn unpaused_copy_token_entry_is_realized_by_the_copy_target_action_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vizier = stage_embalm_vizier(&mut scenario);
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    let mut runner = scenario.build();

    let drive = drive_embalm_copy(&mut runner, vizier, Some("Grizzly Bears"));
    // POSITIVE reach-guard: the copy-target prompt is the only production entrance to the
    // postponed (`Suppress`) route, and this route raises no as-enters pause after it.
    assert_eq!(
        drive.prompts[..2],
        [
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string()
        ],
        "the unpaused route reaches the copy-target prompt with no intervening pause"
    );
    let token = drive.token();

    let copy_step = &drive.steps[1];
    assert_eq!(
        (copy_step.zone_rows, copy_step.entry_rows),
        (1, 1),
        "the FIRST copy-target answer realizes the entry on both ledgers, in its own action"
    );
    assert_eq!(
        copy_step.tokens_created, 1,
        "the entry pair rides that same action's ActionResult, not a later one"
    );
    assert_eq!(
        copy_step.zone_changed_indices,
        vec![ledger_index(&runner, token)],
        "the emitted ZoneChanged carries the index the recorder assigned"
    );
    assert!(
        !copy_step.parked,
        "nothing is left parked once the copy completes with no as-enters pause"
    );
    // Post-copy identity, exactly once — the same pins the other three routes carry.
    let (zone_name, zone_power, battlefield_name) = entry_rows(&runner, token, &drive);
    assert_eq!(zone_name, "Grizzly Bears");
    assert_eq!(zone_power, Some(2));
    assert_eq!(battlefield_name, zone_name);
}
