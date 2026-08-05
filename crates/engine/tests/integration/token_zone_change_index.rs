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
//!
//! # REVERT-PROBE convention — read this before trusting an anchor
//!
//! Most assertions here carry an inline `REVERT-PROBE` anchor naming the mutation that should
//! break that assertion. **`RUN` in an anchor tag is an INSTRUCTION to you, not a claim that
//! anyone ran it.** Only anchors that also say `MEASURED` were executed, and those carry the
//! verbatim failure point (file, line, `left`/`right`). Anything else is a PREDICTION.
//!
//! This distinction is load-bearing: two anchors in this file (`PROBE X` and `PROBE Y`, on the
//! gift-token tests) named failure points their recipes never reached, and read as validated for
//! multiple review rounds precisely because nothing separated "stated" from "executed".
//!
//! The failure mode to watch for is **RECIPE SCOPE**. An *authority-wide* revert (neutering
//! `zones::record_and_emit_entry_from_no_zone` itself) also degrades the OTHER producer in the
//! same fixture — usually a priming batch the test relies on — which can trip an upstream
//! reach-guard and kill the test EARLIER than the anchor's named point. A *site-isolated* revert
//! (one emit site, primer left on the real authority) does not. Prefer site-isolated recipes.
//!
//! A discrimination claim needs BOTH mutants, and each must fail THAT assertion, not merely the
//! suite: **DROP** (delete the fix) and **TRIVIALIZE** (keep the shape, make the recorded index a
//! meaningless constant). Scope a trivialize constant to the `from: None -> Battlefield` arm — a
//! global one is dominated by the ordinary-move `TurnRecordIndexMismatch` invariant in `zones.rs`
//! and panics before any assertion here.
//!
//! **Filter trap:** `cargo test --test integration <bare_fn_name> -- --exact` runs ZERO tests and
//! exits `0`. The module path is mandatory:
//! `cargo test -p phase-engine --test integration token_zone_change_index::<fn> -- --exact`.
//! Always check the `N passed` count — a vacuous filter reports `ok. 0 passed`.
//!
//! MEASURED at this tip: `second_same_turn_token_batch_still_triggers`,
//! `mixed_group_sibling_then_token_each_fire_the_batched_trigger`,
//! `a_realized_copy_token_entry_and_a_same_turn_token_batch_take_distinct_indices` (both arms plus
//! a counter-control on its pre-fix form), the inline probe inside
//! `suppressed_liminal_copy_token_entry_is_recorded_once`, and gift-token `PROBE X` / `PROBE Y`.
//!
//! STATED BUT NOT EXECUTED here — treat as predictions until run:
//! `mixed_group_sibling_last_also_fires`,
//! `battlefield_entries_this_turn_counts_each_token_exactly_once`,
//! `conjured_battlefield_entry_after_a_token_batch_fires_the_batched_trigger`,
//! `unpaused_copy_token_entry_is_realized_by_the_copy_target_action_itself`,
//! the three `suppressed_liminal_copy_token_entry_*` realization-pause tests
//! (`..._mandatory_as_enters_choice`, `..._as_enters_choice_with_a_second_pause`,
//! `..._etb_counter_ordering_pause`), the remaining `suppressed_liminal_...` inline anchors, and
//! the `*_after_a_token_batch_fires_the_batched_trigger` family for gift, copy-token tail,
//! modification-paused copy, counter-paused token, counter-paused attached, counter-paused copy,
//! and incubate-resume.

use engine::game::effects::{conjure, gift_delivery, incubate, token, token_copy};
use engine::game::filter::{matches_target_filter_on_zone_change_record, FilterContext};
use engine::game::game_object::AttachTarget;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ChosenAttribute, ConjureCard, ConjureSource,
    ContinuousModification, Effect, PtValue, QuantityExpr, QuantityRef, ResolvedAbility,
    TargetFilter, TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::GiftKind;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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
/// the index-`0` placeholder. It was routed through `record_zone_change` first because these very
/// tests drive it. The class is now CLOSED: all six are now routed through
/// `zones::record_and_emit_entry_from_no_zone`, the single `from: None → Battlefield` record+emit
/// authority, enforced structurally by `battlefield_entry_authority_census.rs`. Each has its own
/// discriminator and empty-ledger control at the bottom of this file.
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
/// REVERT-PROBE (discriminating, RUN): replace the
/// `zones::record_and_emit_entry_from_no_zone` call inside `push_committed_token_entry_events`
/// with a bare `snapshot_for_zone_change` + `events.push(ZoneChanged)` (index left at the `0`
/// placeholder) ⇒ both batches key on `(def, 0)`, the second is dropped by
/// `batched_zone_change_already_collected`, and P0 gains 1 life instead of 2.
///
/// MEASURED (both arms, this tip). DROP, as written above: fails at the life assertion below —
/// `left: 1 right: 2`, the first failure, no earlier guard trips. TRIVIALIZE, keeping
/// `record_zone_change` wired so both per-turn ledgers hold the REAL index and forcing only the
/// EMITTED record's index to `0`: same assertion, same `left: 1 right: 2`. The trivialize arm
/// proves the stronger property neither prediction claimed — this test discriminates the
/// **emitted** `turn_zone_change_index` specifically, not ledger presence.
///
/// RECIPE-SCOPE NOTE: this recipe is authority-wide, so it degrades the PRIMING batch too. The
/// test survives that only by arithmetic coincidence — batch 1's reach-guard below expects delta
/// `1`, and a degraded batch 1 still fires exactly once (both its tokens key on `(def, 0)`; one
/// batch = one fire either way), so the guard is INSENSITIVE to the mutation and control reaches
/// the named assertion. If that guard's expected value ever changes, re-measure this probe before
/// trusting it: the same authority-wide shape made two sibling anchors in this file name failure
/// points their recipes never reached.
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
/// REVERT-PROBE (discriminating, RUN): replace the `zones::record_and_emit_entry_from_no_zone`
/// call inside `push_committed_token_entry_events` with a bare `snapshot_for_zone_change` +
/// `events.push(ZoneChanged)` ⇒ the token batch ships index `0`, collides with the Incubator's
/// `0`, and P0 gains 1 life instead of 2.
///
/// MEASURED (both arms, this tip). DROP: fails at the life assertion below — `left: 1 right: 2`,
/// first failure, no earlier guard trips. TRIVIALIZE (ledgers keep the real index, only the
/// EMITTED index forced to `0`): same assertion, same values.
///
/// RECIPE-SCOPE NOTE: unlike its same-recipe sibling above, this test is STRUCTURALLY immune to
/// the priming-producer hazard — its primer is Incubate, which reaches the authority through
/// `incubate.rs`, NOT through `push_committed_token_entry_events`. The recipe degrades only the
/// token producer, so the sibling's real index and its reach-guard below are untouched.
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

/// Routing token entries through `record_zone_change` (which performs the CR 608.2i
/// battlefield-entry bookkeeping itself) means an emit site must NOT also call
/// `record_battlefield_entry`.
///
/// NOT the paired-deletion must-not-flip. This drive's route
/// (`token::apply_create_token_after_replacement_with_created_ids`) never had such a call to
/// delete: measured on `4b34e5465`, the seven pre-change `record_battlefield_entry` call sites in
/// `engine/src` outside `restrictions.rs` are `conjure.rs:191`, `counters.rs:518/558/637`,
/// `gift_delivery.rs:157` and `token_copy.rs:851/969` — none in `token.rs`, whose emitter already
/// relied on `record_zone_change` doing the bookkeeping. The paired-deletion claim belongs to
/// `assert_site_records`, which drives the sites the deletion actually touched, and is stated
/// there.
///
/// REVERT-PROBE (discriminating, RUN): ADD a
/// `crate::game::restrictions::record_battlefield_entry` call in
/// `apply_create_token_after_replacement_with_created_ids` ⇒ every token appears TWICE in
/// `battlefield_entries_this_turn` and the per-id count assertion fails with 2. That is a
/// forward-direction discriminator — it proves this assertion can fail — not evidence that the
/// call was ever there.
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
             (ADDING a record_battlefield_entry call at this route ⇒ 2; this route never had \
             one to delete — see the doc comment)"
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

// ───────── the SUPPRESS route (CR 608.2i + CR 603.6a) ─────────
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

