//! t96 — RUNTIME PROBES for the cost-X channel. Measures, never assumes.
//!
//! The charter's premise is that an unrewritten `QuantityRef::Variable{"X"}` in a cost-X
//! ability "fabricates 0". That is a claim about the RUNTIME, and the runtime has a fallback
//! chain the parser cannot see (`game/quantity.rs`, `resolve_ref`):
//!
//!     Variable{"X"}  ->  ability.chosen_x
//!                    ->  current_trigger_event -> extract_source_from_event -> obj.cost_x_paid
//!                    ->  0
//!
//! So a residual X fabricates ONLY where BOTH links are dead. These probes drive the real
//! cast/activate pipeline and read the resulting board, so each MEASURES which case it is
//! rather than predicting it.
//!
//! HARNESS NOTE (learned the hard way): `add_card_to_hand` builds a name-only object
//! "without rules text" — a probe built on it is VACUOUS and reads 0 for everything, which
//! looks exactly like a fabrication. Every card below is therefore synthesized from its
//! VERBATIM Oracle text (pool export, `cargo export-cards`) via the `*_from_oracle` builders,
//! and the control below exists precisely to catch a regression back into that vacuum.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn cost(shards: Vec<ManaCostShard>, generic: u32) -> ManaCost {
    ManaCost::Cost { shards, generic }
}

/// Battlefield objects whose name contains `needle`, as (name, power, toughness).
fn named_on_battlefield(
    runner: &engine::game::scenario::GameRunner,
    needle: &str,
) -> Vec<(String, i32, i32)> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.name.contains(needle))
        .map(|o| {
            (
                o.name.clone(),
                o.power.unwrap_or(0),
                o.toughness.unwrap_or(0),
            )
        })
        .collect()
}

fn zone_size(runner: &engine::game::scenario::GameRunner, zone: Zone) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == zone)
        .count()
}

// ─────────────────────────────────────────────────────────────────────────────
// CONTROL — is the cost_x_paid fallback link actually LIVE?
// ─────────────────────────────────────────────────────────────────────────────

/// CR 107.3m + CR 107.3e — Hydroid Krasis, `{X}{G}{U}`:
/// "When you cast this spell, you gain half X life and draw half X cards. Round down each time."
///
/// A `SpellCast` trigger. Its own `chosen_x` is None (a trigger has no cost of its own), so it
/// can only work by riding the second link: `current_trigger_event` -> the Krasis spell object
/// -> `cost_x_paid`, stamped by `finalize_cast`. Cast for X=4 it must gain half X = 2 life.
///
/// THIS IS THE NON-VACUITY CONTROL FOR THE WHOLE FILE. If it reads 0, the harness is not
/// really casting anything and every "fabrication" verdict below is void.
#[test]
fn control_spellcast_trigger_reads_cast_x_through_the_fallback() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Hydroid Krasis",
            0,
            0,
            "When you cast this spell, you gain half X life and draw half X cards. Round down \
             each time.\nFlying, trample",
        );
        b.with_mana_cost(cost(
            vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Blue],
            0,
        ));
        b.id()
    };
    let mut runner = scenario.build();

    let life_before = runner.state().players[0].life;
    add_mana(&mut runner, ManaType::Green, 4);
    add_mana(&mut runner, ManaType::Blue, 4);

    runner.cast(spell).x(4).resolve();

    let gained = runner.state().players[0].life - life_before;
    assert_eq!(
        gained, 2,
        "CONTROL: the Variable(\"X\") -> current_trigger_event -> cost_x_paid fallback must be \
         LIVE. Hydroid Krasis cast for X=4 gains half X = 2 life. A 0 here means the harness is \
         vacuous (see the HARNESS NOTE at the top of this file) and every fabrication verdict in \
         this file is void. MEASURED: {gained}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// GATED-ETB — the walk RUNS on these, but leaves some slots unrewritten.
// Does the fallback save them, or do they fabricate?
// ─────────────────────────────────────────────────────────────────────────────

/// Hugs, Grisly Guardian, `{X}{R}{R}{G}{G}`:
/// "When Hugs enters, exile the top X cards of your library. Until the end of your next turn,
/// you may play those cards."
///
/// A self-ETB trigger — the cost-X walk's gate PASSES — but the effect is a variant the walk
/// does NOT enumerate, so its `count` keeps a bare `Variable("X")`. Discriminates: does an
/// unenumerated ARM on a gated trigger fabricate, or does the ZoneChanged -> permanent ->
/// `cost_x_paid` fallback bind it anyway (making the missing arm a normalization gap, not a
/// bug)? The answer decides whether a totality guard on this walk may red these faces at all.
#[test]
fn gated_etb_unenumerated_effect_arm() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..8 {
        scenario.add_card_to_library_top(P0, &format!("Filler {i}"));
    }
    let spell = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Hugs, Grisly Guardian",
            5,
            5,
            "Trample\nWhen Hugs enters, exile the top X cards of your library. Until the end of \
             your next turn, you may play those cards.",
        );
        b.with_mana_cost(cost(
            vec![
                ManaCostShard::X,
                ManaCostShard::Red,
                ManaCostShard::Red,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            0,
        ));
        b.id()
    };
    let mut runner = scenario.build();

    let exiled_before = zone_size(&runner, Zone::Exile);
    add_mana(&mut runner, ManaType::Red, 5);
    add_mana(&mut runner, ManaType::Green, 5);

    runner.cast(spell).x(3).resolve();

    let exiled = zone_size(&runner, Zone::Exile) - exiled_before;
    assert_eq!(
        exiled, 3,
        "Hugs cast for X=3 must exile the top 3 cards. 0 = the unenumerated `count` arm \
         FABRICATED. 3 = the cost_x_paid fallback bound it, so the missing arm is a \
         NORMALIZATION gap rather than a live bug. MEASURED: {exiled}"
    );
}

