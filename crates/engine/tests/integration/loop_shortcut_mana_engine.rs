// engine-citation-gate: symbol anchors only
//! P7 v3 (CR 732.2a): capture + drive a MULTI-ACTION mana-engine loop.
//!
//! Real-card acceptance: **Basalt Monolith + Power Artifact** — the canonical 2-card infinite-mana
//! combo. Basalt's `{T}: Add {C}{C}{C}` (an off-stack mana ability, CR 605.3b) then its separate
//! `{3}: Untap this artifact` (on-stack, reduced to `{1}` by Power Artifact, CR 118.9) form ONE
//! loop period of TWO activations whose net progress is `+2 {C}` per cycle while the board returns
//! to equality. This is the multi-action class: no single play represents it.
//!
//! Honesty bar: every card is loaded from the real `shared_card_db()` through the real
//! parser+reducer; Power Artifact's cost reduction materializes through the LAYER system
//! (`attach_to` → `flush_layers`); every beat runs through `apply_action` / `GameAction`.

use super::support::shared_card_db;
use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::{ShortcutResponse, WinKind};
use engine::analysis::resource::ResourceAxis;
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::effects::attach::attach_to;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::types::ability::{AbilityKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, LoopDetectionMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const BASALT: &str = "Basalt Monolith";
const POWER: &str = "Power Artifact";

/// Place a real card on the battlefield after build, bypassing the unattached-aura attach-choice
/// pause (mirrors `loop_shortcut_activation`). Auras must be placed this way then attached.
fn place_on_battlefield(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    db: &CardDatabase,
) -> ObjectId {
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("card '{name}' not found in fixture"));
    let id = create_object_from_card_face(state, face, player);
    remove_from_zone(state, id, Zone::Library, player);
    add_to_zone(state, id, Zone::Battlefield, player);
    state.objects.get_mut(&id).unwrap().zone = Zone::Battlefield;
    id
}

/// The layer-derived mana-ability index on `source` (`{T}: Add {C}{C}{C}`). Read OFF the object.
pub(crate) fn mana_ability_index(state: &GameState, source: ObjectId) -> Option<usize> {
    state
        .objects
        .get(&source)?
        .abilities
        .iter()
        .position(is_mana_ability)
}

/// The layer-derived NON-mana activated ability index on `source` (`{3}: Untap this artifact`).
/// The static "doesn't untap during your untap step" ability is `Static`-kind, so the only
/// non-mana `Activated` ability is the untap.
pub(crate) fn untap_ability_index(state: &GameState, source: ObjectId) -> Option<usize> {
    state
        .objects
        .get(&source)?
        .abilities
        .iter()
        .position(|def| def.kind == AbilityKind::Activated && !is_mana_ability(def))
}

/// Tap an untapped land `player` controls for mana (its mana ability), giving floating mana.
fn tap_untapped_land(runner: &mut GameRunner, player: PlayerId) {
    let land = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            let o = &runner.state().objects[id];
            o.controller == player && !o.tapped && o.card_types.core_types.contains(&CoreType::Land)
        })
        .expect("an untapped land");
    let mana_idx = mana_ability_index(runner.state(), land).expect("land mana ability");
    runner
        .act(GameAction::ActivateAbility {
            source_id: land,
            ability_index: mana_idx,
        })
        .expect("tap land for mana");
}

/// Floating colorless mana in `player`'s pool.
fn colorless(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.count_color(ManaType::Colorless))
        .unwrap_or(0)
}

pub(crate) struct Rig {
    pub(crate) runner: GameRunner,
    pub(crate) basalt: ObjectId,
}

/// Build the 2-player rig: Basalt Monolith on P0's battlefield, optionally with Power Artifact
/// attached (the cost-reduction that makes the untap net-positive). `mode` selects the detector.
pub(crate) fn setup(with_power: bool, mode: LoopDetectionMode, db: &CardDatabase) -> Rig {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let basalt = scenario.add_real_card(P0, BASALT, Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    if with_power {
        let power = place_on_battlefield(runner.state_mut(), P0, POWER, db);
        attach_to(runner.state_mut(), power, basalt);
        assert_eq!(
            runner.state().objects[&power].attached_to,
            Some(basalt.into()),
            "Power Artifact must attach to Basalt (attach_to succeeded)"
        );
    }
    Rig { runner, basalt }
}

/// Activate `ability_index` on `source`, then pass priority (both seats) until the stack settles
/// empty at a `Priority` window OR a `LoopShortcut` offer surfaces.
fn activate_and_settle(runner: &mut GameRunner, source: ObjectId, ability_index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("activation is legal");
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {}
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// Drive one full loop period: the off-stack mana beat, then the on-stack untap beat, settling
/// each. Returns with the CR 732.2a offer surfaced (if the loop is detected).
pub(crate) fn drive_one_period(rig: &mut Rig, mana_idx: usize, untap_idx: usize) {
    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);
    activate_and_settle(&mut rig.runner, rig.basalt, untap_idx);
}

/// T1 ⭐ — real Basalt + Power OFFERS a MULTI-ACTION shortcut `[Mana(Colorless)]` / `Advantage` /
/// (Fixed count picker). The whole STEP B/C/D pipeline end-to-end through `apply_action`.
/// Revert-failing: dropping STEP C's sequence drive (driving only `seq[0]`) re-taps the tapped
/// Basalt on the 2nd iteration ⇒ `RecastAbort` ⇒ no offer (the `activation_loop_without_untapper`
/// twin shows the same abort mechanism). The paired negative is T6 (no Power ⇒ net-0 ⇒ no offer).
#[test]
fn mana_engine_basalt_power_offers_mana_advantage_shortcut() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt's {T}: Add {C}{C}{C} mana ability");
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt's {3}: Untap activated ability");

    drive_one_period(&mut rig, mana_idx, untap_idx);

    // Positive reach-guard: BOTH beats were traced (non-vacuous) before the offer.
    assert_eq!(
        plays(rig.runner.state()).len(),
        2,
        "the period is a 2-activation sequence (mana beat + untap beat)"
    );
    match &rig.runner.state().waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            certificate,
            ..
        } => {
            assert_eq!(*proposer, P0, "the loop's controller proposes the shortcut");
            assert_eq!(
                certificate.unbounded,
                vec![ResourceAxis::Mana(ManaType::Colorless)],
                "the mana-engine certificate names exactly the colorless-mana axis"
            );
            assert_eq!(
                certificate.win_kind,
                WinKind::Advantage,
                "a pure mana engine is an Advantage loop (no lethal/poison/decking axis)"
            );
        }
        other => panic!("expected a CR 732.2a LoopShortcut offer, got {other:?}"),
    }
}