/// CR 608.2i + CR 603.6a: a liminal copy-token entry committed on the `Suppress` route must land on
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
/// `token::finalize_committed_liminal_token_entry_from_action` with a pre-lifecycle RECORD-ONLY
/// inline — take `state.objects.get(&object_id)`'s
/// `snapshot_for_zone_change(object_id, None, Zone::Battlefield)` and pass it straight to
/// `restrictions::record_zone_change`, emitting nothing — ⇒ the row is written from the pre-copy
/// Shapeshifter (assertion (2b) reads `name: "Vizier of Many Faces"`, `power: Some(0)`) and nothing
/// is ever parked for the flush to realize, so assertion (3)'s emit count is 0. The substitution
/// must be record-only (NOT `zones::record_and_emit_entry_from_no_zone`, which also emits), or
/// assertion (3) would see the emit and the isolation claim would be lost.
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

    // (1) DISCRIMINATOR: the suppressed-emission entry is recorded exactly once (CR 608.2i).
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
    //      `zones::record_and_emit_entry_from_no_zone` and push the row onto
    //      `zone_changes_this_turn` directly, leaving the `0` placeholder ⇒
    //      `battlefield_entries_this_turn` never gets its row. MEASURED failure point:
    //      assertion (1) above, at this file's `battlefield_entries_this_turn` count
    //      (`left: 0, right: 1`) — that earlier count assertion dominates, so the test dies
    //      there. The `.expect("...has a battlefield-entry row")` below would panic for the
    //      same missing row but is never reached, and (2b) is never evaluated. This probe
    //      therefore pins the SECOND ledger losing its row; it does NOT isolate one assertion
    //      against another.
    let battlefield_row = runner
        .state()
        .battlefield_entries_this_turn
        .iter()
        .find(|r| r.object_id == token)
        .expect("the Suppress-route copy token has a battlefield-entry row")
        .clone();
    assert_eq!(
        battlefield_row.name, entry_row.name,
        "both CR 608.2i ledgers describe the same entry, so they must name the same creature"
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
        "both CR 608.2i ledgers snapshot the same object, so their subtypes agree"
    );
    assert_eq!(
        battlefield_row.core_types, entry_row.core_types,
        "both CR 608.2i ledgers snapshot the same object, so their core types agree"
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

// ───────── the POSTPONED entry lifecycle (CR 400.7 + CR 608.2i + CR 614.12a) ─────────
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
/// Scryfall-verbatim (`Vorinclex, Monstrous Raider`, KHM 199). Used as the ANY-PERMANENT half of
/// the CR 616.1 counter-doubler pair: its doubling clause parses with `valid_card == None`, so it
/// admits an ARTIFACT entrant (an Incubator, an Equipment token) that the creature-scoped Hardened
/// Scales / Branching Evolution pair provably rejects. It carries no token-creation replacement, so
/// the two-token reach batch stays exactly two entries.
const VORINCLEX_ORACLE: &str = "Trample, haste\nIf you would put one or more counters on a permanent or player, put twice that many of each of those kinds of counters on that permanent or player instead.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";
/// Scryfall-verbatim (`Ozolith, the Shattered Spire`, SOC 281). The `Plus{1}` half of the
/// ANY-PERMANENT pair — `DOUBLE` and `Plus{1}` do not commute, which is what makes the two
/// simultaneously-applicable replacements raise the CR 616.1 ordering prompt the paused fixtures
/// need. Also carries no token-creation replacement.
const OZOLITH_SHATTERED_SPIRE_ORACLE: &str = "If one or more +1/+1 counters would be put on an artifact or creature you control, that many plus one +1/+1 counters are put on it instead.\n{1}{G}, {T}: Put a +1/+1 counter on target artifact or creature you control. Activate only as a sorcery.\nCycling {2} ({2}, Discard this card: Draw a card.)";

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
    /// CR 608.2i rows for the token on `battlefield_entries_this_turn` after this action.
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
/// entry raises, recording each answer's effect on the two CR 400.7 / CR 608.2i ledgers.
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
                "the realized copy token must have a CR 608.2i battlefield-entry row; prompts = {:?}",
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

/// CR 400.7 + CR 608.2i + CR 614.12a — the maintainer's named failure path. Embalm Vizier of Many
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
        "the realized entry lands on both CR 400.7 / CR 608.2i ledgers exactly once"
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
        "both CR 608.2i ledgers are written by the one record_zone_change call, so they agree"
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
        "the Emit-route token is recorded on the CR 608.2i ledger exactly once"
    );
    assert_eq!(
        life_of_p0(runner.state()) - life_start,
        1,
        "Soul Warden observes the plain Embalm token entering — the instrument is not blind"
    );
}

