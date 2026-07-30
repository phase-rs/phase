//! FIX-1 + FIX-2 + FIX-3 (CR 732.2a) acceptance — the Kilo, Apogee Mind + Freed from the Real +
//! Relic of Legends + Pentad Prism proliferate loop, driven from the REAL 4-player playtest dump
//! that failed to offer the ∞-charge shortcut.
//!
//! The loop (measured, mana-neutral, +1 charge/cycle, unbounded — `WinKind::Advantage`, CR 104.4b):
//! activate Relic #1 ("Tap an untapped legendary creature you control: Add one mana of any color"),
//! tap Kilo (402) for BLUE → Kilo's "becomes tapped" trigger proliferates (CR 701.34a), +1 charge
//! on Pentad (405) → activate Freed #1 ("{U}: Untap enchanted creature"), the {U} paid by the Blue.
//!
//! This exercises all three fixes end-to-end through the PUBLIC `apply()` boundary (the
//! "combo FIRES in a real game" criterion): FIX-3 (the conditional load migration
//! `GameState::migrate_transient_loop_sequence` drops the loaded save's 6 stale pinless steps
//! because the dump is at `Priority`, not a shortcut window), FIX-1 (record + replay the
//! tap-target / mana-color / proliferate-target pins), FIX-2 (the counter-growth cover disjunct
//! accepts the +1-charge/cycle growth).
//!
//! DISCLOSED (FIX-3): a loaded PRE-fix save carries 6 pinless steps that the migration drops on
//! load; one live cycle rebuilds a clean, fully-pinned 2-step period the detection drive can replay.
//! The `kilo_reinjected_pinless_history_suppresses_offer` test is the matched-pair proof that the
//! migration is load-bearing (re-injecting the stale prefix flips the offer OFF).

use engine::game::engine::apply;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, LoopAction, LoopActionContext, LoopCollapseAxis, ManaChoice, PayCostKind,
    PayableResource, PersistedGameState, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const KILO: ObjectId = ObjectId(402);
