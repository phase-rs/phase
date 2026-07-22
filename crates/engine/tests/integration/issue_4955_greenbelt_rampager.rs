//! Regression for issue #4955: Greenbelt Rampager's ETB —
//! "When this creature enters, pay {E}{E} (two energy counters). If you
//! can't, return this creature to its owner's hand and you get {E}." — always
//! bounced the creature back to its owner's hand, even when the controller
//! had 2+ energy and actually paid the cost.
//!
//! Root cause (CR 608.2c + CR 118.1 + CR 118.3): the generic "if you can't"
//! rider lowers to `AbilityCondition::Not { ZoneChangedThisWay { Any } }` — a
//! proxy that reads `state.last_zone_changed_ids`, which is populated only by
//! effects that move an object between zones (search/exile/sacrifice/…).
//! `Effect::PayCost` (the "pay {E}{E}" instruction) deducts energy from the
//! player's pool and moves no object anywhere, so the zone-change ledger
//! stayed empty regardless of whether the payment actually succeeded —
//! `Not { ZoneChangedThisWay { Any } }` was therefore true UNCONDITIONALLY,
//! firing the bounce rider every time regardless of affordability.
//!
//! Fix: `rewrite_cant_rider_for_non_zone_change_parent`
//! (`crates/engine/src/parser/oracle_effect/mod.rs`) — already carved out for
//! the analogous `Effect::TurnFaceUp` class (Etrata, Deadly Fugitive: "Turn
//! this creature face up. If you can't, exile it …") — now also recognizes
//! `Effect::PayCost` as a zone-change-ledger-invisible parent and rewrites the
//! rider to `Not { OptionalEffectPerformed }`. That signal is fed by the
//! resolution-time cost-payment authority's `cost_payment_failed_flag`
//! (`game::effects::pay::resolve` / `resolve_ability_cost_payment`), which
//! correctly distinguishes a paid {E}{E} from an unpaid one — and, via the
//! mandatory-rider seed in `resolve_ability_chain`
//! (`game::effects::mod::mandatory_parent_effect_performed` falls through to
//! its `_ => true` default for `PayCost`, so the seed reduces to exactly
//! `!cost_payment_failed_flag`), sets `optional_effect_performed` iff the
//! payment succeeded.
//!
//! Both tests drive the real casting + ETB-trigger pipeline against the
//! verified Oracle text (Scryfall, 2026-07-14): {G} Elephant, 3/4.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GREENBELT_RAMPAGER_ORACLE: &str = "When this creature enters, pay {E}{E} (two energy \
     counters). If you can't, return this creature to its owner's hand and you get {E}.";

/// Build a scenario with Greenbelt Rampager in P0's hand, funded for its real
/// {G} cost, with P0's starting energy set to `starting_energy`.
fn scenario_with_rampager(starting_energy: u32) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rampager = scenario
        .add_creature_to_hand_from_oracle(P0, "Greenbelt Rampager", 3, 4, GREENBELT_RAMPAGER_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Green,
            ObjectId(9_999),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .energy = starting_energy;

    (runner, rampager)
}

fn energy_of(state: &engine::types::game_state::GameState) -> u32 {
    player_energy(state, P0)
}

fn player_energy(state: &engine::types::game_state::GameState, player: PlayerId) -> u32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap_or_else(|| panic!("player {player:?} exists"))
        .energy
}

/// Has enough energy ({E}{E} = 2) and pays it: the creature STAYS on the
/// battlefield, the two energy are spent, and the bounce rider's "you get
/// {E}" must NOT also fire on the paid branch (net energy: 2 -> 0, not 3).
#[test]
fn greenbelt_rampager_pays_energy_and_stays_on_battlefield() {
    let (mut runner, rampager) = scenario_with_rampager(2);

    let outcome = runner.cast(rampager).resolve();

    outcome.assert_zone(&[rampager], Zone::Battlefield);
    let energy = energy_of(outcome.state());
    assert_eq!(
        energy, 0,
        "paying {{E}}{{E}} must deduct both energy and must NOT also grant the \
         bounce rider's {{E}}; energy={energy}"
    );
}

