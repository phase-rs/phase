// engine-citation-gate: symbol anchors only
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

use engine::analysis::decision_template::IterationCount;
use engine::game::derived_views::{CollapseCertainty, FamilyCollapseState, UnboundedFamily};
use engine::game::engine::apply;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, LoopAction, LoopActionContext, LoopCollapseAxis, ManaChoice, PayCostKind,
    PayableResource, PersistedGameState, PersistentAxisMaterialization, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionResponse, InteractionResponseSpec,
    InteractionSessionId, InteractionShortcutCountSpec, InteractionShortcutDecision,
    InteractionShortcutPin, InteractionSubmission,
};
use engine::types::mana::ManaType;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const KILO: ObjectId = ObjectId(402);
const FREED: ObjectId = ObjectId(403);
const RELIC: ObjectId = ObjectId(404);
const PENTAD: ObjectId = ObjectId(405);
/// Relic of Legends ability index 1 = "Tap an untapped legendary creature you control: Add one
/// mana of any color"; Freed from the Real ability index 1 = "{U}: Untap enchanted creature".
const RELIC_TAP_MANA: usize = 1;
const FREED_UNTAP: usize = 1;

/// The four loop permanents, per dump. Both real captures hold the same Kilo/Freed/Relic/Pentad
/// board under P0; only the `ObjectId`s differ, so ONE drive authority serves both and the
/// regression row cannot silently diverge from the rows that already pin this loop's behavior.
struct LoopIds {
    kilo: ObjectId,
    freed: ObjectId,
    relic: ObjectId,
    pentad: ObjectId,
}
const FIXTURE_IDS: LoopIds = LoopIds {
    kilo: KILO,
    freed: FREED,
    relic: RELIC,
    pentad: PENTAD,
};
/// MEASURED off the reported capture, not guessed: Kilo 406, Relic 407, Freed 408, Pentad 409.
/// Note the Freed/Relic order is TRANSPOSED relative to the older fixture (403/404).
const CAPTURE_IDS: LoopIds = LoopIds {
    kilo: ObjectId(406),
    freed: ObjectId(408),
    relic: ObjectId(407),
    pentad: ObjectId(409),
};

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
/// `decode_restored_game_state` funnel through it). The chokepoint now rehydrates the ChaCha20
/// stream, which only `engine-wasm`'s `restore_game_state` used to do on its own — a load that
/// ENDED at the chokepoint, as this one does, was left with a word-0 stream under this dump's
/// saved `rng_word_pos` of 293. WASM's own call is now an idempotent repeat. Callers may still
/// diverge afterwards: `GameSession::from_persisted` re-seeds and zeroes `rng_word_pos` with it,
/// discarding the saved position rather than resuming it as this load does.
/// The sequence deserializes NORMALLY (len 6),
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
    // Decode AS `PersistedGameState` rather than decoding a bare `GameState` and wrapping
    // it in `Raw`: only the former runs `reject_legacy_raw_prompt_authority` and
    // `decode_persisted_resolution_state`, which is the rest of the production chokepoint.
    // `.expect(..)`, not `?`: `into_game_state` returns `GameState`, not `Result`.
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
}