/// T2 — the trace holds both beats in order: one play after the mana beat, two after the untap
/// beat, both P0's.
#[test]
fn mana_engine_accumulates_both_beats() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);
    assert_eq!(
        plays(rig.runner.state()).len(),
        1,
        "the off-stack mana beat is one play"
    );
    activate_and_settle(&mut rig.runner, rig.basalt, untap_idx);
    let seq = plays(rig.runner.state());
    assert_eq!(seq.len(), 2, "the untap beat is the second play");
    assert!(
        seq.iter().all(|(seat, _)| *seat == P0),
        "both plays are P0's"
    );
}

/// T3 — a PARTIAL period (only the mana beat) does NOT offer. The trace holds `[mana]`
/// (non-vacuity), but driving `[mana]` twice re-taps the already-tapped Basalt on the 2nd
/// iteration ⇒ `RecastAbort` ⇒ no offer. The drive+cover IS the period-boundary check. Paired
/// positive = T1 (the full 2-beat period offers).
#[test]
fn mana_engine_partial_period_does_not_offer() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();

    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);

    assert_eq!(
        plays(rig.runner.state()).len(),
        1,
        "reach-guard: the mana beat is traced (non-vacuous)"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "a partial [mana] period never covers (Basalt is tapped) ⇒ no offer"
    );
}

/// T6 — Basalt WITHOUT Power Artifact does NOT offer. The untap costs the full `{3}`, exactly what
/// the mana beat produced, so net mana per period is 0 ⇒ `net_progress_for` fails ⇒ no offer. The
/// trace still holds both beats (non-vacuity), so rejection is the SIGN-CHECK, not a trace
/// failure. Paired positive = T1 (with Power the untap is `{1}` ⇒ net `+2`).
#[test]
fn mana_engine_without_power_does_not_offer() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(false, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert_eq!(
        plays(rig.runner.state()).len(),
        2,
        "reach-guard: both beats traced even without Power (rejection is the sign-check)"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "without Power the untap costs the full {{3}} ⇒ net-0 mana ⇒ no offer"
    );
}

/// T-HET — CR 732.3: each play is traced under the seat that made it, so a window holding P0's and
/// then P1's mana beat holds both, each under its own seat, and no offer stands for it.
#[test]
fn mana_engine_controller_change_resets_accumulator() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    // P1 gets their own Basalt so P1 has a mana ability to activate.
    let p1_basalt = place_on_battlefield(rig.runner.state_mut(), P1, BASALT, db);
    let p0_mana = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let p1_mana = mana_ability_index(rig.runner.state(), p1_basalt).unwrap();

    activate_and_settle(&mut rig.runner, rig.basalt, p0_mana);
    let seq = plays(rig.runner.state());
    assert_eq!(seq.len(), 1, "P0's beat is one play");
    assert_eq!(seq[0].0, P0);

    // Hand priority to P1 and let P1 activate their own mana beat.
    rig.runner.act(GameAction::PassPriority).expect("P0 passes");
    activate_and_settle(&mut rig.runner, p1_basalt, p1_mana);

    let seats: Vec<PlayerId> = plays(rig.runner.state())
        .into_iter()
        .map(|(seat, _)| seat)
        .collect();
    assert_eq!(
        seats,
        vec![P0, P1],
        "each beat is traced under its own seat"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "no offer stands for a two-seat window"
    );
}

/// Create `name` in `player`'s hand after build (mirror of `place_on_battlefield` for Hand).
fn place_in_hand(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    db: &CardDatabase,
) -> ObjectId {
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("card '{name}' not found in fixture"));
    let id = create_object_from_card_face(state, face, player);
    remove_from_zone(state, id, Zone::Library, player);
    add_to_zone(state, id, Zone::Hand, player);
    state.objects.get_mut(&id).unwrap().zone = Zone::Hand;
    id
}

/// Give `player` enough Plains to cast Disenchant ({1}{W}) and a Disenchant in hand.
fn arm_disenchant(rig: &mut Rig, player: PlayerId, db: &CardDatabase) -> (ObjectId, CardId) {
    place_on_battlefield(rig.runner.state_mut(), player, "Plains", db);
    place_on_battlefield(rig.runner.state_mut(), player, "Plains", db);
    let disenchant = place_in_hand(rig.runner.state_mut(), player, "Disenchant", db);
    let card_id = rig.runner.state().objects[&disenchant].card_id;
    (disenchant, card_id)
}

