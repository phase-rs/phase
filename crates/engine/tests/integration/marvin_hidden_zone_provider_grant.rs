//! F1: hidden-zone providers donate no abilities through Marvin's and
//! Experiment Kraj's grant-by-reference statics.
//!
//! CR 613.1f (Layer 6 ability-adding effects) + CR 201.2: Marvin's
//! "creatures you control that don't have the same name as ~" and Kraj's
//! "each other creature with a +1/+1 counter on it" now pin their provider
//! sets to the battlefield (`FilterProp::InZone { Battlefield }`, the
//! Drana-and-Linvala precedent), so an identical creature in hand, graveyard,
//! exile, or library contributes no grant even though it sits in
//! `state.objects`.
//!
//! Verbatim Oracle texts (Scryfall): Marvin — "Marvin has all activated
//! abilities of creatures you control that don't have the same name as
//! this creature." (committed const in
//! `hankyu_two_hop_granter_cost.rs`); Experiment Kraj — "Experiment Kraj
//! has all activated abilities of each other creature with a +1/+1 counter
//! on it.\n{T}: Put a +1/+1 counter on target creature." (committed const
//! in `cost_removed_counter_amount.rs`); Quirion Ranger — "Return a Forest
//! you control to its owner's hand: Untap target creature. Activate only
//! once each turn." (committed const in
//! `announced_key_viewer_redaction.rs`). Elvish Mystic — "{T}: Add {G}."
//! (DERIVED standard printed text; no committed const exists).
//!
//! Revert detectors: restoring the pre-pin filter (dropping `InZone`)
//! donates the hidden Untap into Marvin/Kraj, flipping every
//! `no ... donation` assertion below.

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::{move_to_library_position, move_to_zone};
use engine::types::counter::CounterType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Scryfall Oracle text (Marvin, Murderous Mimic).
const MARVIN: &str = "Marvin has all activated abilities of creatures you control that don't have the same name as this creature.";
// Verbatim Oracle text (Experiment Kraj), committed const in
// `cost_removed_counter_amount.rs`.
const KRAJ: &str = "Experiment Kraj has all activated abilities of each other creature with a +1/+1 counter on it.\n{T}: Put a +1/+1 counter on target creature.";
// Verbatim Scryfall Oracle text (Quirion Ranger).
const RANGER: &str = "Return a Forest you control to its owner's hand: Untap target creature. Activate only once each turn.";
// Standard printed text (Elvish Mystic). DERIVED — see module docs.
const MYSTIC: &str = "{T}: Add {G}.";

fn relayer(state: &mut GameState) {
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

/// The stamp a layer-6 donation from `provider` carries on the recipient.
fn donated_by(state: &GameState, provider: ObjectId) -> ObjectIncarnationRef {
    ObjectIncarnationRef::from_object(&state.objects[&provider])
}

fn has_donation_from(state: &GameState, recipient: ObjectId, provider: ObjectId) -> bool {
    let stamp = donated_by(state, provider);
    state.objects[&recipient]
        .abilities
        .iter()
        .any(|def| def.granting_object == Some(stamp))
}

fn has_untap_donation(state: &GameState, recipient: ObjectId) -> bool {
    state.objects[&recipient].abilities.iter().any(|def| {
        def.description
            .as_deref()
            .is_some_and(|text| text.contains("Untap"))
    })
}

/// CR 613.1f + CR 201.2: a Quirion Ranger in hand donates nothing to Marvin,
/// while a battlefield Elvish Mystic donates its mana ability.
///
/// The positive guard (Mystic's `{G}` IS donated) proves the grant pipeline
/// ran; the negative (no Untap) proves the zone pin, not an upstream parse
/// failure — reverting the `InZone` pin donates the Ranger's Untap and flips
/// the length assertion.
#[test]
fn hidden_hand_ranger_donates_nothing_battlefield_mystic_donates() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marvin = scenario
        .add_creature_from_oracle(P0, "Marvin, Murderous Mimic", 2, 2, MARVIN)
        .id();
    let mystic = scenario
        .add_creature_from_oracle(P0, "Elvish Mystic", 1, 1, MYSTIC)
        .id();
    scenario.add_creature_to_hand_from_oracle(P0, "Quirion Ranger", 1, 1, RANGER);
    let mut runner = scenario.build();
    relayer(runner.state_mut());

    let state = runner.state();
    assert!(
        has_donation_from(state, marvin, mystic),
        "reach-guard: the battlefield Mystic's mana ability IS donated to Marvin"
    );
    assert!(
        !has_untap_donation(state, marvin),
        "the hand Ranger must not donate its Untap to Marvin (CR 613.1f zone pin)"
    );
    assert_eq!(
        state.objects[&marvin].abilities.len(),
        1,
        "Marvin must hold exactly the one battlefield donation; got {:?}",
        state.objects[&marvin].abilities
    );
}

