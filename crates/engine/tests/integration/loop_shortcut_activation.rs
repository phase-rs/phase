//! CORE Unit-2 (P1): capture + drive a repeated ACTIVATION loop under CR 732.2a.
//!
//! Real-card acceptance canary. Presence of Gond (Aura — grants a token-creating `{T}` to the
//! enchanted creature, CR 601? no: CR 602.2a activated ability) + Intruder Alarm (untaps all
//! creatures whenever a creature enters) form a genuine CR 732.2a activation loop: activate
//! `{T}` → Elf Warrior ETB → Intruder Alarm untaps the host → repeat.
//!
//! Honesty bar (run-lead-mandated):
//!  1. Every card is loaded from the real `shared_card_db()` through the real parser+reducer;
//!     no synthetic oracle text, no hand-built loop state.
//!  2. The granted `{T}` materializes through the LAYER system — `attach_to` itself calls
//!     `mark_layers_full` + `flush_layers` → `evaluate_layers` (Layer 6, CR 613.1f) — and is
//!     read OFF the host's layer-derived `abilities`, never hand-injected.
//!  3. Every beat (activation, priority passes, Elf ETB → untap trigger, offer) runs through
//!     the real reducer via `apply_action` / `GameAction`.

use super::support::shared_card_db;
use engine::analysis::resource::ResourceAxis;
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, LoopDetectionMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HOST: &str = "Grizzly Bears";

/// Place a real card on the battlefield after build, bypassing the unattached-aura
/// attach-choice pause (mirrors `issue_2863`). `add_real_card(Zone::Battlefield)` panics on an
/// Aura (`NeedsAuraAttachmentChoice`), so the Gond aura must be placed this way and then
/// attached explicitly with `attach_to`.
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

/// Find the token-creating activated ability on `host`'s LAYER-DERIVED abilities (Gond's
/// granted `{T}: Create a 1/1 green Elf Warrior`). Reads OFF the host — never injects. `None`
/// when the grant is absent (the board-lever negative control).
fn token_ability_index(state: &GameState, host: ObjectId) -> Option<usize> {
    state
        .objects
        .get(&host)?
        .abilities
        .iter()
        .position(|def| matches!(&*def.effect, Effect::Token { .. }))
}

/// Count the Elf Warrior tokens on P0's battlefield — the loop's growing object class. The only
/// tokens this canary ever makes are Gond's Elf Warriors, so a battlefield token count is exact.
fn elf_count(state: &GameState) -> usize {
    state
        .objects
        .values()
        .filter(|o| o.is_token && o.zone == Zone::Battlefield && o.controller == P0)
        .count()
}

struct Canary {
    runner: GameRunner,
    host: ObjectId,
}

/// Build the 2-player canary board: a vanilla host creature, optional Intruder Alarm (the
/// untapper), and an optional Presence of Gond attached to the host (the grant source).
fn setup(with_gond: bool, with_alarm: bool, mode: LoopDetectionMode, db: &CardDatabase) -> Canary {
    let mut scenario = GameScenario::new(); // new_two_player: P0 + P1
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_real_card(P0, HOST, Zone::Battlefield, db);
    if with_alarm {
        scenario.add_real_card(P0, "Intruder Alarm", Zone::Battlefield, db);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    if with_gond {
        let gond = place_on_battlefield(runner.state_mut(), P0, "Presence of Gond", db);
        // `attach_to` returns the PRIOR host (for detach bookkeeping), not a success flag —
        // on a first attach it returns `None` even on success (mirrors `issue_2863`). It ALSO
        // materializes the grant: on success it calls `mark_layers_full` + `flush_layers` →
        // `evaluate_layers` (Layer 6, CR 613.1f), so Gond's `GrantAbility` lands the `{T}` on
        // the host's layer-derived abilities. Verify success via `attached_to`, not the return.
        attach_to(runner.state_mut(), gond, host);
        assert_eq!(
            runner.state().objects[&gond].attached_to,
            Some(host.into()),
            "Gond must be attached to the host (attach_to succeeded)"
        );
    }
    Canary { runner, host }
}

/// Activate the host's `{T}` through the real reducer, then pass priority (both seats) to let
/// the ability + downstream Elf-ETB/untap trigger resolve. Stop at the CR 732.2a `LoopShortcut`
/// offer, or when the stack settles empty at a `Priority` window (no offer).
fn activate_and_drive(runner: &mut GameRunner, host: ObjectId, ability_index: usize) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: host,
            ability_index,
        })
        .expect("the granted {T} activation is legal");
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