/// T-INT-a ⭐ — INTERRUPTIBILITY, UNDEFUSED: P1 HOLDS a real response (Disenchant) but PASSES ⇒
/// the shortcut is GRANTED (offer surfaces). The untap is ON the stack (CR 602.2a; the mana beat is
/// off-stack per CR 605.3b), so P1 has a
/// genuine response window; passing it lets the loop settle and offer. Matched with T-INT-b: P1's
/// pass-vs-respond is the SOLE delta and FLIPS the outcome.
#[test]
fn mana_engine_interruptibility_undefused_opponent_passes_grants() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let _ = arm_disenchant(&mut rig, P1, db); // P1 could respond, but here PASSES (activate_and_settle auto-passes P1)
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert!(
        matches!(
            &rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "UNDEFUSED (P1 passes): the loop settles and the shortcut is OFFERED, got {:?}",
        rig.runner.state().waiting_for
    );
    assert!(
        rig.runner.state().objects.contains_key(&rig.basalt)
            && rig.runner.state().objects[&rig.basalt].zone == Zone::Battlefield,
        "Basalt survives (P1 did not respond)"
    );
}

/// T-INT-b ⭐ — INTERRUPTIBILITY, DEFUSED: P1 RESPONDS to the untap (on the stack, CR 602.2a; the
/// mana beat is off-stack per CR 605.3b) by
/// casting Disenchant on Basalt. Basalt is destroyed, the untap resolves against nothing, and at
/// the settle the drive's per-step `ObjectId` re-find fails (Basalt gone) ⇒ NO offer beyond the
/// stack. The ONLY delta vs T-INT-a is P1's respond-vs-pass, and the outcome FLIPS (offer → no
/// offer). Non-vacuity: T-INT-a proves the same board OFFERS when P1 passes.
#[test]
fn mana_engine_interruptibility_defused_opponent_responds_no_grant() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let (disenchant, dis_card) = arm_disenchant(&mut rig, P1, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    // P0: mana beat (off-stack), settles to P0 priority.
    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);
    // P0: untap beat (ON the stack).
    rig.runner
        .act(GameAction::ActivateAbility {
            source_id: rig.basalt,
            ability_index: untap_idx,
        })
        .expect("untap activation is legal");
    // P0 passes ⇒ P1 gets priority with the untap on the stack (the real response window).
    rig.runner.act(GameAction::PassPriority).expect("P0 passes");
    // P1 RESPONDS: Disenchant destroys Basalt in response to the untap. The reducer surfaces a
    // `TargetSelection` prompt (the action's `targets` field is not consumed by the reducer), which
    // we answer with Basalt.
    rig.runner
        .act(GameAction::CastSpell {
            object_id: disenchant,
            card_id: dis_card,
            targets: vec![rig.basalt],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 may cast Disenchant in response (instant speed)");
    // Settle everything (Disenchant targets Basalt, resolves, destroys it; then the untap resolves
    // against a destroyed Basalt).
    for _ in 0..60 {
        match rig.runner.state().waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::TargetSelection { .. } => {
                rig.runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(rig.basalt)],
                    })
                    .expect("Disenchant targets Basalt (a legal artifact)");
            }
            WaitingFor::Priority { .. } if rig.runner.state().stack.is_empty() => break,
            _ => {
                if rig.runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    assert!(
        rig.runner.state().objects.get(&rig.basalt).map(|o| o.zone) != Some(Zone::Battlefield),
        "reach-guard: P1's Disenchant destroyed Basalt (the response landed)"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "DEFUSED (P1 responds): Basalt is gone ⇒ the drive's re-find aborts ⇒ NO grant, got {:?}",
        rig.runner.state().waiting_for
    );
}

/// P2 (updated 2026-07-18, user directive): Accept on an unbounded MANA engine MARKS the
/// certificate's `Mana(_)` axes via `mark_unbounded_loop` (reusing the infinite-mana machinery)
/// rather than driving N finite periods. The `refill_infinite_mana` pipeline top-up (engine.rs,
/// after every action) then holds the flagged player's pool at `INFINITE_MANA_PER_TYPE`, so the
/// grant is genuine infinite mana — treated as actually infinite within the phase (CR 500.4
/// empties + finite-resolves it at the boundary) and INDEPENDENT of the declared count. Returns
/// `(colorless_delta, flagged_infinite)`.
fn accept_mana_engine(db: &CardDatabase, n: u32) -> (i64, bool) {
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the offer must fire before acceptance"
    );
    let at_offer = colorless(rig.runner.state(), P0) as i64;
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");
    let flagged = rig
        .runner
        .state()
        .unbounded_resources
        .get(&P0)
        .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_))));
    (colorless(rig.runner.state(), P0) as i64 - at_offer, flagged)
}