/// CR 603.2c — a postponed entry must not collide with a normally-recorded one. The realized copy
/// token and a plain `Effect::Token` batch minted in the SAME turn (the `Emit` path, through
/// `push_committed_token_entry_events` → `zones::record_and_emit_entry_from_no_zone` →
/// `record_zone_change`) must occupy DISTINCT `turn_zone_change_index` values, because the batched
/// zone-change replay guard dedups on that index.
///
/// The second producer is the plain `Effect::Token` batch because it is the *pre-existing* routed
/// producer; `token_copy.rs`'s sites are now routed too and get their own dedicated tests (the
/// S4/S5 fixtures at the bottom of this file). This test's subject is the postponed-vs-normal
/// collision, which `mint_token_batch` pins with the fewest moving parts.
///
/// REVERT-PROBE, arm 1 — DROP (discriminating, RUN): delete the `record_zone_change` call inside
/// `zones::record_and_emit_entry_from_no_zone` (push onto `zone_changes_this_turn` directly,
/// leaving the snapshot's `0` placeholder) ⇒ every entry ships index `0`. MEASURED failure point:
/// the distinctness assertion below —
/// `the realized copy entry (0) must not share an index with the same-turn token batch ([0, 0])`.
///
/// REVERT-PROBE, arm 2 — TRIVIALIZE (discriminating, RUN): leave the recorder in place, writing
/// BOTH ledgers' rows, but make `restrictions::record_zone_change` answer a CONSTANT index (`3`)
/// for `from: None → Battlefield` records. Scoping the constant to that arm is required: a global
/// constant is dominated by the ordinary-move `TurnRecordIndexMismatch` invariant
/// (`zones.rs`'s `expect("ordinary zone transition must install its resolved core")`), which
/// panics before this test's own assertions. MEASURED failure point: the same assertion —
/// `the realized copy entry (3) must not share an index with the same-turn token batch ([3, 3])`.
/// A recorder that records but does not COUNT is what arm 1 alone cannot see.
///
/// MEASURED CONTROL for both arms: the pre-fix form of this test read the copy side through
/// `ledger_index` — a ledger POSITION compared against event-borne STORED indices — and reported
/// `ok` under arm 1 (position `1` vs `[0, 0]`) AND under arm 2 (position `1` vs `[3, 3]`). It
/// could not fail for the index-`0` class it names. That is why the copy side is read off its own
/// emitted event below.
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
    // STORED-vs-STORED, deliberately NOT `ledger_index`. The CR 603.2c dedup guard
    // (`triggers.rs::batched_zone_change_already_collected`) keys on the
    // `turn_zone_change_index` it reads off the EVENT, so both sides of the distinctness claim
    // must be event-borne stored indices. `ledger_index` returns a ledger POSITION, which equals
    // the stored index only when every row was recorded correctly — i.e. exactly when the defect
    // is absent. MEASURED: with a position on the copy side this assertion survived its own
    // revert probe (copy POSITION 1 vs minted STORED [0, 0] ⇒ `all(!= 1)` holds), so it could not
    // fail for the index-`0` placeholder class it names.
    //
    // The `drive.token()` reach-guard this replaced is carried structurally: `token_entry_step`
    // filters `zone_changed_indices` by the drive's own token id, so a drive that never reached
    // the copy-target prompt yields an EMPTY vec and fails the length assertion below.
    let copy_indices = &drive.steps[2].zone_changed_indices;
    assert_eq!(
        copy_indices.len(),
        1,
        "the realizing action emits exactly one battlefield ZoneChanged for the copy token; \
         prompts = {:?}",
        drive.prompts
    );
    let copy_index = copy_indices[0];
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
/// prompt.
///
/// STATUS UPDATED — this now carries the same status `counters.rs`'s `EmitCommittedCopyTokenEntry`
/// site carries: EXERCISED, not isolated. The old text here said "this action does not settle (a
/// stale second `CopyTargetChoice` is a known pre-existing defect on this route)". That defect is
/// FIXED: `handle_copy_target_choice`'s liminal-resume branch now clears the answered prompt
/// (CR 614.12a — the choice is made before the permanent enters, so the prompt is spent; CR 603.3 —
/// the ETB abilities are owed the priority boundary the echo denied them). The route settles on the
/// copy-target answer, so `engine::apply_action`'s settled-`Priority` backstop
/// (`realize_settled_token_battlefield_entry`, called immediately before `run_post_action_pipeline`)
/// now realizes the entry inside this same action and ahead of the same SBA pass. The old
/// "one client round trip late" hazard cannot arise on this route any more.
///
/// REVERT-PROBE, RE-MEASURED (NO LONGER DISCRIMINATING): deleting the flush call in
/// `engine_replacement::finish_copy_target_choice_entry` used to fail this test. Measured after the
/// fix: this test and the whole `token_zone_change_index` / `spark_double_as_enters` /
/// `vizier_of_many_faces_embalm_copy_panic_5278` / `metamorphic_alteration` /
/// `constellation_enters_with_choice` / `issue_3260_phantasmal_image_persist` set stay GREEN with
/// that call deleted, because the settled backstop covers it. Stronger than that: the call site is
/// unpinned by the **whole** suite — `cargo test -p phase-engine` with it deleted returns the same
/// 18514 + 12 + 9 + 4550 passing / 0 failed as baseline.
///
/// WHY IT IS STILL KEPT, measured rather than assumed. Probing the call site over the full suite
/// gives 29 calls: 11 realize (`pushed=2`, Token-liminal settled route), 17 are inert, and 1 is the
/// Meld caller — also inert (`pushed=0`; nothing is parked on a meld, because only the `Suppress`
/// TOKEN commit parks `pending_token_battlefield_entry`). The CR 616.1 counter-pause reaches this
/// call **0** times: it returns from `finish_copy_target_choice_entry` at the
/// `apply_etb_counters == false` branch, well above the flush. So an earlier draft of this comment
/// naming "the CR 616.1 counter-pause and Meld returns" as the callers this guards was wrong on
/// both counts.
///
/// The call site's own comment says the flush precedes the replay / batch-drain / aura blocks so
/// THEIR pause returns cannot strand a parked entry. **That guard function is stated, not
/// established.** The intersection it describes — a Token-liminal entry that raises a pause AFTER
/// the flush — IS reachable with the ordinary card pool: Embalm copying a creature whose ETB
/// targets (e.g. Flametongue Kavu) drives
/// `["ReplacementChoice(2)", "CopyTargetChoice", "TriggerTargetSelection"]`, returning the
/// `replay_deferred_entry_events` pause after the flush. Measured at that pause with the flush call
/// deleted vs intact: identical state in both arms (`parked=false`, the same single entry row).
/// **Nothing is stranded**, so the only reachable instance does not demonstrate the guard.
///
/// Honest status, therefore: retained defensively. No shape reachable today has been measured to
/// strand an entry when this call is removed, and no fixture pins it. Keep it for the CR 704.3
/// ordering property it does provide on the 11 realizing calls (the CR 400.7 row is written before
/// the settling action's SBA pass, so CR 704.5f cannot bury a 0-toughness copy first); do not cite
/// a guard this file cannot demonstrate.
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
    //
    // WHOLE vector, not the `[..2]` slice this used to assert. The slice existed only because the
    // unsettled action echoed a stale third `CopyTargetChoice`; with that fixed the full vector is
    // assertable, and asserting it makes this test the cheapest stale-prompt regression detector on
    // the route it names.
    assert_eq!(
        drive.prompts,
        vec![
            "ReplacementChoice(2)".to_string(),
            "CopyTargetChoice".to_string()
        ],
        "the unpaused route reaches the copy-target prompt with no intervening pause, and that \
         answer is the LAST prompt because it settles"
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

// ═════════════════════════════════════════════════════════════════════════════════════════════
// CR 400.7 + CR 608.2i + CR 603.2c — the SIX remaining battlefield-entry emit sites.
//
// `snapshot_for_zone_change` leaves `turn_zone_change_index` at its `0` placeholder for
// `restrictions::record_zone_change` to overwrite. Six production sites (seven fix points) built
// the record and emitted it WITHOUT ever reaching the recorder, so each shipped index `0`:
//   S1 `conjure.rs`                            — conjure onto the battlefield
//   S2 `counters.rs` InjectPredefinedTokenAbilities — incubate resumed past a counter pause
//   S3a `counters.rs` FinalizeTokenEntry        — spec token resumed past a counter pause
//   S3b `counters.rs` FinalizeCopyTokenEntry    — copy token resumed past a counter pause
//   S4  `token_copy.rs` copy-loop tail          — ordinary copy token
//   S5  `token_copy.rs` modification-pause resume
//   S6  `gift_delivery.rs` create_gift_token    — gift Treasure/Food/Fish/Card tokens
//
// THE DISCRIMINATING DIRECTION IS DICTATED BY THE GUARD.
// `triggers.rs::batched_zone_change_already_collected` suppresses only when EVERY index in the
// candidate batch is already in `batched_zone_change_trigger_fired`. So the site's entry must be
// driven SECOND, after a routed two-token batch has already collected `(def, 0)` and `(def, 1)`:
// the unrouted site then offers the single-element list `[0]`, `all()` is true, and its fire is
// swallowed (life delta stays 1). Routed, it offers `[2]`, which is not in the set, and it fires
// (life delta 2). Site FIRST would NOT discriminate — the batch's later entries carry uncollected
// indices, so `all()` is false and both trees fire. Same idiom as
// `mixed_group_sibling_last_also_fires` above.
// ═════════════════════════════════════════════════════════════════════════════════════════════

/// Run the real trigger pipeline over the events a directly-driven resolver produced — the same
/// two-line tail `mint_token_batch` and `incubate_batch` run inline.
fn settle(state: &mut GameState, events: &[GameEvent]) {
    process_triggers(state, events);
    drain_order_triggers_with_identity(state);
}

/// What one site fixture measured.
struct SiteRun {
    /// The permanent the SITE's own drive put onto the battlefield.
    site_obj: ObjectId,
    /// `turn_zone_change_index` of every battlefield `ZoneChanged` the SITE's drive emitted.
    site_indices: Vec<usize>,
    /// P0's life change across the whole fixture (reach batch + site entry).
    life_delta: i32,
    /// The reach batch's indices — the negative sibling: the already-correct `token.rs` route
    /// must be untouched by this change.
    batch_indices: Vec<usize>,
}

/// The single battlefield `ZoneChanged` the SITE's drive emitted: which object entered, and the
/// index the event actually shipped. Reads on BOTH trees — the unrouted sites still emit the
/// event, just carrying the `0` placeholder — which is what makes the index a discriminator
/// rather than a presence check.
fn site_entry(events: &[GameEvent], what: &str) -> (ObjectId, Vec<usize>) {
    let entries: Vec<(ObjectId, usize)> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                record,
                ..
            } => Some((*object_id, record.turn_zone_change_index)),
            _ => None,
        })
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "{what} must emit exactly ONE battlefield ZoneChanged for its own entry \
         (reach-guard: a fixture that never reached its site emits none); got {entries:?}"
    );
    (entries[0].0, vec![entries[0].1])
}

fn install_host_trigger(runner: &mut GameRunner, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&host)
        .expect("host permanent")
        .trigger_definitions
        .push(batched_etb_life_trigger());
}

/// Open the turn under measurement and, when `mint_batch`, drive the routed two-token batch that
/// collects `(def, 0)` and `(def, 1)` — the reach batch every discriminator needs so its own
/// entry is the SECOND occurrence.
fn open_turn(runner: &mut GameRunner, host: ObjectId, mint_batch: bool) -> (i32, u32, Vec<usize>) {
    assert_eq!(
        runner.state().zone_changes_this_turn.len(),
        0,
        "legibility: scenario staging must leave the per-turn zone-change ledger empty, so the \
         indices below are the fixture's own"
    );
    let life_start = life_of_p0(runner.state());
    let turn_start = runner.state().turn_number;
    let batch_indices = if mint_batch {
        let batch = mint_token_batch(runner.state_mut(), host, 2);
        runner.advance_until_stack_empty();
        // POSITIVE reach-guard: without this the site's "unchanged total" below would be a
        // fixture that never triggered rather than a genuine suppression.
        assert_eq!(
            life_of_p0(runner.state()) - life_start,
            1,
            "the two-token reach batch fires the batched trigger exactly ONCE (CR 603.2c)"
        );
        zone_change_indices(&batch)
    } else {
        Vec::new()
    };
    (life_start, turn_start, batch_indices)
}

/// Answer the CR 616.1 counter-ordering pause a directly-driven resolver parked, through the REAL
/// engine dispatch: `runner.act` routes into `engine_replacement::handle_replacement_choice` plus
/// the post-action pipeline, which runs the trigger scan itself — hence no `settle` here. Shipped
/// idiom for "direct resolver parks `waiting_for`, then `runner.act(ChooseReplacement)`":
/// `counter_double_redirect_choice.rs`.
fn answer_counter_order(runner: &mut GameRunner, what: &str) -> Vec<GameEvent> {
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "{what} must park the CR 616.1 counter-ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "{what}: a 1-candidate prompt is an optional replacement, not the CR 616.1 ordering pause"
    );
    let result = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer the CR 616.1 ordering prompt");
    let events = result.events;
    runner.advance_until_stack_empty();
    events
}

