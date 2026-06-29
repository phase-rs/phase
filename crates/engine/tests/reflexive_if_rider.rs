//! Runtime regression coverage for the reflexive-if-rider class (S01): a spell or
//! ability whose second clause is gated by a condition that anaphorically refers
//! to the object the first clause acted on — "Exile target creature. If it was
//! dealt damage this turn, create a Clue token." (Sold Out), "If it had mana
//! value 3 or less, surveil 2." (Consuming Ashes), "If that creature is
//! attacking, ~ deals 2 damage to that creature's controller." (Wisecrack).
//!
//! Before this change the rider's condition parsed to `null`, so the rider fired
//! UNCONDITIONALLY (measured in `card-data.json`). Each test drives the real cast
//! pipeline (`GameRunner::cast(..).resolve()`) and asserts the rider fires ONLY
//! when its condition holds — the negative case is the discriminator: reverting
//! the recognizer makes the condition `null` again and the negative assertion
//! fails (the rider fires when it must not).
//!
//! CR 608.2c: later text in the same effect refers to an object mentioned
//! earlier. CR 400.7 / CR 608.2h: past-tense predicates ("was dealt damage",
//! "had mana value") read last-known information after the object leaves the
//! battlefield; present-tense ("is attacking") reads live state.

use engine::game::combat::{AttackerInfo, CombatState};
use engine::game::scenario::GameScenario;
use engine::types::ability::TargetRef;
use engine::types::counter::CounterType;
use engine::types::game_state::{DamageRecord, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

// Verified identical to the engine's authoritative card data (client/public/card-data.json).
const SOLD_OUT: &str =
    "Exile target creature. If it was dealt damage this turn, create a Clue token. \
     (It's an artifact with \"{2}, Sacrifice this token: Draw a card.\")";
const CONSUMING_ASHES: &str = "Exile target creature. If it had mana value 3 or less, surveil 2. \
     (Look at the top two cards of your library, then put any number of them into your \
     graveyard and the rest on top of your library in any order.)";
const WISECRACK: &str =
    "Target creature deals damage equal to its power to itself. If that creature is \
     attacking, Wisecrack deals 2 damage to that creature's controller.";
const DRIFTGLOOM: &str =
    "When this creature enters, exile target creature an opponent controls until this \
     creature leaves the battlefield. If that creature had power 2 or less, put a +1/+1 \
     counter on this creature.";
// Verified identical to the engine's authoritative card data (data/mtgish-cards.json:
// PutPermanentIntoItsOwnersHand → If(IsTapped) → CreateTokens(MapToken)).
const BRACKISH_BLUNDER: &str =
    "Return target creature to its owner's hand. If it was tapped, create a Map token.";

/// Number of `+1/+1` counters on `object`.
fn plus_counters(
    state: &engine::types::game_state::GameState,
    object: engine::types::identifiers::ObjectId,
) -> u32 {
    state
        .objects
        .get(&object)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// Count battlefield objects named `name` (token-presence assertion).
fn battlefield_named(state: &engine::types::game_state::GameState, name: &str) -> usize {
    state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.name == name)
        .count()
}

// ---------------------------------------------------------------------------
// Sold Out — "If it was dealt damage this turn" (WasDealtDamageThisTurn, LKI
// ledger join). Exercises the zone-change/LKI `WasDealtDamageThisTurn` arm.
// ---------------------------------------------------------------------------

/// RUNTIME (positive) — CR 120.6 + CR 120.9 + CR 608.2h. A creature with a
/// damage record this turn, exiled by Sold Out, satisfies "was dealt damage this
/// turn" via the turn-scoped ledger (keyed by the battlefield ObjectId, survives
/// the exile), so the Clue token is created.
#[test]
fn sold_out_creates_clue_when_target_was_dealt_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sold Out", true, SOLD_OUT)
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    // CR 120.9: the turn's damage ledger records the victim's battlefield id.
    runner
        .state_mut()
        .damage_dealt_this_turn
        .push_back(DamageRecord {
            target: TargetRef::Object(victim),
            amount: 2,
            ..Default::default()
        });

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Exile);
    assert_eq!(
        battlefield_named(outcome.state(), "Clue"),
        1,
        "damaged target must yield a Clue token"
    );
}