/// The ∞-mark is count-INDEPENDENT and yields genuine infinite mana (pool held at
/// `INFINITE_MANA_PER_TYPE`), not the old finite `+2·n`. DISCRIMINATING (revert-probe): without
/// the `mark_unbounded_loop` call, `flagged` is false and the pool is never topped ⇒ both the
/// flag and the ≥90 jump flip to fail.
#[test]
fn mana_engine_accept_marks_infinite_mana_independent_of_count() {
    let Some(db) = shared_card_db() else { return };
    let (delta1, flagged1) = accept_mana_engine(db, 1);
    let (delta5, flagged5) = accept_mana_engine(db, 5);
    assert!(
        flagged1 && flagged5,
        "accept must flag P0 with a Mana axis (∞ mana), not drive N finite periods"
    );
    assert_eq!(
        delta1, delta5,
        "the ∞ mark is count-independent (contrast the old drive-N: +2 vs +10)"
    );
    // At offer the pool held +2 from the one detection period; refill tops all colors to
    // INFINITE_MANA_PER_TYPE (100) ⇒ a large count-independent jump (≥90).
    assert!(
        delta1 >= 90,
        "the pool must be topped to the infinite-mana constant, got {delta1}"
    );
}

/// DESIGN STEP 4 (∞-pile) — MANA-ENGINE PAIRED NEGATIVE: accepting a MANA loop marks the
/// `Mana(_)` axis (reach-guard proving the accept genuinely materialized) but writes NO
/// `unbounded_loop_pile` — a mana engine reproduces no fodder token, so
/// `current_period_fodder` returns `None` and no pile is snapshotted. This proves the
/// fodder gate in `materialize_object_growth_shortcut` discriminates object-growth from mana.
///
/// DISCRIMINATING: the Mana-axis assertion is the positive reach-guard (the accept ran and
/// marked ∞); the empty-pile assertion is the fodder-gate discriminator. Its object-growth
/// counterpart (`combo_infinite_pile.rs`) writes a NON-empty pile from the same accept seam.
#[test]
fn mana_engine_accept_writes_no_pile_but_marks_mana() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the mana-engine offer must fire before acceptance"
    );
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");

    // Positive reach-guard: the accept materialized and marked the Mana axis.
    assert!(
        rig.runner
            .state()
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_)))),
        "the mana-engine accept must mark a Mana(_) axis (reach-guard)"
    );
    // Fodder-gate discriminator: a mana engine reproduces no token ⇒ no ∞ pile.
    assert!(
        rig.runner.state().unbounded_loop_pile.is_empty(),
        "a mana engine has no fodder class ⇒ no unbounded_loop_pile is written"
    );
}

/// T5-analog — `Off` byte-identity (#4603). Under `LoopDetectionMode::Off` the mana engine is
/// NEVER traced (the `samples()` gate) and NEVER offers, while the game plays normally (Basalt
/// untaps, mana is in the pool).
#[test]
fn mana_engine_off_mode_is_byte_identical() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Off, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert!(
        engine::game::play_trace_view(rig.runner.state()).is_none(),
        "Off (#4603): the mana engine must NOT be traced"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "Off never samples ⇒ never offers"
    );
    assert!(
        !rig.runner.state().objects[&rig.basalt].tapped,
        "Off plays normally: the untap resolved and Basalt is untapped"
    );
    assert!(
        colorless(rig.runner.state(), P0) >= 2,
        "Off plays normally: the mana beat produced mana (net +2 after the untap)"
    );
}

/// CR 732.2a: what a save carries of a loop. The window's trace is transient and never crosses a
/// save, while a standing offer's confirmed period does, since its take replays it.
///
/// (a) a save at priority with a traced beat restores with no trace; (b) a save at the offer
/// restores with the same non-empty period; (c) an offer with no period writes no period key.
#[test]
fn loop_action_sequence_conditional_load_migration() {
    use engine::types::game_state::PersistedGameState;

    let Some(db) = shared_card_db() else { return };
    let restore = |state: &GameState| -> GameState {
        let json = serde_json::to_string(state).expect("serialize");
        let reloaded: GameState = serde_json::from_str(&json).expect("deserialize");
        PersistedGameState::Raw(Box::new(reloaded))
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract")
    };
    let period = |state: &GameState| match &state.waiting_for {
        WaitingFor::LoopShortcut { period, .. } => period.clone(),
        other => panic!("expected an offer, got {other:?}"),
    };

    // (a) a priority save drops the transient trace.
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);
    assert_eq!(
        plays(rig.runner.state()).len(),
        1,
        "reach: the beat is traced"
    );
    assert!(
        engine::game::play_trace_view(&restore(rig.runner.state())).is_none(),
        "a save at priority restores with no trace"
    );

    // (b) an offer save keeps the period its take replays.
    activate_and_settle(&mut rig.runner, rig.basalt, untap_idx);
    let at_offer = rig.runner.state().clone();
    let offered = period(&at_offer);
    assert!(
        !offered.is_empty(),
        "reach: the offer carries its confirmed period"
    );
    assert_eq!(
        period(&restore(&at_offer)),
        offered,
        "an offer save restores with the same period"
    );

    // (c) an offer with no period writes no period key.
    let mut bare = at_offer;
    if let WaitingFor::LoopShortcut { period, .. } = &mut bare.waiting_for {
        *period = Default::default();
    }
    let json = serde_json::to_value(&bare).expect("serialize");
    assert!(
        json["waiting_for"]["data"].get("period").is_none(),
        "an empty period is skipped on the wire: {}",
        json["waiting_for"]
    );
}