const FREED: ObjectId = ObjectId(403);
const RELIC: ObjectId = ObjectId(404);
const PENTAD: ObjectId = ObjectId(405);
/// Relic of Legends ability index 1 = "Tap an untapped legendary creature you control: Add one
/// mana of any color"; Freed from the Real ability index 1 = "{U}: Untap enchanted creature".
const RELIC_TAP_MANA: usize = 1;
const FREED_UNTAP: usize = 1;

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Load the real 4p dump's `["gameState"]` and route it through the REAL production restore
/// chokepoint `PersistedGameState::into_game_state` (both server `from_persisted` and WASM
/// `decode_restored_game_state` funnel through it). The sequence deserializes NORMALLY (len 6),
/// then `GameState::migrate_transient_loop_sequence` DROPS it because the dump was captured at
/// empty-stack `Priority` (NOT a shortcut window) — exactly the production load behavior. Reverting
/// the migration (or its `Priority`-drops-it branch) leaves the 6 stale pinless steps intact ⇒ the
/// `is_empty()` assertion below flips and `try_offer` aborts on the pinless `seq[0]`.
fn load_migrated_dump() -> GameState {
    let json = gunzip(include_bytes!(
        "../fixtures/kilo_freed_relic_pentad_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let raw: GameState = serde_json::from_value(envelope["gameState"].clone())
        .expect("the real 4p gameState must deserialize into the current GameState");
    PersistedGameState::Raw(Box::new(raw)).into_game_state()
}

/// The acting player for the current beat (choice prompts carry their own `player`; a priority beat
/// is answered by the live holder so the multiplayer APNAP pass is authorized).
fn beat_actor(state: &GameState) -> PlayerId {
    match &state.waiting_for {
        WaitingFor::Priority { player } => *player,
        WaitingFor::PayCost { player, .. } => *player,
        WaitingFor::ChooseManaColor { player, .. } => *player,
        WaitingFor::ProliferateChoice { player, .. } => *player,
        WaitingFor::LoopShortcut { proposer, .. } => *proposer,
        other => panic!("unexpected beat: {other:?}"),
    }
}

/// Drive ONE full live cycle of the Kilo loop via the PUBLIC `apply()` boundary (recording arms
/// fire live — this is NOT a simulation probe). Answers each fixed choice with the loop's demanded
/// value (tap Kilo, Blue mana, proliferate Pentad), activates Freed once, and settles at the first
/// of `{empty-stack Priority, LoopShortcut}` reached after Freed resolves.
fn drive_one_live_cycle(state: &mut GameState) {
    apply(
        state,
        P0,
        GameAction::ActivateAbility {
            source_id: RELIC,
            ability_index: RELIC_TAP_MANA,
        },
    )
    .expect("activate Relic's tap-a-legendary mana ability");

    let mut freed_activated = false;
    for _ in 0..200 {
        let actor = beat_actor(state);
        match state.waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => return,
            // Relic's tap cost: tap Kilo (the loop's legendary).
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                ..
            } => {
                apply(state, actor, GameAction::SelectCards { cards: vec![KILO] })
                    .expect("tap Kilo for the Relic mana ability");
            }
            // Relic's "add one mana of any color": choose BLUE to pay Freed's {U}.
            WaitingFor::ChooseManaColor { .. } => {
                apply(
                    state,
                    actor,
                    GameAction::ChooseManaColor {
                        choice: ManaChoice::SingleColor(ManaType::Blue),
                        count: 1,
                    },
                )
                .expect("choose Blue for the loop's mana-neutrality");
            }
            // Kilo's becomes-tapped proliferate trigger: proliferate Pentad only.
            WaitingFor::ProliferateChoice { .. } => {
                apply(
                    state,
                    actor,
                    GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(PENTAD)],
                    },
                )
                .expect("proliferate Pentad");
            }
            WaitingFor::Priority { .. } => {
                if state.stack.is_empty() {
                    if freed_activated {
                        return; // settled with no offer
                    }
                    freed_activated = true;
                    apply(
                        state,
                        P0,
                        GameAction::ActivateAbility {
                            source_id: FREED,
                            ability_index: FREED_UNTAP,
                        },
                    )
                    .expect("activate Freed's {U}: untap Kilo");
                } else {
                    apply(state, actor, GameAction::PassPriority)
                        .expect("pass priority to resolve the stack");
                }
            }
            other => panic!("unexpected beat during the live drive: {other:?}"),
        }
    }
    panic!("live drive did not settle within the beat cap");
}