/// Load the REPORTED playtest capture — the dump the "offer says ∞, collapse allows 1" bug was
/// filed from — through the same production restore chokepoint `load_migrated_dump` uses. It is a
/// DIFFERENT game from `kilo_freed_relic_pentad_4p.json.gz` (different seed, board size, phase and
/// ObjectIds); this is the one the regression row drives, so nobody has to argue that the older
/// fixture stands in for it.
fn load_reported_capture() -> GameState {
    let json = gunzip(include_bytes!(
        "../fixtures/kilo_freed_relic_pentad_max_of_one_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("capture envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("the reported capture's gameState deserializes through the production decoder")
        .into_game_state()
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
fn drive_one_live_cycle(state: &mut GameState, ids: &LoopIds) {
    apply(
        state,
        P0,
        GameAction::ActivateAbility {
            source_id: ids.relic,
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
                apply(
                    state,
                    actor,
                    GameAction::SelectCards {
                        cards: vec![ids.kilo],
                    },
                )
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
                        targets: vec![TargetRef::Object(ids.pentad)],
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
                            source_id: ids.freed,
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
///
/// R4c — NAMED ACCEPTANCE ARM for the player-choice legality authority (CR 115.10a). Routing
/// `resolve_target`'s `TargetPin::Player` arm through the CHOICE authority
/// (`players::player_exists_for_choice`) rather than the TARGET one must not suppress a
/// shipped offer on a REAL dump, and this row is what says so: if a later change routes that
/// arm through `targeting::player_is_legal_target`, the over-veto class returns and this row
/// must still pass — so it is the acceptance side of the pair whose refusal side is
/// `analysis::decision_template::tests::a_shrouded_seat_is_untargetable_yet_still_choosable_
/// at_the_pin_recheck`.
///
/// ⚠ WHAT THIS ROW DOES NOT WITNESS, stated so nobody credits it with more than it covers:
/// this dump's pins are `ByIdentity` and `ManaColor`, NOT `TargetPin::Player`, so the Player
/// arm is not on its path at all. It is an acceptance arm for the offer PIPELINE, not a
/// witness for the Player-pin seam; that witness is R2b.
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

    drive_one_live_cycle(&mut state, &FIXTURE_IDS);

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

    drive_one_live_cycle(&mut state, &FIXTURE_IDS);

    // The stale pinless prefix makes `try_offer` re-drive from a pinless `seq[0]` and abort ⇒
    // no offer surfaces (the C2 / R3.0-A baseline).
    assert!(
        !matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }),
        "with the migration disabled (stale pinless prefix re-injected) the offer must NOT fire, \
         got {:?}",
        state.waiting_for
    );
}

/// Drive the APNAP accept of the ∞ offer through the PUBLIC `apply()` boundary at the harness
/// default of one cycle. CR 732.2c makes the accepted count BINDING on the boundary collapse
/// prompt, so a caller that later collapses to N must use [`drive_all_accept_n`].
fn drive_all_accept(state: &mut GameState) {
    drive_all_accept_n(state, 1);
}

/// Drive the APNAP accept at `n`: P0 (the proposer) declares `Fixed(n)`, then every prompted
/// opponent accepts in turn order until the protocol closes back to ordinary priority (CR
/// 800.4a). `template: None` skips declare-time pin validation; the materialize re-derives from
/// the intact `last_loop_action_sequence`. CR 732.2c: `n` bounds the CR 500.5 collapse prompt.
fn drive_all_accept_n(state: &mut GameState, n: u32) {
    use engine::analysis::decision_template::IterationCount;
    use engine::analysis::loop_check::ShortcutResponse;
    apply(
        state,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
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

/// Declare the offer's OWN stated count — exactly what the real frontend dispatches
/// (`LoopShortcutModal`'s `handleConfirm` sends `count: schema.iteration_count`, there being no
/// declare-time picker) — then accept in APNAP order. Returns the ceiling the SAME offer
/// published, so a caller can compare the CR 500.5 collapse range against it without restating a
/// literal that would pass on both sides of a regression.
fn drive_all_accept_as_offered(state: &mut GameState) -> u32 {
    use engine::analysis::loop_check::ShortcutResponse;
    let (proposer, ceiling, offered) = match &state.waiting_for {
        WaitingFor::LoopShortcut {
            proposer, schema, ..
        } => (
            *proposer,
            schema.max_iterations,
            schema.iteration_count.clone(),
        ),
        other => panic!("expected a CR 732.2a loop-shortcut offer, got {other:?}"),
    };
    apply(
        state,
        proposer,
        GameAction::DeclareShortcut {
            count: offered,
            template: None,
        },
    )
    .expect("the proposer declares the offer's own stated count, as the modal does");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts the as-offered ∞-charge shortcut");
    }
    ceiling
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
/// (`DerivedViews::counter_display`), and the serde wire shape, all driven through the real
/// accept pipeline from the real 4p dump.
///
/// REVERT-PROBE (measured, non-vacuous): deleting the `register_unbounded_counter_targets`
/// write in `materialize_object_growth_shortcut` (or the `current_period_counter_growth`
/// derivation it projects from) leaves `unbounded_counter_targets` empty ⇒ assertions (2) the
/// field write,
/// (3) the derived-view projection, and (4) the wire round-trip all FLIP to fail. The
/// offer-fires reach-guard (1) and the `charge == Some(4)` rules-correctness anchor
/// (display-only: the real count is untouched) HOLD BOTH WAYS.
#[test]
fn kilo_accept_marks_pentad_charge_as_unbounded_display_target() {
    use engine::game::derived_views::{
        derive_views, CounterMagnitude, CounterRowView, DerivedViews, ObjectCounterDisplay,
    };
    use engine::types::counter::CounterType;

    let mut state = load_migrated_dump();
    drive_one_live_cycle(&mut state, &FIXTURE_IDS);

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

    // (3) THE PER-SURFACE COUNTER-PILL ROW, on a REAL production fixture. The accept registers an
    // observed-growth `DriveSequence` naming the charge-counter axis, but the engine DEFERS
    // applying it to the CR 500.5 boundary, while advancing to the proposal's ending point (CR 732.2c). The
    // real charge count is unchanged (asserted just above) and the ∞ mark is live, so the pill
    // stays ∞ throughout that window. Filter nothing: (2) above is unchanged and still passes.
    //
    // REVERT-PROBE (RP-1c, RUN): restore the `collapse_scheduled(..)` guard in `derive_views`'
    // counter-pill loop ⇒ THIS `assert_eq!` fails (`left: None`) while (2) above and the pile
    // and row channels stay green.
    // PREMISE GUARD — the one deliberate exception to the WRITE-first ordering documented at the
    // wire pin below (PART 1). This asserts the input FRAME, never `views`: if the stash shape is
    // wrong the golden would be minted from the wrong frame, so it must abort BEFORE the write. It
    // can never be the assertion a revert probe reds — `derive_views(&GameState, ..)` takes an
    // IMMUTABLE borrow (cited by signature, not by line: this change moves `derived_views.rs`
    // wholesale, so a line anchor into it goes stale on its own edit), so no change to it can move
    // `pending_unbounded_materialization`.
    assert!(
        matches!(
            state
                .pending_unbounded_materialization
                .get(&P0)
                .map(Vec::as_slice),
            Some([PersistentAxisMaterialization::DriveSequence { .. }])
        ),
        "premise: the observed kilo accept registers exactly ONE DriveSequence — the only stash \
         shape that yields Scheduled(Committed); if this changes, the Committed witness below \
         measures something else. got={:?}",
        state.pending_unbounded_materialization.get(&P0)
    );

    let views = derive_views(&state, None);

    // Cross-seam wire pin, PART 1 — compute + (optionally) REGENERATE. Provenance: every
    // key/value below is ENGINE-EMITTED (`serde_json::to_value(&derive_views(..))`). The four ∞
    // keys are lifted BY NAME from the real serialized DerivedViews so unrelated derived-view churn
    // cannot move this golden, while the field names and value encodings — the part the TS mirror
    // must match — stay engine-authored.
    //
    // The WRITE deliberately precedes every ∞ assertion in this fn, and the drift COMPARE
    // deliberately follows them: a revert probe that reds one of those assertions must still be
    // able to regenerate the client goldens with `UPDATE_WIRE_GOLDEN=1`, or the client-side half of
    // that probe (RP-1b, RP-2) is unreachable. An assert panic aborts the test.
    //
    // ONE DELIBERATE EXCEPTION, above the `derive_views` call: the stash-shape PREMISE guard. It
    // asserts the INPUT FRAME, not this projection's output, and its purpose is exactly to stop a
    // golden being minted from a wrong frame — so it must abort before the write, not after it. It
    // cannot be the assertion a revert probe reds, because `derive_views` takes `&GameState` and no
    // change to it can move `pending_unbounded_materialization`.
    //
    // THE RULE FOR NEW ASSERTIONS, IN EVERY GOLDEN EMITTER: anything that reads `views` goes BELOW
    // the WRITE. Anything that checks the state `derive_views` is about to be handed goes above it.
    // Where a pre-WRITE frame must be asserted, CAPTURE it into a local above and assert the local
    // below (see combo_infinite_pile.rs's declined-wire emitter).
    //
    // DETERMINISM: `counter_display` is a std `HashMap<ObjectId, ObjectCounterDisplay>`
    // (derived_views.rs) — the VALUE is a pre-partitioned row set, not a bare counter-type list —
    // but `serde_json::Map` is BTreeMap-backed (serde_json has no `preserve_order` feature in this
    // workspace — see Cargo.lock), so `to_value` re-sorts every map key. Measured byte-identical
    // across independent test processes. No normalization needed.
    let wire = serde_json::to_value(&views).expect("derived views serialize");
    // Shared with `combo_infinite_pile`'s emitter — see `WIRE_GOLDEN_CHANNELS` for why the two
    // copies of this list had to become one.
    let golden: serde_json::Map<String, serde_json::Value> =
        crate::combo_infinite_pile::WIRE_GOLDEN_CHANNELS
            .into_iter()
            .filter_map(|k| wire.get(k).map(|v| (k.to_string(), v.clone())))
            .collect();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../client/src/test/fixtures/unbounded-counter-wire.json"
    );
    if std::env::var_os("UPDATE_WIRE_GOLDEN").is_some() {
        // `client/src/test/fixtures/` may not exist yet; `fs::write` does not create parents.
        std::fs::create_dir_all(
            std::path::Path::new(path)
                .parent()
                .expect("golden has a parent"),
        )
        .expect("create the client wire-golden directory");
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&golden).unwrap()),
        )
        .expect("write the wire golden");
    }

    // The row's count is READ FROM THE DUMP, never invented — this frame's Pentad really carries
    // this many charge counters, and the committed wire golden compared below carries that literal.
    let pentad_charge = state
        .objects
        .get(&PENTAD)
        .and_then(|o| o.counters.get(&charge).copied())
        .unwrap_or(0);
    assert!(
        pentad_charge >= 1,
        "reach-guard: this real-dump frame's Pentad must actually carry charge counters, so the \
         row's `count` below is a NONZERO live value and not vacuously 0; got {pentad_charge}"
    );
    assert_eq!(
        views.counter_display.get(&PENTAD),
        Some(&ObjectCounterDisplay {
            pills: vec![CounterRowView {
                counter: charge.clone(),
                count: pentad_charge,
                magnitude: CounterMagnitude::Unbounded,
            }],
            loyalty: None,
        }),
        "the ∞ charge pill stays projected while the collapse is merely SCHEDULED, and it carries \
         the LIVE count so the display never has to join back to `objects[..].counters`"
    );

    assert!(
        views.unbounded_families.iter().any(|f| f.player == P0
            && f.family == UnboundedFamily::Counters
            && f.state
                == FamilyCollapseState::Scheduled {
                    certainty: CollapseCertainty::Committed,
                    prompted: Some(P0),
                }),
        "the real kilo accept's single DriveSequence yields a Committed family on a REAL \
         production dump — that is this witness's distinct property, NOT uniqueness: two other \
         Committed witnesses exist on synthetic boards (combo_infinite_pile's grafted \
         DriveSequence for Tokens, loop_shortcut_mana_engine's R4/agree Life), and neither is \
         redundant with this one. This is the ∞→N matched positive the FE tests read out of \
         unbounded-counter-wire.json; it sits AFTER the WRITE so a mutation that reds it can \
         still regenerate the golden (M1-e(c), M2-d(b) depend on that). got={:?}",
        views.unbounded_families
    );

    // NON-VACUITY GUARD for the key list above, and it sits HERE — below the WRITE — under this
    // emitter's own stated rule, because it reads `golden`, which is derived from `views`.
    // `filter_map` DROPS a name that matches no `DerivedViews` field, and the drift compare below
    // then reads a committed file the same typo wrote — so both sides omit the channel and the
    // compare agrees with itself. Asserting the exact key SET turns a mistyped name into a RED.
    // `BTreeSet` so this does not depend on which container backs `serde_json::Map`.
    //
    // PER-FILE RESIDUAL, CLOSED BY THE PAIR: this frame legitimately carries no `unbounded_pile`,
    // and a name a frame never populates is indistinguishable from a mistyped one from inside that
    // frame. `combo_infinite_pile`'s twin guard covers `unbounded_pile` (and this file covers the
    // `counter_display` its frame lacks). The union spans all four BY CONSTRUCTION: both guards are
    // `WIRE_GOLDEN_CHANNELS` minus the one name their own frame lacks, so a name added to the
    // shared array reds whichever frame does not carry it instead of being silently dropped.
    let channels: std::collections::BTreeSet<&str> = golden.keys().map(String::as_str).collect();
    let mut expected =
        std::collections::BTreeSet::from(crate::combo_infinite_pile::WIRE_GOLDEN_CHANNELS);
    expected.remove("unbounded_pile");
    assert_eq!(
        channels, expected,
        "the golden key list names a field `DerivedViews` does not have, or this frame stopped \
         carrying one it must: a mistyped name is dropped silently and the drift compare below \
         then agrees with itself. Check every name against `DerivedViews`."
    );

    // Cross-seam wire pin, PART 2 — the drift COMPARE (see PART 1 for why it sits here).
    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("committed wire golden"))
            .unwrap();
    assert_eq!(
        serde_json::Value::Object(golden),
        committed,
        "the client's wire golden drifted from engine output — re-run with UPDATE_WIRE_GOLDEN=1"
    );

    // (4) WIRE ROUND-TRIP (FLIPS on revert): the populated channel serializes, is present on
    // the wire, and survives a round-trip; an EMPTY derived view omits it (skip_serializing_if).
    let json = serde_json::to_string(&views).expect("derived views serialize");
    assert!(
        json.contains("counter_display"),
        "the populated counter-display channel is present on the wire"
    );
    let round: DerivedViews = serde_json::from_str(&json).expect("derived views round-trip");
    assert_eq!(
        round.counter_display.get(&PENTAD),
        Some(&ObjectCounterDisplay {
            pills: vec![CounterRowView {
                counter: charge,
                count: pentad_charge,
                magnitude: CounterMagnitude::Unbounded,
            }],
            loyalty: None,
        }),
        "the counter-display channel survives a serde round-trip, count and magnitude included"
    );
    let empty_json =
        serde_json::to_string(&DerivedViews::default()).expect("empty derived views serialize");
    assert!(
        !empty_json.contains("counter_display"),
        "the field is omitted (skip_serializing_if) when empty"
    );
}