/// P1-1 (P3 landed) — real Gond + Intruder Alarm + host: the activation loop CAPTURES, OFFERS a
/// CR 732.2a `LoopShortcut`, and (on DECLINE) SUSTAINS at the real-game level (each `{T}` makes an
/// Elf; Intruder Alarm untaps the host so the next `{T}` is legal), driven entirely through
/// `apply_action`. Under DEFERRED-8 (P3) the firewall no longer over-vetoes: Intruder Alarm's
/// "untap all creatures" is a `SetTapState{Typed{Creature}}` effect body, and the CR 732.2a
/// `Typed`-precision relaxation (the `LoopFirewall` census-vs-relax split in `ability_scan.rs`)
/// passes it through the promoted `LoopFirewall` trigger-effect-body scan (`resource.rs` block-1,
/// `ability_definition_reads_sibling_mutable_for_loop`), so this exact board OFFERS. This test owns
/// the DECLINE→continue→sustain path (the offer is not a dead-end); the twin
/// `..._offers_shortcut` owns the certificate shape.
#[test]
fn activation_loop_gond_intruder_alarm_captures_and_sustains() {
    let Some(db) = shared_card_db() else { return };
    let mut c = setup(true, true, LoopDetectionMode::Interactive, db);

    // The `{T}` is read OFF the host's layer-derived abilities (proves it came through Gond's
    // layer grant, not a hand-injection). No `host.abilities` mutation anywhere in this test.
    let idx = token_ability_index(c.runner.state(), c.host)
        .expect("Gond's granted token-creating {T} must be on the host's layer-derived abilities");

    // First activation: the capture ARMS, one Elf enters, Intruder Alarm untaps the host, and the
    // CR 732.2a firewall now OFFERS (P3: the Typed-precision relaxation passes Intruder Alarm's
    // untap-all effect body through the LoopFirewall trigger-effect-body scan).
    activate_and_drive(&mut c.runner, c.host, idx);
    assert!(
        !c.runner.state().last_loop_action_sequence.is_empty(),
        "the token-creating activation ARMS the capture (CR 602.2a Activate)"
    );
    assert_eq!(
        elf_count(c.runner.state()),
        1,
        "the first activation makes one Elf Warrior"
    );
    assert!(
        !c.runner.state().objects[&c.host].tapped,
        "Intruder Alarm untapped the host after the Elf ETB (loop is sustainable)"
    );
    // The offer is now REACHED (was firewall-blocked pre-P3): P0 proposes the shortcut.
    assert!(
        matches!(
            &c.runner.state().waiting_for,
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "P3: the firewall now OFFERS a CR 732.2a LoopShortcut to the loop's controller"
    );

    // DECLINE the shortcut: the game must CONTINUE (not dead-end) so the loop can be played out by
    // hand. Decline restores a normal Priority window to the proposer.
    c.runner
        .act(GameAction::DeclineShortcut)
        .expect("the proposer may decline the CR 732.2a shortcut");
    assert!(
        matches!(
            &c.runner.state().waiting_for,
            WaitingFor::Priority { player } if *player == P0
        ),
        "declining restores a normal Priority window to the proposer (the offer is not a dead-end)"
    );

    // Second activation succeeds through the real reducer AFTER a decline ⇒ the loop SUSTAINS at
    // the game level (this is exactly what the offer hook's clone-drive replays).
    activate_and_drive(&mut c.runner, c.host, idx);
    assert_eq!(
        elf_count(c.runner.state()),
        2,
        "the host re-activates after declining ⇒ a second Elf ⇒ the activation loop sustains"
    );
}

/// P1-1 acceptance TARGET (P3 landed): the same real board asserts the CR 732.2a `LoopShortcut`
/// offer's CERTIFICATE — `proposer == P0` and `unbounded == [TokensCreated]`. Un-ignored with
/// DEFERRED-8: the `Typed`-precision relaxation (`ability_scan.rs` `LoopFirewall` split) passes
/// Intruder Alarm's `SetTapState{Typed{Creature}}` untap-all effect body through the promoted
/// `LoopFirewall` trigger-effect-body scan, so this exact board OFFERS. Revert-probe: reverting the
/// `Typed` relaxation (Conservative `sibling:true` for the effect-target) re-vetoes and turns the
/// `expected a CR 732.2a LoopShortcut offer` arm below RED.
#[test]
fn activation_loop_gond_intruder_alarm_offers_shortcut() {
    let Some(db) = shared_card_db() else { return };
    let mut c = setup(true, true, LoopDetectionMode::Interactive, db);

    let idx = token_ability_index(c.runner.state(), c.host)
        .expect("Gond's granted token-creating {T} must be on the host's layer-derived abilities");

    activate_and_drive(&mut c.runner, c.host, idx);

    match &c.runner.state().waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            certificate,
            ..
        } => {
            assert_eq!(*proposer, P0, "the loop's controller proposes the shortcut");
            assert_eq!(
                certificate.unbounded,
                vec![ResourceAxis::TokensCreated],
                "the object-growth certificate names exactly the TokensCreated axis"
            );
        }
        other => panic!("expected a CR 732.2a LoopShortcut offer, got {other:?}"),
    }
}