/// FIX-3 primary + FIX-1 + FIX-2 composite acceptance: a LOADED PRE-fix save fires the ∞-charge
/// CR 732.2a offer PROMPTLY. Reverting ANY of the three fixes flips this to no-offer:
/// - FIX-3 (`#[serde(skip)]`) — the 6 pinless steps survive load, `try_offer` re-drives the pinless
///   `seq[0]` and aborts at `PayCost{TapCreatures}` (see the matched `reinjected` test).
/// - FIX-1 (E11 drive replay arms) — the drive aborts at the same `PayCost` beat with the pins
///   unreplayable.
/// - FIX-2 (counter-growth cover disjunct) — the completed drive's +1-charge frames fail
///   `loop_states_equal_modulo_resources`.
#[test]
fn kilo_migrated_dump_fires_object_growth_offer() {
    let mut state = load_migrated_dump();

    // FIX-3 migration (observable effect): the loaded save's 6 pinless steps are dropped on load.
    // Pre-FIX-3 this is len 6 — the matched-pair discriminator for FIX-3 itself.
    assert!(
        state.last_loop_action_sequence.is_empty(),
        "FIX-3: the loaded save's stale pinless loop history is dropped on load (was 6 steps)"
    );
    // Board is the untouched real 4p dump.
    assert_eq!(state.objects.len(), 411, "the real 4p board loads intact");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the dump is at P0's empty-stack priority, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.objects[&PENTAD]
            .counters
            .get(&engine::types::counter::CounterType::Generic(
                "charge".into()
            ))
            .copied(),
        Some(3),
        "Pentad carries 3 charge counters in the real dump"
    );

    drive_one_live_cycle(&mut state);

    // Non-vacuous reach-guard: the live drive rebuilt a clean, fully-recorded 2-step period.
    assert_eq!(
        state.last_loop_action_sequence.len(),
        2,
        "one live cycle rebuilds the clean 2-step period [Relic#1, Freed#1]"
    );
    assert_eq!(
        state.last_loop_action_sequence[0].action,
        LoopAction::Activate {
            source_id: RELIC,
            ability_index: RELIC_TAP_MANA,
        },
        "the first step is the Relic mana activation (which carries the recorded pins)"
    );
    assert!(
        !state.last_loop_action_sequence[0].pins.is_empty(),
        "FIX-1: the Relic step carries the recorded fixed choices (tap/color/proliferate pins)"
    );

    // THE OFFER: the ∞-charge CR 732.2a shortcut surfaces for P0, carrying the reified schema.
    match &state.waiting_for {
        WaitingFor::LoopShortcut {
            proposer, schema, ..
        } => {
            assert_eq!(*proposer, P0, "the loop's controller proposes the shortcut");
            // B1: the schema reifies the recorded pins as read-side decision points (the two
            // ByIdentity target pins + the latched mana-color pin).
            use engine::analysis::decision_template::DecisionPointKind;
            let has_color = schema
                .points
                .iter()
                .any(|p| matches!(p.kind, DecisionPointKind::ManaColor { .. }));
            let has_targets = schema
                .points
                .iter()
                .any(|p| matches!(p.kind, DecisionPointKind::Targets { .. }));
            assert!(
                has_color && has_targets,
                "B1: the offer schema reifies the ManaColor + Targets decision points, got {:?}",
                schema.points
            );
        }
        other => panic!("expected the CR 732.2a ∞-charge LoopShortcut offer for P0, got {other:?}"),
    }
}

/// FIX-3 non-vacuity (matched pair): re-injecting the dump's original 6 PINLESS steps before the
/// drive reproduces the pre-migration load state — the drive appends a fresh pinned period AFTER
/// the stale prefix, so `try_offer` re-drives from the pinless `seq[0]` (Relic → `PayCost` with no
/// pin) and aborts ⇒ NO offer on this cycle. Undefused (migration ON) fires; migration disabled
/// (stale prefix re-injected) does not. Flip ⇒ FIX-3 is load-bearing.
#[test]
fn kilo_reinjected_pinless_history_suppresses_offer() {
    let mut state = load_migrated_dump();
    assert!(
        state.last_loop_action_sequence.is_empty(),
        "precondition: the migration dropped the history"
    );

    // Re-inject the dump's original 6 pinless steps: [Activate 404#1, Activate 403#1] × 3.
    let relic_card = state.objects[&RELIC].card_id;
    let freed_card = state.objects[&FREED].card_id;
    let mut pinless = Vec::new();
    for _ in 0..3 {
        pinless.push(LoopActionContext {
            card_id: relic_card,
            controller: P0,
            action: LoopAction::Activate {
                source_id: RELIC,
                ability_index: RELIC_TAP_MANA,
            },
            convoke: None,
            pins: Vec::new(),
        });
        pinless.push(LoopActionContext {
            card_id: freed_card,
            controller: P0,
            action: LoopAction::Activate {
                source_id: FREED,
                ability_index: FREED_UNTAP,
            },
            convoke: None,
            pins: Vec::new(),
        });
    }
    state.last_loop_action_sequence = pinless;

    drive_one_live_cycle(&mut state);

    // The stale pinless prefix makes `try_offer` re-drive from a pinless `seq[0]` and abort ⇒
    // no offer surfaces (the C2 / R3.0-A baseline).
    assert!(
        !matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }),
        "with the migration disabled (stale pinless prefix re-injected) the offer must NOT fire, \
         got {:?}",
        state.waiting_for
    );
}

