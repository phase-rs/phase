// engine-citation-gate: symbol anchors only
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
use engine::game::derived_views::{CollapseCertainty, FamilyCollapseState, UnboundedFamily};
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
            per_cycle: None,
        },
        schema: ShortcutDecisionSchema::default(),
        declaration: None,
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
/// `loop_shortcut::unregistered_axis_still_renders_its_infinity_badge` and
/// `scheduled_drive_still_renders_the_already_spendable_mana_badge` below, whose ONE stash names
/// both a `Mana(_)` and a deferred `Life(P0)`.
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

/// R6a FIX-ROUND-3 (CR 500.5), now the MULTI-AXIS row: the
/// `PersistentAxisMaterialization::DriveSequence` arm of `scheduled_collapse_axes` returns the
/// loop's WHOLE axis set (`collapsed_axes` == `proposal.unbounded`), so ONE stash here names TWO
/// axes — an already-materialized `Mana(Colorless)` and a deferred `Life(P0)`. Both keep their ∞
/// row while the collapse is merely scheduled, and they get there for DIFFERENT reasons, which is
/// what makes this the strongest rig in the file for the projection's schedule-independence.
///
/// The `Life(P0)` axis is DEFERRED: no life has been gained, and none will be until the CR 500.5
/// boundary applies the growth. The growth is in flight along CR 732.2c's advance to the
/// proposal's ending point (`types::game_state`'s `scheduled_collapse_axes` doc). For the DISPLAY
/// what matters is only
/// that the mark and its enablers are still live through the window, so the ∞ renders current
/// engine state rather than a stale mark.
///
/// The `Mana(Colorless)` axis is ALREADY MATERIALIZED at accept:
/// `mana_payment::refill_infinite_mana` re-tops the flagged pool to `INFINITE_MANA_PER_TYPE` off
/// `unbounded_resources` (the STORE, which the projection deliberately never filters) after every
/// action, so throughout the accept→boundary window the player can really spend an unbounded
/// pool. CR 500.5 is what ends that badge: the step/phase end drains the pool and
/// `turns::drain_pending_phase_transition_progress` clears the axis (covered by
/// `combo_infinite_pile`'s E4 mana axis-clear row, not re-proved here) — NOT a materialization.
///
/// HONEST SCOPE. Everything except one write is real: real cards through the real parser, a real
/// two-beat Basalt+Power period, a real `DeclareShortcut`/`RespondToShortcut` accept that marks
/// `Mana(Colorless)` and holds the pool at the cap. What is NOT reachable on this rig — and the
/// R6a reviewer could not reach it on any production board either — is a single loop spanning
/// BOTH a `Mana(_)` axis and an OBSERVED counter/life axis, which is what routes an accept into
/// the `DriveSequence` arm (`game::engine::materialize_object_growth_shortcut`). So the stash is
/// grafted through the same single-authority writers the accept path itself calls
/// (`GameState::mark_unbounded_loop` for the second axis, `register_pending_materialization` for
/// the item), with `collapsed_axes` set to exactly the store's mark set — byte-for-byte the
/// `proposal.unbounded.clone()` that production writes. Same graft technique as
/// `combo_infinite_pile::real_4p_observed_drive_sequence_replays_captured_period_n_times`.
///
/// REVERT-PROBE (RP-1d, RUN): restore `if collapse_scheduled(controller, &axis) { continue; }` in
/// `derive_views`' resource-row loop ⇒ (6) FAILS — `Life(P0)` is in the `DriveSequence`'s
/// `collapsed_axes`, so the restored guard hides its row. (5) is the paired control that keeps
/// the probe honest: BASE also carried an `axes.retain(|a| !matches!(a, ResourceAxis::Mana(_)))`
/// on the hide-set, so the mana row survived that guard and (5) stayed green — a blanket "hide
/// every scheduled axis" and a blanket "hide nothing" are distinguished by this pair.
#[test]
fn scheduled_drive_still_renders_the_already_spendable_mana_badge() {
    use engine::types::game_state::PersistentAxisMaterialization;

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
    // The real captured two-beat period, read AT THE OFFER — `materialize_fixed_shortcut`
    // clears `last_loop_action_sequence` on its way out, and production reads it at the same
    // pre-clear point (`game::engine`'s capture-before-clear).
    let sequence = rig.runner.state().last_loop_action_sequence.clone();
    assert!(
        sequence.len() == 2,
        "reach-guard: the offer carries the real two-beat Basalt+Power period the DriveSequence \
         would replay, got {} beats",
        sequence.len()
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

    // (1) REACH-GUARD: the real accept marked the Mana axis in the STORE. Capture the exact
    // axes — the graft below reuses them as `collapsed_axes`, mirroring production.
    let mana_axes: Vec<ResourceAxis> = rig
        .runner
        .state()
        .unbounded_resources
        .get(&P0)
        .expect("reach-guard: the mana-engine accept marks P0's ∞ axes")
        .iter()
        .copied()
        .filter(|a| matches!(a, ResourceAxis::Mana(_)))
        .collect();
    assert!(
        mana_axes.contains(&ResourceAxis::Mana(ManaType::Colorless)),
        "reach-guard: Basalt+Power nets colorless, so the accept marks Mana(Colorless), got \
         {mana_axes:?}"
    );

    // (2) REACH-GUARD: that axis is ALREADY SPENDABLE — the pipeline refill holds the pool at
    // the infinite-mana cap right now. This is what makes hiding the badge a lie rather than a
    // harmless early cleanup. (`INFINITE_MANA_PER_TYPE` is `pub(crate)`; 100 spelled literally,
    // matching `real_4p_basalt_power_artifact_refills_colorless_only`.)
    let pool = colorless(rig.runner.state(), P0);
    assert!(
        pool >= 90,
        "reach-guard: refill_infinite_mana holds P0's colorless pool at the cap (~100) during \
         the accept→CR-500.5 window, got {pool}"
    );

    // (3) GRAFT (see HONEST SCOPE): a second, genuinely DEFERRED axis plus the one
    // `DriveSequence` an observed-growth accept would register over the real captured period.
    // Both writes go through the production single-authority writers.
    rig.runner
        .state_mut()
        .mark_unbounded_loop(P0, &[ResourceAxis::Life(P0)]);
    let collapsed_axes: Vec<ResourceAxis> = rig
        .runner
        .state()
        .unbounded_resources
        .get(&P0)
        .expect("both axes marked")
        .iter()
        .copied()
        .collect();
    rig.runner.state_mut().register_pending_materialization(
        P0,
        PersistentAxisMaterialization::DriveSequence {
            sequence,
            collapsed_axes: collapsed_axes.clone(),
        },
    );

    // (4) REACH-GUARD ON THE SEAM: the collapse authority really does name BOTH axes, so a
    // schedule-keyed hide filter would have suppressed both rows below. Without this, (5) and (6)
    // could pass because the stash never reached the `DriveSequence` arm at all.
    let state = rig.runner.state();
    let scheduled = state.scheduled_collapse_axes(
        state
            .pending_unbounded_materialization
            .get(&P0)
            .expect("the grafted stash is present"),
    );
    assert!(
        scheduled.contains(&ResourceAxis::Mana(ManaType::Colorless))
            && scheduled.contains(&ResourceAxis::Life(P0)),
        "reach-guard: scheduled_collapse_axes returns BOTH axes unfiltered (the boundary must \
         still clear the mana one), got {scheduled:?}"
    );

    for viewer in [None, Some(P0), Some(P1)] {
        let views = engine::game::derived_views::derive_views(state, viewer);
        let rows = views.unbounded_resources;
        let families = views.unbounded_families;
        let axes: Vec<ResourceAxis> = rows.iter().map(|r| r.axis).collect();
        // (5) the already-materialized mana axis keeps its ∞ row on the WIRE.
        assert!(
            axes.contains(&ResourceAxis::Mana(ManaType::Colorless)),
            "CR 500.5: mana is already in the pool and still being refilled, so a \
             merely-scheduled drive must NOT hide its ∞ row (viewer {viewer:?}), got {axes:?}"
        );
        // (6) DISCRIMINATOR — the DEFERRED axis of the SAME `DriveSequence` also keeps its ∞ row.
        // Nothing has been applied yet, so both rows project even though the collapse authority
        // names both axes at (4).
        assert!(
            axes.contains(&ResourceAxis::Life(P0)),
            "the deferred Life axis of the same scheduled drive still projects its ∞ \
             row while the collapse is merely scheduled (viewer {viewer:?}), got {axes:?}"
        );

        // (8) R4 — the documented `Mana(_)` scope limit is FALSIFIABLE, not dead code: (4) above
        // proves the collapse authority names BOTH axes on this exact stash, so the mana axis
        // going unflagged below can only come from the projection's own guard. Assertion (5)
        // already pins that the mana ROW still exists, so the scope limit governing the AFFORDANCE
        // rather than row EXISTENCE is covered there; a duplicate pin here would be subsumed by it
        // and by the same `derive_views` output, so this reuses `rows` instead of recomputing.

        // (9) R4/agree — the FAMILY COLLAPSE STATE obeys the `Mana(_)` scope limit.
        //
        // MEASURED DEFECT this pins: the limit once lived in a separate tag channel's loop and not
        // in the row loop, so on this exact state the mana row shipped `scheduled: true`. The HUD
        // folded that flag into the "mana" family and rendered `∞→N` with a "collapse pending; a
        // finite amount will be chosen" tooltip — beside a pool `refill_infinite_mana` is still
        // topping up, and beside `ManaPoolSummary`'s plain `∞` for the same pool in the same
        // frame. The whole suite was green over it: every other schedule assertion in the repo
        // sits on a non-mana axis, so nothing chose between that behaviour and its opposite. The
        // tag channel is gone and so is the row flag; this assertion is what keeps the scope limit
        // honest on the channel that replaced them.
        //
        // TWO-SIDED on purpose. The `life` half is the matched positive, from the SAME stash and
        // the SAME `derive_views` call: without it, `Unscheduled` everywhere satisfies the mana
        // half, and this row would pass against a channel that can never report a schedule.
        let state_of = |want: UnboundedFamily| {
            families
                .iter()
                .find(|f| f.player == P0 && f.family == want)
                .unwrap_or_else(|| panic!("R4/agree reach: no {want:?} family (viewer {viewer:?})"))
                .state
        };
        assert_eq!(
            state_of(UnboundedFamily::Mana),
            FamilyCollapseState::Unscheduled,
            "R4/agree: the mana family must not report a schedule — the accepted count bounds \
             nothing the player can spend (viewer {viewer:?})"
        );
        assert_eq!(
            state_of(UnboundedFamily::Life),
            FamilyCollapseState::Scheduled {
                certainty: CollapseCertainty::Committed,
                prompted: Some(P0),
            },
            "R4/agree positive: the deferred life family of the SAME stash IS scheduled, so the \
             mana assertion above is discriminating rather than vacuous. It is COMMITTED because \
             a `DriveSequence` replays real cycles and has no non-push exit (viewer {viewer:?})"
        );
    }

    // (7) THE STORE IS UNTOUCHED — the projection read, it did not mutate. The boundary
    // clear still reads both axes from here.
    assert_eq!(
        state
            .unbounded_resources
            .get(&P0)
            .map(|a| a.iter().copied().collect::<Vec<_>>()),
        Some(collapsed_axes),
        "the ∞ store survives the projection (engine-state enabler lockstep)"
    );
}