/// Broodlord, `{3}{X}{G}`:
/// "When this creature enters, distribute X +1/+1 counters among any number of other target
/// creatures you control."
///
/// Also a gated self-ETB, but the X lives in `multi_target.max` — a SIBLING FIELD of `effect`
/// that `rewrite_cost_x_in_ability` never visits at all. `multi_target` is consumed during
/// TARGET SELECTION, before `current_trigger_event` is set for resolution, so this is the slot
/// most likely to fabricate even where the fallback rescues `effect` slots.
#[test]
fn gated_etb_multi_target_sibling_field() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let spell = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Broodlord",
            4,
            4,
            "When this creature enters, distribute X +1/+1 counters among any number of other \
             target creatures you control.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::X, ManaCostShard::Green], 3));
        b.id()
    };
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Green, 9);

    runner.cast(spell).x(2).target_object(bear).resolve();

    let counters = runner
        .state()
        .objects
        .get(&bear)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0);
    assert_eq!(
        counters, 2,
        "Broodlord cast for X=2 must distribute 2 +1/+1 counters onto the Bears. 0 means \
         `multi_target.max` FABRICATED — the walk never visits that sibling field. MEASURED: \
         {counters}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// UNGATED — the walk NEVER runs. X comes from an ACTIVATED ability's cost
// (CR 107.3k), which nothing stamps anywhere.
// ─────────────────────────────────────────────────────────────────────────────

/// CR 107.3k — Hydra Broodmaster, "{X}{X}{G}: Monstrosity X."
/// "When this creature becomes monstrous, create X X/X green Hydra creature tokens."
///
/// The trigger's X is the X of the ACTIVATED ABILITY that caused the event. CR 107.3k
/// (grep-verified): "If an object's activated ability has an {X} ... in its activation cost, the
/// value of X for that ability is INDEPENDENT of any other values of X chosen for that object."
/// So this X cannot ride `cost_x_paid` (the CR 107.3m *cast* channel) — that would be the wrong
/// X by rule, not merely a missing one.
///
/// This is the SAME defect as Shark Typhoon's cycling trigger on a different trigger mode,
/// which is what makes it a CLASS rather than a Shark Typhoon special case.
///
/// t96 left this `#[ignore]`d as a verified-red witness. t97 built the carrier
/// (`GameState::activated_ability_x`, published by `casting_costs::push_ability_entry` and
/// `stack::resolve_top`, consumed by `triggers::build_triggered_ability`) and the `#[ignore]`
/// is removed here: MEASURED 0 -> 2 tokens. Monstrosity emits its `EffectResolved` during
/// RESOLUTION of the activated ability, so this exercises the resolution-scoped publication.
#[test]
fn ungated_monstrosity_trigger_reads_the_activated_ability_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = {
        let b = scenario.add_creature_from_oracle(
            P0,
            "Hydra Broodmaster",
            7,
            7,
            "{X}{X}{G}: Monstrosity X.\nWhen this creature becomes monstrous, create X X/X green \
             Hydra creature tokens.",
        );
        b.id()
    };
    let mut runner = scenario.build();

    add_mana(&mut runner, ManaType::Green, 10);

    runner.activate(hydra, 0).x(2).resolve();

    let tokens: Vec<_> = named_on_battlefield(&runner, "Hydra")
        .into_iter()
        .filter(|(n, _, _)| !n.contains("Broodmaster"))
        .collect();

    assert_eq!(
        tokens.len(),
        2,
        "CR 107.3k: Monstrosity X=2 must create X=2 Hydra tokens. 0 tokens means the activated \
         ability's announced X was DROPPED — nothing carries it to the trigger. MEASURED: \
         {tokens:?}"
    );
    assert!(
        tokens.iter().all(|(_, p, t)| (*p, *t) == (2, 2)),
        "CR 107.3k: each Hydra token is X/X = 2/2. MEASURED: {tokens:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// UNGATED, CYCLED CHANNEL — the other half of the class. `Cycled` is emitted at
// ACTIVATION (`push_ability_entry`), not at resolution, so these exercise the
// announce-scoped publication rather than the resolution-scoped one.
// ─────────────────────────────────────────────────────────────────────────────

const SHARK_TYPHOON: &str = "Whenever you cast a noncreature spell, create an X/X blue Shark \
                             creature token with flying, where X is that spell's mana value.\n\
                             Cycling {X}{1}{U} ({X}{1}{U}, Discard this card: Draw a card.)\n\
                             When you cycle this card, create an X/X blue Shark creature token \
                             with flying.";

fn cycling_index(state: &engine::types::game_state::GameState, card: ObjectId) -> usize {
    state.objects[&card]
        .abilities
        .iter()
        .position(|ability| ability.ability_tag == Some(AbilityTag::Cycling))
        .expect("synthesized cycling ability")
}

/// CR 107.3a + CR 107.3i — Shark Typhoon, `Cycling {X}{1}{U}`:
/// "When you cycle this card, create an X/X blue Shark creature token with flying."
///
/// The X of the cycling ACTIVATION cost (CR 107.3a) is the X the trigger reads (CR 107.3i).
/// It cannot ride `cost_x_paid`: that is the CR 107.3m *cast* channel, and Shark Typhoon was
/// never cast — it was discarded as a cycling cost. Before the carrier this created a 0/0.
#[test]
fn cycling_trigger_reads_the_activated_ability_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Cycled Draw");
    let typhoon = scenario
        .add_spell_to_hand_from_oracle(P0, "Shark Typhoon", false, SHARK_TYPHOON)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Blue, 8);

    let idx = cycling_index(runner.state(), typhoon);
    runner.activate(typhoon, idx).x(3).resolve();

    let sharks = named_on_battlefield(&runner, "Shark");
    assert_eq!(
        sharks.len(),
        1,
        "cycling Shark Typhoon must create exactly one Shark token. MEASURED: {sharks:?}"
    );
    assert_eq!(
        (sharks[0].1, sharks[0].2),
        (3, 3),
        "CR 107.3a + CR 107.3i: cycled for X=3, the Shark is X/X = 3/3. A 0/0 here means the \
         cycling ability's announced X was DROPPED. MEASURED: {sharks:?}"
    );
}