/// Drive the APNAP accept of the ∞ offer through the PUBLIC `apply()` boundary: P0 (the
/// proposer) declares `Fixed(1)`, then every prompted opponent accepts in turn order until the
/// protocol closes back to ordinary priority (CR 800.4a). `template: None` skips declare-time
/// pin validation; the materialize re-derives from the intact `last_loop_action_sequence`.
fn drive_all_accept(state: &mut GameState) {
    use engine::analysis::decision_template::IterationCount;
    use engine::analysis::loop_check::ShortcutResponse;
    apply(
        state,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        },
    )
    .expect("P0 (proposer) declares the counter-growth shortcut");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts the ∞-charge shortcut");
    }
}

/// Pass priority (for whichever seat holds it) until the next CR 500.5 phase/step boundary raises
/// the deferred-collapse prompt. No player re-drives the loop — the accept cleared the recorded
/// `last_loop_action_sequence` — so the phase simply ends and the boundary drain surfaces the
/// `PayAmountChoice { LoopCollapse }` prompt for the stash-holder.
fn drive_to_collapse_boundary(state: &mut GameState) {
    for _ in 0..200 {
        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                resource: PayableResource::LoopCollapse { .. },
                ..
            } => return,
            WaitingFor::Priority { player } => {
                let p = *player;
                apply(state, p, GameAction::PassPriority)
                    .expect("pass priority toward the CR 500.5 collapse boundary");
            }
            other => panic!("unexpected beat while driving to the collapse boundary: {other:?}"),
        }
    }
    panic!("did not reach the LoopCollapse boundary prompt within the beat cap");
}

