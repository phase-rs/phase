//! Regression tests for issue #5943: Emptiness's "if {W}{W}/{B}{B} was spent
//! to cast it" ETB riders must survive until the CR 603.4 intervening-if
//! re-check at resolution.
//!
//! Root cause: `clear_post_collection_transients` (triggers.rs) wiped
//! `colors_spent_to_cast` on ALL objects after every trigger-collection pass,
//! so by the time the conditional ETB trigger resolved (or was even collected,
//! after the cast-event collection pass), the per-color tally was gone and the
//! rider silently did nothing. The fix preserves the tally for objects on the
//! Battlefield or Stack (mirroring `cast_from_zone`), clears all five
//! cast-payment stamps for objects in every other zone (a countered/fizzled
//! spell loses its payment record at the next collection pass) and at
//! battlefield exit via `GameObject::clear_cast_payment_stamps` (CR 400.7),
//! and resets the stamps on every spell-copy birth (CR 707.10 / CR 707.12).
//!
//! Dropped row (plan R7b, prepared-copy sibling): `prepare.rs`'s exile-copy
//! is out of the stack-copy-birth class — a later cast of the prepared copy
//! re-stamps through the normal `casting.rs` payment blocks (see the
//! `GameObject::clear_cast_payment_stamps` doc) — and a prepare-class fixture
//! is not cheaply constructible here.
//!
//! https://github.com/phase-rs/phase/issues/5943

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{TargetRef, TriggerCondition};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Emptiness — verbatim Oracle text (Scryfall, fetched 2026-07-22).
/// {4}{W/B}{W/B} Creature — Elemental Incarnation 3/5.
const EMPTINESS_ORACLE: &str = "When this creature enters, if {W}{W} was spent to cast it, return target creature card with mana value 3 or less from your graveyard to the battlefield.\nWhen this creature enters, if {B}{B} was spent to cast it, put three -1/-1 counters on up to one target creature.\nEvoke {W/B}{W/B}";

/// Verbatim white-rider target phrase used to identify the conditional ETB
/// trigger inside `OrderTriggers` prompts.
const WHITE_RIDER_NEEDLE: &str = "return target creature card";

fn add_mana(runner: &mut GameRunner, player: PlayerId, mana_type: ManaType, amount: u32) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(mana_type, dummy, false, vec![]));
    }
}

/// Build the shared fixture: Emptiness in P0's hand ({4}{W/B}{W/B}, verbatim
/// Oracle text, Evoke keyword) and a Grizzly Bears (vanilla MV<=3 creature
/// card) in P0's graveyard as white-rider bait.
fn emptiness_scenario() -> (GameScenario, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let emptiness = {
        let mut builder = scenario.add_creature_to_hand(P0, "Emptiness", 3, 5);
        builder.from_oracle_text_with_keywords(&["Evoke {W/B}{W/B}"], EMPTINESS_ORACLE);
        // {4}{W/B}{W/B} — CR 107.4e: each hybrid pip is payable with either
        // white or black mana, and the color actually paid is what the
        // CR 601.2h per-color tally records.
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlack, ManaCostShard::WhiteBlack],
            generic: 4,
        });
        builder.id()
    };
    (scenario, emptiness, bears)
}

/// How the test wants same-controller simultaneous triggers ordered
/// (CR 603.3b).
#[derive(Clone, Copy)]
enum OrderPolicy {
    /// Place the white rider (description contains `WHITE_RIDER_NEEDLE`) on
    /// TOP of this controller's stack slot so it resolves FIRST.
    WhiteRiderResolvesFirst,
    /// Place the white rider at the BOTTOM so every other trigger (the evoke
    /// sacrifice) resolves before it.
    WhiteRiderResolvesLast,
    /// Identity order — used where ordering is immaterial to the assertion.
    Identity,
}