/// Drive a cycling activation for a given X by hand. The `AbilityActivation` builder cannot be
/// used for these: a `Cycled` trigger that needs targets raises `TriggerTargetSelection` during
/// the ANNOUNCEMENT loop (before the post-announcement Priority window the builder waits for),
/// and the builder panics on that state. This is a harness limit, not engine behaviour.
fn cycle_for_x(runner: &mut engine::game::scenario::GameRunner, card: ObjectId, x: u32) {
    cycle_announce(runner, card, x);
    runner.advance_until_stack_empty();
}

/// Announce + pay a cycling activation for `x` and stop at the Priority window — the point
/// where the `Cycled` triggered ability is ON THE STACK but has not resolved, so its bound
/// `chosen_x` can be observed directly.
fn cycle_announce(runner: &mut engine::game::scenario::GameRunner, card: ObjectId, x: u32) {
    let idx = cycling_index(runner.state(), card);
    runner
        .act(GameAction::ActivateAbility {
            source_id: card,
            ability_index: idx,
        })
        .expect("activate cycling");

    // CR 601.2c: each target slot takes a DISTINCT object, so the picks must be tracked —
    // `choose_first_legal_target` would re-offer the object already chosen for slot 0 and
    // the engine rejects it.
    let mut chosen: Vec<engine::types::ability::TargetRef> = Vec::new();
    for _ in 0..32 {
        match &runner.state().waiting_for {
            // CR 107.3a + CR 601.2f (via CR 602.2b): announce X for the activation cost.
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: x })
                    .expect("announce X for the cycling cost");
            }
            // CR 602.2b + CR 601.2h: finalize mana payment.
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalize the cycling mana payment");
            }
            // CR 603.3d: the Cycled trigger picks its targets as it goes on the stack.
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let pick = slot
                    .legal_targets
                    .iter()
                    .find(|t| !chosen.contains(t))
                    .cloned();
                if let Some(target) = pick.clone() {
                    chosen.push(target);
                }
                runner
                    .act(GameAction::ChooseTarget { target: pick })
                    .expect("choose a legal target for the cycle trigger");
            }
            _ => break,
        }
    }
}