/// DISPLAY-render acceptance (CR 732.2a / CR 701.34a): accepting the Kilo proliferate ∞-charge
/// loop marks Pentad Prism's charge counter as an unbounded DISPLAY target — so the frontend
/// renders `∞` on that pill — WITHOUT mutating the real charge count. Composite of the new
/// field write (`register_unbounded_counter_targets`), the derived-view projection
/// (`DerivedViews::unbounded_counters`), and the serde wire shape, all driven through the real
/// accept pipeline from the real 4p dump.
///
/// REVERT-PROBE (measured, non-vacuous): deleting the `register_unbounded_counter_targets`
/// write in `materialize_object_growth_shortcut` (or the `grown_generic_counter_targets`
/// re-derivation) leaves `unbounded_counter_targets` empty ⇒ assertions (2) the field write,
/// (3) the derived-view projection, and (4) the wire round-trip all FLIP to fail. The
/// offer-fires reach-guard (1) and the `charge == Some(4)` rules-correctness anchor
/// (display-only: the real count is untouched) HOLD BOTH WAYS.
#[test]
fn kilo_accept_marks_pentad_charge_as_unbounded_display_target() {
    use engine::game::derived_views::{derive_views, DerivedViews};
    use engine::types::counter::CounterType;

    let mut state = load_migrated_dump();
    drive_one_live_cycle(&mut state);

    // (1) Reach-guard (holds both ways under revert): the ∞-charge offer surfaced for P0. If
    // this ever regresses, every downstream assertion is vacuous — so it gates them.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the CR 732.2a ∞-charge offer for P0, got {:?}",
        state.waiting_for
    );
    let charge = CounterType::Generic("charge".into());
    // Rules-correctness anchor: the REAL charge count at the offer (grew 3→4 this cycle).
    assert_eq!(
        state.objects[&PENTAD].counters.get(&charge).copied(),
        Some(4),
        "Pentad carries 4 real charge counters at the offer (grew 3→4 in the driven cycle)"
    );

    drive_all_accept(&mut state);

    // The protocol closed cleanly back to ordinary priority (CR 800.4a).
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "after all accept, materialize hands priority back, got {:?}",
        state.waiting_for
    );

    // (2) THE NEW WRITE (FLIPS on revert): accepting marks Pentad's charge as an unbounded
    // (object, counter) DISPLAY target for P0 — object-agnostic axis re-derived to the concrete
    // (405, charge) pair. This is the `register_unbounded_counter_targets` revert target.
    let targets = state
        .unbounded_counter_targets
        .get(&P0)
        .expect("accepting the counter-growth loop must write P0's ∞ counter targets");
    assert!(
        targets.contains(&(PENTAD, charge.clone())),
        "the ∞ counter target is exactly (Pentad 405, charge), got {targets:?}"
    );

    // (RULES-CORRECTNESS, holds both ways) the DISPLAY mark does NOT mutate the real count —
    // CR 701.34a proliferate added the real counter each live cycle; the ∞ is render-only.
    assert_eq!(
        state.objects[&PENTAD].counters.get(&charge).copied(),
        Some(4),
        "display-only: Pentad's REAL charge count is unchanged by the ∞ mark (CR 701.34a)"
    );

    // (3) DERIVED VIEW (FLIPS on revert): the projection surfaces Pentad's charge as ∞ for the
    // FE, filtered to battlefield objects.
    let views = derive_views(&state, None);
    assert_eq!(
        views.unbounded_counters.get(&PENTAD),
        Some(&vec![charge.clone()]),
        "derive_views projects (Pentad → [charge]) so the FE renders ∞ on the charge pill"
    );

    // (4) WIRE ROUND-TRIP (FLIPS on revert): the populated channel serializes, is present on
    // the wire, and survives a round-trip; an EMPTY derived view omits it (skip_serializing_if).
    let json = serde_json::to_string(&views).expect("derived views serialize");
    assert!(
        json.contains("unbounded_counters"),
        "the populated ∞-counter channel is present on the wire"
    );
    let round: DerivedViews = serde_json::from_str(&json).expect("derived views round-trip");
    assert_eq!(
        round.unbounded_counters.get(&PENTAD),
        Some(&vec![charge]),
        "the ∞ counter channel survives a serde round-trip"
    );
    let empty_json =
        serde_json::to_string(&DerivedViews::default()).expect("empty derived views serialize");
    assert!(
        !empty_json.contains("unbounded_counters"),
        "the field is omitted (skip_serializing_if) when empty"
    );
}

