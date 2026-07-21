//! P7 v3 (CR 732.2a): capture + drive a MULTI-ACTION mana-engine loop.
//!
//! Real-card acceptance: **Basalt Monolith + Power Artifact** — the canonical 2-card infinite-mana
//! combo. Basalt's `{T}: Add {C}{C}{C}` (an off-stack mana ability, CR 605.3b) then its separate
//! `{3}: Untap this artifact` (on-stack, reduced to `{1}` by Power Artifact, CR 118.9) form ONE
//! loop period of TWO activations whose net progress is `+2 {C}` per cycle while the board returns
//! to equality. This is the class OPTION 2 (multi-action) enables — a single `LoopAction` cannot
//! represent it.
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
use engine::types::game_state::{
    CastPaymentMode, GameState, LoopAction, LoopActionContext, LoopDetectionMode, WaitingFor,
};
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
fn mana_ability_index(state: &GameState, source: ObjectId) -> Option<usize> {
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
fn untap_ability_index(state: &GameState, source: ObjectId) -> Option<usize> {
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

struct Rig {
    runner: GameRunner,
    basalt: ObjectId,
}

/// Build the 2-player rig: Basalt Monolith on P0's battlefield, optionally with Power Artifact
/// attached (the cost-reduction that makes the untap net-positive). `mode` selects the detector.
fn setup(with_power: bool, mode: LoopDetectionMode, db: &CardDatabase) -> Rig {
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
fn drive_one_period(rig: &mut Rig, mana_idx: usize, untap_idx: usize) {
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

    // Positive reach-guard: BOTH beats accumulated (armed, non-vacuous) before the offer.
    assert_eq!(
        rig.runner.state().last_loop_action_sequence.len(),
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

/// T2 — the sequence ACCUMULATES both beats in order. After the mana beat `len==1`; after the
/// untap beat `len==2`, both `Activate`, same controller. Revert-failing: removing the else-arm
/// APPEND branch makes the untap CLEAR (pre-P7 behavior) ⇒ `len` never reaches 2 ⇒ no offer.
#[test]
fn mana_engine_accumulates_both_beats() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    activate_and_settle(&mut rig.runner, rig.basalt, mana_idx);
    assert_eq!(
        rig.runner.state().last_loop_action_sequence.len(),
        1,
        "the off-stack mana beat SEEDS a 1-step period"
    );
    activate_and_settle(&mut rig.runner, rig.basalt, untap_idx);
    let seq = rig.runner.state().last_loop_action_sequence.clone();
    assert_eq!(seq.len(), 2, "the untap beat APPENDS ⇒ a 2-step period");
    assert!(
        seq.iter()
            .all(|c| matches!(c.action, LoopAction::Activate { .. }) && c.controller == P0),
        "both steps are P0 Activate steps (homogeneous controller)"
    );
}

/// T3 — a PARTIAL period (only the mana beat) does NOT offer. The accumulator arms `[mana]`
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
        rig.runner.state().last_loop_action_sequence.len(),
        1,
        "reach-guard: the mana beat armed a 1-step accumulator (non-vacuous)"
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
/// accumulator still arms both beats (non-vacuity), so rejection is the SIGN-CHECK, not a capture
/// failure. Paired positive = T1 (with Power the untap is `{1}` ⇒ net `+2`).
#[test]
fn mana_engine_without_power_does_not_offer() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(false, LoopDetectionMode::Interactive, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert_eq!(
        rig.runner.state().last_loop_action_sequence.len(),
        2,
        "reach-guard: both beats armed even without Power (rejection is the sign-check, not capture)"
    );
    assert!(
        !matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "without Power the untap costs the full {{3}} ⇒ net-0 mana ⇒ no offer"
    );
}

/// T-HET — capture-level identity protection: a CONTROLLER CHANGE resets the accumulator to a
/// fresh single-controller period, so a heterogeneous (multi-controller) sequence NEVER forms.
/// P0 seeds `[mana(P0)]`; when P1 activates their OWN Basalt's mana beat the accumulator resets to
/// `[mana(P1)]` (not `[mana(P0), mana(P1)]`). Revert-failing: dropping the controller-change reset
/// in `accumulate_loop_action_step` grows a mixed `[P0, P1]` sequence. (The drive's per-step
/// `src.controller != step.controller` re-find in `drive_loop_action_iteration` is the runtime
/// backstop, byte-unchanged from the recast path and covered by the recast tests.)
#[test]
fn mana_engine_controller_change_resets_accumulator() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Interactive, db);
    // P1 gets their own Basalt so P1 has a mana ability to activate.
    let p1_basalt = place_on_battlefield(rig.runner.state_mut(), P1, BASALT, db);
    let p0_mana = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let p1_mana = mana_ability_index(rig.runner.state(), p1_basalt).unwrap();

    activate_and_settle(&mut rig.runner, rig.basalt, p0_mana);
    let seq = rig.runner.state().last_loop_action_sequence.clone();
    assert_eq!(seq.len(), 1, "P0 seeds a 1-step period");
    assert_eq!(seq[0].controller, P0);

    // Hand priority to P1 and let P1 activate their own mana beat.
    rig.runner.act(GameAction::PassPriority).expect("P0 passes");
    activate_and_settle(&mut rig.runner, p1_basalt, p1_mana);

    let seq = rig.runner.state().last_loop_action_sequence.clone();
    assert_eq!(
        seq.len(),
        1,
        "the controller change RESET the accumulator (no [P0, P1] heterogeneous sequence)"
    );
    assert_eq!(
        seq[0].controller, P1,
        "the reset re-seeded with P1's beat only"
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

/// T5-analog — `Off` byte-identity (#4603). Under `LoopDetectionMode::Off` the mana engine NEVER
/// arms the sequence (the `samples()` gate) and NEVER offers, while the game plays normally (Basalt
/// untaps, mana is in the pool). Revert-failing: dropping the `samples()` gate on the mana-arm /
/// else-arm capture writes the sequence under `Off`.
#[test]
fn mana_engine_off_mode_is_byte_identical() {
    let Some(db) = shared_card_db() else { return };
    let mut rig = setup(true, LoopDetectionMode::Off, db);
    let mana_idx = mana_ability_index(rig.runner.state(), rig.basalt).unwrap();
    let untap_idx = untap_ability_index(rig.runner.state(), rig.basalt).unwrap();

    drive_one_period(&mut rig, mana_idx, untap_idx);

    assert!(
        rig.runner.state().last_loop_action_sequence.is_empty(),
        "Off (#4603): the mana engine must NOT arm the sequence"
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

/// FIX-3 (CR 732.2a, CONDITIONAL load migration): `last_loop_action_sequence` deserializes NORMALLY
/// (its `pins` round-trip — B2 restored), but the PRODUCTION restore hook
/// `PersistedGameState::into_game_state` → `GameState::migrate_transient_loop_sequence` DROPS it on
/// load UNLESS the save sits in an object-growth shortcut proposal/response window
/// (`WaitingFor::LoopShortcut` / `RespondToShortcut`), whose pending accept→materialize resolution
/// re-derives the ∞ pile from the sequence. This REPLACES the Design-A blanket `#[serde(skip)]`
/// (always-drop) contract, which regressed the predecessor `combo_infinite_pile` offer-saves by
/// starving accept→materialize of the pile.
///
/// DISCRIMINATING — the ONLY guard on the load migration + the B2 pins round-trip (the field is
/// EXCLUDED from `impl PartialEq for GameState`). Parts (a) and (b) round-trip the SAME populated,
/// PINNED sequence through the real production hook and differ ONLY in `waiting_for`, so the
/// outcome FLIPS: a hook that ignored `waiting_for` (Design A, always-drop) fails (b); a hook that
/// never dropped fails (a). Part (b) additionally asserts the pin survived (Design A dropped pins).
#[test]
fn loop_action_sequence_conditional_load_migration() {
    use engine::analysis::decision_template::{
        DecisionSlot, PinnedDecision, ShortcutDecisionSchema,
    };
    use engine::analysis::loop_check::LoopCertificate;
    use engine::analysis::resource::BoardDelta;
    use engine::types::game_state::{PersistedGameState, YieldTarget};
    use engine::types::mana::ManaColor;

    let mana_color_pin = || PinnedDecision::ManaColor {
        slot: DecisionSlot {
            source: YieldTarget::ThisObject {
                source_id: ObjectId(7),
                incarnation: None,
                trigger_description: None,
            },
            index: 1,
        },
        color: ManaColor::Blue,
    };
    let pinned_step = || LoopActionContext {
        card_id: CardId(4242),
        controller: P0,
        action: LoopAction::Activate {
            source_id: ObjectId(7),
            ability_index: 1,
        },
        convoke: None,
        pins: vec![mana_color_pin()],
    };

    // (a) captured at empty-stack `Priority` (NOT a shortcut window) → the production hook DROPS the
    //     sequence. It deserializes NON-EMPTY first, proving the drop is the migration hook, not the
    //     `#[serde(skip)]` derive (which Design A used and which regressed the predecessor tests).
    let mut at_priority = GameState::new_two_player(1);
    at_priority.waiting_for = WaitingFor::Priority { player: P0 };
    at_priority.last_loop_action_sequence = vec![pinned_step(), pinned_step()];
    let raw = serde_json::to_string(&at_priority).expect("serialize");
    assert!(
        raw.contains("last_loop_action_sequence"),
        "a populated sequence IS serialized (skip_serializing_if only skips the EMPTY case)"
    );
    let deserialized: GameState = serde_json::from_str(&raw).expect("deserialize");
    assert_eq!(
        deserialized.last_loop_action_sequence.len(),
        2,
        "the sequence deserializes NORMALLY (len 2) — the drop is the load hook, not the derive"
    );
    let restored = PersistedGameState::Raw(Box::new(at_priority)).into_game_state();
    assert!(
        restored.last_loop_action_sequence.is_empty(),
        "FIX-3: a Priority-captured save DROPS the transient sequence on load"
    );

    // (b) captured at a `LoopShortcut` offer window → the production hook KEEPS the sequence, and the
    //     recorded pin round-trips (B2). SAME sequence as (a); ONLY `waiting_for` differs ⇒ the
    //     keep/drop outcome flips, isolating the discriminator to `waiting_for`.
    let mut at_offer = GameState::new_two_player(1);
    at_offer.waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: None,
        certificate: LoopCertificate {
            unbounded: vec![ResourceAxis::TokensCreated],
            win_kind: WinKind::Advantage,
            mandatory: false,
            residual_board_delta: BoardDelta::default(),
        },
        schema: ShortcutDecisionSchema::default(),
    };
    at_offer.last_loop_action_sequence = vec![pinned_step()];
    let json = serde_json::to_string(&at_offer).expect("serialize offer save");
    let reloaded: GameState = serde_json::from_str(&json).expect("deserialize offer save");
    let restored_offer = PersistedGameState::Raw(Box::new(reloaded)).into_game_state();
    assert_eq!(
        restored_offer.last_loop_action_sequence.len(),
        1,
        "FIX-3: a LoopShortcut-captured offer-save KEEPS the sequence on load (accept→materialize needs it)"
    );
    assert_eq!(
        restored_offer.last_loop_action_sequence[0].pins,
        vec![mana_color_pin()],
        "B2: the recorded pins round-trip for a kept offer-save (Design A's #[serde(skip)] dropped them)"
    );

    // (c) an empty sequence is skipped on the wire and a missing field defaults to empty (UNCHANGED).
    let empty = GameState::new_two_player(1);
    let json = serde_json::to_string(&empty).expect("serialize empty");
    assert!(
        !json.contains("last_loop_action_sequence"),
        "an empty sequence is skipped on the wire (skip_serializing_if)"
    );
    let back: GameState = serde_json::from_str(&json).expect("deserialize missing field");
    assert!(
        back.last_loop_action_sequence.is_empty(),
        "a missing field defaults to an empty Vec"
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
