//! REGRESSION PIN — a singular battlefield recall binds to its self-move
//! antecedent. Jill, Shiva's Dominant // Shiva, Warden of Ice (FIN),
//! Chapter III "Cold Snap": `Tap all lands your opponents control. Exile
//! Shiva, then return it to the battlefield (front face up).`
//!
//! Before the parser fix, the return leg's cross-clause sentinel
//! `TargetFilter::TrackedSet(TrackedSetId(0))` bound to the resolution
//! chain's tracked set, which the tap leg had polluted with the three tapped
//! lands. That surfaced a spurious "Put onto Battlefield"
//! `WaitingFor::EffectZoneChoice` prompt whose candidate set contained Jill
//! AND every land the tap leg just tapped — four candidates for a count-1
//! move that should never have asked.
//!
//! The parser now binds the singular anaphor "return it" to the object the
//! preceding clause moved (CR 608.2c English anaphora + CR 400.7j public-zone
//! findability) as `TargetFilter::SelfRef`, which rides the existing CR 400.7j
//! relatch machinery: the recall auto-resolves the just-exiled Saga with no
//! prompt, and the Saga re-enters front face up (CR 712.8a).
//!
//! Why the chain set is polluted (and why the return leg must not read it):
//!
//! 1. The tap leg `Effect::SetTapState` publishes every object it taps into
//!    the resolution chain's tracked set (the `SetTapState` arm of the
//!    tracked-set publication dispatch — the machinery behind riders like
//!    "tap all creatures your opponents control. Those creatures...").
//! 2. The exile leg `ChangeZone { SelfRef -> Exile }` publishes Shiva into
//!    that SAME chain-scoped set (the generic `ChangeZone` arm).
//! 3. The return leg therefore must not read the chain set: the singular
//!    pronoun binds `SelfRef` at parse time and the polluted set stays inert
//!    for this class.
//!
//! Contrast case baked in below: Jill's own `{3}{U}{U}, {T}` activation uses
//! the identical exile-then-return shape, and its chain set contains ONLY
//! Jill (nothing else publishes into it) — it transforms silently and must
//! stay silent.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AbilityKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::{PlayerId, WaitingFor};

/// Verbatim Oracle text of the FIN front face (MTGJSON, nonfoil printing).
const JILL_FRONT_ORACLE: &str = "When Jill enters, return up to one other target nonland permanent to its owner's hand.\n\
{3}{U}{U}, {T}: Exile Jill, then return it to the battlefield transformed under its owner's control. Activate only as a sorcery.";

/// Verbatim Oracle text of the FIN back face (MTGJSON, nonfoil printing).
const SHIVA_BACK_ORACLE: &str = "(As this Saga enters and after your draw step, add a lore counter.)\n\
I, II — Mesmerize — Target creature can't be blocked this turn.\n\
III — Cold Snap — Tap all lands your opponents control. Exile Shiva, then return it to the battlefield (front face up).";

/// The printed back face: `Shiva, Warden of Ice` 4/5, Enchantment Creature —
/// Saga Elemental, {U}{R} identity. The chapter triggers and the CR 714.3a
/// ETB lore-counter replacement come from parsing the verbatim back-face
/// Oracle text with the production parser.
fn shiva_back_face() -> BackFaceData {
    let parsed = parse_oracle_text(
        SHIVA_BACK_ORACLE,
        "Shiva, Warden of Ice",
        &[],
        &["Enchantment".to_string(), "Creature".to_string()],
        &["Saga".to_string(), "Elemental".to_string()],
    );
    let mut card_types = CardType::default();
    card_types
        .core_types
        .extend([CoreType::Enchantment, CoreType::Creature]);
    card_types
        .subtypes
        .extend(["Saga".to_string(), "Elemental".to_string()]);
    BackFaceData {
        name: "Shiva, Warden of Ice".to_string(),
        power: Some(4),
        toughness: Some(5),
        card_types,
        trigger_definitions: parsed.triggers.into(),
        replacement_definitions: parsed.replacements.into(),
        color: vec![ManaColor::Blue, ManaColor::Red],
        ..Default::default()
    }
}