/// ⭐ COND A — the crux measurement (team-lead PATH-2 (iii)): does a per-cycle action that depletes
/// a FINITE OPPONENT resource become ILLEGAL / error at exhaustion (which would make a
/// break-on-err flip demonstrable, PATH-1), or does it NO-OP (which makes the loop genuinely
/// infinite-advantage, not finite-fuel, ⇒ PATH-2)?
///
/// The ONLY opponent-resource-depleting action that can be DRIVEN (offered) is a NON-targeted one
/// (a targeted one raises a `TargetSelection` the drive answers with `RecastAbort` — it is never
/// offered, so it can't reach materialize). Pyrohemia's `{R}: deals 1 damage to each creature and
/// each player` is exactly that: a repeatable, non-targeted activated ability that depletes a
/// finite opponent resource (the 2/2's toughness/existence). We drive it PAST exhaustion and
/// MEASURE the reducer result.
///
/// RESULT (measured): the post-exhaustion activation is LEGAL and fully RESOLVES (it no-ops on the
/// absent creatures, still hits players) — it does NOT error and does NOT become illegal. So no
/// offered loop's drive aborts at an opponent's resource boundary ⇒ there is NO finite opp-fuel
/// loop for the `if drive.is_err() break` to self-limit ⇒ PATH-2: the break is a DEFENSIVE guard
/// over a provably-empty class (cost-fuel is CR 601.2f/602.2b/118.3 CASE 0; controller-fuel is
/// firewall-vetoed pre-offer, `sign_check_object_counter_decrease_rejects`). Revert-failing for the
/// (iii)(a) claim: if the reducer ever made a non-targeted depletion ILLEGAL at exhaustion, the
/// `res.is_ok()` / `is_creature` reach-guard pair would flip.
#[test]
fn cond_a_nontargeted_opponent_depletion_noops_at_exhaustion_not_abort() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pyro = scenario.add_real_card(P0, "Pyrohemia", Zone::Battlefield, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
    }
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    // Off keeps the offer machinery out of the way — this measures the REDUCER's exhaustion
    // behavior (mode-independent), not an offer.
    runner.state_mut().loop_detection = LoopDetectionMode::Off;

    // Pyrohemia's only non-mana Activated ability is the `{R}: damage-each` ability.
    let dmg_idx =
        untap_ability_index(runner.state(), pyro).expect("Pyrohemia's {R}: damage-each ability");

    // Two activations (2 damage) kill the 2/2 Bears — the finite opponent resource is exhausted.
    for _ in 0..2 {
        tap_untapped_land(&mut runner, P0);
        activate_and_settle(&mut runner, pyro, dmg_idx);
    }
    assert!(
        runner.state().objects.get(&bears).map(|o| o.zone) != Some(Zone::Battlefield),
        "reach-guard: two non-targeted pings killed the 2/2 (opponent resource depleted)"
    );
    let creatures_left = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects[id]
                .card_types
                .core_types
                .contains(&CoreType::Creature)
        })
        .count();
    assert_eq!(
        creatures_left, 0,
        "reach-guard: no creatures remain (fully exhausted)"
    );

    // THE MEASUREMENT: activate the SAME non-targeted depletion action AGAIN, resource exhausted.
    tap_untapped_land(&mut runner, P0);
    let res = runner.act(GameAction::ActivateAbility {
        source_id: pyro,
        ability_index: dmg_idx,
    });
    assert!(
        res.is_ok(),
        "(iii)(a): a non-targeted opponent-depletion activation is LEGAL at exhaustion (no target \
         requirement) — it does NOT become illegal"
    );
    // Drive it to full resolution: it must NOT abort/error (it no-ops on the absent creatures).
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "(iii)(a): the depletion action fully RESOLVED at exhaustion (no-op, no error, no abort) ⇒ \
         no finite opp-fuel loop exists ⇒ the break-on-err is a defensive guard (PATH-2)"
    );
}

/// R6a-2, RECLASSIFIED under option (B): a CHANNEL-LIVENESS row, no longer a discriminator.
/// A mana engine registers NO deferred materialization — `current_period_fodder` finds no
/// fodder, `current_period_counter_growth` / `current_period_life_growth` are empty — so
/// nothing will ever collapse its `Mana(_)` axis at the CR 500.5 boundary. It is genuinely
/// unbounded within the phase (`refill_infinite_mana` holds the pool at
/// `INFINITE_MANA_PER_TYPE`) and MUST keep rendering its `∞` row on the wire. That claim is
/// true, user-visible and revert-detectable (RP-6 below) — it is simply no longer the thing
/// that distinguishes candidate implementations, because option (B) projects EVERY ∞ row.
///
/// WHY IT NO LONGER DISCRIMINATES: reach-guard (2) below asserts `pending_unbounded_
/// materialization` is EMPTY on this rig, so no schedule-keyed hide filter — stash-keyed or
/// count-keyed (`pending_materialization_count` is empty here too, asserted by the sibling
/// `mana_engine_accept_records_no_collapse_bound`) — could fire against this row anyway. The
/// schedule-independence discrimination therefore lives on rigs where a schedule IS present:
/// `loop_shortcut::unregistered_axis_still_renders_its_infinity_badge`.
///
/// REVERT-PROBE (RP-6, RUN): append `views.unbounded_resources.clear();` at the END of
/// `derive_views` (re-kill the row channel unconditionally) ⇒ this row FAILS while the ∞ PILE
/// assertion in `loop_shortcut::scheduled_collapse_still_renders_the_unbounded_badge`
/// stays green — a different channel.
///
/// The `shared_card_db()` guard below is DORMANT in a normal checkout: `integration_cards.json.gz`
/// is tracked, so it only fires in a checkout without the card-data pipeline.
#[test]
fn mana_engine_accept_still_renders_its_infinity_badge() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the mana-engine offer must fire before acceptance"
    );
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");

    let state = rig.runner.state();
    // (1) reach-guard: the accept ran and marked a Mana axis in the store.
    assert!(
        state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_)))),
        "reach-guard: the mana-engine accept marks a Mana(_) ∞ axis"
    );
    // (2) reach-guard: it registered NOTHING — this is the unscheduled-axis shape.
    assert!(
        state.pending_unbounded_materialization.is_empty(),
        "reach-guard: a mana engine registers no deferred materialization, got {:?}",
        state.pending_unbounded_materialization
    );

    // (3) DISCRIMINATOR — on the WIRE, the Mana row still renders for every viewer.
    for viewer in [None, Some(P0), Some(P1)] {
        let rows = engine::game::derived_views::derive_views(state, viewer).unbounded_resources;
        assert!(
            rows.iter().any(|r| matches!(r.axis, ResourceAxis::Mana(_))),
            "FAIL-CLOSED: nothing is scheduled to collapse the mana axis, so its ∞ row must \
             still render (viewer {viewer:?}), got {rows:?}"
        );
    }
}