/// M1–M4 plus the paired-deletion must-not-flip and the T9 negative sibling — the mechanism half
/// of every discriminator. NOT called by the T8 controls (their whole job is to read identically
/// on both trees).
fn assert_site_records(runner: &GameRunner, run: &SiteRun, expected_index: usize) {
    // M1 — ledger self-consistency. Every production push now routes through `record_zone_change`,
    // which sets index = position by construction. Unfixed at S1/S2 the direct `push_back` lands a
    // row at position 2 still carrying the placeholder `0`.
    assert!(
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .enumerate()
            .all(|(position, record)| record.turn_zone_change_index == position),
        "every zone-change row's index must equal its position — a direct `push_back` that skips \
         `record_zone_change` lands a row carrying the `0` placeholder; ledger = {:?}",
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .map(|record| (record.object_id, record.turn_zone_change_index))
            .collect::<Vec<_>>()
    );

    // M2 — row presence. The five sites that never reached the recorder pushed NO row at all.
    assert_eq!(
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|record| record.object_id == run.site_obj
                && record.to_zone == Zone::Battlefield)
            .count(),
        1,
        "the site's entry is on the CR 400.7 ledger exactly once (unrouted ⇒ 0; a kept direct \
         push alongside the recorder ⇒ 2)"
    );

    // M3 — the EMITTED index, the dedup key `triggers.rs` reads off the event.
    assert_eq!(
        run.site_indices,
        vec![expected_index],
        "the emitted ZoneChanged carries the index the recorder assigned (placeholder ⇒ [0])"
    );

    // M4 — subscript contract. `ability.rs::self_ref_own_departure_successor` uses this index as a
    // ROW SUBSCRIPT into `zone_changes_this_turn`; unfixed it subscripts row 0, which is the reach
    // batch's first Saproling, not the emitting object.
    assert_eq!(
        runner.state().zone_changes_this_turn[run.site_indices[0]].object_id,
        run.site_obj,
        "the emitted index must subscript the EMITTING object's own ledger row"
    );

    // MUST-NOT-FLIP FOR THE PAIRED DELETION — this is the assertion that carries that claim, and
    // it carries it because these sites are the ones the deletion actually touched. Measured on
    // `4b34e5465`, the pre-change `record_battlefield_entry` call sites outside `restrictions.rs`
    // are `conjure.rs:191`, `counters.rs:518/558/637`, `gift_delivery.rs:157` and
    // `token_copy.rs:851/969` — the routes `SiteRun` drives. `record_zone_change` performs the
    // CR 608.2i bookkeeping itself, so re-adding any one of those deleted calls makes this 2.
    // MEASURED: re-adding `gift_delivery.rs:157` fails this assertion, `left: 2 right: 1`.
    assert_eq!(
        runner
            .state()
            .battlefield_entries_this_turn
            .iter()
            .filter(|record| record.object_id == run.site_obj)
            .count(),
        1,
        "the site's entry is on the CR 608.2i ledger exactly once (re-adding the deleted \
         `record_battlefield_entry` ⇒ 2)"
    );

    // T9 NEGATIVE SIBLING: the already-correct `token.rs` route is untouched. Neither counter
    // doubler replaces token creation, so the reach batch is exactly two entries on both trees.
    assert_eq!(
        run.batch_indices,
        vec![0, 1],
        "the reach batch keeps its own two legitimate indices"
    );
}

/// Both doublers of the CREATURE pair. Their `valid_card` is `Typed{[Creature], You}`, so they
/// admit a creature entrant only.
fn stage_creature_counter_pair(scenario: &mut GameScenario) {
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES_ORACLE);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION_ORACLE);
}

/// Both doublers of the ANY-PERMANENT pair — required whenever the entrant is an ARTIFACT (the
/// Incubator, the Equipment token), which the creature pair provably rejects. Vorinclex's
/// doubling clause parses with `valid_card == None`; Ozolith's admits `artifact or creature you
/// control`. `add_enchantment_from_oracle` stands in for the missing artifact builder: replacement
/// candidacy gates the SOURCE only on its zone, and `valid_card` filters the AFFECTED object.
fn stage_any_permanent_counter_pair(scenario: &mut GameScenario) {
    scenario.add_creature_from_oracle(P0, "Vorinclex, Monstrous Raider", 6, 6, VORINCLEX_ORACLE);
    scenario.add_enchantment_from_oracle(
        P0,
        "Ozolith, the Shattered Spire",
        OZOLITH_SHATTERED_SPIRE_ORACLE,
    );
}

fn copy_token_effect(additional_modifications: Vec<ContinuousModification>) -> Effect {
    Effect::CopyTokenOf {
        target: TargetFilter::Any,
        owner: TargetFilter::Controller,
        source_filter: None,
        enters_attacking: false,
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        extra_keywords: Vec::new(),
        additional_modifications,
    }
}

// ── S1 · conjure.rs ──────────────────────────────────────────────────────────────────────────

fn drive_s1(mint_batch: bool) -> (GameRunner, SiteRun) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    let mut runner = scenario.build();
    install_host_trigger(&mut runner, host);

    let (life_start, turn_start, batch_indices) = open_turn(&mut runner, host, mint_batch);

    // A missing card-registry entry is harmless here: `ConjuredIdentity::Named { face: None }`
    // still creates the object and still takes the `destination == Battlefield` arm.
    let ability = ResolvedAbility::new(
        Effect::Conjure {
            cards: vec![ConjureCard {
                source: ConjureSource::Named {
                    name: "Verdant Dread".to_string(),
                },
                count: QuantityExpr::Fixed { value: 1 },
            }],
            destination: Zone::Battlefield,
            tapped: false,
            library_position: None,
            library_players: None,
        },
        Vec::new(),
        host,
        P0,
    );
    let mut events = Vec::new();
    conjure::resolve(runner.state_mut(), &ability, &mut events).expect("the conjure resolves");
    let (site_obj, site_indices) = site_entry(&events, "the battlefield conjure");
    settle(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().turn_number,
        turn_start,
        "the whole fixture is ONE turn (both ledgers are per-turn)"
    );
    let life_delta = life_of_p0(runner.state()) - life_start;
    (
        runner,
        SiteRun {
            site_obj,
            site_indices,
            life_delta,
            batch_indices,
        },
    )
}

/// S1 (CR 603.6a + CR 603.2c): a conjured battlefield entry driven AFTER a routed token batch is a
/// distinct occurrence and must fire the batched ETB trigger again.
///
/// REVERT-PROBE (discriminating, RUN): replace the
/// `zones::record_and_emit_entry_from_no_zone` call at `conjure.rs` with the hand-rolled
/// `snapshot_for_zone_change` + `state.zone_changes_this_turn.push_back(…)` +
/// `events.push(ZoneChanged)` ⇒ the conjured entry ships the `0` placeholder, collides
/// with the batch's already-collected `(def, 0)`, its fire is swallowed, and the life delta reads
/// 1 instead of 2 (M1, M3 and M4 fail with it).
#[test]
fn conjured_battlefield_entry_after_a_token_batch_fires_the_batched_trigger() {
    let (runner, run) = drive_s1(true);
    assert_eq!(
        run.life_delta, 2,
        "the conjured entry after a token batch fires the batched trigger AGAIN \
         (index-0 placeholder ⇒ collides with the batch's legitimate 0 ⇒ 1)"
    );
    assert_site_records(&runner, &run, 2);
}

// ── S6 · gift_delivery.rs ────────────────────────────────────────────────────────────────────

/// Consumer 1's PRODUCTION seam: `quantity.rs`'s `ZoneChangeCountThisTurn` population scan, driven
/// through the real resolver `game::quantity::resolve_quantity`. `TargetFilter::Any` reaches
/// `zone_change_filter_inner`'s `Any => true` arm, so no filter conjunct can dominate the answer,
/// and `source` is only the filter-context source id — which `Any` ignores. That is what makes the
/// same query legible at a point where the site object does not exist yet.
fn zone_change_count_this_turn(runner: &GameRunner, source: ObjectId) -> i32 {
    resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::ZoneChangeCountThisTurn {
                from: None,
                to: Some(Zone::Battlefield),
                filter: TargetFilter::Any,
            },
        },
        P0,
        source,
    )
}

/// The battlefield `ZoneChanged` record the SITE's own drive emitted for `site_obj`.
fn site_entry_record(
    events: &[GameEvent],
    site_obj: ObjectId,
) -> engine::types::game_state::ZoneChangeRecord {
    events
        .iter()
        .find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                record,
                ..
            } if *object_id == site_obj => Some((**record).clone()),
            _ => None,
        })
        .expect("the site's own battlefield ZoneChanged")
}

