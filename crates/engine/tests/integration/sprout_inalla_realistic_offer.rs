//! CR 732.2a acceptance — the object-growth loop-shortcut firewall must SKIP an ETB observer
//! whose entry matcher provably can't watch the loop's growing fodder class.
//!
//! Loads the REAL 4-player "realistic lands" dump: a Witherbloom, the Balancer + Sprout Swarm
//! Saproling object-growth loop for P0, coexisting with an OPPONENT's Inalla, Archmage Ritualist
//! in the command zone. Inalla's Eminence ETB observer ("Whenever another nontoken Wizard you
//! control enters, ... you may pay {1}. If you do, create a token that's a copy of that Wizard")
//! is TRIPLE-disjoint from the P0 Saproling token (wrong subtype, controller You = P1, NonToken),
//! so per CR 603.6a it can never fire on the loop's per-cycle token creation — yet the pre-fix
//! firewall (`fire_time_conditions_read_growing_class` block(1)) scanned its `CopyTokenOf` execute
//! body as a live loop observer and VETOED the offer.
//!
//! Per memory [real-game-fixtures-not-synthetic] + [combo-detector-must-fire-in-real-games]: this
//! LOADS the real dump through the production restore path (`PersistedGameState::into_game_state`)
//! and DRIVES a live Sprout Swarm cast through the public GameRunner/`apply()` boundary. The load
//! migration DROPS the primed loop sequence at the empty-stack Priority beat (measured: seq_len
//! 1 → 0), so a load-then-probe would be vacuous; one live cast rebuilds the loop history the
//! detector replays.
//!
//! REVERT-PROBE (documented, implementer-run, non-vacuous): deleting the block-(1) gate `continue`
//! in `fire_time_conditions_read_growing_class` (analysis/resource.rs) makes the firewall veto on
//! Inalla again ⇒ `sprout_inalla_realistic_offer_fires` flips `LoopShortcut{P0}` → `Priority{P0}`.
//! The buyback-return + Saproling-+1 reach-guards hold BOTH ways (the live cast resolves
//! identically; only the clone-drive detection firewall differs), so the offer assertion is not
//! vacuous.

use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::types::ability::{TargetFilter, TriggerCondition, TriggerDefinition, TypedFilter};
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const INALLA: ObjectId = ObjectId(398);
const SPROUT: ObjectId = ObjectId(405);
/// An untapped P0 fodder Saproling to convoke for the {G} (406–410, 412 are untapped).
const FODDER: ObjectId = ObjectId(406);

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Load the realistic 4p dump's `["gameState"]` through the REAL production restore chokepoint
/// `PersistedGameState::into_game_state` (the same path server `from_persisted` and WASM
/// `decode_restored_game_state` funnel through). The migration drops the primed loop sequence
/// because the dump sits at empty-stack Priority (NOT a shortcut window), so the offer must be
/// rebuilt by a live cast below.
fn load_realistic_dump() -> GameState {
    let json = gunzip(include_bytes!(
        "../fixtures/sprout_witherbloom_realistic_lands_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let raw: GameState = serde_json::from_value(envelope["gameState"].clone())
        .expect("the realistic 4p gameState must deserialize into the current GameState");
    PersistedGameState::Raw(Box::new(raw)).into_game_state()
}

/// Count the battlefield Saprolings `who` controls (tapped or not) — the fodder reach-guard oracle.
fn count_saprolings(state: &GameState, who: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == who && o.name == "Saproling")
        })
        .count()
}

/// Drive the real Sprout Swarm object-growth cast (accept Buyback + convoke one fodder Saproling
/// for the {G}) through the public GameRunner boundary, exactly as the sibling untapped-precast
/// acceptance test does.
fn drive_sprout_cast(state: GameState) -> engine::game::scenario::Outcome {
    GameRunner::from_state(state)
        .cast(SPROUT)
        .accept_optional()
        .convoke_with(&[FODDER])
        .commit()
        .resolve()
}