/// PERSISTENT-AXIS BOUNDARY COLLAPSE (CR 732.2a / CR 500.5 / CR 701.34a): the accepted Kilo
/// proliferate ∞-charge loop is PROMPTED at the next phase/step boundary to name a finite N, then
/// resolves to EXACTLY N more charge counters on Pentad Prism — driven end-to-end through the
/// public `apply()` boundary from the real 4p dump.
///
/// WHY THIS TEST EXISTS (the gap it closes): every OTHER committed Counters/Life/DriveSequence
/// collapse test manually GRAFTS the deferred stash onto an offer state (`register_pending_
/// materialization` / `pending_unbounded_materialization` graft), BYPASSING the real accept-time
/// δ-capture + routing in `materialize_object_growth_shortcut` (engine.rs:
/// `current_period_counter_growth` + `counter_growth_is_observed` ⇒ `register_pending_
/// materialization(DriveSequence{..})`). This test drives that REAL registration — it never grafts
/// a stash — so a regression that stops registering the DriveSequence for observed counter loops
/// is caught here (and ONLY here).
///
/// REVERT-PROBE (measured, non-vacuous): disabling the DriveSequence registration in
/// `materialize_object_growth_shortcut` (engine.rs, the `if counter_observed || life_observed`
/// arm's `state.register_pending_materialization(.. DriveSequence ..)` push) leaves the Kilo
/// counter loop with NO deferred stash ⇒ `next_apnap_player_with_pending_materialization` finds
/// nothing at the CR 500.5 boundary ⇒ the `PayAmountChoice { LoopCollapse }` prompt never fires
/// (priority advances straight into combat) ⇒ the boundary reach-guard (2) FLIPS to a panic.
#[test]
fn kilo_accept_collapses_at_boundary_to_exactly_n_counters() {
    use engine::game::derived_views::derive_views;
    use engine::types::counter::CounterType;

    const N: u32 = 5;
    let charge = CounterType::Generic("charge".into());

    let mut state = load_migrated_dump();
    drive_one_live_cycle(&mut state);

    // (1) Reach-guard (gates everything downstream): the ∞-charge offer surfaced for P0.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the CR 732.2a ∞-charge offer for P0, got {:?}",
        state.waiting_for
    );
    // Baseline: the REAL charge count at the offer (grew 3→4 in the driven cycle). Neither accept
    // nor the boundary collapse may touch this until N is named.
    let baseline = state.objects[&PENTAD]
        .counters
        .get(&charge)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        baseline, 4,
        "Pentad carries 4 real charge counters at the offer"
    );

    // Accept the ∞-charge shortcut through the REAL APNAP pipeline — routes through
    // `materialize_object_growth_shortcut`, where `counter_growth_is_observed` is true for the
    // real proliferate loop, so a DriveSequence stash is REGISTERED (not grafted). Accept is
    // display-only: the real count is deferred to the boundary.
    drive_all_accept(&mut state);
    assert_eq!(
        state.objects[&PENTAD].counters.get(&charge).copied(),
        Some(baseline),
        "accept is display-only: the real charge count is untouched until the boundary collapse"
    );

    // Drive priority to the next CR 500.5 boundary (PreCombatMain → BeginCombat). The deferred
    // DriveSequence stash makes the boundary drain prompt P0 for the finite collapse count.
    drive_to_collapse_boundary(&mut state);

    // (2) THE BOUNDARY PROMPT (FLIPS on revert of the DriveSequence registration): P0 is asked to
    // name the finite count the accepted ∞-charge loop collapses into (CR 732.2a).
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::PayAmountChoice {
                resource: PayableResource::LoopCollapse { axis: LoopCollapseAxis::Counters },
                player,
                ..
            } if player == P0
        ),
        "at the CR 500.5 boundary P0 is prompted to name the finite COUNTER-axis collapse count \
         (CR 732.2a); the axis label must be Counters, got {:?}",
        state.waiting_for
    );

    // (3) SUBMIT N: the collapse replays N REAL proliferate cycles (drive_persistent_axis_collapse),
    // each firing CR 701.34a proliferate and adding +1 charge.
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: N })
        .expect("P0 names the finite collapse count N");

    // (4) EXACTLY +N counters (the measured 4→9 for N=5): the persistent axis collapsed to a
    // finite, rules-correct count — not ∞, not off-by-one.
    assert_eq!(
        state.objects[&PENTAD].counters.get(&charge).copied(),
        Some(baseline + N),
        "the accepted ∞-charge loop collapsed to EXACTLY baseline+N charge counters"
    );

    // (5) THE ∞ DISPLAY PILL CLEARS once the axis collapses to a finite N — both the raw field
    // (`clear_collapsed_materializations`) and the derived FE projection — so the pill renders 9
    // not ∞.
    assert!(
        !state.unbounded_counter_targets.contains_key(&P0),
        "the collapsed ∞ counter target is cleared for P0, got {:?}",
        state.unbounded_counter_targets.get(&P0)
    );
    assert_eq!(
        derive_views(&state, None).unbounded_counters.get(&PENTAD),
        None,
        "the derived ∞-counter view no longer projects Pentad after the collapse"
    );

    // (6) The boundary protocol closed cleanly back to ordinary priority (CR 800.4a).
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "after the collapse submit, priority is restored, got {:?}",
        state.waiting_for
    );
}