/// R6a FIX-ROUND-2 (CR 732.2c). MEASURED REGRESSION in the first cut of the collapse bound:
/// `materialize_fixed_shortcut` wrote `pending_materialization_count` UNCONDITIONALLY, before
/// either route ran. A mana engine reaches that function and registers NO deferred
/// materialization (proved by
/// [`mana_engine_accept_still_renders_its_infinity_badge`]'s reach-guard (2)) — so a
/// `Fixed(1)` mana accept left a bound with NOTHING to bound.
///
/// That stray bound is UNCLEARABLE and PERSISTENT: all three clears
/// (`take_pending_materialization`, `clear_collapsed_materializations`,
/// `clear_unbounded_loop`) are keyed on the stash, `clear_unbounded_mana_loop` deliberately
/// does not touch it, and the field is `#[serde(default)]`. It therefore survives the phase,
/// the game and a save/load — and the NEXT accept that really does register a stash gets
/// `min(1, N)` = 1. A table that unanimously agreed to `Fixed(500)` object growth would be
/// offered `max: 1` at the CR 500.5 boundary and have `SubmitPayAmount { 500 }` REJECTED with
/// `"[0, 1]"`. BASE offered `MAX_SHORTCUT_CYCLES` and honored the agreed 500, so this was a
/// regression against BASE, not merely an incomplete fix.
///
/// REVERT-PROBE (RUN, MEASURED): hoist the write back out of the stash-gate in
/// `materialize_fixed_shortcut` ⇒ assertion (3) FAILS with
/// `pending_materialization_count = {PlayerId(0): 1}`. (3) short-circuits the run, so (5) is
/// not reached in the same execution; a SECOND probe run with (3) temporarily downgraded to an
/// `eprintln!` reached it and observed (5) FAIL with `left: Some(1) right: Some(1000)`.
///
/// HONEST SCOPE. Two real accepts — a stash-less one followed by a stash-bearing one — need a
/// board carrying BOTH a mana engine and an object-growth loop; that is not this rig and is
/// not reachable here without building a second combo. So the consequence at (4)/(5) is
/// pinned on the REAL `turns.rs` `max` read instead: the stash is grafted through the same
/// single-authority writer the accept path itself calls
/// (`GameState::register_pending_materialization`), and the CR 500.5 boundary is then reached
/// by passing priority through the real `apply()` reducer.
#[test]
fn mana_engine_accept_records_no_collapse_bound() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();
    drive_one_period(&mut rig, mana_idx, untap_idx);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "precondition: the mana-engine offer must fire before acceptance"
    );
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare shortcut");
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("opponent accepts");

    // (1) REACH-GUARD: the accept really ran `materialize_fixed_shortcut` — it marked the
    // Mana axis, which only the materialize path does. Without this the emptiness at (3)
    // would be the vacuous "nothing happened" pass.
    assert!(
        rig.runner
            .state()
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.iter().any(|a| matches!(a, ResourceAxis::Mana(_)))),
        "reach-guard: the mana-engine accept reaches materialize_fixed_shortcut and marks a \
         Mana(_) ∞ axis"
    );
    // (2) REACH-GUARD: and it registered NOTHING — there is no stash for a bound to bound.
    assert!(
        rig.runner
            .state()
            .pending_unbounded_materialization
            .is_empty(),
        "reach-guard: a mana engine registers no deferred materialization, got {:?}",
        rig.runner.state().pending_unbounded_materialization
    );

    // (3) DISCRIMINATOR: no bound is recorded either. Asserted on the SAME state the two
    // reach-guards above measured as post-accept and stash-less.
    assert!(
        rig.runner.state().pending_materialization_count.is_empty(),
        "CR 732.2c: a stash-less accept must record NO collapse bound (it would be \
         unclearable and would cap the next accept), got {:?}",
        rig.runner.state().pending_materialization_count
    );

    // (4) CONSEQUENCE, on the real `turns.rs` read. Graft a stash through the production
    // single-authority writer (as if a later object-growth accept had registered one) and
    // reach the CR 500.5 boundary through real priority passes.
    rig.runner.state_mut().register_pending_materialization(
        P0,
        engine::types::game_state::PersistentAxisMaterialization::Life {
            player: P0,
            per_cycle_delta: 1,
        },
    );
    let mut prompt_max = None;
    for _ in 0..64 {
        if let WaitingFor::PayAmountChoice {
            resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
            max,
            ..
        } = rig.runner.state().waiting_for
        {
            prompt_max = Some(max);
            break;
        }
        if rig.runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // (5) The grafted stash carries NO accepted bound, so the prompt falls back to the
    // engine-wide safety bound. Under the unconditional write the stray `Fixed(1)` mana
    // bound is still sitting in the map and caps this prompt at 1.
    // 1_000 is `game::engine::MAX_SHORTCUT_CYCLES`, spelled literally because the const is
    // `pub(crate)` and this is an integration test.
    assert_eq!(
        prompt_max,
        Some(1_000),
        "CR 732.2c: a stash-less mana accept must not bound a LATER stash's collapse prompt"
    );
}