/// RUNTIME (negative / discriminator) — an UNDAMAGED target yields NO Clue. With
/// the pre-fix `condition: null` the rider would fire unconditionally and this
/// assertion would fail.
#[test]
fn sold_out_no_clue_when_target_undamaged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Sold Out", true, SOLD_OUT)
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    // No damage record this turn.

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Exile);
    assert_eq!(
        battlefield_named(outcome.state(), "Clue"),
        0,
        "undamaged target must NOT yield a Clue token (rider must be gated)"
    );
}

// ---------------------------------------------------------------------------
// Consuming Ashes — "If it had mana value 3 or less" (Cmc, LKI snapshot).
// Surveil is not auto-answered by the driver, so the resolved-and-gated surveil
// surfaces as a `SurveilChoice` halt; an ungated path resolves to Priority.
// ---------------------------------------------------------------------------

/// RUNTIME (positive) — CR 202.3 + CR 400.7. A mana-value-2 creature exiled by
/// Consuming Ashes satisfies "had mana value 3 or less" against its LKI snapshot,
/// so surveil 2 resolves and the pipeline halts at the SurveilChoice prompt.
#[test]
fn consuming_ashes_surveils_when_target_mana_value_le_3() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consuming Ashes", true, CONSUMING_ASHES)
        .id();
    // MV 2 ≤ 3.
    let mut victim_builder = scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    victim_builder.with_mana_cost(ManaCost::generic(2));
    let victim = victim_builder.id();
    // Library cards so surveil 2 has something to look at (else no prompt).
    scenario.add_spell_to_library_top(P0, "Filler A", true);
    scenario.add_spell_to_library_top(P0, "Filler B", true);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Exile);
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::SurveilChoice { .. }
        ),
        "MV-2 target must gate surveil ON (halt at SurveilChoice), got {:?}",
        outcome.final_waiting_for()
    );
}

/// RUNTIME (negative / discriminator) — a mana-value-5 creature fails the gate;
/// surveil does NOT happen and the pipeline never surfaces a SurveilChoice. With
/// the pre-fix `condition: null` the surveil would fire and this would fail.
#[test]
fn consuming_ashes_no_surveil_when_target_mana_value_gt_3() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Consuming Ashes", true, CONSUMING_ASHES)
        .id();
    // MV 5 > 3.
    let mut victim_builder = scenario.add_creature(P1, "Hulking Brute", 5, 5);
    victim_builder.with_mana_cost(ManaCost::generic(5));
    let victim = victim_builder.id();
    scenario.add_spell_to_library_top(P0, "Filler A", true);
    scenario.add_spell_to_library_top(P0, "Filler B", true);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Exile);
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::SurveilChoice { .. }
        ),
        "MV-5 target must gate surveil OFF (no SurveilChoice), got {:?}",
        outcome.final_waiting_for()
    );
}

// ---------------------------------------------------------------------------
// Wisecrack — "If that creature is attacking" (Attacking, present-tense LIVE).
// ---------------------------------------------------------------------------

/// RUNTIME (positive) — CR 508.1b. An ATTACKING target creature triggers the
/// rider: Wisecrack deals 2 damage to that creature's controller (P1).
#[test]
fn wisecrack_damages_controller_when_target_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Wisecrack", true, WISECRACK)
        .id();
    let victim = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    // CR 508.1b: P1's creature is attacking (live combat state read by the
    // present-tense "is attacking" predicate).
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(victim, P0)],
        ..Default::default()
    });

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_life_delta(P1, -2);
}

/// RUNTIME (negative / discriminator) — a NON-attacking target leaves the
/// controller's life unchanged. With the pre-fix `condition: null` the 2 damage
/// would land unconditionally and this would fail.
#[test]
fn wisecrack_no_controller_damage_when_target_not_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Wisecrack", true, WISECRACK)
        .id();
    let victim = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    // No combat — the creature is not attacking.

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_life_delta(P1, 0);
}