/// TARGET-DEPARTURE RELATION (CR 732.2a / CR 110.1), pinned end-to-end on the real 4p dump: when a
/// registered ∞ counter target leaves the battlefield, the per-object PILL disappears from the wire
/// while the aggregate counter ROW remains — and the STORE keeps the departed pair.
///
/// That the pill and the row disagree is the point, and the REASON has changed — the assertion
/// outlived its original justification, which is why the discriminator arm below now exists.
///
/// A pill is keyed by `ObjectId` and departure is an OBJECT event, so a pill has the identity it
/// needs to filter itself, and it filters unconditionally. A row is keyed by `ResourceAxis`. This
/// test used to explain the row's survival by "no axis-scoped backing authority exists" — that is
/// no longer true: `object_growth_backing` answers `Counter(..)` by deriving each registered
/// `(ObjectId, CounterType)` pair's own axis (`collapsed_counter_axis`), and on this very state
/// that answer is `Some(false)`. The row survives for a DIFFERENT reason: this fixture's collapse
/// was ACCEPTED (`drive_all_accept` above), and CR 732.2c makes an accepted shortcut binding, so
/// the acceptance conjunct keeps the row regardless of what happened to its targets.
///
/// THAT DISTINCTION IS WHY (6) IS LOAD-BEARING. With the stash present, (4) passes whether or not
/// the counter authority works at all — every wrong answer in that subsystem (`None` from an
/// unmatched axis, an unregistered axis, a drifted bridge) also keeps the badge. So (4) alone is
/// vacuous in the direction that matters, and (6) is the arm that removes the acceptance and
/// requires the row to DIE. Only (6) proves the accept registered a pair whose derived axis equals
/// a marked axis — i.e. that the bridge join succeeds on PRODUCTION-DERIVED data rather than on
/// hand-built state, which no building-block test can establish.
///
/// Nothing pinned this relation before: the token family's analog
/// (`loop_shortcut::stale_pile_member_is_omitted_from_the_wire_but_kept_in_the_store`) covers the
/// pile, and the counter pill's battlefield filter had no runnable guard on a real accept.
///
/// MUTATIONS (to be RUN and recorded, one expected red each — if any reds more than its own row
/// that is reported, not trimmed):
/// - delete the `!state.battlefield.contains(id)` filter in `derive_views`' counter-pill loop
///   => (3) reds alone;
/// - restore the controller-keyed `Some(false)` `Counter(..)` arm in `object_growth_backing`
///   => (4) reds alone;
/// - "fix" it by pruning the STORE instead of the wire => (5) reds alone. (5) is the discriminator
///   against that wrong fix: the boundary collapse reads the store;
/// - revert the `Counter(..)` arm to `None` (the refusing revision) => (6) reds ALONE, and
///   nothing else here moves. That isolation is the proof (6) is measuring the authority and not
///   the acceptance gate.
#[test]
fn departed_counter_target_drops_its_pill_but_keeps_its_row_and_store_entry() {
    use engine::analysis::resource::ResourceAxis;
    use engine::game::derived_views::derive_views;
    use engine::game::zones::move_to_zone;
    use engine::types::counter::CounterType;
    use engine::types::events::GameEvent;
    use engine::types::zones::Zone;

    let mut state = load_migrated_dump();
    drive_one_live_cycle(&mut state, &FIXTURE_IDS);
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the CR 732.2a ∞-charge offer for P0, got {:?}",
        state.waiting_for
    );
    drive_all_accept(&mut state);

    let charge = CounterType::Generic("charge".into());

    // (1) REACH-GUARD, holds under every mutation below: the accept registered the target AND it
    // is on the battlefield right now — so any divergence after the move is caused by the
    // departure and by nothing else.
    assert!(
        state
            .unbounded_counter_targets
            .get(&P0)
            .is_some_and(|t| t.contains(&(PENTAD, charge.clone()))),
        "reach-guard: the accept registered (Pentad, charge) as a ∞ display target"
    );
    assert!(
        state.battlefield.contains(&PENTAD),
        "reach-guard: BEFORE the departure the target is on the battlefield"
    );

    // (2) REACH-GUARD: the pill and the row are BOTH present beforehand. Without this the
    // post-departure assertions could pass on a wire that never carried either.
    let before = derive_views(&state, None);
    assert!(
        before
            .counter_display
            .get(&PENTAD)
            .is_some_and(|display| display.pills.iter().any(|r| r.counter == charge)),
        "reach-guard: the pill is on the wire before the departure"
    );
    let row_axes_before: Vec<_> = before.unbounded_resources.iter().map(|r| r.axis).collect();
    assert!(
        row_axes_before
            .iter()
            .any(|a| matches!(a, ResourceAxis::Counter(..))),
        "reach-guard: a counter ROW is on the wire before the departure, got {row_axes_before:?}"
    );

    // The departure itself, through the production chokepoint (CR 110.1: it stops being a
    // permanent).
    let mut events: Vec<GameEvent> = Vec::new();
    move_to_zone(&mut state, PENTAD, Zone::Graveyard, &mut events);
    assert!(
        !state.battlefield.contains(&PENTAD),
        "the departure really happened"
    );

    let after = derive_views(&state, None);

    // (3) THE PILL IS GONE — departure is an object event and the pill has object identity.
    assert!(
        !after.counter_display.contains_key(&PENTAD),
        "(3) the departed target's ∞ pill must leave the wire, got {:?}",
        after.counter_display
    );

    // (4) THE ROW REMAINS — because the collapse was ACCEPTED (CR 732.2c), not because nothing
    // could revoke it. (6) below is what distinguishes those two explanations.
    let row_axes_after: Vec<_> = after.unbounded_resources.iter().map(|r| r.axis).collect();
    assert!(
        row_axes_after
            .iter()
            .any(|a| matches!(a, ResourceAxis::Counter(..))),
        "(4) the counter ROW must survive its target's departure — the table already accepted \
         this collapse and it still lands at the boundary, got {row_axes_after:?}"
    );

    // (5) THE STORE IS NOT PRUNED — discriminator against "fixing" this by mutating the store:
    // the CR 500.5 boundary collapse reads it.
    assert!(
        state
            .unbounded_counter_targets
            .get(&P0)
            .is_some_and(|t| t.contains(&(PENTAD, charge.clone()))),
        "(5) the STORE must still carry the departed (object, counter) pair — only the wire filters"
    );

    // (6) THE DISCRIMINATOR, and the only non-vacuous half of (4). Same post-departure state with
    // the ACCEPTANCE removed: the row must now DIE. This is the single assertion in this file that
    // requires the counter authority to actually work — it forces `object_growth_backing` to
    // derive the departed pair's axis through `collapsed_counter_axis` and match it against a
    // MARKED axis, both sides produced by the real accept on a real dump. Nothing hand-built can
    // show that the two agree on production-derived data; that is why this arm lives here rather
    // than at building-block level.
    //
    // `pending_unbounded_materialization` is a public field and this is a local clone, so removing
    // it mutates nothing the rest of the test observes.
    let mut unaccepted = state.clone();
    unaccepted.pending_unbounded_materialization.clear();
    let after_unaccepted = derive_views(&unaccepted, None);
    let unaccepted_axes: Vec<_> = after_unaccepted
        .unbounded_resources
        .iter()
        .map(|r| r.axis)
        .collect();
    assert!(
        !unaccepted_axes
            .iter()
            .any(|a| matches!(a, ResourceAxis::Counter(..))),
        "(6) with the accepted collapse removed, the departed targets leave the counter row with \
         no live backing and it MUST be revoked — if it survives here, (4) above is passing for \
         no reason and the counter authority is not working, got {unaccepted_axes:?}"
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
    use engine::game::derived_views::{
        derive_views, CounterMagnitude, CounterRowView, ObjectCounterDisplay,
    };
    use engine::types::counter::CounterType;

    const N: u32 = 5;
    let charge = CounterType::Generic("charge".into());

    let mut state = load_migrated_dump();
    drive_one_live_cycle(&mut state, &FIXTURE_IDS);

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
    // CR 732.2c: the accepted count binds the boundary collapse, so accept at exactly N.
    drive_all_accept_n(&mut state, N);
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
    // (5b) THE ∞ ANNOTATION CLEARS BUT THE FINITE ROW SURVIVES, on an object that never left the
    // battlefield: `clear_collapsed_materializations` drops the registered pair, and the finite
    // pass in `counter_display_views` keeps publishing the now-real count — so the pill renders
    // the real number rather than `∞`, and it does not vanish.
    let display = derive_views(&state, None)
        .counter_display
        .get(&PENTAD)
        .cloned()
        .expect(
            "after the collapse Pentad still renders a FINITE charge row — the `∞` ANNOTATION is \
             what `clear_collapsed_materializations` clears, not the row itself",
        );
    assert_eq!(
        display,
        ObjectCounterDisplay {
            pills: vec![CounterRowView {
                counter: charge.clone(),
                count: baseline + N,
                magnitude: CounterMagnitude::Finite,
            }],
            loyalty: None,
        },
        "the collapsed pair renders as EXACTLY one FINITE row carrying the real collapsed count"
    );

    // (6) The boundary protocol closed cleanly back to ordinary priority (CR 800.4a).
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "after the collapse submit, priority is restored, got {:?}",
        state.waiting_for
    );
}