/// CR 611.3a + CR 732.2a: Basalt Monolith + Power Artifact beside a Professor
/// Hojo that isn't on the battlefield, in `zone`. Drives one period and
/// returns whether the mana-engine shortcut was offered. Reach guards: Hojo
/// (its Oracle text) carries its once-per-turn modifier in that zone, and the
/// period's untap really was journaled.
fn mana_engine_offered_beside_hojo_in(zone: Zone) -> bool {
    use engine::types::statics::{CastFrequency, StaticMode};
    // Scryfall Oracle text; the shared test database has no Hojo.
    const HOJO: &str = "The first activated ability you activate during your turn that targets a creature you control costs {2} less to activate.\nWhenever one or more creatures you control become the target of an activated ability, draw a card. This ability triggers only once each turn.";
    let db = shared_card_db().expect("the shared card database");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let basalt = scenario.add_real_card(P0, BASALT, Zone::Battlefield, db);
    let hojo = scenario
        .add_creature_to_hand_from_oracle(P0, "Professor Hojo", 2, 2, HOJO)
        .id();
    let mut runner = scenario.build();
    if zone != Zone::Hand {
        remove_from_zone(runner.state_mut(), hojo, Zone::Hand, P0);
        add_to_zone(runner.state_mut(), hojo, zone, P0);
        runner.state_mut().objects.get_mut(&hojo).unwrap().zone = zone;
    }
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let power = place_on_battlefield(runner.state_mut(), P0, POWER, db);
    attach_to(runner.state_mut(), power, basalt);
    assert_eq!(runner.state().objects[&hojo].zone, zone);
    assert!(
        runner.state().objects[&hojo]
            .static_definitions
            .iter_unchecked()
            .any(|def| matches!(
                def.mode,
                StaticMode::ReduceAbilityCost {
                    frequency: Some(CastFrequency::OncePerTurn),
                    ..
                }
            )),
        "reach guard: Hojo carries its first-activation modifier in {zone:?}"
    );
    let mut rig = Rig { runner, basalt };
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).expect("mana ability");
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).expect("untap ability");

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert!(
        rig.runner
            .state()
            .abilities_activated_this_turn_by_player
            .get(&P0)
            .is_some_and(|rows| rows.iter().any(|row| row.source == basalt)),
        "reach guard: the untap was journaled"
    );
    matches!(
        rig.runner.state().waiting_for,
        WaitingFor::LoopShortcut { .. }
    )
}

/// End-to-end guard for a HIDDEN library Hojo. The P7 cover compares frames
/// built from `proposer_hidden_view`, which blanks the library card's statics,
/// so its comparator never sees this Hojo (probed: the reader is live on the
/// board and absent from every compared frame). This does not show the path
/// ignores the journal; it pins that a hidden Hojo never withholds the offer.
#[test]
fn a_hidden_library_hojo_does_not_block_the_mana_engine_shortcut() {
    if shared_card_db().is_none() {
        return;
    }
    assert!(mana_engine_offered_beside_hojo_in(Zone::Library));
}

/// CR 611.3a + CR 732.2a: a REVEALED Hojo (in the graveyard, which the cover's
/// frames keep) is seen by the comparator, which does gate on the journal.
/// Basalt's untap targets no creature, so no row qualifies for Hojo, the
/// per-definition projection keeps none, and the shortcut is still offered.
/// Keeping every row while any dormant reader exists (45fdbed7d's rule)
/// withholds it.
#[test]
fn a_revealed_hojo_does_not_block_the_mana_engine_shortcut() {
    if shared_card_db().is_none() {
        return;
    }
    assert!(mana_engine_offered_beside_hojo_in(Zone::Graveyard));
}

/// A source's latest mana producer is its most recent activation, before and after a reload.
#[test]
fn latest_producer_names_the_second_basalt_activation() {
    let db = shared_card_db().expect("the integration card fixture loads");
    let mut rig = setup(false, LoopDetectionMode::Off, db);
    let mana = mana_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt publishes its mana ability");
    let untap = untap_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt publishes its untap ability");
    activate_and_settle(&mut rig.runner, rig.basalt, mana);
    activate_and_settle(&mut rig.runner, rig.basalt, untap);
    activate_and_settle(&mut rig.runner, rig.basalt, mana);

    let live = rig.runner.state();
    let reloaded: GameState =
        serde_json::from_str(&serde_json::to_string(live).expect("the state serializes"))
            .expect("the state reloads");
    for (label, state) in [("live", live), ("reloaded", &reloaded)] {
        let journal = &state.resolved_rules_journal;
        let producers: Vec<_> = journal
            .produced_mana()
            .iter()
            .filter(|record| record.unit.source_id == rig.basalt)
            .map(|record| record.producer)
            .collect();
        assert!(
            producers
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                >= 2,
            "reach ({label}): two Basalt activations produced mana, got {producers:?}"
        );
        let latest = journal.latest_mana_producer_for_source(rig.basalt);
        assert_eq!(
            latest,
            producers.last().copied(),
            "{label}: the latest producer is the last record's"
        );
        assert_ne!(
            latest,
            producers.first().copied(),
            "{label}: the latest producer is not the first activation's"
        );
    }
}