/// Doesn't have enough energy (0 available, needs 2) and can't pay: the
/// creature BOUNCES to its owner's hand and the controller gets {E} (net
/// energy: 0 -> 1). This is the issue #4955 regression assertion in reverse —
/// before the fix, this branch was the ONLY branch, even when energy was
/// available (see the paid-branch test above).
#[test]
fn greenbelt_rampager_cant_pay_energy_bounces_to_hand() {
    let (mut runner, rampager) = scenario_with_rampager(0);

    let outcome = runner.cast(rampager).resolve();

    outcome.assert_zone(&[rampager], Zone::Hand);
    let energy = energy_of(outcome.state());
    assert_eq!(
        energy, 1,
        "an unpayable {{E}}{{E}} must bounce the creature to hand and grant {{E}} \
         exactly once; energy={energy}"
    );
}

/// Maintainer review on PR #5869 (stale-flag blocker): a successful mandatory
/// `PayCost` must not inherit a failure from an EARLIER, unrelated resolution.
/// `cost_payment_failed_flag` is only ever set on failure; before the fix,
/// `pay::resolve` never cleared it, so the mandatory-rider seed's
/// `!cost_payment_failed_flag` guard saw the stale `true`, skipped seeding
/// `optional_effect_performed` for a payment that SUCCEEDED, and the
/// `Not { OptionalEffectPerformed }` bounce rider fired anyway.
///
/// Production-path fixture: the same Rampager is cast twice. The first cast's
/// {E}{E} is unpayable (0 energy) — it bounces, grants {E}, and leaves the
/// global failure flag set. The second cast, after topping energy up to
/// exactly {E}{E}, pays successfully and MUST stay on the battlefield with
/// both energy spent and no rider {E}.
#[test]
fn greenbelt_rampager_pays_after_earlier_unpayable_payment_left_stale_state() {
    let (mut runner, rampager) = scenario_with_rampager(0);

    // First resolution: unpayable payment (the stale-flag producer).
    let outcome = runner.cast(rampager).resolve();
    outcome.assert_zone(&[rampager], Zone::Hand);
    assert_eq!(
        energy_of(runner.state()),
        1,
        "sanity: the unpayable first cast must bounce and grant exactly {{E}}"
    );

    // Second resolution: fund the recast ({G}) and top energy up to exactly
    // the {E}{E} the ETB cost needs (1 from the rider + 1 here).
    runner.state_mut().add_mana_to_pool(
        P0,
        ManaUnit::new(ManaType::Green, ObjectId(9_998), false, vec![]),
    );
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .energy = 2;

    let outcome = runner.cast(rampager).resolve();

    outcome.assert_zone(&[rampager], Zone::Battlefield);
    let energy = energy_of(outcome.state());
    assert_eq!(
        energy, 0,
        "a successfully paid {{E}}{{E}} after an earlier unpayable resolution \
         must keep the creature on the battlefield and must NOT fire the bounce \
         rider (stale cost_payment_failed_flag); energy={energy}"
    );
}

// ---------------------------------------------------------------------------
// Controller/owner divergence — maintainer review on PR #5869
// ---------------------------------------------------------------------------
//
// The two tests above never distinguish owner from controller (P0 is always
// both). This section drives a REAL stolen/reanimated-permanent scenario
// through the production cast + ETB pipeline: Greenbelt Rampager starts in
// P0's graveyard (P0 is its owner throughout), and P1 reanimates it with a
// real Oracle-text-parsed spell — "Put target creature card from a graveyard
// onto the battlefield under your control." (the first sentence of
// Necromantic Summons, verified real Oracle text) — which lowers to
// `Effect::ChangeZone { enters_under: Some(ControllerRef::You), .. }` (CR
// 110.2a). That makes P1 the CONTROLLER of an object P0 still OWNS the moment
// it enters, exactly the fixture the review asked for, driven through the
// real production resolver (`change_zone::resolve` -> real ETB trigger scan
// -> real `pay::resolve`), not a hand-built synthetic AST.
//
// CR 109.5: "you"/"your" in an ability always means that ability's
// controller. Greenbelt Rampager's ETB ability is controlled by whoever
// controls Greenbelt Rampager when it enters (CR 603.3d) — P1 here — so both
// the "{E}{E}" cost and the "you get {E}" reward must bind to P1, never P0.
// The bounce destination ("return this creature to its OWNER's hand") is the
// one explicitly owner-relative phrase in the Oracle text and must go to P0
// regardless of who controls it.

const NECROMANTIC_SUMMONS_FIRST_SENTENCE: &str =
    "Put target creature card from a graveyard onto the battlefield under your control.";