/// CR 732.2a + CR 732.2c REGRESSION, driven from the ACTUAL REPORTED PLAYTEST CAPTURE (the dump the
/// "the offer says ∞ but the collapse only allows 1" report was filed from — a DIFFERENT game from
/// the older fixture the rows above drive).
///
/// The unbounded object-growth producer publishes the global safety limit as its ceiling but seeded
/// its stated count with a bare 1. The frontend echoes that stated count verbatim (there is no
/// declare-time picker), CR 732.2c makes the accepted count binding, and the accepted count caps the
/// CR 500.5 collapse prompt — so a stated count below the published ceiling silently picks the
/// controller's number for them.
///
/// ONE flipping conjunct and THREE reach-guards. The flipping one is the boundary range: it reads
/// the ceiling off the SAME live offer rather than restating a literal, so no arm of it can pass on
/// both sides of the regression. Pre-fix it reads a collapse max of 1 against a published ceiling of
/// 1000.
///
/// Never hard-code the declared count here: `drive_all_accept_as_offered` reads
/// `schema.iteration_count` to reproduce the frontend echo exactly, and that echo is what makes the
/// row discriminating. Never submit the amount either — the row stops at the prompt, because
/// asserting the offered RANGE is both the claim under test and the cheap path.
#[test]
fn kilo_reported_capture_offer_states_the_full_ceiling_it_publishes() {
    let mut state = load_reported_capture();

    // (1) LOAD REACH-GUARD (holds both ways): the reported capture is what loaded, not a stand-in
    // for it. The board is the untouched 4p playtest capture, with the loop's four permanents on
    // the controller's battlefield under the MEASURED ids.
    assert_eq!(
        state.objects.len(),
        409,
        "the reported 4p playtest capture loads intact"
    );
    for (label, id) in [
        ("Kilo", CAPTURE_IDS.kilo),
        ("Freed", CAPTURE_IDS.freed),
        ("Relic", CAPTURE_IDS.relic),
        ("Pentad", CAPTURE_IDS.pentad),
    ] {
        let permanent = &state.objects[&id];
        assert_eq!(
            (permanent.zone, permanent.controller),
            (Zone::Battlefield, P0),
            "{label} is on the loop controller's battlefield in the reported capture"
        );
    }

    drive_one_live_cycle(&mut state, &CAPTURE_IDS);

    // (2) OFFER REACH-GUARD (holds both ways; gates 3 and 4). This assertion MUST sit here, between
    // the live drive and the accept: `drive_all_accept_as_offered` CONSUMES the offer, and its own
    // first statement panics on any non-offer beat, so placed after the accept this guard would be
    // dead code and its failure mode unreadable.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the CR 732.2a ∞-charge offer surfaced for the loop's controller, got {:?}",
        state.waiting_for
    );

    // Declare the offer's OWN stated count and accept in APNAP order — the exact dispatch the modal
    // makes. Returns the ceiling that same offer published.
    let ceiling = drive_all_accept_as_offered(&mut state);

    // (3) NON-VACUITY FLOOR (holds both ways): a published ceiling of 1 could not tell a capped
    // boundary apart from an honest one.
    assert!(
        ceiling > 1,
        "the offer publishes a ceiling above 1, so a capped boundary is a real narrowing"
    );

    drive_to_collapse_boundary(&mut state);

    // (4) THE FLIPPING ASSERTION. CR 732.2c binds the accepted count, and CR 500.5's collapse prompt
    // is capped by it — so the range the controller is offered must reach the ceiling the offer
    // itself published. Pre-fix this reads a max of 1 against a ceiling of 1000.
    let WaitingFor::PayAmountChoice {
        player,
        resource: PayableResource::LoopCollapse { .. },
        min,
        max,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "the boundary drive must end at the deferred-collapse prompt it exists to reach, \
             got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        *player, P0,
        "the loop's controller is the seat asked to name the collapse count"
    );
    assert_eq!(
        *min, 0,
        "CR 732.2b: declining to shorten at every place makes every prefix consented to"
    );
    assert_eq!(
        *max, ceiling,
        "CR 732.2c: the collapse prompt offers the very ceiling the accepted offer published"
    );
}