// ---------------------------------------------------------------------------
// Driftgloom Coyote — ETB trigger "If that creature had power 2 or less"
// (PtComparison, LKI snapshot). Drives the real ETB-trigger path: the creature
// is cast from hand, enters, its ETB trigger goes on the stack, targets the
// opponent's creature, exiles it, and reads the exiled creature's LKI power.
// ---------------------------------------------------------------------------

/// RUNTIME (positive) — CR 208.1 + CR 400.7. A power-2 creature exiled by
/// Driftgloom's ETB satisfies "had power 2 or less" against its LKI snapshot, so
/// Driftgloom gets a +1/+1 counter.
#[test]
fn driftgloom_counter_when_exiled_creature_power_le_2() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let coyote = scenario
        .add_creature_to_hand_from_oracle(P0, "Driftgloom Coyote", 2, 2, DRIFTGLOOM)
        .id();
    let victim = scenario.add_creature(P1, "Power Two", 2, 2).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(coyote).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Exile);
    assert_eq!(
        plus_counters(outcome.state(), coyote),
        1,
        "power-2 exiled creature must yield a +1/+1 counter on Driftgloom"
    );
}

/// RUNTIME (negative / discriminator) — a power-3 creature fails the gate; no
/// counter. With the pre-fix `condition: null` the counter would land
/// unconditionally and this would fail.
#[test]
fn driftgloom_no_counter_when_exiled_creature_power_gt_2() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let coyote = scenario
        .add_creature_to_hand_from_oracle(P0, "Driftgloom Coyote", 2, 2, DRIFTGLOOM)
        .id();
    let victim = scenario.add_creature(P1, "Power Three", 3, 3).id();
    let mut runner = scenario.build();

    let outcome = runner.cast(coyote).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Exile);
    assert_eq!(
        plus_counters(outcome.state(), coyote),
        0,
        "power-3 exiled creature must NOT yield a counter (rider must be gated)"
    );
}

// ---------------------------------------------------------------------------
// Brackish Blunder — "If it was tapped" (Tapped, LKI snapshot). The bounce
// (Return to hand) moves the creature OFF the battlefield BEFORE the rider
// resolves, so the rider reads the exit-time tap state captured into the LKI
// snapshot (CR 110.5d: an object not on the battlefield is neither tapped nor
// untapped — the live object cannot answer). This pair is the discriminator:
// tapped → Map token; untapped → none.
//
// Revert-probe (MEASURED): reverting the `filter.rs` zone-change `Tapped` arm
// from the `lki_cache` read back to `=> false` makes the TAPPED case fail —
// the Map token never appears (the rider reads false post-bounce).
// ---------------------------------------------------------------------------

/// RUNTIME (positive) — CR 110.5 + CR 400.7. A TAPPED creature bounced by
/// Brackish Blunder satisfies "if it was tapped" against its exit-time LKI
/// snapshot, so a Map token is created.
#[test]
fn brackish_blunder_creates_map_when_target_was_tapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Brackish Blunder", true, BRACKISH_BLUNDER)
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    // CR 110.5a: tap the victim so the exit-time LKI snapshot records tapped=true.
    runner.state_mut().objects.get_mut(&victim).unwrap().tapped = true;

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Hand);
    assert_eq!(
        battlefield_named(outcome.state(), "Map"),
        1,
        "tapped bounced creature must yield a Map token (rider fires)"
    );
}

/// RUNTIME (negative / discriminator) — an UNTAPPED bounced creature fails the
/// gate; NO Map token. This negative guards the PARSER half: if the rider's
/// condition parsed to `null` (pre-recognizer), it would fire UNCONDITIONALLY and
/// the untapped case would wrongly create a Map, failing this assertion. The
/// POSITIVE test above guards the RUNTIME half (the `filter.rs` lki_cache arm).
/// Together the pair is non-vacuous on both halves.
#[test]
fn brackish_blunder_no_map_when_target_untapped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Brackish Blunder", true, BRACKISH_BLUNDER)
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    // Victim left UNTAPPED (default).

    let outcome = runner.cast(spell).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Hand);
    assert_eq!(
        battlefield_named(outcome.state(), "Map"),
        0,
        "untapped bounced creature must NOT yield a Map token (rider gated)"
    );
}