/// NEGATIVE CONTROL, from the card's own official ruling (read from the pool export, never from
/// memory): "You can choose 0 as the value of X in Shark Typhoon's cycling cost. The last
/// ability will trigger, and you'll create a 0/0 blue Shark creature token with flying."
///
/// So for X=0 a 0/0 Shark is CORRECT. Zero is the one value where the correct board and the
/// FABRICATED board coincide — an unbound X also resolves to 0 — so the board alone cannot
/// discriminate here, and the 0/0 token dies to SBA (CR 704.5f) and is purged (CR 111.7) before
/// it can even be read. The discriminating observation is therefore taken while the trigger is
/// ON THE STACK: it must carry `chosen_x == Some(0)` — an announcement of zero — and NOT `None`.
/// That is what pins `Option<u32>` as the right carrier type: a carrier that treated 0 as
/// "nothing announced" would leave `None` here and pass a board-only test by luck.
/// The board is then checked too: a survivor would mean some non-zero X was fabricated.
#[test]
fn cycling_trigger_x_zero_announces_a_real_zero_and_the_shark_dies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Cycled Draw");
    let typhoon = scenario
        .add_spell_to_hand_from_oracle(P0, "Shark Typhoon", false, SHARK_TYPHOON)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Blue, 4);

    cycle_announce(&mut runner, typhoon, 0);

    let bound_x = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } if *source_id == typhoon => Some(ability.chosen_x),
            _ => None,
        })
        .expect("the Cycled trigger must be on the stack (if it is not, nothing below is a test)");
    assert_eq!(
        bound_x,
        Some(0),
        "WotC ruling: X=0 is a legal cycling announcement and creates a 0/0 Shark. The trigger \
         must carry an ANNOUNCED zero, not `None` — `None` means the carrier collapsed 0 into \
         'no X announced' and the correct board here would be a coincidence. MEASURED: {bound_x:?}"
    );

    runner.advance_until_stack_empty();
    let surviving = named_on_battlefield(&runner, "Shark");
    assert!(
        surviving.is_empty(),
        "cycled for X=0 the Shark is 0/0 and must die to SBA. A survivor means a NON-ZERO X was \
         fabricated — a value the player never announced. MEASURED: {surviving:?}"
    );
}