/// After `k` Basalt periods, the pool entries walked by a {1} payment pinned to the pool's last
/// unit, and after Forest's {G} by an unpinned {1}{G} payment; each window is one direct payment
/// call, so no action-boundary serialization is counted.
fn payment_walks_after_periods(k: usize, db: &CardDatabase) -> (u64, u64) {
    use engine::game::mana_payment::pay_cost_with_demand_and_choices;
    use engine::game::perf_counters::take_cost_snapshot;
    use engine::types::mana::{LifePaymentColors, ManaCost, ManaCostShard};

    let mut rig = setup(true, LoopDetectionMode::Off, db);
    let forest = place_on_battlefield(rig.runner.state_mut(), P0, "Forest", db);
    let bears = place_in_hand(rig.runner.state_mut(), P0, "Grizzly Bears", db);
    let mana_idx =
        mana_ability_index(rig.runner.state(), rig.basalt).expect("Basalt taps for mana");
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).expect("Basalt untaps");
    for _ in 0..k {
        drive_one_period(&mut rig, mana_idx, untap_idx);
    }

    let mut pool = rig.runner.state().players[0].mana_pool.clone();
    let last = pool.units().last().expect("Basalt's mana floats").pip_id;
    let before = take_cost_snapshot();
    let (paid, _) = pay_cost_with_demand_and_choices(
        &mut pool,
        &ManaCost::generic(1),
        None,
        None,
        None,
        None,
        LifePaymentColors::EMPTY,
        &[last],
    )
    .expect("the pinned {1} is payable");
    let pinned_walk = take_cost_snapshot().since(before).pool_entries_walked;
    assert_eq!(
        paid.iter().map(|unit| unit.pip_id).collect::<Vec<_>>(),
        [last],
        "the pinned unit pays"
    );

    let forest_idx = mana_ability_index(rig.runner.state(), forest).expect("Forest taps for mana");
    activate_and_settle(&mut rig.runner, forest, forest_idx);
    let pool = &rig.runner.state().players[0].mana_pool;
    let colorless_before = pool.count_color(ManaType::Colorless);
    assert_eq!(
        (pool.count_color(ManaType::Green), colorless_before),
        (1, 2 * k),
        "reach: Forest's {{G}} floats after Basalt's colorless"
    );
    assert_eq!(
        pool.units().last().map(|unit| unit.color),
        Some(ManaType::Green),
        "reach: the {{G}} sits behind every {{C}} in pool order"
    );

    let mut unpinned_pool = pool.clone();
    let before = take_cost_snapshot();
    let (paid, _) = pay_cost_with_demand_and_choices(
        &mut unpinned_pool,
        &ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        },
        None,
        None,
        None,
        None,
        LifePaymentColors::EMPTY,
        &[],
    )
    .expect("the unpinned {1}{G} is payable");
    let unpinned_walk = take_cost_snapshot().since(before).pool_entries_walked;
    let mut paid_colors: Vec<ManaType> = paid.iter().map(|unit| unit.color).collect();
    paid_colors.sort_by_key(|color| *color == ManaType::Colorless);
    assert_eq!(
        paid_colors,
        [ManaType::Green, ManaType::Colorless],
        "the {{G}} and one {{C}} pay the unpinned {{1}}{{G}}"
    );

    let card_id = rig.runner.state().objects[&bears].card_id;
    rig.runner
        .act(GameAction::CastSpell {
            object_id: bears,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Grizzly Bears is castable from the pool");
    for _ in 0..60 {
        if rig.runner.state().stack.is_empty() {
            break;
        }
        rig.runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
    let state = rig.runner.state();
    assert!(state.battlefield.contains(&bears), "Grizzly Bears resolves");
    assert_eq!(
        (
            state.players[0].mana_pool.count_color(ManaType::Green),
            state.players[0].mana_pool.count_color(ManaType::Colorless),
        ),
        (0, colorless_before - 1),
        "the {{G}} and one {{C}} pay for Grizzly Bears"
    );
    (pinned_walk, unpinned_walk)
}

#[test]
fn an_ineligible_shape_ahead_costs_a_payment_no_walk_per_unit() {
    let db = shared_card_db().expect("the integration card fixture loads");
    let (small_pinned, small_unpinned) = payment_walks_after_periods(3, db);
    let (large_pinned, large_unpinned) = payment_walks_after_periods(40, db);
    assert!(
        small_pinned > 0 && small_unpinned > 0,
        "reach: both payments walk the pool"
    );
    assert_eq!(
        (large_unpinned, large_pinned),
        (small_unpinned, small_pinned),
        "(unpinned, pinned) payment walks grow with the colorless ahead of the {{G}}"
    );
}

/// The window's plays, each with the seat that made it.
fn plays(state: &GameState) -> Vec<(PlayerId, engine::game::PlayLocus)> {
    engine::game::play_trace_view(state).map_or_else(Vec::new, |view| {
        view.entries
            .into_iter()
            .filter_map(|entry| match entry.kind {
                engine::game::EntryKind::Play { locus, .. } => Some((entry.seat, locus)),
                _ => None,
            })
            .collect()
    })
}