/// `drive_s6` with its two observation points exposed, so tests **H** and **L** can read the state
/// a monolithic drive hides: the trigger host, consumer 1's answer BEFORE the site enters, and the
/// site's own emitted events (which carry the `ZoneChangeRecord` that went on the wire). One body,
/// so the S6 discriminator, its T8 control, H and L all run the identical fixture.
fn drive_s6_observed(mint_batch: bool) -> (GameRunner, SiteRun, Vec<GameEvent>, ObjectId, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    let mut runner = scenario.build();
    install_host_trigger(&mut runner, host);

    let (life_start, turn_start, batch_indices) = open_turn(&mut runner, host, mint_batch);

    // OBSERVATION POINT (1) — consumer 1's seam AFTER the priming batch, BEFORE the site's entry.
    // The site object does not exist yet, so the host is the filter-context source at both points
    // and the two calls are literally the same query.
    let before = zone_change_count_this_turn(&runner, host);

    // Both context fields are load-bearing: `resolve` no-ops without `additional_cost_paid`, and
    // returns early without a latched recipient (CR 702.174a). Same shape as the in-repo
    // `make_gift_ability`.
    let mut ability = ResolvedAbility::new(
        Effect::GiftDelivery {
            kind: GiftKind::Treasure,
        },
        Vec::new(),
        host,
        P0,
    );
    ability.context.additional_cost_paid = true;
    ability.context.gift_recipient = Some(PlayerId(1));

    let mut events = Vec::new();
    gift_delivery::resolve(runner.state_mut(), &ability, &mut events)
        .expect("the gift delivery resolves");
    // Reach-guard: the Treasure was actually created. The batched trigger has no `valid_card`, so
    // P1's token still fires it and its `GainLife { player: Controller }` still pays P0.
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::TokenCreated { .. })),
        "the promised gift must create the Treasure token"
    );
    let (site_obj, site_indices) = site_entry(&events, "the gift token");
    settle(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().turn_number, turn_start, "one turn");
    let life_delta = life_of_p0(runner.state()) - life_start;
    (
        runner,
        SiteRun {
            site_obj,
            site_indices,
            life_delta,
            batch_indices,
        },
        events,
        host,
        before,
    )
}

fn drive_s6(mint_batch: bool) -> (GameRunner, SiteRun) {
    let (runner, run, _events, _host, _before) = drive_s6_observed(mint_batch);
    (runner, run)
}

/// **H** — CR 400.7 + CR 603.6a, the event↔ledger IDENTITY agreement (requirement R3).
///
/// `Ability::self_ref_own_departure_successor` (`types/ability.rs`) uses the index the EVENT
/// carries as a SUBSCRIPT into `state.zone_changes_this_turn` and then requires the row it lands on
/// to carry the SAME `trigger_source_context().identity.reference` as the event's own record. An
/// entry that emits without recording ships the `0` placeholder, so that subscript lands on a row
/// belonging to a DIFFERENT object and the `SelfRef` binding silently fails.
///
/// This asserts R3, not `Some`-ness: `trigger_source_context` is `Some` on BOTH trees by
/// construction (`game_object.rs`'s snapshot builds it unconditionally), so an
/// `assert!(…is_some())` here would be a snapshot of the constructor and could never fail. The
/// payload is assertion (2).
///
/// VACUITY: the priming batch is MANDATORY. With an empty ledger the site's index would legally be
/// `0` and row `0` would be the site's own row, so (2) would pass pre-fix for the wrong reason.
/// The batch-first ordering is what makes H discriminating, exactly as for T1–T7; the
/// `batch_indices == [0, 1]` guard below pins it.
///
/// REVERT-PROBE (discriminating, RUN — this is PROBE X): revert the SITE, not the shared
/// authority. In `gift_delivery.rs::create_gift_token`, replace the
/// `token::push_committed_token_entry_events` call with the hand-rolled pair —
/// `record_battlefield_entry`,
/// `zone_changes_this_turn.push_back(snapshot_for_zone_change(obj, None, Battlefield))`, then
/// `events.push(ZoneChanged { .. })` + `events.push(TokenCreated { .. })` — so the site's row
/// EXISTS but keeps the `0` placeholder index.
///
/// Site-isolated, not authority-wide, and that is load-bearing: reverting the shared authority
/// also strips the PRIMING batch's rows, so this test dies at the vacuity guard above
/// (MEASURED: `left: [0, 0] right: [0, 1]` at the `batch_indices` assertion) and assertion (2) is
/// never evaluated. The site-isolated form leaves the priming batch on the real authority, so the
/// guard passes and the payload is what flips.
///
/// MEASURED, site-isolated: `idx == 0`, the ledger row at `0` is the priming batch's first
/// Saproling, and assertion (2) fails —
/// `left: ObjectIncarnationRef { object_id: ObjectId(2), incarnation: 1 }`,
/// `right: ObjectIncarnationRef { object_id: ObjectId(5), incarnation: 1 }`,
/// ledger `[(ObjectId(2), 0), (ObjectId(3), 1), (ObjectId(5), 0)]`.
/// MEASURED on the same probe build: **L** and its folded L-b assertions stay GREEN — that
/// contrast is what proves H's payload is the index/identity while L's is row presence.
#[test]
fn the_site_row_the_event_subscripts_carries_the_entering_objects_identity() {
    let (runner, run, events, _host, _before) = drive_s6_observed(true);

    // Vacuity guard, before touching the site row: the priming batch really did take 0 and 1.
    assert_eq!(
        run.batch_indices,
        vec![0, 1],
        "H is only discriminating when the site's entry is the SECOND occurrence — the priming \
         batch must own indices 0 and 1"
    );

    let record = site_entry_record(&events, run.site_obj);
    let idx = record.turn_zone_change_index;

    // (1) PRECONDITION R1 — an `.expect`, deliberately not an `assert!(…is_some())`: this is a
    // precondition of the payload, and it holds on both trees.
    let ev_ctx = record
        .trigger_source_context()
        .expect("a real zone-change event carries its source context");

    // (2) PAYLOAD — R3. The ledger row the EVENT subscripts is the row that event wrote.
    assert_eq!(
        runner.state().zone_changes_this_turn[idx]
            .trigger_source_context()
            .expect("the recorded row carries its source context")
            .identity
            .reference,
        ev_ctx.identity.reference,
        "CR 400.7: the ledger row at the event's own `turn_zone_change_index` must be the record \
         that event wrote — an unrecorded entry ships the `0` placeholder and subscripts a row \
         belonging to a different object (ledger = {:?})",
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .map(|row| (row.object_id, row.turn_zone_change_index))
            .collect::<Vec<_>>()
    );

    // (3) R2 — the identity is the entering object's live incarnation.
    assert_eq!(ev_ctx.identity.reference.object_id, run.site_obj);
    assert_eq!(
        ev_ctx.identity.reference.incarnation,
        runner
            .state()
            .objects
            .get(&run.site_obj)
            .expect("the gift token is on the battlefield")
            .incarnation
    );

    // (4) R4 — free and NON-DISCRIMINATING, labelled as such: `ObjectIdentityBinding::new(…,
    // from.unwrap_or(self.zone))` falls back to the live zone when `from` is `None`, and that
    // fallback is independently guaranteed by `record_battlefield_entry`'s
    // `obj.zone != Battlefield → return` guard, which `assert_site_records`'s must-not-flip clause
    // already exercises. It passes on both trees.
    assert_eq!(ev_ctx.identity.expected_zone, Zone::Battlefield);
}