/// Drive a cast to completion through `apply()` (precedent:
/// `drive_sowing_mycospawn`, triggers.rs). Answers:
/// - `AlternativeCastChoice` with `decision` (CR 601.2b / CR 702.74a),
/// - `OrderTriggers` per `order_policy` (CR 603.3b: output position 0 is the
///   BOTTOM of the controller's slot, i.e. resolves last),
/// - target prompts with `trigger_target`,
/// - `CopyRetarget` by keeping current targets (CR 707.10c),
/// - passes priority otherwise.
///
/// Returns the zone `watch` occupied when a `TriggerCondition::ManaColorSpent`
/// conditional entry was last about to resolve — the CR 603.4 re-check
/// reach-guard for R1/R2.
fn drive_emptiness(
    runner: &mut GameRunner,
    decision: Option<AlternativeCastDecision>,
    order_policy: OrderPolicy,
    trigger_target: Option<ObjectId>,
    watch: ObjectId,
) -> Option<Zone> {
    let mut zone_at_rider_resolution = None;
    for _ in 0..120 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::AlternativeCastChoice { .. } => {
                let choice =
                    decision.expect("AlternativeCastChoice surfaced without a declared decision");
                runner
                    .act(GameAction::ChooseAlternativeCast { choice })
                    .expect("alternative-cast decision must be accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let rider_idx = triggers
                    .iter()
                    .position(|t| t.description.to_lowercase().contains(WHITE_RIDER_NEEDLE));
                // CR 603.3b: `order[position] = input index`; output position 0
                // is the bottom of this controller's slot (resolves LAST).
                let order: Vec<usize> = match (order_policy, rider_idx) {
                    (OrderPolicy::Identity, _) | (_, None) => (0..triggers.len()).collect(),
                    (OrderPolicy::WhiteRiderResolvesFirst, Some(idx)) => {
                        let mut order: Vec<usize> =
                            (0..triggers.len()).filter(|i| *i != idx).collect();
                        order.push(idx); // top of slot → resolves first
                        order
                    }
                    (OrderPolicy::WhiteRiderResolvesLast, Some(idx)) => {
                        let mut order = vec![idx]; // bottom of slot → resolves last
                        order.extend((0..triggers.len()).filter(|i| *i != idx));
                        order
                    }
                };
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("explicit OrderTriggers must succeed");
            }
            WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::TargetSelection { .. }
            | WaitingFor::MultiTargetSelection { .. } => {
                let target =
                    trigger_target.expect("target prompt surfaced without a declared target");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .or_else(|_| {
                        runner.act(GameAction::SelectTargets {
                            targets: vec![TargetRef::Object(target)],
                        })
                    })
                    .expect("target selection must be accepted");
            }
            WaitingFor::CopyRetarget { .. } => {
                // CR 707.10c: keep the copy's current targets.
                runner
                    .act(GameAction::KeepAllCopyTargets)
                    .expect("keeping copy targets must be accepted");
            }
            WaitingFor::Priority { .. } => {
                // CR 603.4 reach-guard: record where the watched object is when
                // a ManaColorSpent-conditioned trigger is about to resolve.
                if let Some(entry) = runner.state().stack.back() {
                    if matches!(
                        &entry.kind,
                        StackEntryKind::TriggeredAbility {
                            condition: Some(TriggerCondition::ManaColorSpent { .. }),
                            ..
                        }
                    ) {
                        zone_at_rider_resolution = Some(runner.state().objects[&watch].zone);
                    }
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority must be accepted");
            }
            other => panic!("unhandled waiting state in emptiness drive loop: {other:?}"),
        }
    }
    zone_at_rider_resolution
}

fn cast_spell_action(runner: &GameRunner, spell: ObjectId) -> GameAction {
    GameAction::CastSpell {
        object_id: spell,
        card_id: runner.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    }
}

fn minus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Minus1Minus1)
        .copied()
        .unwrap_or(0)
}

/// R1 — evoking with {W}{W} fires the white rider when the player orders the
/// conditional ETB to resolve BEFORE the evoke sacrifice (CR 603.3b), i.e.
/// while Emptiness is still on the battlefield at the CR 603.4 re-check.
#[test]
fn evoke_ww_fires_white_rider_etb_first() {
    let (scenario, emptiness, bears) = emptiness_scenario();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::White, 2);

    runner
        .act(cast_spell_action(&runner, emptiness))
        .expect("casting Emptiness must be accepted");
    let rider_zone = drive_emptiness(
        &mut runner,
        Some(AlternativeCastDecision::Alternative),
        OrderPolicy::WhiteRiderResolvesFirst,
        Some(bears),
        emptiness,
    );

    // Reach-guard: the conditional resolved while Emptiness was on the
    // battlefield (rider ordered before the evoke sacrifice).
    assert_eq!(
        rider_zone,
        Some(Zone::Battlefield),
        "white rider must resolve while Emptiness is still on the battlefield"
    );
    // CR 603.4 + CR 601.2h: the {W}{W} tally must still be readable at the
    // resolution re-check — the rider returns the Bears.
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "white rider must return Grizzly Bears to the battlefield"
    );
    // CR 702.74a: the evoke sacrifice still happens.
    assert_eq!(
        runner.state().objects[&emptiness].zone,
        Zone::Graveyard,
        "evoked Emptiness must be sacrificed"
    );
    // Negative (paired with the positive return above as reach-guard): the
    // {B}{B} rider must NOT have fired — no -1/-1 counters anywhere.
    assert_eq!(
        minus_counters(&runner, bears),
        0,
        "black rider must not fire on a {{W}}{{W}} cast"
    );
}