/// Fund an exactly-sized pool from an unspecified source, matching the
/// pool-funded-cast convention: the ManaPayment window finalizes via
/// PassPriority against whatever the pool holds.
fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

/// Park the game at the end of P0's turn so the next `advance_to_phase` walks
/// through P1's turn and back into a fresh P0 precombat main — the CR 714.3c
/// turn-based action that adds the Saga's next lore counter.
fn park_for_next_p0_precombat_main(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 1;
    state.active_player = P0;
    state.phase = Phase::End;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

/// Answer every prompt of the current turn-cycle window. Returns the
/// `EffectZoneChoice` payload the moment an unexpected prompt surfaces (before any further
/// progression), or `None` once the window settles at Priority with an empty
/// stack. Anything else is a hard failure: this test must never paper over an
/// unexpected prompt.
fn drain_until_prompt_or_settled(
    runner: &mut GameRunner,
    blocker: ObjectId,
) -> Option<(PlayerId, Vec<ObjectId>, usize)> {
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                ..
            } => return Some((player, cards, count)),
            // CR 603.3b: single-controller trigger ordering is auto-drained.
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            // CR 603.3d: chapter triggers (I/II — Mesmerize) declare their
            // target when put on the stack; the vanilla creature is the only
            // legal "target creature".
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(blocker)),
                    })
                    .expect("Mesmerize must accept the vanilla creature as its target");
            }
            // P1's creature is a legal attacker during the walked combat
            // phases; the walk only needs P1 to decline combat.
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("P1 must be able to decline combat");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return None;
                }
                let _ = runner.act(GameAction::PassPriority);
                let _ = runner.act(GameAction::PassPriority);
            }
            other => panic!("unexpected prompt during the chapter walk: {other:?}"),
        }
    }
    panic!("prompt loop failed to settle within 256 iterations");
}