/// The stamp must land at trigger INSTANTIATION, not at trigger resolution: Rampaging War
/// Mammoth's "destroy up to X target artifacts" spends X in `multi_target.max`, which is
/// consumed during TARGET SELECTION — before the triggered ability ever resolves. With X
/// unbound the trigger offers "up to 0" targets and destroys nothing, which is exactly the
/// silent zero this campaign exists to kill. Cycled for X=2, both artifacts must die.
#[test]
fn cycling_trigger_x_reaches_the_target_count_slot() {
    const MAMMOTH: &str = "Trample\nCycling {X}{2}{R} ({X}{2}{R}, Discard this card: Draw a \
                           card.)\nWhen you cycle this card, destroy up to X target artifacts.";

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Cycled Draw");
    let relic_a = scenario
        .add_creature(P1, "Relic A", 0, 0)
        .as_artifact()
        .id();
    let relic_b = scenario
        .add_creature(P1, "Relic B", 0, 0)
        .as_artifact()
        .id();
    let mammoth = scenario
        .add_spell_to_hand_from_oracle(P0, "Rampaging War Mammoth", false, MAMMOTH)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, 8);

    cycle_for_x(&mut runner, mammoth, 2);

    let survivors: Vec<_> = [relic_a, relic_b]
        .into_iter()
        .filter(|id| {
            runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.zone == Zone::Battlefield)
        })
        .collect();
    assert!(
        survivors.is_empty(),
        "cycled for X=2, 'destroy up to X target artifacts' must destroy BOTH. A survivor means \
         `multi_target.max` never saw the announced X, so the trigger offered 'up to 0' targets. \
         MEASURED survivors: {survivors:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// GREEN CONTROLS — the carrier MUST NOT reach these.
// ─────────────────────────────────────────────────────────────────────────────

/// THE HAZARD CONTROL (CR 107.3m + CR 107.3k). A resolving SPELL must never publish an
/// activation X. Two things depend on this and both are load-bearing:
///
///  1. A permanent PUT onto the battlefield by an unrelated resolving X-spell (the
///     Sneak-Attack-for-X shape) must have X = 0 — CR 107.3m: "the value of X for that
///     permanent is 0". If a spell published its X, that permanent's ETB would inherit it.
///  2. `QuantityRef::CostXPaid` — the cast channel that commit 1 rewrites gated-ETB sibling
///     slots to — falls back to `chosen_x` when the object has no `cost_x_paid`. A published
///     spell-X would leak into that fallback and poison exactly the slots commit 1 fixed.
///
/// `stack::resolve_top` publishes for `StackEntryKind::ActivatedAbility` ONLY, so the carrier
/// is provably `None` for the whole of a spell's resolution. This asserts that directly, and
/// asserts the cast-X path still works (Krasis gains half X), so it cannot pass vacuously by
/// the carrier being dead everywhere.
#[test]
fn a_resolving_spell_never_publishes_an_activation_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Hydroid Krasis",
            0,
            0,
            "When you cast this spell, you gain half X life and draw half X cards. Round down \
             each time.\nFlying, trample",
        );
        b.with_mana_cost(cost(
            vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Blue],
            0,
        ));
        b.id()
    };
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;
    add_mana(&mut runner, ManaType::Green, 4);
    add_mana(&mut runner, ManaType::Blue, 4);

    runner.cast(spell).x(4).resolve();

    assert_eq!(
        runner.state().players[0].life - life_before,
        2,
        "non-vacuity: the cast-X channel (cost_x_paid) must still bind — Krasis cast for X=4 \
         gains half X = 2 life. If this is 0 the assertion below proves nothing."
    );
    assert_eq!(
        runner.state().activated_ability_x,
        None,
        "CR 107.3m + CR 107.3k: a resolving SPELL must never publish an activation X. A \
         published value here would (a) let a permanent this spell puts onto the battlefield \
         inherit X instead of 0, and (b) leak into the CostXPaid -> chosen_x fallback and \
         poison the gated-ETB sibling slots. MEASURED: {:?}",
        runner.state().activated_ability_x
    );
}