/// R2 — same cast, opposite CR 603.3b ordering: the evoke sacrifice resolves
/// first, so Emptiness is already in the graveyard at the rider's CR 603.4
/// re-check. The rider must STILL fire via the latched TriggerSourceContext
/// snapshot (captured at collection time).
#[test]
fn evoke_ww_fires_white_rider_sac_first() {
    let (scenario, emptiness, bears) = emptiness_scenario();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::White, 2);

    runner
        .act(cast_spell_action(&runner, emptiness))
        .expect("casting Emptiness must be accepted");
    let rider_zone = drive_emptiness(
        &mut runner,
        Some(AlternativeCastDecision::Alternative),
        OrderPolicy::WhiteRiderResolvesLast,
        Some(bears),
        emptiness,
    );

    // Reach-guard: the conditional resolved AFTER the sacrifice — Emptiness
    // was already in the graveyard.
    assert_eq!(
        rider_zone,
        Some(Zone::Graveyard),
        "white rider must resolve after the evoke sacrifice in this ordering"
    );
    // CR 702.74a: evoke sacrifice intact.
    assert_eq!(
        runner.state().objects[&emptiness].zone,
        Zone::Graveyard,
        "evoked Emptiness must be sacrificed"
    );
    // CR 603.4: the latched snapshot still carries the {W}{W} tally.
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "white rider must fire via the latched source snapshot after the sacrifice"
    );
}

/// R3 — evoking with {B}{B} fires the black rider: three -1/-1 counters on the
/// chosen target. Negative: the graveyard creature stays put.
#[test]
fn evoke_bb_fires_black_rider() {
    let (mut scenario, emptiness, bears) = emptiness_scenario();
    let victim = scenario.add_vanilla(P1, 4, 4);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Black, 2);

    runner
        .act(cast_spell_action(&runner, emptiness))
        .expect("casting Emptiness must be accepted");
    drive_emptiness(
        &mut runner,
        Some(AlternativeCastDecision::Alternative),
        OrderPolicy::Identity,
        Some(victim),
        emptiness,
    );

    // CR 603.4 + CR 601.2h: the {B}{B} tally fires the black rider.
    assert_eq!(
        minus_counters(&runner, victim),
        3,
        "black rider must put three -1/-1 counters on the target"
    );
    // Negative (reach-guard: the counters landed above): white rider did not fire.
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "white rider must not fire on a {{B}}{{B}} cast"
    );
    // CR 702.74a: evoke sacrifice intact.
    assert_eq!(runner.state().objects[&emptiness].zone, Zone::Graveyard);
}

/// R4 — evoking with one white + one black mana fires NEITHER rider
/// (each needs two pips of a single color). Reach-guard: Emptiness entered
/// and the evoke sacrifice resolved.
#[test]
fn evoke_mixed_wb_fires_neither() {
    let (mut scenario, emptiness, bears) = emptiness_scenario();
    let victim = scenario.add_vanilla(P1, 4, 4);
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::White, 1);
    add_mana(&mut runner, P0, ManaType::Black, 1);

    runner
        .act(cast_spell_action(&runner, emptiness))
        .expect("casting Emptiness must be accepted");
    drive_emptiness(
        &mut runner,
        Some(AlternativeCastDecision::Alternative),
        OrderPolicy::Identity,
        None,
        emptiness,
    );

    // Reach-guard: Emptiness entered the battlefield and its evoke sacrifice
    // trigger resolved (CR 702.74a) — the cast went all the way through.
    assert_eq!(
        runner.state().objects[&emptiness].zone,
        Zone::Graveyard,
        "evoked Emptiness must have entered and been sacrificed"
    );
    // Neither rider: W tally is 1 and B tally is 1 (CR 601.2h).
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "white rider must not fire on a mixed {{W}}{{B}} evoke"
    );
    assert_eq!(
        minus_counters(&runner, victim),
        0,
        "black rider must not fire on a mixed {{W}}{{B}} evoke"
    );
}

/// R5 — hard-casting {4}{W/B}{W/B} paying the hybrids with two white fires the
/// white rider, and Emptiness is NOT sacrificed (no evoke).
#[test]
fn hard_cast_ww_fires_white() {
    let (scenario, emptiness, bears) = emptiness_scenario();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Colorless, 4);
    add_mana(&mut runner, P0, ManaType::White, 2);

    runner
        .act(cast_spell_action(&runner, emptiness))
        .expect("casting Emptiness must be accepted");
    drive_emptiness(
        &mut runner,
        Some(AlternativeCastDecision::Normal),
        OrderPolicy::Identity,
        Some(bears),
        emptiness,
    );

    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "white rider must fire on a hard cast paying {{W}}{{W}} for the hybrids"
    );
    assert_eq!(
        runner.state().objects[&emptiness].zone,
        Zone::Battlefield,
        "a hard-cast Emptiness must not be sacrificed"
    );
    assert_eq!(minus_counters(&runner, bears), 0);
}