/// CR 613.1f: the same Ranger moved to graveyard, exile, or library donates
/// nothing. Each move goes through the production zone-change path, so the
/// card keeps its parsed Untap ability — only the zone pin refuses.
#[test]
fn graveyard_exile_library_rangers_donate_nothing() {
    for zone in [Zone::Graveyard, Zone::Exile, Zone::Library] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let marvin = scenario
            .add_creature_from_oracle(P0, "Marvin, Murderous Mimic", 2, 2, MARVIN)
            .id();
        let mystic = scenario
            .add_creature_from_oracle(P0, "Elvish Mystic", 1, 1, MYSTIC)
            .id();
        let ranger = scenario
            .add_creature_from_oracle(P0, "Quirion Ranger", 1, 1, RANGER)
            .id();
        let mut runner = scenario.build();
        let mut events = Vec::new();
        if zone == Zone::Library {
            move_to_library_position(runner.state_mut(), ranger, true, &mut events);
        } else {
            move_to_zone(runner.state_mut(), ranger, zone, &mut events);
        }
        relayer(runner.state_mut());

        let state = runner.state();
        assert_eq!(
            state.objects[&ranger].zone, zone,
            "reach-guard: the Ranger reached {zone:?}"
        );
        assert!(
            has_donation_from(state, marvin, mystic),
            "reach-guard ({zone:?}): the battlefield grant still applies"
        );
        assert!(
            !has_untap_donation(state, marvin),
            "the {zone:?} Ranger must not donate its Untap to Marvin"
        );
    }
}

/// CR 613.1f: an opponent-controlled Marvin gains nothing from a Ranger in
/// that opponent's hand. `You` resolves against the recipient's controller,
/// which the hand card still matches — only the zone pin refuses.
#[test]
fn opponent_hand_ranger_donates_nothing_to_opponent_marvin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marvin = scenario
        .add_creature_from_oracle(P1, "Marvin, Murderous Mimic", 2, 2, MARVIN)
        .id();
    scenario.add_creature_to_hand_from_oracle(P1, "Quirion Ranger", 1, 1, RANGER);
    let mut runner = scenario.build();
    relayer(runner.state_mut());

    let state = runner.state();
    assert!(
        !has_untap_donation(state, marvin),
        "the opponent's hand Ranger must not donate its Untap to their Marvin"
    );
    assert!(
        state.objects[&marvin].abilities.is_empty(),
        "opponent Marvin must hold no donations at all; got {:?}",
        state.objects[&marvin].abilities
    );
}

/// CR 613.1f: Kraj gains the mana ability of a counter-bearing battlefield
/// Mystic, but nothing from an identical counter-bearing Ranger in the
/// graveyard. The counter is placed surgically AFTER the zone change, so the
/// reach-guard (counter present) proves the zone pin — not CR 122.2 counter
/// clearing — is what refuses.
#[test]
fn kraj_graveyard_counter_bearer_donates_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kraj = scenario
        .add_creature_from_oracle(P0, "Experiment Kraj", 4, 6, KRAJ)
        .id();
    let mystic = scenario
        .add_creature_from_oracle(P0, "Elvish Mystic", 1, 1, MYSTIC)
        .id();
    scenario.with_counter(mystic, CounterType::Plus1Plus1, 1);
    let ranger = scenario
        .add_creature_from_oracle(P0, "Quirion Ranger", 1, 1, RANGER)
        .id();
    let mut runner: GameRunner = scenario.build();
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), ranger, Zone::Graveyard, &mut events);
    runner
        .state_mut()
        .objects
        .get_mut(&ranger)
        .expect("graveyard Ranger")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    relayer(runner.state_mut());

    let state = runner.state();
    assert_eq!(
        state.objects[&ranger]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "reach-guard: the graveyard Ranger bears a +1/+1 counter — only the \
         zone pin (not counter absence) can refuse the grant"
    );
    assert!(
        has_donation_from(state, kraj, mystic),
        "reach-guard: the counter-bearing battlefield Mystic's mana ability \
         IS donated to Kraj"
    );
    assert!(
        !has_untap_donation(state, kraj),
        "the counter-bearing graveyard Ranger must not donate its Untap to Kraj"
    );
}

/// CR 613.1f residual, documented not desired: a face-down battlefield
/// creature still matches `InZone { Battlefield }`, so its abilities are
/// donated. Face-down permanents have no abilities under CR 708.2 — the pin
/// is a zone gate, not a face gate — and the viewer-projection redaction is
/// what keeps such lineage from disclosing hidden identities.
#[test]
fn face_down_battlefield_provider_still_donates() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marvin = scenario
        .add_creature_from_oracle(P0, "Marvin, Murderous Mimic", 2, 2, MARVIN)
        .id();
    let ranger = scenario
        .add_creature_from_oracle(P0, "Quirion Ranger", 1, 1, RANGER)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&ranger)
        .expect("battlefield Ranger")
        .face_down = true;
    relayer(runner.state_mut());

    let state = runner.state();
    assert!(
        has_untap_donation(state, marvin),
        "residual: the face-down battlefield Ranger matches InZone and its \
         Untap is donated (the zone pin is not a face gate)"
    );
}