/// Build a scenario where Greenbelt Rampager sits in P0's graveyard (P0 is its
/// owner) and P1 holds a real reanimation spell that will put it onto the
/// battlefield under P1's control. Returns the runner with the reanimation
/// spell still in P1's hand (uncast) plus both object ids; the caller casts +
/// resolves the reanimation spell to drive Rampager's real ETB trigger with
/// P1 as controller and P0 as owner.
fn scenario_with_stolen_rampager(
    controller_energy: u32,
    owner_energy: u32,
) -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rampager = scenario
        .add_creature_to_graveyard(P0, "Greenbelt Rampager", 3, 4)
        .from_oracle_text(GREENBELT_RAMPAGER_ORACLE)
        .id();

    let reanimate = scenario
        .add_spell_to_hand(P1, "Test Reanimation Spell", false)
        .from_oracle_text(NECROMANTIC_SUMMONS_FIRST_SENTENCE)
        // Free cast — the divergence under test is orthogonal to the mana cost.
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();

    let mut runner = scenario.build();
    for p in runner.state_mut().players.iter_mut() {
        if p.id == P0 {
            p.energy = owner_energy;
        } else if p.id == P1 {
            p.energy = controller_energy;
        }
    }
    // `GameScenario::at_phase` hands the active player/priority to P0 (the
    // scenario default). The reanimation spell is cast by P1, so hand P1 both
    // active-player status and priority — otherwise the sorcery-speed cast is
    // rejected as "not a castable zone" (CR 601.3g: sorcery-speed casting
    // requires the caster to be the active player with an empty stack).
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };

    (runner, reanimate, rampager)
}

/// Controller (P1) has enough energy and pays; the OWNER (P0) has none. If the
/// cost payment ever read the owner's pool instead of the controller's, this
/// would wrongly fail (P0 has 0) even though the actual controller can pay.
#[test]
fn greenbelt_rampager_controller_pays_when_owner_has_no_energy() {
    let (mut runner, reanimate, rampager) = scenario_with_stolen_rampager(2, 0);

    let outcome = runner.cast(reanimate).target_object(rampager).resolve();

    outcome.assert_zone(&[rampager], Zone::Battlefield);
    let state = outcome.state();
    assert_eq!(
        state.objects.get(&rampager).map(|o| o.controller),
        Some(P1),
        "sanity: the reanimation must actually make P1 the controller"
    );
    assert_eq!(
        state.objects.get(&rampager).map(|o| o.owner),
        Some(P0),
        "sanity: reanimation changes control, never ownership (CR 110.2)"
    );

    assert_eq!(
        player_energy(state, P1),
        0,
        "the CONTROLLER's (P1) {{E}}{{E}} must be spent to pay the ETB cost"
    );
    assert_eq!(
        player_energy(state, P0),
        0,
        "the OWNER's (P0) energy must be untouched by a payment that belongs \
         to the controller, not the owner"
    );
}

/// Controller (P1) cannot pay (0 energy); the OWNER (P0) has plenty. If the
/// cost payment ever read the owner's pool instead of the controller's, this
/// would wrongly succeed (P0 has energy to spare) even though the actual
/// controller cannot pay. The creature must bounce to its OWNER's hand (P0,
/// per the Oracle text's explicit "owner's hand" wording) while the "you get
/// {E}" reward must still go to the CONTROLLER (P1), not the owner.
#[test]
fn greenbelt_rampager_cant_pay_bounces_to_owner_hand_reward_to_controller() {
    let (mut runner, reanimate, rampager) = scenario_with_stolen_rampager(0, 5);

    let outcome = runner.cast(reanimate).target_object(rampager).resolve();

    outcome.assert_zone(&[rampager], Zone::Hand);
    let state = outcome.state();
    let owner_hand = state
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .hand
        .contains(&rampager);
    assert!(
        owner_hand,
        "CR 110.2 + Oracle text ('return this creature to its owner's hand'): \
         the unpayable bounce must land in the OWNER's (P0) hand, not the \
         controller's (P1)"
    );

    assert_eq!(
        player_energy(state, P1),
        1,
        "the CONTROLLER (P1) must receive the bounce rider's {{E}}, not the owner"
    );
    assert_eq!(
        player_energy(state, P0),
        5,
        "the OWNER's (P0) energy must be untouched: an unpayable cost the \
         controller couldn't afford must not siphon from the owner's pool, \
         and the reward must not land on the owner either"
    );
}