/// R6 — an intermediate trigger-collection batch between the cast and the ETB
/// (a "whenever you cast a creature spell" draw trigger) must not wipe the
/// per-color tally: the post-collection transient clear runs after the cast
/// batch, while Emptiness is still a spell on the Stack.
#[test]
fn evoke_ww_survives_intermediate_batch() {
    let (mut scenario, emptiness, bears) = emptiness_scenario();
    scenario.add_creature_from_oracle(
        P0,
        "Spell Scribe",
        1,
        1,
        "Whenever you cast a creature spell, draw a card.",
    );
    let filler = scenario.add_card_to_library_top(P0, "Filler Card");
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::White, 2);

    runner
        .act(cast_spell_action(&runner, emptiness))
        .expect("casting Emptiness must be accepted");
    drive_emptiness(
        &mut runner,
        Some(AlternativeCastDecision::Alternative),
        OrderPolicy::WhiteRiderResolvesFirst,
        Some(bears),
        emptiness,
    );

    // Reach-guard: the intermediate cast-trigger batch really ran (the draw
    // resolved), so `clear_post_collection_transients` executed while the
    // Emptiness spell was on the Stack.
    let p0 = runner.state().players.iter().find(|p| p.id == P0).unwrap();
    assert!(
        p0.hand.contains(&filler),
        "cast trigger must have drawn the filler card"
    );
    // CR 601.2h + CR 603.4: the tally survived the intermediate batch.
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Battlefield,
        "white rider must still fire after an intermediate trigger batch"
    );
}

/// R7 — a COPY of a hard-cast Emptiness (CR 707.10: a copy is not cast, and no
/// mana was spent on it) enters the battlefield firing NEITHER rider, while
/// the original fires its white rider normally.
#[test]
fn copy_of_emptiness_fires_neither() {
    let (mut scenario, emptiness, bears1) = emptiness_scenario();
    // Second bait: if the copy's white rider wrongly fired, it would return
    // this one too.
    let bears2 = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let copy_spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Spell Duplicator",
            true,
            "Copy target creature spell you control. You may choose new targets for the copy.",
        )
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, P0, ManaType::Colorless, 4);
    add_mana(&mut runner, P0, ManaType::White, 2);

    // Hard-cast Emptiness paying the hybrids with {W}{W}.
    runner
        .act(cast_spell_action(&runner, emptiness))
        .expect("casting Emptiness must be accepted");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::AlternativeCastChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Normal,
            })
            .expect("normal-cast decision must be accepted");
    }
    // Respond with the copy instant while Emptiness is still on the Stack.
    runner
        .act(cast_spell_action(&runner, copy_spell))
        .expect("casting the copy instant must be accepted");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(emptiness)),
            })
            .expect("targeting the Emptiness spell must be accepted");
    }
    drive_emptiness(
        &mut runner,
        None,
        OrderPolicy::Identity,
        Some(bears1),
        emptiness,
    );

    // Reach-guard 1: the copy resolved into a token permanent (CR 707.10f +
    // CR 608.3f).
    let copy_token = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Emptiness" && o.is_token && o.zone == Zone::Battlefield)
        .map(|o| o.id)
        .expect("the spell copy must have become a battlefield token");
    // Reach-guard 2: the ORIGINAL's white rider fired (the {W}{W} payment is
    // real and readable at the CR 603.4 re-check).
    let bears_on_battlefield = [bears1, bears2]
        .iter()
        .filter(|id| runner.state().objects[*id].zone == Zone::Battlefield)
        .count();
    let bears_in_graveyard = [bears1, bears2]
        .iter()
        .filter(|id| runner.state().objects[*id].zone == Zone::Graveyard)
        .count();
    assert_eq!(
        bears_on_battlefield, 1,
        "exactly one Bears returned: the original's rider fired once"
    );
    // Negative (reach-guarded above): the copy fired NEITHER rider — CR 707.10:
    // a spell copy is not cast and no mana was spent to cast it.
    assert_eq!(
        bears_in_graveyard, 1,
        "the copy's white rider must NOT fire — one Bears stays in the graveyard"
    );
    assert_eq!(
        minus_counters(&runner, copy_token),
        0,
        "the copy's black rider must not fire either"
    );
    // The original also entered and was not sacrificed (hard cast).
    assert_eq!(runner.state().objects[&emptiness].zone, Zone::Battlefield);
}

