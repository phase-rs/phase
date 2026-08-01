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
use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, ResolvedAbility, TargetFilter,
    TriggerDefinition,
};
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
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
// `finalize_committed_liminal_token_entry_from_action` records the entry through
// `push_committed_token_entry_events`, which is gated on `TokenEntryEventEmission::Emit`. The one
// `Suppress` caller is the liminal branch of `engine_replacement.rs::handle_copy_target_choice`,
// which emits the entry itself after the commit returns. Deleting the unconditional
// `record_battlefield_entry` from the finalize tail therefore leaves this route's CR 403.3 record
// entirely to that caller — which is what this test pins.
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
// So: unreached by the current card pool, not impossible. The post-finalize emit handed to the
// commit for the paused case is kept for that reason — it keeps the record local to this route
// rather than resting on a `liminal_immediate ⇒ no counters` argument that spans two files and
// holds only as long as the card-pool measurement above does.

/// Verbatim Oracle text (Amonkhet). The Embalm line is a keyword hint so the scenario's parse
/// pipeline synthesizes the graveyard-activated token-copy ability, exactly as
/// `vizier_of_many_faces_embalm_copy_panic_5278.rs` does — the token it creates is a copy of
/// Vizier, so it carries Vizier's own "enter as a copy" replacement and pauses for a copy target,
/// which is the only production route to `TokenEntryEventEmission::Suppress`.
const VIZIER_ORACLE: &str = "You may have this creature enter as a copy of any creature on the battlefield, except if this creature was embalmed, the token has no mana cost, it's white, and it's a Zombie in addition to its other types.\nEmbalm {3}{U}{U}";

/// CR 403.3 + CR 603.6a: a liminal copy-token entry committed with entry-event emission
/// SUPPRESSED must still land on both per-turn ledgers exactly once, and must still emit the
/// battlefield-entry event its caller defers.
///
/// This is the route the paired deletion had to compensate. The finalize tail no longer records
/// the entry itself (`record_zone_change`, inside `push_committed_token_entry_events`, does), and
/// on this route that call is made by `handle_copy_target_choice` rather than by the finalize —
/// so if the emit and the record were ever separated again, the copy token would enter invisibly.
///
/// REVERT-PROBE (discriminating, RUN): restore the direct `snapshot_for_zone_change` emit inside
/// `push_committed_token_entry_events` (the pre-change form that never reached the recorder) while
/// keeping the deleted `record_battlefield_entry` deleted ⇒ the Embalm copy token appears in
/// NEITHER ledger and both count assertions fail with 0.
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
