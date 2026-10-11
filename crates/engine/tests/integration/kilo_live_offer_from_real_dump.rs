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
//! This exercises the offer end-to-end through the PUBLIC `apply()` boundary (the "combo FIRES in
//! a real game" criterion): the tap-target / mana-color / proliferate-target answers are replayed,
//! and the counter-growth cover disjunct accepts the +1-charge/cycle growth.

use engine::analysis::decision_template::IterationCount;
use engine::game::engine::apply;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    GameState, ManaChoice, PayCostKind, PersistedGameState, WaitingFor,
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
pub(crate) const FREED: ObjectId = ObjectId(403);
pub(crate) const RELIC: ObjectId = ObjectId(404);
const PENTAD: ObjectId = ObjectId(405);
/// Relic of Legends ability index 1 = "Tap an untapped legendary creature you control: Add one
/// mana of any color"; Freed from the Real ability index 1 = "{U}: Untap enchanted creature".
pub(crate) const RELIC_TAP_MANA: usize = 1;
pub(crate) const FREED_UNTAP: usize = 1;

/// The four loop permanents, per dump. Both real captures hold the same Kilo/Freed/Relic/Pentad
/// board under P0; only the `ObjectId`s differ, so ONE drive authority serves both and the
/// regression row cannot silently diverge from the rows that already pin this loop's behavior.
pub(crate) struct LoopIds {
    kilo: ObjectId,
    freed: ObjectId,
    relic: ObjectId,
    pentad: ObjectId,
}
pub(crate) const FIXTURE_IDS: LoopIds = LoopIds {
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
pub(crate) fn load_migrated_dump() -> GameState {
    let json = gunzip(include_bytes!(
        "../fixtures/kilo_freed_relic_pentad_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    // Decode AS `PersistedGameState` rather than decoding a bare `GameState` and wrapping
    // it in `Raw`: only the former runs `reject_legacy_raw_prompt_authority` and
    // `decode_persisted_resolution_state`, which is the rest of the production chokepoint.
    // The test unwraps the fallible persistence boundary after asserting this fixture decodes.
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract")
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
        .expect("persisted test snapshot satisfies the checked restore contract")
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
pub(crate) fn drive_one_live_cycle(state: &mut GameState, ids: &LoopIds) {
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

    // Reach-guard: the live cycle's first play is the Relic mana activation.
    let first_play = engine::game::play_trace_view(&state).and_then(|view| {
        view.entries.into_iter().find_map(|entry| match entry.kind {
            engine::game::EntryKind::Play { action, .. } => Some(action),
            engine::game::EntryKind::Answer { .. } | engine::game::EntryKind::Resolution { .. } => {
                None
            }
        })
    });
    assert_eq!(
        first_play,
        Some(GameAction::ActivateAbility {
            source_id: RELIC,
            ability_index: RELIC_TAP_MANA,
        }),
        "the window's first play is the Relic mana activation"
    );

    // THE OFFER: the ∞-charge CR 732.2a shortcut surfaces for P0, carrying the reified schema.
    match &state.waiting_for {
        WaitingFor::LoopShortcut {
            proposer, schema, ..
        } => {
            assert_eq!(*proposer, P0, "the loop's controller proposes the shortcut");
            let wire = serde_json::to_value(&state.waiting_for).expect("the offer serializes");
            let plays: Vec<(u64, GameAction)> = wire["data"]["period"]["items"]
                .as_array()
                .expect("the offer carries its period")
                .iter()
                .filter(|item| !item["play"].is_null())
                .map(|item| {
                    (
                        item["seat"].as_u64().expect("a seat"),
                        serde_json::from_value(item["action"].clone()).expect("an action"),
                    )
                })
                .collect();
            assert_eq!(
                plays,
                vec![
                    (
                        0,
                        GameAction::ActivateAbility {
                            source_id: RELIC,
                            ability_index: RELIC_TAP_MANA,
                        }
                    ),
                    (
                        0,
                        GameAction::ActivateAbility {
                            source_id: FREED,
                            ability_index: FREED_UNTAP,
                        }
                    ),
                ],
                "the offered period is P0's Relic and Freed activations"
            );
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

/// Drive the APNAP accept at `n`: P0 (the proposer) declares `Fixed(n)`, then every prompted
/// opponent accepts in turn order until the protocol closes at its CR 732.2a ending point — a
/// place where a player has priority. `template: None` skips declare-time pin validation.
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

/// The offer's OWN stated count — exactly what the real frontend dispatches
/// (`LoopShortcutModal`'s `handleConfirm` sends `count: schema.iteration_count`, there being no
/// declare-time picker) — and the ceiling the SAME offer published.
fn offered_count_and_ceiling(
    state: &GameState,
) -> (engine::analysis::decision_template::IterationCount, u32) {
    match &state.waiting_for {
        WaitingFor::LoopShortcut { schema, .. } => {
            (schema.iteration_count.clone(), schema.deliverable_capacity)
        }
        other => panic!("expected a CR 732.2a loop-shortcut offer, got {other:?}"),
    }
}

/// CR 732.2c / CR 701.34a: the accepted Kilo proliferate ∞-charge loop is observed growth, so its
/// take performs it: EXACTLY N more charge counters on Pentad Prism at the accept, driven
/// end-to-end through the public `apply()` boundary from the real 4p dump.
#[test]
fn kilo_accept_delivers_exactly_n_counters_at_the_take() {
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
    // Baseline: the REAL charge count at the offer (grew 3→4 in the driven cycle).
    let baseline = state.objects[&PENTAD]
        .counters
        .get(&charge)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        baseline, 4,
        "Pentad carries 4 real charge counters at the offer"
    );

    drive_all_accept_n(&mut state, N);

    // (2) EXACTLY +N counters (the measured 4→9 for N=5), each from a real CR 701.34a proliferate.
    assert_eq!(
        state.objects[&PENTAD].counters.get(&charge).copied(),
        Some(baseline + N),
        "the accepted ∞-charge loop delivers EXACTLY baseline+N charge counters at the take"
    );

    // (3) Nothing is left for the step end: no stash, no ∞ counter target, and the pill renders
    // the real count.
    assert!(
        !state.pending_unbounded_materialization.contains_key(&P0)
            && !state.unbounded_counter_targets.contains_key(&P0),
        "the performed take leaves no ∞ counter target or stash for P0"
    );
    assert_eq!(
        derive_views(&state, None).counter_display.get(&PENTAD),
        Some(&ObjectCounterDisplay {
            pills: vec![CounterRowView {
                counter: charge.clone(),
                count: baseline + N,
                magnitude: CounterMagnitude::Finite,
            }],
            loyalty: None,
        }),
        "Pentad renders EXACTLY one FINITE row carrying the real count"
    );

    // (4) CR 732.2a: the take closed at its ending point — a place where a player has priority.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "after the take, priority is restored, got {:?}",
        state.waiting_for
    );
}

/// CR 732.2a + CR 732.2c REGRESSION, driven from the ACTUAL REPORTED PLAYTEST CAPTURE (the dump the
/// "the offer says ∞ but the collapse only allows 1" report was filed from — a DIFFERENT game from
/// the older fixture the rows above drive).
///
/// The unbounded object-growth producer publishes the global safety limit as its ceiling but seeded
/// its stated count with a bare 1. The frontend echoes that stated count verbatim (there is no
/// declare-time picker), and CR 732.2c makes the accepted count binding — so a stated count below
/// the published ceiling silently picks the controller's number for them.
///
/// The flipping conjunct is the stated count: it reads the ceiling off the SAME live offer rather
/// than restating a literal, so it cannot pass on both sides of the regression. Pre-fix the offer
/// states 1 against a published ceiling of 1000. The take then accepts a bounded count, because
/// performing the full ceiling at the take is the accepted quadratic cost of this board's route.
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

    let charge = engine::types::counter::CounterType::Generic("charge".into());
    let charges = |state: &GameState| {
        state.objects[&CAPTURE_IDS.pentad]
            .counters
            .get(&charge)
            .copied()
            .unwrap_or(0)
    };
    let offered = charges(&state);
    let (stated, ceiling) = offered_count_and_ceiling(&state);

    // (3) NON-VACUITY FLOOR (holds both ways): a published ceiling of 1 could not tell a capped
    // boundary apart from an honest one.
    assert!(
        ceiling > 1,
        "the offer publishes a ceiling above 1, so a capped boundary is a real narrowing"
    );

    // (4) THE FLIPPING ASSERTION. The modal echoes the stated count, and CR 732.2c binds it, so the
    // offer must state the very ceiling it publishes. Pre-fix it states 1 against a ceiling of 1000.
    assert_eq!(
        stated,
        engine::analysis::decision_template::IterationCount::Fixed(ceiling),
        "the offer states the very ceiling it publishes"
    );

    // (5) The smallest count a one-cycle take cannot satisfy.
    const K: u32 = 2;
    drive_all_accept_n(&mut state, K);
    assert_eq!(
        charges(&state),
        offered + K,
        "CR 732.2c: the take delivers the accepted count"
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
    let ceiling = schema.deliverable_capacity;
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
            amounts: Vec::new(),
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

/// The Kilo collapse's per-cycle history work does not grow with its count.
#[test]
fn kilo_take_history_work_is_flat_per_cycle() {
    use crate::loop_shortcut::{
        assert_take_history_work_is_flat, TakeHistoryMap, TakeHistoryVector,
    };

    assert_take_history_work_is_flat(
        32,
        &[
            TakeHistoryVector::JournalEntries,
            TakeHistoryVector::ProducedMana,
            TakeHistoryVector::SpentMana,
            TakeHistoryVector::CountersAdded,
        ],
        &[
            TakeHistoryMap::AbilityResolutions,
            TakeHistoryMap::ActivatedAbilities,
        ],
        |n| {
            let mut state = load_migrated_dump();
            drive_one_live_cycle(&mut state, &FIXTURE_IDS);
            engine::game::perf_counters::reset();
            drive_all_accept_n(&mut state, n);
            state
        },
    );
}