/// P1-1 probe 1 — BOARD lever (grant-source control). NO Gond attached ⇒ the host's
/// layer-derived abilities have no `{T}` ⇒ the activation is impossible ⇒ no offer. The test
/// never touches `host.abilities`; the delta vs the P1-1 positive is the grant source alone,
/// proving the ability comes through the real layer path.
#[test]
fn activation_loop_without_grant_source_does_not_offer() {
    let Some(db) = shared_card_db() else { return };
    let c = setup(false, true, LoopDetectionMode::Interactive, db);

    assert!(
        token_ability_index(c.runner.state(), c.host).is_none(),
        "with no Gond attached, the host's layer-derived abilities must have no token-creating tap ability"
    );
    assert!(
        !matches!(
            c.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "no grant ⇒ no activation ⇒ no offer"
    );
    assert!(
        c.runner.state().last_loop_action_sequence.is_empty(),
        "no activation happened, so nothing was captured"
    );
}

/// P1-2 — NEGATIVE TWIN (untapper-absent lever). SAME real cards MINUS Intruder Alarm ⇒ the
/// first activation is legal and the capture ARMS, but the host stays tapped (no untap
/// trigger), so the drive's 2nd activation is illegal and DECLINES ⇒ no offer. The
/// non-vacuity guard: rejection comes from the DRIVE, not a failure to capture.
#[test]
fn activation_loop_without_untapper_does_not_offer() {
    let Some(db) = shared_card_db() else { return };
    let mut c = setup(true, false, LoopDetectionMode::Interactive, db);

    let idx = token_ability_index(c.runner.state(), c.host)
        .expect("Gond's granted {T} is present even without the untapper");

    activate_and_drive(&mut c.runner, c.host, idx);

    // Positive reach-guard: the capture ARMED (the input got past the setter — not vacuous).
    assert!(
        !c.runner.state().last_loop_action_sequence.is_empty(),
        "the token-creating activation must ARM the capture (non-vacuity guard)"
    );
    // SUSTAIN-FAILURE discriminator — the load-bearing negation vs the positive
    // `captures_and_sustains` (which asserts the host is `!tapped`, OFFERS, and sustains to 2 Elves
    // post-P3). Without Intruder Alarm the `{T}` cost leaves the host TAPPED and nothing untaps it,
    // so the loop cannot sustain a 2nd activation. This flips the SUSTAIN axis: the positive OFFERS
    // and sustains, this one cannot sustain ⇒ the drive declines ⇒ no offer (a clean, non-vacuous
    // delta on the untapper lever alone).
    assert!(
        c.runner.state().objects[&c.host].tapped,
        "without the untapper the host stays tapped after activation ⇒ the loop cannot sustain"
    );
    assert_eq!(
        elf_count(c.runner.state()),
        1,
        "exactly one activation was possible without the untapper (no sustain to a 2nd Elf)"
    );
    // The loop does not sustain ⇒ the drive declines ⇒ no offer.
    assert!(
        !matches!(
            c.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "without the untapper the 2nd activation is illegal ⇒ the drive declines ⇒ no offer"
    );
}

/// P1-8 — `Off` byte-identical (#4603). The P1-1 board under `LoopDetectionMode::Off` ⇒ the
/// capture is NEVER written (the `.samples()` gate) ⇒ no offer + the driving permanent behaves
/// exactly as pre-feature. Revert-probe: flip the `.samples()` gate to always-write ⇒ `Off`
/// writes the capture ⇒ `is_none()` flips.
#[test]
fn activation_loop_off_mode_is_byte_identical() {
    let Some(db) = shared_card_db() else { return };
    let mut c = setup(true, true, LoopDetectionMode::Off, db);

    let idx = token_ability_index(c.runner.state(), c.host)
        .expect("the grant materializes regardless of loop-detection mode");

    activate_and_drive(&mut c.runner, c.host, idx);

    assert!(
        c.runner.state().last_loop_action_sequence.is_empty(),
        "Off (#4603): a token-creating activation must NOT write the capture"
    );
    assert!(
        !matches!(
            c.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "Off never samples ⇒ never offers"
    );
    // The Elf token still materialized (the game played normally, just no detector): the
    // battlefield grew past the {host, Gond, Alarm} = 3 permanents by exactly one Elf.
    let elves = c
        .runner
        .state()
        .battlefield
        .iter()
        .filter(|id| c.runner.state().objects[id].name == "Elf Warrior")
        .count();
    assert_eq!(
        elves, 1,
        "Off plays the game normally: one Elf from one activation"
    );
}