/// R10 — battlefield-exit stamp hygiene (CR 400.7): a creature cast with mana,
/// destroyed, then reanimated re-enters with ALL cast-payment stamps at their
/// no-payment defaults (the Satoru-class "no mana was spent to cast it"
/// condition would be TRUE for the reanimated entry). A fresh-cast sibling
/// keeps its stamps (negative pair).
#[test]
fn blink_reanimate_clears_cast_payment_stamps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ogre = {
        let mut b = scenario.add_creature_to_hand(P0, "Gray Ogre", 2, 2);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        });
        b.id()
    };
    let sibling = {
        let mut b = scenario.add_creature_to_hand(P0, "Gray Ogre Sibling", 2, 2);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        });
        b.id()
    };
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Plain Murder", true, "Destroy target creature.")
        .id();
    let zombify = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Plain Zombify",
            false,
            "Return target creature card from your graveyard to the battlefield.",
        )
        .id();
    let mut runner = scenario.build();

    // Cast the ogre paying {1}{W} — CR 601.2h stamps land.
    add_mana(&mut runner, P0, ManaType::White, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    let outcome = runner.cast(ogre).resolve();
    outcome.assert_zone(&[ogre], Zone::Battlefield);
    // Reach-guard: the stamps existed before the exit.
    assert_eq!(
        runner.state().objects[&ogre].mana_spent_to_cast_amount,
        2,
        "fresh cast must stamp the spent-mana amount"
    );

    // Destroy it (battlefield exit → CR 400.7 reset).
    let outcome = runner.cast(murder).target_object(ogre).resolve();
    outcome.assert_zone(&[ogre], Zone::Graveyard);

    // Reanimate it.
    let outcome = runner.cast(zombify).target_object(ogre).resolve();
    outcome.assert_zone(&[ogre], Zone::Battlefield);

    // CR 400.7: the re-entering permanent is a new object with no memory of
    // the cast that paid for its previous existence.
    let reanimated = &runner.state().objects[&ogre];
    assert_eq!(
        reanimated.mana_spent_to_cast_amount, 0,
        "reanimated entry must read 0 spent mana (Satoru-class condition true)"
    );
    assert!(
        !reanimated.mana_spent_to_cast,
        "reanimated entry must not claim mana was spent"
    );
    assert!(
        reanimated.colors_spent_to_cast.is_empty(),
        "reanimated entry must carry no per-color tally"
    );
    assert!(
        reanimated.mana_spent_source_snapshots.is_empty(),
        "reanimated entry must carry no payment-source snapshots"
    );

    // Negative pair: a fresh-cast sibling KEEPS its stamps on the battlefield.
    add_mana(&mut runner, P0, ManaType::White, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    let outcome = runner.cast(sibling).resolve();
    outcome.assert_zone(&[sibling], Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&sibling].mana_spent_to_cast_amount,
        2,
        "fresh-cast sibling must keep its stamps (Satoru-class condition false)"
    );
}