/// **L** (with **L-b** folded in) — CR 400.7: the new ledger row is visible at the two production
/// seams that read `zone_changes_this_turn` by content rather than by subscript.
///
/// **L** drives consumer 1, `quantity.rs`'s `ZoneChangeCountThisTurn` population scan, through the
/// real resolver at TWO points of ONE run: after the priming batch (`before`) and after the site's
/// drive (`after`). The assertion is a DELTA (`after == before + 1`), never an absolute — the
/// primed fixture is what gives L-b its cross-producer property, and a delta cannot be invalidated
/// by a change in how many rows the priming batch mints.
///
/// **L-b** then runs consumer 4's own seam on the same post-drive state:
/// `filter::matches_target_filter_on_zone_change_record` with `TargetFilter::SelfRef` and a
/// `FilterContext::from_trigger_source` built from the site event's context. Three
/// `(None, Battlefield)` rows from TWO producers are on the ledger; the seam must select exactly
/// the site's.
///
/// ZERO-CENSUS POSITIVE CONTROL (mandatory): `before > 0` is asserted in the same run. An
/// instrument that can only ever answer `0` proves nothing about an absence.
///
/// DOMINATING-CONJUNCT CHECK: `TargetFilter::Any` reaches `zone_change_filter_inner`'s
/// `Any => true`, and the `from`/`to` conjuncts are `is_none_or`, so nothing upstream of the row
/// can dominate L's answer. `matches_target_filter_on_zone_change_record` is a pure pass-through to
/// `zone_change_filter_inner`, so nothing sits between the row and the `SelfRef` arm for L-b.
///
/// DISCLOSED NON-DISCRIMINATION (do not upgrade this claim): `FilterContext::from_trigger_source`
/// sets `source_id` from the same identity, so the `SelfRef` arm's `map_or` fallback would select
/// the same row. L-b measures ADMISSION at consumer 4's seam, not identity-vs-ObjectId inside the
/// arm. The identity payload is H's job.
///
/// REVERT-PROBE (discriminating, RUN — this is PROBE Y): revert the SITE, not the shared
/// authority. In `gift_delivery.rs::create_gift_token`, replace the
/// `token::push_committed_token_entry_events` call with `events.push(ZoneChanged { .. })` +
/// `events.push(TokenCreated { .. })` over a bare `snapshot_for_zone_change` and NO recording at
/// all — no `push_back`, no `record_battlefield_entry`, index left at its `0` placeholder.
///
/// Site-isolated, not authority-wide, and that is load-bearing: reverting the shared authority
/// also strips the PRIMING batch's rows, so the zero-census positive control above fails first
/// (`before` collapses to `0`) and the fixture does NOT stay intact. The site-isolated form leaves
/// the priming batch on the real authority, so `before` stays non-zero and the delta is what flips.
///
/// MEASURED, site-isolated: the DELTA assertion fails — `left: 2 right: 3`, `before = 2,
/// after = 2` — with the `before > 0` control green. The L-b `selected` assertion is NOT reached
/// (the delta assertion dominates), so this probe pins row PRESENCE at consumer 1's seam only;
/// no claim is made here about L-b flipping. PROBE X — the site's row still pushed, index left at
/// `0` — leaves this test GREEN (measured); that is H's probe, not L's.
#[test]
fn zone_change_count_this_turn_sees_the_gift_token_entry() {
    let (runner, run, events, host, before) = drive_s6_observed(true);

    // Zero-census positive control: the instrument answers non-zero before the site ever runs.
    assert!(
        before > 0,
        "the priming batch must already be visible at consumer 1's seam — an instrument stuck at \
         0 could not distinguish 'the site added nothing' from 'the query sees nothing'"
    );

    let after = zone_change_count_this_turn(&runner, host);
    assert_eq!(
        after,
        before + 1,
        "CR 400.7: the gift token's battlefield entry must add exactly one row that consumer 1's \
         production scan can see (before = {before}, after = {after})"
    );

    // L-b — consumer 4 (`filter.rs`'s `TargetFilter::SelfRef` arm) at its own production seam.
    let record = site_entry_record(&events, run.site_obj);
    let ev_ctx = record
        .trigger_source_context()
        .expect("a real zone-change event carries its source context");
    let ctx = FilterContext::from_trigger_source(ev_ctx);
    let select_with = |filter: &TargetFilter| -> Vec<ObjectId> {
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .filter(|row| {
                matches_target_filter_on_zone_change_record(runner.state(), row, filter, &ctx)
            })
            .map(|row| row.object_id)
            .collect()
    };

    // REACHABILITY EXHIBIT for the exclusion below. `selected == vec![site_obj]` is an asserted
    // NEGATIVE about the two priming rows, and a negative is vacuous if the excluded rows could
    // never have been selected in the first place. Run the SAME loop over the SAME ledger with an
    // admitting filter first: all three rows ARE reachable at this seam, so the exclusion is the
    // `SelfRef` arm's doing and not an artefact of the population.
    let reachable = select_with(&TargetFilter::Any);
    assert_eq!(
        reachable.len(),
        3,
        "reachability exhibit: with an admitting filter this seam selects all three \
         `(None, Battlefield)` rows this turn — two priming Saprolings and the site's own. \
         Without that, the SelfRef exclusion below would be a negative about rows that were never \
         selectable; got {reachable:?}"
    );
    assert!(
        reachable.contains(&run.site_obj),
        "reachability exhibit: the site's own row must be among them; got {reachable:?}"
    );

    let selected = select_with(&TargetFilter::SelfRef);
    assert_eq!(
        selected,
        vec![run.site_obj],
        "CR 400.7: consumer 4's SelfRef seam must select exactly the site's own row out of the \
         three `(None, Battlefield)` rows two different producers wrote this turn (all three are \
         reachable here — see the exhibit above)"
    );
}

/// S6 (CR 111.1 + CR 603.6a): a gift token entering after a routed token batch fires the batched
/// ETB trigger again.
///
/// REVERT-PROBE (discriminating, RUN): restore `record_battlefield_entry` plus the
/// snapshot/`ZoneChanged`/`TokenCreated` block in `gift_delivery.rs::create_gift_token` in place
/// of `push_committed_token_entry_events` ⇒ life delta 2→1, M2 1→0, M3 [2]→[0], M4 fails.
#[test]
fn gift_token_entry_after_a_token_batch_fires_the_batched_trigger() {
    let (runner, run) = drive_s6(true);
    assert_eq!(
        run.life_delta, 2,
        "the gift token entry after a token batch fires the batched trigger AGAIN"
    );
    assert_site_records(&runner, &run, 2);
}

// ── S4 · token_copy.rs copy-loop tail ────────────────────────────────────────────────────────

fn drive_s4(mint_batch: bool) -> (GameRunner, SiteRun) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    // The Watchdog's "enters with three +1/+1 counters" seeds `etb_counters`, which forces the
    // NON-liminal copy branch. With NO doubler on board the counter addition executes without
    // pausing, so the loop falls through to the S4 tail.
    let watchdog = scenario
        .add_creature(P0, "Faithful Watchdog", 0, 0)
        .with_plus_counters(3)
        .from_oracle_text_with_keywords(&["Vigilance"], FAITHFUL_WATCHDOG_ORACLE)
        .id();
    let mut runner = scenario.build();
    install_host_trigger(&mut runner, host);

    let (life_start, turn_start, batch_indices) = open_turn(&mut runner, host, mint_batch);

    let ability = ResolvedAbility::new(
        copy_token_effect(Vec::new()),
        vec![TargetRef::Object(watchdog)],
        host,
        P0,
    );
    let mut events = Vec::new();
    token_copy::resolve(runner.state_mut(), &ability, &mut events).expect("the copy resolves");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::TokenCreated { .. })),
        "the copy token must be created"
    );
    let (site_obj, site_indices) = site_entry(&events, "the copy-loop tail");

    // POSITIVE CONTROL for the `etb_counters` seed that routes this fixture (and T6's) into the
    // non-liminal branch: if this reads 0 the seed never materialized and the S3b fixture's
    // premise is dead too. Declared escalation, not a degradable assertion.
    assert_eq!(
        runner.state().objects[&site_obj]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        3,
        "the copy carries the Watchdog's three +1/+1 counters — this is what seeds `etb_counters` \
         and forces the non-liminal branch"
    );

    settle(runner.state_mut(), &events);
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().turn_number, turn_start, "one turn");
    let life_delta = life_of_p0(runner.state()) - life_start;
    (
        runner,
        SiteRun {
            site_obj,
            site_indices,
            life_delta,
            batch_indices,
        },
    )
}

/// S4 (CR 707.2 + CR 603.6a): an ordinary copy token's entry after a routed token batch fires the
/// batched ETB trigger again.
///
/// REVERT-PROBE (discriminating, RUN): restore `record_battlefield_entry` plus the
/// snapshot/`ZoneChanged`/`TokenCreated` block at the `token_copy.rs` copy-loop tail ⇒ life delta
/// 2→1, M2 1→0, M3 [2]→[0], M4 fails.
#[test]
fn copy_token_tail_entry_after_a_token_batch_fires_the_batched_trigger() {
    let (runner, run) = drive_s4(true);
    assert_eq!(
        run.life_delta, 2,
        "the copy token's entry after a token batch fires the batched trigger AGAIN"
    );
    assert_site_records(&runner, &run, 2);
}

// ── S5 · token_copy.rs modification-pause resume ─────────────────────────────────────────────