/// RULING CONDITION 2 — save/wire compatibility, CHECKED rather than asserted.
///
/// `activated_ability_x` is NEW PERSISTED `GameState` (it is serialized so a mid-activation
/// pause — e.g. an interactive cost payment between announcement and the trigger going on the
/// stack — round-trips). It is therefore a save-format change and must be shown compatible:
///
///  1. A save written by an OLDER binary has no `activated_ability_x` key at all. `#[serde(default)]`
///     must make that load as `None` rather than fail. Because `skip_serializing_if` omits the key
///     whenever it is `None`, a fresh serialization of a state with no live activation IS
///     byte-identical to an old save on this axis — so serializing a `None` state and reloading it
///     exercises exactly the old-save path.
///  2. A live value must survive a round-trip.
///
/// (`GameState` does not use `deny_unknown_fields`, so the reverse direction — an OLD binary
/// reading a NEW save that carries the key — ignores it rather than erroring.)
#[test]
fn activated_ability_x_is_save_compatible() {
    let scenario = GameScenario::new();
    let runner = scenario.build();
    let mut state = runner.state().clone();

    // (1) OLD-SAVE SHAPE: `None` omits the key entirely.
    state.activated_ability_x = None;
    let old_shape = serde_json::to_value(&state).expect("serialize");
    assert!(
        old_shape.get("activated_ability_x").is_none(),
        "a `None` carrier must omit the key, so pre-field saves are byte-identical on this axis"
    );
    let reloaded: engine::types::game_state::GameState =
        serde_json::from_value(old_shape).expect("an old save (no key) must load, not fail");
    assert_eq!(
        reloaded.activated_ability_x, None,
        "a save with no `activated_ability_x` key must default to None"
    );

    // (2) LIVE VALUE: round-trips intact.
    state.activated_ability_x = Some((ObjectId(4242), 7));
    let new_shape = serde_json::to_value(&state).expect("serialize");
    assert!(
        new_shape.get("activated_ability_x").is_some(),
        "a live announced X must be persisted (it must survive a mid-activation pause)"
    );
    let reloaded: engine::types::game_state::GameState =
        serde_json::from_value(new_shape).expect("deserialize");
    assert_eq!(
        reloaded.activated_ability_x,
        Some((ObjectId(4242), 7)),
        "the announced X and its source must round-trip through a save"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// THE FILTER-MANA-VALUE SUB-CLASS — t96 left this MEASURED-UNKNOWN, and the totality
// guard's scoping depends on the answer. These two pin it in BOTH directions.
//
// ~9 gated-ETB faces (Dune Drifter, Kinetic Ooze, Thieving Skydiver, Halo Forager, In
// Residence, Invasion of Ikoria, Knickknack Ouphe, Rocco, Taj-Nar Swordsmith) keep a bare
// `Variable{"X"}` in a filter's mana-value bound INSIDE the effect. The cost-X walk does not
// rewrite it — the AST residual is real. The question is whether it FABRICATES.
//
// MEASURED: it does not. The runtime binds it. So these faces WORK, and a totality guard keyed
// on tree-presence of an X would red all nine of them — manufacturing honest-looking reds out
// of cards that play correctly. That is why the guard is keyed on the target-selection sibling
// slots ONLY. These tests are the evidence for that scoping; if they ever go red, the guard's
// scope must be revisited.
// ─────────────────────────────────────────────────────────────────────────────

/// Kinetic Ooze `{X}{G}`, MEASURED — direction 1: an object WITHIN the bound is a legal target.
/// "This creature enters with X +1/+1 counters on it.
///  When this creature enters, destroy up to one target artifact or enchantment with mana value
///  X or less."
///
/// BUILT-IN NON-VACUITY CONTROL: one card, one X, two slots. "Enters with X +1/+1 counters"
/// reads the spell's X (CR 107.3m); if that reads 3, X was available and the harness is live.
/// A 3-counter Ooze beside a SURVIVING artifact would isolate the filter slot as fabricating.
#[test]
fn filter_mana_value_bound_binds_x_and_destroys_within_bound() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let relic = scenario
        .add_creature(P1, "Worn Powerstone", 0, 0)
        .as_artifact()
        .id();
    let ooze = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Kinetic Ooze",
            0,
            0,
            "This creature enters with X +1/+1 counters on it.\nWhen this creature enters, \
             destroy up to one target artifact or enchantment with mana value X or less.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::X, ManaCostShard::Green], 0));
        b.id()
    };
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&relic)
        .unwrap()
        .mana_cost = ManaCost::Cost {
        shards: vec![],
        generic: 3,
    };
    add_mana(&mut runner, ManaType::Green, 8);

    runner.cast(ooze).x(3).target_object(relic).resolve();

    let counters = runner
        .state()
        .objects
        .get(&ooze)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0);
    let relic_alive = runner
        .state()
        .objects
        .get(&relic)
        .is_some_and(|o| o.zone == Zone::Battlefield);
    assert_eq!(
        counters, 3,
        "NON-VACUITY CONTROL: the enters-with-X counters read the spell's X (CR 107.3m). If this \
         is not 3 the X never reached the card at all and the filter verdict below is void. \
         MEASURED: {counters}"
    );
    assert!(
        !relic_alive,
        "the filter's mana-value bound must bind X: cast for X=3, an MV-3 artifact IS a legal \
         target and must be destroyed. A survivor here means the bound fabricated 0 ('mana value \
         0 or less' matches nothing) — and the totality guard would then need to red this class."
    );
}