/// R10-counter — the countered-spell path: a spell that is COUNTERED goes
/// Stack → Graveyard, so `reset_for_battlefield_exit` (the CR 400.7
/// battlefield-exit clear) never runs on it. The post-collection transient
/// clear must wipe all five cast-payment stamps on the graveyard object at
/// the next trigger-collection pass — otherwise reanimating it produces a
/// battlefield permanent with a phantom payment record (the Satoru-class
/// "no mana was spent to cast it" condition would wrongly read false, and
/// `CastManaSpentMetric::Total` would wrongly read 2).
#[test]
fn countered_spell_clears_cast_payment_stamps_before_reanimation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Draw-bystander: guarantees a non-empty trigger-collection batch (and
    // its post-collection transient clear) runs while the countered creature
    // card sits in the graveyard — the reanimation cast itself provides that
    // batch, before Zombify resolves.
    scenario.add_creature_from_oracle(
        P0,
        "Spell Scribe",
        1,
        1,
        "Whenever you cast a spell, draw a card.",
    );
    let filler1 = scenario.add_card_to_library_top(P0, "Filler One");
    let filler2 = scenario.add_card_to_library_top(P0, "Filler Two");
    let filler3 = scenario.add_card_to_library_top(P0, "Filler Three");
    let ogre = {
        let mut b = scenario.add_creature_to_hand(P0, "Gray Ogre", 2, 2);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        });
        b.id()
    };
    let counter = scenario
        .add_spell_to_hand_from_oracle(P0, "Plain Cancel", true, "Counter target spell.")
        .id();
    let zombify = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Plain Zombify",
            false,
            "Return target creature card from your graveyard to the battlefield.",
        )
        .id();
    let mut runner = scenario.build();

    // Cast the ogre paying {1}{W} — CR 601.2h stamps land at cast
    // finalization while the spell is on the Stack.
    add_mana(&mut runner, P0, ManaType::White, 1);
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    runner
        .act(cast_spell_action(&runner, ogre))
        .expect("casting the ogre must be accepted");
    // Reach-guard: real payment stamped while the spell is on the Stack.
    assert_eq!(
        runner.state().objects[&ogre].zone,
        Zone::Stack,
        "the ogre spell must be on the stack awaiting resolution"
    );
    assert_eq!(
        runner.state().objects[&ogre].mana_spent_to_cast_amount,
        2,
        "cast must stamp the spent-mana amount while on the stack"
    );

    // COUNTER it in response (verbatim "Counter target spell.").
    let outcome = runner.cast(counter).target_objects(&[ogre]).resolve();
    outcome.assert_zone(&[ogre], Zone::Graveyard);

    // Reanimate it. Casting Zombify collects the Spell Scribe draw trigger —
    // a non-empty batch whose post-collection clear runs while the ogre card
    // is in the graveyard, wiping the stale stamps before re-entry.
    let outcome = runner.cast(zombify).target_object(ogre).resolve();
    outcome.assert_zone(&[ogre], Zone::Battlefield);

    // Reach-guard: all three cast-trigger batches really ran (each drew a
    // filler card), so a post-collection clear executed while the ogre sat
    // in the graveyard (the Zombify cast batch at the latest).
    let p0 = runner.state().players.iter().find(|p| p.id == P0).unwrap();
    for filler in [filler1, filler2, filler3] {
        assert!(
            p0.hand.contains(&filler),
            "every cast trigger must have drawn its filler card"
        );
    }

    // CR 400.7 family: the reanimated permanent is a new object identity with
    // NO memory of the countered cast's payment — all five stamps default.
    let reanimated = &runner.state().objects[&ogre];
    assert_eq!(
        reanimated.mana_spent_to_cast_amount, 0,
        "countered spell's spent-mana amount must not leak into the reanimated \
         permanent (Satoru-class 'no mana was spent to cast it' must read true)"
    );
    assert!(
        reanimated.mana_spent_source_snapshots.is_empty(),
        "countered spell's payment-source snapshots must not leak into the \
         reanimated permanent"
    );
    assert!(
        reanimated.colors_spent_to_cast.is_empty(),
        "countered spell's per-color tally must not leak into the reanimated \
         permanent"
    );
    assert!(
        !reanimated.mana_spent_to_cast,
        "reanimated permanent must not claim mana was spent to cast it"
    );
}

/// R10-convoke — CR 702.51a: a fully-convoked cast pays no mana, so the
/// battlefield permanent's cast-payment stamps stay at their no-payment
/// defaults.
#[test]
fn fully_convoked_cast_leaves_stamps_default() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let helper1 = scenario.add_vanilla(P0, 1, 1);
    let helper2 = scenario.add_vanilla(P0, 1, 1);
    let convoker = {
        let mut b = scenario.add_creature_to_hand(P0, "Convoke Test", 2, 2);
        b.from_oracle_text_with_keywords(&["convoke"], "Convoke");
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 2,
        });
        b.id()
    };
    let mut runner = scenario.build();

    let outcome = runner
        .cast(convoker)
        .convoke_with(&[helper1, helper2])
        .resolve();
    outcome.assert_zone(&[convoker], Zone::Battlefield);

    // Reach-guard: the convoke payment really happened.
    assert!(
        runner.state().objects[&helper1].tapped && runner.state().objects[&helper2].tapped,
        "both convoke helpers must be tapped"
    );
    // CR 702.51a + CR 601.2h: no mana was spent.
    let obj = &runner.state().objects[&convoker];
    assert_eq!(obj.mana_spent_to_cast_amount, 0);
    assert!(obj.colors_spent_to_cast.is_empty());
    assert!(!obj.mana_spent_to_cast);
}