fn drive_s5(mint_batch: bool) -> (GameRunner, SiteRun) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    // Vanilla source: it seeds NO `etb_counters`, so the only pausable stage is
    // `apply_token_modifications` — which is the S5 resume, not S3b's.
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // The copy is a CREATURE, so the creature-scoped doubler pair admits it.
    stage_creature_counter_pair(&mut scenario);
    let mut runner = scenario.build();
    install_host_trigger(&mut runner, host);

    let (life_start, turn_start, batch_indices) = open_turn(&mut runner, host, mint_batch);

    // `AddCounterOnEnter` is NOT liminal-immediate, so the non-liminal branch runs regardless of
    // `etb_counters`; its counter addition meets two competing replacements and parks CR 616.1.
    let ability = ResolvedAbility::new(
        copy_token_effect(vec![ContinuousModification::AddCounterOnEnter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            if_type: None,
        }]),
        vec![TargetRef::Object(bears)],
        host,
        P0,
    );
    let mut events = Vec::new();
    token_copy::resolve(runner.state_mut(), &ability, &mut events).expect("the copy resolves");
    let resume_events = answer_counter_order(&mut runner, "the copy's AddCounterOnEnter");
    let (site_obj, site_indices) = site_entry(&resume_events, "the modification-pause resume");

    assert_eq!(runner.state().turn_number, turn_start, "one turn");
    let life_delta = life_of_p0(runner.state()) - life_start;
    (
        runner,
        SiteRun {
            site_obj,
            site_indices,
            life_delta,
            batch_indices,
        },
    )
}

/// S5 (CR 616.1 + CR 603.6a): a copy token whose entry was postponed across a counter-ordering
/// pause fires the batched ETB trigger when it finally enters after a routed token batch.
///
/// REVERT-PROBE (discriminating, RUN): restore `record_battlefield_entry` plus the
/// snapshot/`ZoneChanged`/`TokenCreated` block in
/// `token_copy.rs::apply_remaining_token_modifications_after_counter_pause` ⇒ life delta 2→1,
/// M2 1→0, M3 [2]→[0], M4 fails.
#[test]
fn modification_paused_copy_token_entry_after_a_token_batch_fires_the_batched_trigger() {
    let (runner, run) = drive_s5(true);
    assert_eq!(
        run.life_delta, 2,
        "the modification-paused copy's entry fires the batched trigger AGAIN"
    );
    assert_site_records(&runner, &run, 2);
}

// ── S3a · counters.rs FinalizeTokenEntry (unattached) ────────────────────────────────────────

fn spec_token_effect(types: Vec<String>, attach_to: Option<TargetFilter>, name: &str) -> Effect {
    Effect::Token {
        name: name.to_string(),
        power: PtValue::Fixed(0),
        toughness: PtValue::Fixed(0),
        types,
        colors: Vec::new(),
        keywords: Vec::new(),
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        owner: TargetFilter::Controller,
        attach_to,
        enters_attacking: false,
        supertypes: Vec::new(),
        static_abilities: Vec::new(),
        enter_with_counters: vec![(CounterType::Plus1Plus1, QuantityExpr::Fixed { value: 1 })],
    }
}

fn drive_s3a_unattached(mint_batch: bool) -> (GameRunner, SiteRun) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    stage_creature_counter_pair(&mut scenario);
    let mut runner = scenario.build();
    install_host_trigger(&mut runner, host);

    let (life_start, turn_start, batch_indices) = open_turn(&mut runner, host, mint_batch);

    // A CREATURE token with `enter_with_counters`: the two competing +1/+1 replacements park
    // CR 616.1 mid-entry, and the answer drains through `FinalizeTokenEntry`.
    let ability = ResolvedAbility::new(
        spec_token_effect(vec!["Creature".to_string()], None, "Counter Saproling"),
        Vec::new(),
        host,
        P0,
    );
    let mut events = Vec::new();
    token::resolve(runner.state_mut(), &ability, &mut events).expect("the token resolves");
    let resume_events = answer_counter_order(&mut runner, "the token's enter_with_counters");
    let (site_obj, site_indices) = site_entry(&resume_events, "the FinalizeTokenEntry resume");

    assert_eq!(runner.state().turn_number, turn_start, "one turn");
    let life_delta = life_of_p0(runner.state()) - life_start;
    (
        runner,
        SiteRun {
            site_obj,
            site_indices,
            life_delta,
            batch_indices,
        },
    )
}

/// S3a (CR 616.1 + CR 603.6a), unattached arm: a spec token whose entry was postponed across a
/// counter-ordering pause fires the batched ETB trigger when it enters after a routed batch.
///
/// REVERT-PROBE (discriminating, RUN): restore the private `counters.rs::push_token_entry_events`
/// clone, repoint the `FinalizeTokenEntry` arm back at it, and restore its
/// `record_battlefield_entry` ⇒ life delta 2→1, M2 1→0, M3 [2]→[0], M4 fails.
#[test]
fn counter_paused_token_entry_after_a_token_batch_fires_the_batched_trigger() {
    let (runner, run) = drive_s3a_unattached(true);
    assert_eq!(
        run.life_delta, 2,
        "the counter-paused token's entry fires the batched trigger AGAIN"
    );
    assert_site_records(&runner, &run, 2);
}

// ── S3a · counters.rs FinalizeTokenEntry (attach arm) ────────────────────────────────────────

fn drive_s3a_attached(mint_batch: bool) -> (GameRunner, SiteRun) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    let attach_host = scenario.add_creature(P0, "Equipped Host", 2, 2).id();
    let painter = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT_ORACLE)
        .id();
    // The entrant is an ARTIFACT Equipment, which the creature-scoped pair would reject.
    stage_any_permanent_counter_pair(&mut scenario);
    let mut runner = scenario.build();
    install_host_trigger(&mut runner, host);
    // `add_creature_from_oracle` places the object directly on the battlefield without running the
    // entry pipeline, so Painter's "As this creature enters, choose a color" never raised its
    // `NamedChoice` and `chosen_color()` would answer `None` — leaving the colour instrument below
    // inert. Stage the choice the pipeline would have recorded.
    runner
        .state_mut()
        .objects
        .get_mut(&painter)
        .expect("Painter's Servant is on the battlefield")
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::Blue));

    let (life_start, turn_start, batch_indices) = open_turn(&mut runner, host, mint_batch);

    let ability = ResolvedAbility::new(
        spec_token_effect(
            vec!["Artifact".to_string(), "Equipment".to_string()],
            Some(TargetFilter::ParentTarget),
            "Bladed Rig",
        ),
        vec![TargetRef::Object(attach_host)],
        host,
        P0,
    );
    let mut events = Vec::new();
    token::resolve(runner.state_mut(), &ability, &mut events).expect("the token resolves");
    let resume_events = answer_counter_order(&mut runner, "the Equipment token's counters");
    let (site_obj, site_indices) = site_entry(&resume_events, "the attached FinalizeTokenEntry");

    // Reach-guard for the ATTACH arm specifically: without this the fixture would be a second copy
    // of the unattached test.
    assert_eq!(
        runner.state().objects[&site_obj].attached_to,
        Some(AttachTarget::Object(attach_host)),
        "the Equipment token must enter attached (CR 301.5) — this is what makes the entry record \
         post-`flush_layers`"
    );

    assert_eq!(runner.state().turn_number, turn_start, "one turn");
    let life_delta = life_of_p0(runner.state()) - life_start;
    (
        runner,
        SiteRun {
            site_obj,
            site_indices,
            life_delta,
            batch_indices,
        },
    )
}

/// S3a (CR 616.1 + CR 301.5 + CR 603.6a), ATTACH arm. Two claims:
///
/// (A) FIX — unconditional discriminator, identical to the unattached arm's.
/// REVERT-PROBE (discriminating, RUN): restore `counters.rs::push_token_entry_events`, repoint
/// `FinalizeTokenEntry` back at it, restore its `record_battlefield_entry` ⇒ life delta 2→1,
/// M2 1→0, M3 [2]→[0], M4 fails.
///
/// (B) ORDERING — the record point moves from before `attach::attach_to` to after it, and
/// `attach_to` ends in a SYNCHRONOUS `flush_layers`, so every layer-derived field of the entry
/// record (`colors`, `keywords`, types, controller) is now snapshotted post-flush. That is the
/// CR-correct point (it is what the unpaused twin in `token.rs` already does: attach, then call
/// the same helper). REVERT-PROBE (conditional, RUN and journal either way): move the S3a record
/// point back to before the attach block. If the `colors` equality below flips, this test covers
/// the ordering change too; if it does not, the ordering change ships untested and that is
/// disclosed rather than papered over.
#[test]
fn counter_paused_attached_token_entry_after_a_token_batch_fires_the_batched_trigger() {
    let (runner, run) = drive_s3a_attached(true);
    assert_eq!(
        run.life_delta, 2,
        "the counter-paused ATTACHED token's entry fires the batched trigger AGAIN"
    );
    assert_site_records(&runner, &run, 2);

    // FIXTURE-VALIDITY GUARD (fail-loud, deliberately identical on both trees): Painter's Servant
    // must actually colour the Equipment token. An uncoloured token makes the ordering assertion
    // below vacuous.
    let live_color = runner.state().objects[&run.site_obj].color.clone();
    assert!(
        !live_color.is_empty(),
        "Painter's Servant must colour the Equipment token — an inert fixture makes the ordering \
         assertion vacuous"
    );

    // (B) ORDERING: the entry record is taken after `attach_to`'s synchronous `flush_layers`, so
    // its layer-derived colours agree with the live object.
    let entry_row = runner
        .state()
        .battlefield_entries_this_turn
        .iter()
        .find(|record| record.object_id == run.site_obj)
        .expect("the attached token has a CR 608.2i battlefield-entry row");
    assert_eq!(
        entry_row.colors, live_color,
        "the entry record snapshots the token's post-attach, post-flush colours (CR 608.2i)"
    );
}