/// Kinetic Ooze, MEASURED — direction 2, THE DISCRIMINATOR. Direction 1 alone is ambiguous: a
/// filter whose mana-value prop was DROPPED ENTIRELY (over-permissive — a different bug) would
/// also destroy the artifact. Here X=1 against an MV-3 artifact. Bound to X => MV 3 > 1 is an
/// ILLEGAL target and it survives. Prop dropped => it dies anyway. It survives, so the bound is
/// really bound to X — not fabricated, and not dropped.
#[test]
fn filter_mana_value_bound_excludes_targets_above_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let relic = scenario
        .add_creature(P1, "Worn Powerstone", 0, 0)
        .as_artifact()
        .id();
    let ooze = {
        let mut b = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Kinetic Ooze",
            0,
            0,
            "This creature enters with X +1/+1 counters on it.\nWhen this creature enters, \
             destroy up to one target artifact or enchantment with mana value X or less.",
        );
        b.with_mana_cost(cost(vec![ManaCostShard::X, ManaCostShard::Green], 0));
        b.id()
    };
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&relic)
        .unwrap()
        .mana_cost = ManaCost::Cost {
        shards: vec![],
        generic: 3,
    };
    add_mana(&mut runner, ManaType::Green, 8);

    runner.cast(ooze).x(1).resolve();

    let counters = runner
        .state()
        .objects
        .get(&ooze)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0);
    let relic_alive = runner
        .state()
        .objects
        .get(&relic)
        .is_some_and(|o| o.zone == Zone::Battlefield);
    assert_eq!(
        counters, 1,
        "NON-VACUITY CONTROL: the enters-with-X counters must read 1. MEASURED: {counters}"
    );
    assert!(
        relic_alive,
        "cast for X=1, an MV-3 artifact is NOT a legal target and must survive. Its destruction \
         would mean the mana-value prop was dropped entirely — an over-permissive filter that \
         destroys anything, which direction 1 alone cannot distinguish from a correct bind."
    );
}