/// R7-sibling (Dawnglow-class) — the RESOLUTION-side spend-color condition
/// (`AbilityCondition::ManaColorSpent`, CR 601.2h + CR 608.2c) reads the spell
/// object's tally while it is still on the Stack: the post-collection
/// transient clear must not wipe it before the spell resolves. Verbatim
/// Dawnglow Infusion Oracle text.
///
/// NOTE (pre-existing parser gap, out of scope for issue #5943): the parser
/// currently collapses the two conjoined spend-color branches into a single
/// `GainLife` conditioned on `ManaColorSpent { White }`, dropping the {G}
/// branch. This row therefore exercises the surviving {W} branch — which is
/// also rules-correct for the real card — and adds no {G}-branch sibling so
/// the misparse is not enshrined as expected behavior.
#[test]
fn dawnglow_spend_color_condition_reads_stack_tally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Force a trigger-collection pass (and its post-collection transient
    // clear) between the cast and the spell's own resolution — without it the
    // clear never runs and the row would pass even without the Stack-zone
    // guard.
    scenario.add_creature_from_oracle(
        P0,
        "Spell Scribe",
        1,
        1,
        "Whenever you cast a spell, draw a card.",
    );
    scenario.add_card_to_library_top(P0, "Filler Card");
    let infusion = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Dawnglow Infusion",
            false,
            "You gain X life if {G} was spent to cast this spell and X life if {W} was spent to cast this spell. (Do both if {G}{W} was spent.)",
        );
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::GreenWhite, ManaCostShard::X],
            generic: 0,
        });
        b.id()
    };
    let mut scenario_runner = scenario.build();

    // Reach-guard: the compound spend-color sentence parsed without gaps.
    assert!(
        scenario_runner.state().objects[&infusion]
            .abilities
            .iter()
            .all(|a| a.effect.unimplemented_description().is_none()),
        "Dawnglow Infusion must parse without Unimplemented gaps: {:?}",
        scenario_runner.state().objects[&infusion].abilities
    );

    // Pay {X=2}{G/W} with white + colorless only, so the hybrid pip must take
    // the white mana ({W} spent: true — CR 107.4e).
    add_mana(&mut scenario_runner, P0, ManaType::White, 1);
    add_mana(&mut scenario_runner, P0, ManaType::Colorless, 2);
    let outcome = scenario_runner.cast(infusion).x(2).resolve();

    // Reach-guard: the intermediate cast-trigger batch really ran.
    outcome.assert_hand_drawn(P0, 1);
    // CR 601.2h + CR 608.2c: the {W} spend-color branch reads the Stack
    // object's surviving tally at resolution and pays out X (=2) life.
    outcome.assert_life_delta(P0, 2);
}

// ---------------------------------------------------------------------------
// Review-round fixtures: source-qualified payment provenance must survive a
// battlefield exit between trigger collection and resolution (CR 603.4 +
// CR 400.7d), via the latched TriggerSourceContext — while the re-entered
// incarnation keeps reading empty/0 (the CR 400.7 clearing that stays).
// ---------------------------------------------------------------------------

/// Marut — verbatim Oracle text (data/card-data.json, byte-identical).
/// {8} Artifact Creature — Construct 7/7.
const MARUT_ORACLE: &str = "Trample\nWhen this creature enters, if mana from a Treasure was spent to cast it, create a Treasure token for each mana from a Treasure spent to cast it. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";

/// Cloudshift — verbatim Oracle text (data/card-data.json, byte-identical).
const CLOUDSHIFT_ORACLE: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

/// A real battlefield Treasure (artifact, subtype Treasure) to serve as the
/// PRODUCING source of tagged pool mana — the `FromSource { Subtype Treasure }`
/// filter matches the payment-time snapshot of this object (precedent:
/// `issue_1156_coin_of_mastery.rs::make_treasure`).
fn make_treasure_source(runner: &mut GameRunner, card_id: u64) -> ObjectId {
    let state = runner.state_mut();
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Treasure".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Treasure".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Pool mana whose `ManaUnit::source` points at a REAL object, so the payment
/// block stamps a `ManaSpentSourceSnapshot` for each unit (CR 601.2h).
fn add_mana_from_source(
    runner: &mut GameRunner,
    player: PlayerId,
    mana_type: ManaType,
    amount: u32,
    source: ObjectId,
) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(mana_type, source, false, vec![]));
    }
}

/// Battlefield Treasure TOKENS only — Marut's rider output, disjoint from the
/// non-token Treasure the fixture uses as a mana source.
fn treasure_token_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.is_token
                && o.card_types.subtypes.iter().any(|s| s == "Treasure")
        })
        .count()
}

/// Pass priority (draining any single-slot OrderTriggers prompt with identity)
/// until `done` holds, panicking on any other waiting state.
fn pass_until(runner: &mut GameRunner, mut done: impl FnMut(&GameRunner) -> bool, what: &str) {
    for _ in 0..20 {
        if done(runner) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority must be accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("identity OrderTriggers must succeed");
            }
            other => panic!("unhandled waiting state while {what}: {other:?}"),
        }
    }
    panic!("drive loop exhausted while {what}");
}