// ── S3b · counters.rs FinalizeCopyTokenEntry ─────────────────────────────────────────────────

fn drive_s3b(mint_batch: bool) -> (GameRunner, SiteRun) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    let watchdog = scenario
        .add_creature(P0, "Faithful Watchdog", 0, 0)
        .with_plus_counters(3)
        .from_oracle_text_with_keywords(&["Vigilance"], FAITHFUL_WATCHDOG_ORACLE)
        .id();
    // The copy is a CREATURE, so the creature pair admits it — and unlike S5 there are NO
    // modifications, so stage 1 cannot pause and the `etb_counters` loop is what parks CR 616.1.
    stage_creature_counter_pair(&mut scenario);
    let mut runner = scenario.build();
    install_host_trigger(&mut runner, host);

    let (life_start, turn_start, batch_indices) = open_turn(&mut runner, host, mint_batch);

    let ability = ResolvedAbility::new(
        copy_token_effect(Vec::new()),
        vec![TargetRef::Object(watchdog)],
        host,
        P0,
    );
    let mut events = Vec::new();
    token_copy::resolve(runner.state_mut(), &ability, &mut events).expect("the copy resolves");
    let resume_events = answer_counter_order(&mut runner, "the copy's seeded etb_counters");
    let (site_obj, site_indices) = site_entry(&resume_events, "the FinalizeCopyTokenEntry resume");

    assert_eq!(runner.state().turn_number, turn_start, "one turn");
    let life_delta = life_of_p0(runner.state()) - life_start;
    (
        runner,
        SiteRun {
            site_obj,
            site_indices,
            life_delta,
            batch_indices,
        },
    )
}

/// S3b (CR 616.1 + CR 707.2 + CR 603.6a): a copy token whose SEEDED etb-counter placement parked
/// the ordering choice fires the batched ETB trigger when it enters after a routed batch.
///
/// REVERT-PROBE (discriminating, RUN): restore `counters.rs::push_token_entry_events`, repoint the
/// `FinalizeCopyTokenEntry` arm back at it, restore its `record_battlefield_entry` ⇒ life delta
/// 2→1, M2 1→0, M3 [2]→[0], M4 fails.
#[test]
fn counter_paused_copy_token_entry_after_a_token_batch_fires_the_batched_trigger() {
    let (runner, run) = drive_s3b(true);
    assert_eq!(
        run.life_delta, 2,
        "the counter-paused copy token's entry fires the batched trigger AGAIN"
    );
    assert_site_records(&runner, &run, 2);
}

// ── S2 · counters.rs InjectPredefinedTokenAbilities (incubate resume) ────────────────────────

fn drive_s2(mint_batch: bool) -> (GameRunner, SiteRun) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Batched Watcher", 1, 1).id();
    // The Incubator is a colorless ARTIFACT: the creature-scoped pair provably does NOT admit it,
    // so this fixture must use the any-permanent pair or it would never pause.
    stage_any_permanent_counter_pair(&mut scenario);
    let mut runner = scenario.build();
    install_host_trigger(&mut runner, host);

    let (life_start, turn_start, batch_indices) = open_turn(&mut runner, host, mint_batch);

    let ability = ResolvedAbility::new(
        Effect::Incubate {
            count: QuantityExpr::Fixed { value: 1 },
        },
        Vec::new(),
        host,
        P0,
    );
    let mut events = Vec::new();
    incubate::resolve(runner.state_mut(), &ability, &mut events).expect("the incubate resolves");
    // The entry is DEFERRED behind the counter pause: nothing has entered yet.
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, GameEvent::ZoneChanged { .. })),
        "the Incubator's entry is deferred until its counters settle — a ZoneChanged here means \
         the fixture never paused and is measuring the unpaused incubate route instead"
    );
    let resume_events = answer_counter_order(&mut runner, "the Incubator's counter");
    let (site_obj, site_indices) = site_entry(&resume_events, "the incubate resume");

    // POSITIVE CONTROL: both doublers really applied. Base is 1; either alone reaches 2; the pair
    // reaches 3 (`(1*2)+1`) or 4 (`(1+1)*2`) depending on the chosen order. A reading of 2 means
    // Ozolith's parsed `quantity_modification` is not `Plus{1}` — escalate, do not weaken to `>=2`
    // (that would be satisfied by either doubler alone and so is vacuous).
    assert!(
        runner.state().objects[&site_obj]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
            >= 3,
        "both counter doublers must have applied to the Incubator; got {:?}",
        runner.state().objects[&site_obj].counters
    );

    assert_eq!(runner.state().turn_number, turn_start, "one turn");
    let life_delta = life_of_p0(runner.state()) - life_start;
    (
        runner,
        SiteRun {
            site_obj,
            site_indices,
            life_delta,
            batch_indices,
        },
    )
}

/// S2 (CR 616.1 + CR 603.6a): an Incubator whose entry was deferred behind a counter-ordering
/// pause fires the batched ETB trigger when it enters after a routed token batch.
///
/// REVERT-PROBE (discriminating, RUN): replace the `zones::record_and_emit_entry_from_no_zone`
/// call in the `InjectPredefinedTokenAbilities` arm with the hand-rolled
/// `snapshot_for_zone_change` + `state.zone_changes_this_turn.push_back(…)` +
/// `events.push(ZoneChanged)`, and restore its `record_battlefield_entry` ⇒ life delta 2→1,
/// M1 fails (a row at position 2 carrying index 0), M3 [2]→[0], M4 fails.
#[test]
fn incubate_resume_entry_after_a_token_batch_fires_the_batched_trigger() {
    let (runner, run) = drive_s2(true);
    assert_eq!(
        run.life_delta, 2,
        "the resumed Incubator's entry fires the batched trigger AGAIN"
    );
    assert_site_records(&runner, &run, 2);
}

// ── T8 · single-entry controls, one per site class ───────────────────────────────────────────
//
// Each drives the SAME `drive_<site>` body with no reach batch, into an EMPTY ledger. The
// instrument is the EMITTED index, which reads `[0]` on BOTH trees (unfixed: the placeholder;
// fixed: `len() == 0`) — a deliberate no-flip. These are NOT discriminators and carry no
// revert-probe: their job is to prove each fixture reaches its site and fires at all, so that the
// `== 2` failure of the tests above on an unfixed tree reads as SUPPRESSION rather than fixture
// breakage. They deliberately do not call `assert_site_records` or `ledger_index` — `ledger_index`
// panics on the unfixed tree at the five sites that record no row, which would destroy the
// control property.

fn assert_single_entry_control(run: &SiteRun) {
    assert_eq!(
        run.life_delta, 1,
        "the site's entry alone fires the batched trigger exactly once"
    );
    assert_eq!(
        run.site_indices,
        vec![0],
        "the first entry of an empty-ledger turn legitimately takes index 0"
    );
    assert!(
        run.batch_indices.is_empty(),
        "the control drives no reach batch"
    );
}

#[test]
fn t8_conjure_alone_into_an_empty_ledger_fires_once() {
    let (_runner, run) = drive_s1(false);
    assert_single_entry_control(&run);
}

#[test]
fn t8_gift_token_alone_into_an_empty_ledger_fires_once() {
    let (_runner, run) = drive_s6(false);
    assert_single_entry_control(&run);
}

#[test]
fn t8_copy_token_tail_alone_into_an_empty_ledger_fires_once() {
    let (_runner, run) = drive_s4(false);
    assert_single_entry_control(&run);
}

#[test]
fn t8_modification_paused_copy_alone_into_an_empty_ledger_fires_once() {
    let (_runner, run) = drive_s5(false);
    assert_single_entry_control(&run);
}

#[test]
fn t8_counter_paused_token_alone_into_an_empty_ledger_fires_once() {
    let (_runner, run) = drive_s3a_unattached(false);
    assert_single_entry_control(&run);
}

#[test]
fn t8_counter_paused_copy_alone_into_an_empty_ledger_fires_once() {
    let (_runner, run) = drive_s3b(false);
    assert_single_entry_control(&run);
}

#[test]
fn t8_incubate_resume_alone_into_an_empty_ledger_fires_once() {
    let (_runner, run) = drive_s2(false);
    assert_single_entry_control(&run);
}