#[test]
fn jill_shiva_cold_snap_returns_front_face_up_without_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Draw-step survival while the chapters walk to III (CR 104.3c).
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);

    // The tap leg's victims: three untapped opponent lands.
    let p1_lands = [
        scenario.add_basic_land(P1, ManaColor::Blue),
        scenario.add_basic_land(P1, ManaColor::Blue),
        scenario.add_basic_land(P1, ManaColor::Blue),
    ];
    // Chapters I/II (Mesmerize) each need a legal "target creature".
    let blocker = scenario.add_creature(P1, "Vanilla Blocker", 2, 2).id();

    // Jill on the battlefield with Shiva as her printed back face. Staging her
    // directly bypasses the ETB pipeline, which is fine: the "When Jill
    // enters" trigger is irrelevant to the bug and no ETB event is emitted.
    let jill_id = scenario
        .add_creature(P0, "Jill, Shiva's Dominant", 2, 2)
        .as_legendary()
        .from_oracle_text(JILL_FRONT_ORACLE)
        .id();

    // CR 602.2b: announce the {3}{U}{U} cost against an exactly-sized pool.
    let mut pool = mana(ManaType::Colorless, 3);
    pool.extend(mana(ManaType::Blue, 2));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&jill_id)
        .unwrap()
        .back_face = Some(shiva_back_face());

    let activated_index = runner.state().objects[&jill_id]
        .abilities
        .iter()
        .position(|a| a.kind == AbilityKind::Activated)
        .expect("Jill's exile-and-return must parse as an activated ability");
    // The return leg's candidate pool during the activation is ONLY Jill
    // (nothing else publishes into this chain set), so this must settle
    // silently; the reach-guards below prove the transform completed.
    runner
        .activate(jill_id, activated_index)
        // Declared intent for the CR 714.3a ETB lore counter's chapter I slot.
        .target_object(blocker)
        .resolve();

    // REACH GUARDS — the transform happened and Shiva's chapters are live.
    {
        let shiva = &runner.state().objects[&jill_id];
        assert_eq!(
            shiva.zone,
            Zone::Battlefield,
            "the transformed Saga must be on the battlefield"
        );
        assert!(shiva.transformed, "the Saga must be back-face-up (Shiva)");
        assert_eq!(shiva.name, "Shiva, Warden of Ice");
    }

    // CR 714.3a + CR 714.3c: Shiva enters with a lore counter (chapter I
    // fired during the activation) and each subsequent precombat main adds
    // the next one. Two additional ticks are needed; four iterations are
    // allowed for robustness.
    //
    // `advance_to_phase` stops at ANY precombat main (P1's included) and
    // aborts mid-walk at non-priority prompts (e.g. P1's declare-attackers
    // TBA), so the walk to P0's next precombat main is done in small rounds
    // of advance -> drain, answering combat prompts (P1 declines) and chapter
    // triggers (Mesmerize targets the vanilla creature) as they surface. One
    // outer iteration = one P0 lore tick.
    //
    // With the fix there is NO prompt to break the walk, so the loop stops at
    // the observable return: the Saga back on the battlefield front face up.
    // The lands must still be tapped at THAT instant — the walk never crosses
    // the next untap step, which is the only thing that would legitimately
    // untap them.
    let mut prompt: Option<(PlayerId, Vec<ObjectId>, usize)> = None;
    let mut returned = false;
    for _ in 0..4 {
        park_for_next_p0_precombat_main(&mut runner);
        for _ in 0..8 {
            runner.advance_to_phase(Phase::PreCombatMain);
            prompt = drain_until_prompt_or_settled(&mut runner, blocker);
            if prompt.is_some() {
                break;
            }
            {
                let shiva = &runner.state().objects[&jill_id];
                if shiva.zone == Zone::Battlefield && !shiva.transformed {
                    // Chapter III just resolved: Shiva returned front face up.
                    returned = true;
                    break;
                }
            }
            if runner.state().active_player == P0 && runner.state().phase == Phase::PreCombatMain {
                break;
            }
            runner.pass_both_players();
        }
        if prompt.is_some() || returned {
            break;
        }
    }

    // ---- THE FIX (inverted): chapter III resolves with NO return-leg prompt -
    assert!(
        prompt.is_none(),
        "chapter III's return leg must resolve silently (no EffectZoneChoice), got {prompt:?}"
    );
    // Reach-guard: the return is only observable after chapter III resolved,
    // so the no-prompt assertion cannot pass vacuously.
    assert!(
        returned,
        "chapter III must complete: the Saga must be back front face up"
    );
    // The resolution window is P0's precombat main (CR 714.3c cadence),
    // settled at priority with an empty stack.
    assert_eq!(runner.state().active_player, P0, "the walk must end at P0");
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "the walk must end settled in P0's precombat main"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the game must settle at Priority with no outstanding prompt, got {:?}",
        runner.state().waiting_for
    );

    // ---- REACH GUARDS: all three Cold Snap legs demonstrably ran ----------
    for land in &p1_lands {
        let land_obj = &runner.state().objects[land];
        assert_eq!(
            land_obj.zone,
            Zone::Battlefield,
            "lands stay in play (the tap leg ran; the return leg moved none of them)"
        );
        assert!(
            land_obj.tapped,
            "the tap leg must have tapped the opponent's lands (user-observed)"
        );
    }
    let jill = &runner.state().objects[&jill_id];
    assert_eq!(
        jill.zone,
        Zone::Battlefield,
        "the exile+return legs must cycle Shiva back out of exile"
    );
    assert!(
        !jill.transformed,
        "the Saga returns front face up (CR 712.8a)"
    );
    assert_eq!(jill.name, "Jill, Shiva's Dominant");
    assert_eq!(jill.controller, P0, "controlled by P0");
}