/// CR 732.2a + CR 732.2c: the offer has TWO live declare authorities, and they must state the same
/// count. `LoopShortcutModal` echoes `schema.iteration_count` verbatim; the interaction wire echoes
/// it through the published `suggested`, and `AcceptSuggested` turns that `suggested` into the
/// declared `IterationCount`. If they disagree, a client on the wire binds a different CR 732.2c
/// count than the React client binds for the SAME offer.
///
/// Driven from the REPORTED capture through the real producer — no hand-built schema anywhere, which
/// is exactly what the two `interaction_contract` rows this replaces could not offer.
///
/// TWO assertions, with DIFFERENT jobs — do not read them as two revert-failing conjuncts.
/// The first is the revert-failing one: pre-fix the published pair reads a suggestion of 1 against a
/// max of 1000, it fails, and because a failing assertion panics, the second never evaluates on that
/// arm. The second is a MUTATION GUARD on the arm that maps the published suggestion to the declared
/// count: post-fix both are green, and forcing that arm to declare a bare 1 reds this row and only
/// this row. Without the second assertion that mutation leaves the row green, which would move the
/// coverage gap by one line instead of closing it.
#[test]
fn kilo_reported_capture_interaction_picker_suggests_the_full_ceiling() {
    let mut state = load_reported_capture();
    drive_one_live_cycle(&mut state, &CAPTURE_IDS);

    // Reach-guard (holds both ways): the live offer is what we are about to project.
    let WaitingFor::LoopShortcut {
        proposer, schema, ..
    } = &state.waiting_for
    else {
        // Wording is deliberately unlike every other abort message in this file — the regression
        // triage procedure routes on message text, so two sites must never print a near-match.
        panic!(
            "the interaction-picker row needs the offer still live at this beat, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*proposer, P0, "the loop's controller proposes the shortcut");
    let ceiling = schema.max_iterations;
    // Non-vacuity floor (holds both ways): a ceiling of 1 could not discriminate.
    assert!(ceiling > 1, "the offer publishes a ceiling above 1");

    // Probe on a CLONE. `bind_interaction_authority` takes `&mut GameState`, and nothing in this
    // row may perturb a drive; cloning makes the whole projection provably inert.
    let mut probe = state.clone();
    bind_interaction_authority(
        &mut probe,
        InteractionSessionId("wb7048-ceiling".to_string()),
    )
    .expect("bind the interaction authority over the live offer");
    let filtered = filter_state_for_viewer(&probe, P0);
    let view = derive_viewer_interaction(&probe, &filtered, P0);
    let opportunity = view
        .opportunities
        .first()
        .expect("the live offer publishes an interaction opportunity");
    let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Shortcut { count, points, .. },
        ..
    } = &opportunity.response
    else {
        panic!(
            "the live offer publishes a Shortcut response schema, got {:?}",
            opportunity.response
        );
    };
    let InteractionShortcutCountSpec::Fixed { suggested, max, .. } = count else {
        panic!("an Advantage offer publishes a Fixed count spec, got {count:?}");
    };

    // ASSERTION 1 — THE REVERT-FAILING ONE (hops 1-3): the producer's seed survives the clamp, at
    // the offer's own bound. Pre-fix this reads a suggestion of 1 against a max of 1000, fails, and
    // panics — so assertion 2 below does not evaluate on the pre-fix arm.
    assert_eq!(
        (*suggested, *max),
        (ceiling, ceiling),
        "CR 732.2a: the picker suggests the very ceiling this offer publishes"
    );

    // ASSERTION 2 — THE MUTATION GUARD (hop 4): `AcceptSuggested` declares that suggestion. It is
    // green on BOTH arms of the seed fix; what it catches is a change to the arm that maps the
    // published suggestion onto the declared count, which assertion 1 cannot see at all. Pins are
    // derived from the PUBLISHED points, never by index — one pin per non-read-only point, holding
    // exactly that point's `min` choices, which is what the materializer validates.
    let pins: Vec<InteractionShortcutPin> = points
        .iter()
        .filter(|point| !point.read_only)
        .map(|point| InteractionShortcutPin {
            group: point.group,
            choice_ids: point
                .candidate_ids
                .iter()
                .take(point.min as usize)
                .cloned()
                .collect(),
        })
        .collect();
    let action = resolve_interaction_response(
        &probe,
        P0,
        &InteractionSubmission {
            interaction_id: opportunity.interaction_id.clone(),
            response: InteractionResponse::Shortcut {
                decision: InteractionShortcutDecision::AcceptSuggested,
                pins,
            },
        },
    )
    .expect("AcceptSuggested materializes a declare against the live offer");
    let GameAction::DeclareShortcut {
        count: declared, ..
    } = &action
    else {
        panic!("AcceptSuggested materializes a DeclareShortcut, got {action:?}");
    };
    assert_eq!(
        *declared,
        IterationCount::Fixed(ceiling),
        "CR 732.2c: the wire declare binds the same count the React echo binds"
    );
}