/// R11 — the review-round fixture: Marut's source-qualified ETB rider ("create
/// a Treasure token for each mana from a Treasure spent to cast it") is
/// collected, then Marut is BLINKED (Cloudshift) before the rider resolves.
///
/// CR 603.4 + CR 400.7d: both the intervening-if re-check and the effect
/// quantity must read the payment provenance through the LATCHED trigger
/// source context — the re-entered incarnation is a new object whose stamps
/// were cleared at the battlefield exit (CR 400.7) and can only answer 0.
/// The rider must still create exactly TWO Treasures (two Treasure-tagged
/// mana units paid), and the re-entered Marut must read empty/0.
#[test]
fn marut_treasure_rider_survives_blink_via_latched_snapshots() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marut = {
        let mut b = scenario.add_creature_to_hand(P0, "Marut", 7, 7);
        b.from_oracle_text(MARUT_ORACLE);
        b.with_mana_cost(ManaCost::generic(8));
        b.id()
    };
    let cloudshift = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        });
        b.id()
    };
    let mut runner = scenario.build();

    // Two Treasure-tagged units + six untagged units pay Marut's {8}.
    let treasure = make_treasure_source(&mut runner, 9001);
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, treasure);
    add_mana(&mut runner, P0, ManaType::Colorless, 6);

    {
        let commit = runner.cast(marut).commit();
        // Reach-guard (non-degenerate fixture): the payment stamped exactly two
        // Treasure-source snapshots on the spell (CR 601.2h).
        assert_eq!(
            commit.state().objects[&marut]
                .mana_spent_source_snapshots
                .len(),
            2,
            "both Treasure-tagged mana units must snapshot their source at payment"
        );
    }

    // Resolve the SPELL only: Marut enters, its conditional ETB is collected
    // (intervening-if true at collection: 2 > 0) and sits on the stack.
    pass_until(
        &mut runner,
        |r| {
            matches!(
                r.state().stack.back().map(|e| &e.kind),
                Some(StackEntryKind::TriggeredAbility { .. })
            )
        },
        "resolving the Marut spell",
    );
    assert_eq!(
        runner.state().objects[&marut].zone,
        Zone::Battlefield,
        "Marut must be on the battlefield with its ETB rider pending"
    );

    // Blink Marut IN RESPONSE to its own pending rider (CR 405.5: the trigger
    // stays on the stack; Cloudshift, added later, is on top and resolves
    // first — CR 400.7 makes the returned Marut a new incarnation).
    add_mana(&mut runner, P0, ManaType::White, 1);
    {
        let _commit = runner.cast(cloudshift).target_object(marut).commit();
    }
    pass_until(
        &mut runner,
        |r| r.state().objects[&cloudshift].zone == Zone::Graveyard,
        "resolving Cloudshift",
    );

    // Hostile mid-state (the seam the review flagged): the rider is still
    // pending, and the ONLY surviving payment provenance is the latched
    // trigger-source snapshot — the re-entered Marut was cleared at the
    // battlefield exit (CR 400.7) and no Treasure token exists yet.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the ETB rider must still be pending after the blink"
    );
    let marut_reentered = runner
        .state()
        .objects
        .values()
        .find(|o| o.zone == Zone::Battlefield && o.name == "Marut")
        .expect("blinked Marut must have returned to the battlefield")
        .id;
    assert!(
        runner.state().objects[&marut_reentered]
            .mana_spent_source_snapshots
            .is_empty(),
        "the re-entered incarnation must carry no payment-source snapshots (CR 400.7)"
    );
    assert_eq!(
        treasure_token_count(&runner),
        0,
        "no Treasure token may exist before the rider resolves"
    );

    // Resolve the pending rider.
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the pending Marut rider",
    );

    // CR 603.4 + CR 400.7d: the rider read the LATCHED source-payment vector —
    // exactly two Treasures. (Exactly two also proves the re-entry produced no
    // second rider: its collection-time intervening-if read the cleared new
    // object as 0.)
    assert_eq!(
        treasure_token_count(&runner),
        2,
        "the rider must create one Treasure per Treasure-tagged mana unit via the latch"
    );
    // Mirror guard: the CR 400.7 clearing STAYS — the new incarnation still
    // reads empty/0 after the rider resolved from the latch.
    let reentered = &runner.state().objects[&marut_reentered];
    assert!(
        reentered.mana_spent_source_snapshots.is_empty(),
        "the re-entered incarnation must still read no payment-source snapshots"
    );
    assert_eq!(
        reentered.mana_spent_to_cast_amount, 0,
        "the re-entered incarnation must still read 0 mana spent"
    );
}