/// PRIMARY acceptance: the realistic board — a P0 Sprout Swarm loop coexisting with an OPPONENT's
/// disjoint Inalla Eminence ETB observer in the command zone — now OFFERS the CR 732.2a
/// object-growth shortcut. Pre-fix the firewall scanned Inalla's `CopyTokenOf` body and vetoed.
#[test]
fn sprout_inalla_realistic_offer_fires() {
    let state = load_realistic_dump();

    // ── Preconditions: the loaded state IS the realistic failing configuration ──
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "fixture precondition: ordinary priority for P0 (pre-cast), got {:?}",
        state.waiting_for
    );
    // The disjoint OPPONENT observer is present — the whole point of the fixture.
    let inalla = state.objects.get(&INALLA).expect("Inalla present");
    assert_eq!(inalla.name, "Inalla, Archmage Ritualist");
    assert_eq!(
        (inalla.zone, inalla.controller),
        (Zone::Command, P1),
        "Inalla is an opponent's (P1) commander in the command zone"
    );
    assert_eq!(
        state
            .objects
            .get(&SPROUT)
            .map(|o| (o.name.as_str(), o.zone)),
        Some(("Sprout Swarm", Zone::Hand)),
        "fixture precondition: Sprout Swarm is in P0's hand"
    );
    let f = state
        .objects
        .get(&FODDER)
        .expect("fodder Saproling present");
    assert!(
        f.name == "Saproling" && f.controller == P0 && !f.tapped,
        "fixture precondition: {FODDER:?} is an untapped P0 fodder Saproling"
    );
    let before = count_saprolings(&state, P0);
    assert_eq!(
        before, 7,
        "fixture: P0 controls 7 Saprolings before the cast (406–412)"
    );

    let outcome = drive_sprout_cast(state);

    // ── Reach-guards (hold in BOTH revert modes ⇒ the offer assertion is non-vacuous) ──
    assert_eq!(
        outcome.zone_of(SPROUT),
        Zone::Hand,
        "Buyback must return Sprout Swarm to P0's hand (reach-guard: the cast resolved)"
    );
    assert_eq!(
        count_saprolings(outcome.state(), P0),
        before + 1,
        "the first iteration created exactly one more Saproling (reach-guard: +1 fodder)"
    );

    // ── DISCRIMINATOR: the offer FIRES despite the disjoint Inalla observer (revert-probe →
    //    Priority{P0}) ──
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "the disjoint Inalla ETB observer must be SKIPPED so the CR 732.2a LoopShortcut offer \
         surfaces for P0, got {:?}",
        outcome.final_waiting_for()
    );
}

/// DISCRIMINATING NEGATIVE (matched pair): the fix skips ONLY provably-disjoint observers — a
/// BROAD "whenever a creature enters" ETB observer whose matcher DOES match the P0 Saproling
/// fodder still vetoes the offer, proving the firewall is not neutered.
///
/// The synthesized observer's execute is `None` and its intervening-`if`
/// (`ControlsType` of a subtype nobody controls) is FALSE live, so per CR 603.4 it
/// fires-then-is-removed with NO board change (the cover stays intact). The ONLY suppression
/// mechanism is the firewall veto on its sibling-reading condition — so half A no-offer isolates
/// the firewall, and the flip vs half B (offer, only the disjoint Inalla present) proves the gate
/// is appropriately NARROW (an over-broad gate that also skipped this observer would make A offer).
#[test]
fn sprout_broad_matching_observer_still_vetoes_offer() {
    // ── half B (baseline): only the disjoint Inalla observer ⇒ the offer fires ──
    let outcome_b = drive_sprout_cast(load_realistic_dump());
    assert!(
        matches!(
            outcome_b.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "baseline (only the disjoint Inalla observer): the offer fires, got {:?}",
        outcome_b.final_waiting_for()
    );

    // ── half A (hostile): add a P1 BROAD creature-matching ETB observer ⇒ the firewall must
    //    still veto ⇒ NO offer ──
    let mut state = load_realistic_dump();
    let card_id = CardId(state.next_object_id);
    let observer = create_object(
        &mut state,
        card_id,
        P1,
        "Broad ETB Observer".to_string(),
        Zone::Battlefield,
    );
    // "Whenever a creature enters" (matches the P0 Saproling), gated by a sibling-reading
    // intervening-`if` (`ControlsType`) so the firewall's block(1) condition scan vetoes; the
    // filter names a subtype nobody controls, so the live intervening-`if` is FALSE and the
    // trigger fires-then-removes with no board change (execute `None`).
    let mut trig = TriggerDefinition::new(TriggerMode::ChangesZone)
        .destination(Zone::Battlefield)
        .valid_card(TargetFilter::Typed(TypedFilter::creature()));
    trig.condition = Some(TriggerCondition::ControlsType {
        filter: TargetFilter::Typed(TypedFilter::creature().subtype("Yeti".to_string())),
    });
    state
        .objects
        .get_mut(&observer)
        .unwrap()
        .trigger_definitions
        .push(trig);

    let outcome_a = drive_sprout_cast(state);
    // Positive reach-guard (not merely `!LoopShortcut`): the identical live cast completes and
    // returns to `Priority{P0}` — the no-offer state. If the narrow gate had over-skipped this
    // broad matcher, the drive would surface `LoopShortcut{P0}` here (the offer half B reaches),
    // so pinning `Priority{P0}` makes the pair self-evidently discriminating against half B.
    assert!(
        matches!(
            outcome_a.final_waiting_for(),
            WaitingFor::Priority { player, .. } if *player == P0
        ),
        "a BROAD ETB observer whose matcher matches the fodder must still veto the offer (the gate \
         is narrow) ⇒ the live cast returns to Priority{{P0}}, got {:?}",
        outcome_a.final_waiting_for()
    );
}
