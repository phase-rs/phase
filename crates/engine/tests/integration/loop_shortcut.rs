// engine-citation-gate: symbol anchors only
//! PR-7 Phase 3 — interactive loop-shortcut protocol + APNAP response window.
//!
//! Covers the CR 732.2a/b/c live-detect bridge, `LoopDetectionMode::Interactive`, the
//! `WaitingFor::LoopShortcut`/`RespondToShortcut` states, the `DeclareShortcut`/
//! `RespondToShortcut` actions, the CR 732.4 all-mandatory no-loss draw, and the
//! conservative Shorten → priority window.
//!
//! # Golden discipline (non-circular byte-identity)
//!
//! `GOLDEN_ON` is the exact accumulated `Vec<GameEvent>` Debug string captured from HEAD
//! `dc67bd130` BEFORE the reconcile mode-`match` wrap landed (via a temporary On/Off-only
//! harness run against the UNMODIFIED reconcile body). T-ON replays the same fixture under
//! the wrapped `On` arm and asserts equality — it fails if wrapping the body in the mode
//! `match` perturbed even one event. Because the golden is pre-edit, this is not circular.
//! Re-pinned since: the cast sorcery's records now carry `cast_from_zone: Some(Hand)` /
//! `cast_controller: Some(PlayerId(0))`, because `finalize_cast` stamps cast provenance on
//! every cast spell (CR 601.2a + CR 601.2i), not only on spells without a spell ability.

use engine::analysis::decision_template::{
    AnnouncementSubject, ChoicePoint, DecisionGroupKey, DecisionKind, DecisionPoint,
    DecisionPointKind, DecisionSlot, DecisionTemplate, IterationCount, PinnedDecision, Ranking,
    ReplayMode, ShortcutDecisionSchema, TargetPin, TargetSchedule,
};
use engine::analysis::loop_check::{LoopCertificate, ShortcutProposal, ShortcutResponse, WinKind};
use engine::analysis::resource::{
    loop_detect_cost, loop_states_equal_modulo_resources, reset_loop_detect_cost, BoardDelta,
    ChargeBound, LoopDetectCost, PeriodSignature, PeriodicDelta, ResourceAxis,
};
use engine::game::derived_views::{FamilyCollapseState, UnboundedFamily};
use engine::game::engine::{apply, shorten_refusal, EngineError, ShortenRefusal};
use engine::game::perf_counters::{
    take_cost_records, take_cost_snapshot, TakeCostCounters, TakeCostRecord,
};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, LifeTotalReading};
use engine::types::game_state::{
    AutoPassRequest, CastPaymentMode, GameEnd, GameState, LoopDetectionMode, StackEntryKind,
    WaitingFor, YieldTarget,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::match_config::{MatchConfig, MatchPhase, MatchType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::loop_period_remade_token_source::{activate, markers, settle, take_recorded, Answers};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

/// The one-element ranking a schedule step carries when it names a single object: a
/// `Ranking::one(Object(src))` IS the pre-parameterization constant subject, so every site
/// below is a re-spelling with no behaviour delta (CR 732.2a — only the head is ever
/// resolved within one drive).
fn obj_rank(src: YieldTarget) -> Ranking {
    Ranking::one(AnnouncementSubject::Object(src))
}

const DRAIN_CLERIC: &str = "Whenever you gain life, each opponent loses 1 life.";
const BLOOD_SIPPER: &str = "Whenever an opponent loses life, you gain 1 life.";
const KICKOFF: &str = "You gain 1 life.";
const SELF_LIFE_ENGINE: &str = "Whenever you gain life, you gain 1 life.";

// Verbatim Oracle text (data/card-data.json) for the PR-7 gate-relax witness — the
// REAL escalating TARGETED drain that only detects once item-3 (forced-unique) and
// item-4 (Typed-target projected refinement) both land.
const VITO: &str = "Whenever you gain life, target opponent loses that much life.\n{3}{B}{B}: Creatures you control gain lifelink until end of turn.";
const SANGUINE_BOND: &str = "Whenever you gain life, target opponent loses that much life.";
const BLOODTHIRSTY_CONQUEROR: &str =
    "Flying, deathtouch\nWhenever an opponent loses life, you gain that much life. (Damage causes loss of life.)";

/// The exact accumulated event Debug string of the 2p drain under `On`, captured from
/// HEAD `dc67bd130` on the UNMODIFIED reconcile body. See the module docs.
/// `subject: None` was appended to each `EffectResolved` when that field was added to the
/// event — it is `None` on every path this test drives, so the stream is otherwise unchanged.
const GOLDEN_ON: &str = r#"[StackPushed { object_id: ObjectId(3) }, ZoneChanged { object_id: ObjectId(3), from: Some(Hand), to: Stack, record: ZoneChangeRecord { object_id: ObjectId(3), name: "Test Lifegain Kickoff", core_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], trigger_definitions: [], trigger_source_context: Some(TriggerSourceContext { identity: ObjectIdentityBinding { reference: ObjectIncarnationRef { object_id: ObjectId(3), incarnation: 0 }, expected_zone: Hand }, lki: LKISnapshot { name: "Test Lifegain Kickoff", token_image_ref: None, power: None, toughness: None, base_power: None, base_toughness: None, mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), card_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], colors: [], chosen_attributes: [], counters: {}, tapped: false, is_suspected: false, attachments: [] }, card_id: CardId(3), printed_ref: None, is_token: false, face_down: false, transformed: false, is_renowned: false, is_saddled: false, class_level: None, trigger_entries: [], timestamp: 0, entered_battlefield_turn: None, paired_with: None, pair_controller: None, attached_to: None, attachments: [], linked_exile_snapshot: [], combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, cast_from_zone: Some(Hand), played_from_zone: None, cast_controller: Some(PlayerId(0)), phase_status: PhasedIn, cast_variant_paid: None, cast_timing_permission: None, cost_x_paid: None, cast_spell_keywords: [], mana_spent_to_cast: false, colors_spent_to_cast: ColoredManaCount { white: 0, blue: 0, black: 0, red: 0, green: 0 }, mana_spent_to_cast_amount: 0, kickers_paid: [], additional_cost_payment_count: 0, additional_cost_payments: [], cast_cost_paid_object: None }), power: None, toughness: None, base_power: None, base_toughness: None, colors: [], mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), from_zone: Some(Hand), cast_from_zone: Some(Hand), played_from_zone: None, to_zone: Stack, attachments: [], linked_exile_snapshot: [], is_token: false, combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, co_departed: [], entered_incarnation: None, attached_to: None, turn_zone_change_index: 0, recorded_turn_number: 2, is_suspected: false } }, SpellCast { card_id: CardId(3), controller: PlayerId(0), object_id: ObjectId(3) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(0), amount: 1 }, EffectResolved { kind: GainLife, source_id: ObjectId(3), subject: None }, ZoneChanged { object_id: ObjectId(3), from: Some(Stack), to: Graveyard, record: ZoneChangeRecord { object_id: ObjectId(3), name: "Test Lifegain Kickoff", core_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], trigger_definitions: [], trigger_source_context: Some(TriggerSourceContext { identity: ObjectIdentityBinding { reference: ObjectIncarnationRef { object_id: ObjectId(3), incarnation: 1 }, expected_zone: Stack }, lki: LKISnapshot { name: "Test Lifegain Kickoff", token_image_ref: None, power: None, toughness: None, base_power: None, base_toughness: None, mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), card_types: [Sorcery], subtypes: [], supertypes: [], keywords: [], colors: [], chosen_attributes: [], counters: {}, tapped: false, is_suspected: false, attachments: [] }, card_id: CardId(3), printed_ref: None, is_token: false, face_down: false, transformed: false, is_renowned: false, is_saddled: false, class_level: None, trigger_entries: [], timestamp: 0, entered_battlefield_turn: None, paired_with: None, pair_controller: None, attached_to: None, attachments: [], linked_exile_snapshot: [], combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, cast_from_zone: Some(Hand), played_from_zone: None, cast_controller: Some(PlayerId(0)), phase_status: PhasedIn, cast_variant_paid: None, cast_timing_permission: None, cost_x_paid: None, cast_spell_keywords: [], mana_spent_to_cast: false, colors_spent_to_cast: ColoredManaCount { white: 0, blue: 0, black: 0, red: 0, green: 0 }, mana_spent_to_cast_amount: 0, kickers_paid: [], additional_cost_payment_count: 0, additional_cost_payments: [], cast_cost_paid_object: None }), power: None, toughness: None, base_power: None, base_toughness: None, colors: [], mana_value: 0, controller: PlayerId(0), owner: PlayerId(0), from_zone: Some(Stack), cast_from_zone: Some(Hand), played_from_zone: None, to_zone: Graveyard, attachments: [], linked_exile_snapshot: [], is_token: false, combat_status: ZoneChangeCombatStatus { attacking: false, blocking: false, blocked: false, attacking_alone: false, blocking_alone: false, defending_player: None }, co_departed: [], entered_incarnation: None, attached_to: None, turn_zone_change_index: 1, recorded_turn_number: 2, is_suspected: false } }, StackResolved { object_id: ObjectId(3) }, StackPushed { object_id: ObjectId(4) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(1), amount: -1 }, EffectResolved { kind: LoseLife, source_id: ObjectId(1), subject: None }, StackResolved { object_id: ObjectId(4) }, StackPushed { object_id: ObjectId(5) }, PriorityPassed { player_id: PlayerId(1) }, LifeChanged { player_id: PlayerId(0), amount: 1 }, EffectResolved { kind: GainLife, source_id: ObjectId(2), subject: None }, StackResolved { object_id: ObjectId(5) }, StackPushed { object_id: ObjectId(6) }, GameOver { winner: Some(PlayerId(0)) }]"#;

fn life(runner: &GameRunner, p: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|pl| pl.id == p)
        .map(|pl| pl.life)
        .unwrap()
}

fn is_eliminated(runner: &GameRunner, p: PlayerId) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|pl| pl.id == p)
        .map(|pl| pl.is_eliminated)
        .unwrap()
}

/// 2-player self-refilling mutual drain controlled by P0 (constant-depth). P1 starts low so
/// the OFF natural-death stream is short. Returns runner + kick-off sorcery id.
pub(crate) fn setup_2p_drain(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

// ─────────── #7899: projected cost journals must not veto an unrelated recurrence ───────────

/// Blaster Hulk verbatim (`data/card-data.json`). Its first line is a self-spell cost reduction
/// whose `active_zones` opt into hand and library (CR 113.6), so the static FUNCTIONS while the
/// card sits uncast in hand — the metadata that must not veto an energy-free recurrence.
const BLASTER_HULK: &str = "This spell costs {1} less to cast for each {E} (energy counter) you've paid or lost this turn.\nHaste\nWhenever this creature attacks, you get {E}{E}, then you may pay eight {E}. When you do, this creature deals 8 damage divided as you choose among up to eight targets.";

/// Longtusk Cub verbatim (`data/card-data.json`): a `{E}{E}` ACTIVATION cost — the production
/// route by which `energy_paid_or_lost_this_turn` advances (CR 107.14), as opposed to the
/// hand-assigned journal value the helper-level comparison uses.
const LONGTUSK_CUB: &str = "Whenever this creature deals combat damage to a player, you get {E}{E} (two energy counters).\nPay {E}{E}: Put a +1/+1 counter on this creature.";

/// Park a REAL Blaster Hulk in `P0`'s hand, parsed by the production parser.
fn add_uncast_blaster_hulk(scenario: &mut GameScenario) -> ObjectId {
    let mut builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Blaster Hulk", 8, 8, BLASTER_HULK);
    builder
        .as_artifact_creature()
        .with_subtypes(vec!["Pirate"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 6,
        });
    builder.id()
}

/// The shipped 2p constant-depth drain pair (the Exquisite Blood / Marauding Blight-Priest shape)
/// plus an uncast Hulk in hand, under the production `LoopDetectionMode::On`.
fn setup_2p_drain_with_uncast_hulk() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let hulk = add_uncast_blaster_hulk(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::On;
    (runner, kickoff, hulk)
}

/// CR 732.2a + CR 601.2f: an UNCAST Blaster Hulk in hand must not stop an unrelated, energy-free
/// recurrence from certifying. Its self-spell reduction functions in hand (CR 113.6 —
/// `active_zones` opts into Hand/Library), but the period pays no energy, so the journal it
/// prices on is zero on both compared frames and its price cannot differ between repeats. Under
/// `LoopDetectionMode::On` the winner is crowned EARLY, with the victim still at positive life —
/// a natural CR 704.5a death would mean the recurrence was refused.
#[test]
fn uncast_blaster_hulk_in_hand_does_not_block_drain_loop_certification() {
    let (mut runner, kickoff, hulk) = setup_2p_drain_with_uncast_hulk();

    // Reach guard: the production parser put the real self-spell reduction on the hand card,
    // and it functions from Hand — the exact metadata that used to trip the veto.
    {
        let state = runner.state();
        assert_eq!(state.objects[&hulk].zone, Zone::Hand);
        let reduction = state.objects[&hulk]
            .static_definitions
            .iter_all()
            .find(|def| {
                matches!(
                    def.mode,
                    StaticMode::ModifyCost {
                        dynamic_count: Some(
                            engine::types::ability::QuantityRef::EnergyPaidOrLostThisTurn
                        ),
                        ..
                    }
                )
            })
            .expect("the parsed Blaster Hulk carries its energy cost reduction");
        assert!(
            reduction.active_zones.contains(&Zone::Hand),
            "CR 113.6: the self-spell reduction is live from hand"
        );
        assert_eq!(
            state.players[P0.0 as usize].energy_paid_or_lost_this_turn,
            0
        );
    }

    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "the drain recurrence must still certify with an uncast Hulk in hand"
    );
    assert!(
        life(&runner, P1) > 0,
        "the shortcut crowned early — the victim should still be at positive life, not {}",
        life(&runner, P1)
    );
}

/// CR 601.2f + CR 107.14 + CR 732.2a: the control that keeps the test above from being a blanket
/// exemption. On the SAME production board with the SAME real Hulk, a frame where the journal the
/// reduction reads has MOVED (the shape one `{E}{E}` payment leaves) must compare UNEQUAL: the
/// journal is projected out of the comparison, so without the period-tied veto these frames would
/// certify an energy-paying recurrence whose price changes every repeat.
#[test]
fn moved_energy_journal_with_live_hulk_rejects_the_recurrence() {
    let (runner, _kickoff, hulk) = setup_2p_drain_with_uncast_hulk();
    assert_eq!(runner.state().objects[&hulk].zone, Zone::Hand);
    assert!(
        loop_states_equal_modulo_resources(runner.state(), &runner.state().clone()),
        "control: unchanged frames compare EQUAL"
    );

    let mut paid = runner.state().clone();
    paid.players[P0.0 as usize].energy_paid_or_lost_this_turn = 2;
    assert!(
        !loop_states_equal_modulo_resources(runner.state(), &paid),
        "CR 601.2f: the live Hulk prices the moved journal ⇒ the recurrence is not constant"
    );
}

/// The shipped 2p drain pair plus a REAL Longtusk Cub holding two energy counters.
/// `include_hulk` parks the uncast Blaster Hulk in `P0`'s hand — the only variable between the
/// two runs below. Returns runner, kickoff, and the payer.
fn setup_2p_drain_with_energy_payer(include_hulk: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let cub = {
        let mut builder = scenario.add_creature_from_oracle(P0, "Longtusk Cub", 2, 2, LONGTUSK_CUB);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        builder.id()
    };
    if include_hulk {
        add_uncast_blaster_hulk(&mut scenario);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::On;
    // CR 107.14: the Cub holds, but has not spent, two energy counters.
    runner.state_mut().players[P0.0 as usize].energy = 2;
    (runner, kickoff, cub)
}

/// CR 107.14 + CR 601.2f: SUPPLEMENTAL payment coverage — the production route that advances the
/// journal, isolated from the recurrence machinery.
/// `GameRunner::activate` pays the Cub's `{E}{E}` through the shared cost resolver (`costs.rs`),
/// the only production route that advances `energy_paid_or_lost_this_turn`, and the engine's own
/// comparator refuses the resulting pair while a live modifier prices the journal. The no-Hulk
/// control is the SAME payment on the SAME board: with nothing pricing the journal (its reserve,
/// counter and journal are all projected out) the frames compare EQUAL, so the refusal in the
/// first half is the modifier's, not the payment's.
///
/// This row does NOT claim a recurrence was presented to production: it activates once in
/// `LoopDetectionMode::On`, where the mandatory-loop winner route — not the recorded/interactive
/// offer — is what runs, and no take is declared.
/// `printed_energy_activation_period_reaches_the_recorded_offer_and_replays_each_payment`
/// carries the production proof through the recorded offer and its take.
#[test]
fn real_energy_payment_through_game_runner_updates_the_journal_it_prices() {
    let (mut with_hulk, _kickoff, cub) = setup_2p_drain_with_energy_payer(true);
    let before = with_hulk.state().clone();
    with_hulk.activate(cub, 0).resolve();

    // Reach guard: the production resolver really paid, so the journal the Hulk prices on moved.
    assert_eq!(
        with_hulk.state().players[P0.0 as usize].energy,
        0,
        "CR 107.14: paying {{E}}{{E}} removes both counters"
    );
    assert_eq!(
        with_hulk.state().players[P0.0 as usize].energy_paid_or_lost_this_turn,
        2,
        "the production cost resolver advances the journal Blaster Hulk's reduction reads"
    );
    assert!(
        !loop_states_equal_modulo_resources(&before, with_hulk.state()),
        "CR 601.2f: the live Hulk prices the journal the real payment moved ⇒ the pair is not \
         constant"
    );

    // Control: identical payment, nothing pricing the journal ⇒ the frames compare EQUAL.
    let (mut no_hulk, _kickoff, cub) = setup_2p_drain_with_energy_payer(false);
    let before = no_hulk.state().clone();
    no_hulk.activate(cub, 0).resolve();
    assert_eq!(
        no_hulk.state().players[P0.0 as usize].energy_paid_or_lost_this_turn,
        2,
        "reach guard: the control ran the same real payment"
    );
    assert!(
        loop_states_equal_modulo_resources(&before, no_hulk.state()),
        "control: with no live modifier reading the journal the same payment stays projected \
         history ⇒ EQUAL"
    );
}

/// Automated Assembly Line verbatim (Scryfall). A PRINTED `{E}`-paying activation whose effect
/// mints one inert vanilla token per period — the production route by which
/// `energy_paid_or_lost_this_turn` advances once per repeat, with nothing else moving.
const AUTOMATED_ASSEMBLY_LINE: &str = "Whenever one or more artifact creatures you control deal combat damage to a player, you get {E} (an energy counter).\nPay {E}{E}{E}: Create a tapped 3/3 colorless Robot artifact creature token.";

/// Decoction Module verbatim (MTGJSON): three copies refund each Robot's three-energy payment.
const DECOCTION_MODULE: &str = "Whenever a creature you control enters, you get {E} (an energy counter).\n{4}, {T}: Return target creature you control to its owner's hand.";

/// The activation's banked energy: enough for every repetition these rows drive.
const BANKED_ENERGY: u32 = 30;

/// The 2p recorded-road board: P0 controls Automated Assembly Line with `BANKED_ENERGY` energy
/// banked. `include_hulk` parks the REAL Blaster Hulk in P0's hand — the only variable between
/// the bounded runs below. Each Module refunds one energy on entry. Interactive mode, so the
/// recorded road (and the `take_route` it feeds) runs.
fn setup_energy_activation_loop(include_hulk: bool, module_count: usize) -> (GameState, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 40);
    scenario.with_life(P1, 40);
    let mut builder =
        scenario.add_artifact_from_oracle(P0, "Automated Assembly Line", AUTOMATED_ASSEMBLY_LINE);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 2,
    });
    let line = builder.id();
    for _ in 0..module_count {
        scenario.add_artifact_from_oracle(P0, "Decoction Module", DECOCTION_MODULE);
    }
    if include_hulk {
        add_uncast_blaster_hulk(&mut scenario);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state_mut().players[P0.0 as usize].energy = BANKED_ENERGY;
    (runner.state().clone(), line)
}

/// The parsed `{E}{E}{E}` activation — the index the drive activates, and a reach guard on the
/// production parser.
fn energy_activation_index(state: &GameState, line: ObjectId) -> usize {
    state.objects[&line]
        .abilities
        .iter()
        .position(|ability| {
            ability.kind == engine::types::ability::AbilityKind::Activated
                && matches!(
                    ability.cost,
                    Some(engine::types::ability::AbilityCost::PayEnergy { .. })
                )
        })
        .expect("the production parser must give Automated Assembly Line its {E}{E}{E} activation")
}

/// One recorded period: the printed `{E}{E}{E}` activation and its resolution.
fn energy_activation_period(state: &mut GameState, line: ObjectId, index: usize) {
    activate(state, line, index);
    settle(
        state,
        &Answers {
            color: ManaType::Red,
            aim: None,
            cost: None,
            accept: false,
        },
    );
}

/// The Robot tokens P0 controls.
fn robots(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.name == "Robot" && object.controller == P0
        })
        .count()
}

/// CR 107.14 + CR 601.2f + CR 732.2a/c: a PRINTED `{E}{E}{E}` period driven through the
/// production recorded road. Its first activation is enough for the confirmer to name, CONFIRM
/// and OFFER the period (reach control: the recurrence/offer boundary really is reached), and the
/// offer is the counting replay's SPEND bound — the reserve left at the offer divided by three —
/// not an unbounded ∞ certificate, because every repetition consumes counters. A
/// bounded period is PERFORMED, so an accepted take replays the printed payment each time
/// instead of marking the period and omitting the journal change (CR 732.2c).
///
/// The real Blaster Hulk in hand is the ONLY variable between the two runs (CR 601.2f): its
/// projection-priced reduction must neither suppress the confirmed period nor change what the
/// take performs, and the journal assertions are exactly what a mark route would omit.
#[test]
fn printed_energy_activation_period_reaches_the_recorded_offer_and_replays_each_payment() {
    let (mut control, line) = setup_energy_activation_loop(false, 0);
    let index = energy_activation_index(&control, line);
    let mut offered_after = 0;
    for cycle in 1..=3 {
        energy_activation_period(&mut control, line, index);
        if let WaitingFor::LoopShortcut {
            road,
            schema,
            period,
            ..
        } = &control.waiting_for
        {
            assert_eq!(
                *road,
                engine::analysis::loop_check::OfferRoad::RecordedPeriod
            );
            assert!(
                !period.is_empty(),
                "the offer must carry the confirmed period"
            );
            assert_eq!(
                schema.measured_repetition_bound,
                Some((BANKED_ENERGY - 3 * cycle) / 3),
                "the counting replay bounds the spend by the reserve left at the offer"
            );
            offered_after = cycle;
            break;
        }
    }
    assert!(
        offered_after > 0,
        "reach control: the printed energy loop must reach the recorded offer boundary; got {:?}",
        control.waiting_for
    );
    assert_eq!(
        control.players[P0.0 as usize].energy_paid_or_lost_this_turn,
        3 * offered_after,
        "each confirmed period paid {{E}}{{E}}{{E}}"
    );

    let (mut refused, line) = setup_energy_activation_loop(true, 0);
    let index = energy_activation_index(&refused, line);
    // Reach guard: the parsed reduction FUNCTIONS from hand — the metadata the veto prices.
    {
        let hulk = refused
            .objects
            .values()
            .find(|object| object.name == "Blaster Hulk")
            .expect("the uncast Hulk");
        assert_eq!(hulk.zone, Zone::Hand);
        let reduction = hulk
            .static_definitions
            .iter_all()
            .find(|def| {
                matches!(
                    def.mode,
                    StaticMode::ModifyCost {
                        dynamic_count: Some(
                            engine::types::ability::QuantityRef::EnergyPaidOrLostThisTurn
                        ),
                        ..
                    }
                )
            })
            .expect("the parsed Blaster Hulk carries its energy cost reduction");
        assert!(
            reduction.active_zones.contains(&Zone::Hand),
            "CR 113.6: the self-spell reduction is live from hand"
        );
    }
    for cycle in 1..=3 {
        energy_activation_period(&mut refused, line, index);
        assert_eq!(
            refused.players[P0.0 as usize].energy_paid_or_lost_this_turn,
            3 * cycle,
            "the production activation paid {{E}}{{E}}{{E}} each period"
        );
        assert_eq!(
            refused.players[P0.0 as usize].energy,
            BANKED_ENERGY - 3 * cycle
        );
        if matches!(refused.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
    }
    assert!(
        matches!(refused.waiting_for, WaitingFor::LoopShortcut { .. }),
        "reach guard: the printed, repeated energy activation must reach the offer boundary; got {:?}",
        refused.waiting_for
    );
    let journal = refused.players[P0.0 as usize].energy_paid_or_lost_this_turn;
    let reserve = refused.players[P0.0 as usize].energy;
    let take = take_recorded(&mut refused, 3);
    assert_eq!(
        markers(&take.events),
        vec![(P0, 1), (P0, 3)],
        "the accepted shortcut commits the first and last markers (middle markers are elided)"
    );
    assert_eq!(
        refused.players[P0.0 as usize].energy_paid_or_lost_this_turn,
        journal + 9,
        "CR 601.2f: Replay performs the agreed payment that Mark would omit"
    );
    assert_eq!(
        refused.players[P0.0 as usize].energy,
        reserve - 9,
        "CR 107.14: every replayed period removes its three energy counters"
    );
    assert!(
        robots(&refused) >= 4,
        "each replayed period creates its Robot"
    );
}

/// CR 107.14 + CR 601.2f + CR 732.2a/c: three printed Modules refund Assembly Line's
/// payment, so the recorded offer is unbounded even though Hulk's cost journal keeps moving.
/// The accepted fixed take must replay those gross payments, not merely mark token growth.
#[test]
fn printed_refunding_energy_period_replays_the_journal_at_the_accepted_take() {
    let (mut state, line) = setup_energy_activation_loop(true, 3);
    let index = energy_activation_index(&state, line);
    for cycle in 1..=3 {
        energy_activation_period(&mut state, line, index);
        assert_eq!(state.players[P0.0 as usize].energy, BANKED_ENERGY);
        assert_eq!(
            state.players[P0.0 as usize].energy_paid_or_lost_this_turn,
            3 * cycle,
            "the settled printed payment is refunded, not removed from its journal"
        );
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
    }
    let WaitingFor::LoopShortcut {
        road,
        schema,
        period,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "the refunding printed period must reach its offer: {:?}",
            state.waiting_for
        );
    };
    assert_eq!(
        *road,
        engine::analysis::loop_check::OfferRoad::RecordedPeriod
    );
    assert!(!period.is_empty());
    assert_eq!(
        schema.measured_repetition_bound, None,
        "reach guard: refunds leave no net spend bound on this offer"
    );
    let journal = state.players[P0.0 as usize].energy_paid_or_lost_this_turn;
    let tokens = robots(&state);
    let take = take_recorded(&mut state, 3);
    assert_eq!(markers(&take.events), vec![(P0, 1), (P0, 3)]);
    assert_eq!(
        state.players[P0.0 as usize].energy_paid_or_lost_this_turn,
        journal + 9,
        "Replay preserves the three payments that Mark would omit"
    );
    assert_eq!(state.players[P0.0 as usize].energy, BANKED_ENERGY);
    assert_eq!(robots(&state), tokens + 3);
    assert!(state.stack.is_empty());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// 2p ESCALATING TARGETED drain (PR-7 gate-relax witness): Vito + Sanguine Bond (two
/// identical `Whenever you gain life, target opponent loses that much life` triggers) +
/// Bloodthirsty Conqueror (`Whenever an opponent loses life, you gain that much life`).
/// A seed lifegain fans out a GROWING cascade of TARGETED drains — the ω-cover path that
/// reaches item-3 (forced-unique in 2p) + item-4 (opponent `Typed` target). The two
/// identical drainers make each gain fire two simultaneous triggers ⇒ the CR 603.3b
/// OrderTriggers beat the loop-detect ring must survive.
fn setup_2p_vito(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Vito, Thorn of the Dusk Rose", 1, 4, VITO);
    scenario.add_enchantment_from_oracle(P0, "Sanguine Bond", SANGUINE_BOND);
    scenario.add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 3, 4, BLOODTHIRSTY_CONQUEROR);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// 2-player drain (as above) but P1 also holds a castable Lightning Bolt off an untapped
/// Mountain — a meaningful priority action that makes the loop OPTIONAL (CR 732.5 probe
/// FALSE). Returns runner + (kickoff, bolt, drain-cleric enabler id).
pub(crate) fn setup_2p_optional_drain(
    mode: LoopDetectionMode,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    let cleric = scenario
        .add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC)
        .id();
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_basic_land(P1, ManaColor::Red);
    let bolt = scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff, bolt, cleric)
}

/// Marauding Blight-Priest and Exquisite Blood under P0, P1 holding Lightning Bolt off a
/// Mountain, P0 with Blessed Wine in hand, and twenty cards in each library so the game can go on
/// past Blessed Wine's delayed draw. Returns the runner, Blessed Wine, the Bolt and Blight-Priest.
pub(crate) fn setup_2p_blight_priest_drain() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    let priest = scenario
        .add_creature_from_oracle(P0, "Marauding Blight-Priest", 3, 2, MARAUDING_BLIGHT_PRIEST)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Exquisite Blood", EXQUISITE_BLOOD);
    scenario.add_basic_land(P1, ManaColor::Red);
    let bolt = scenario.add_bolt_to_hand(P1);
    let wine = scenario
        .add_spell_to_hand_from_oracle(P0, "Blessed Wine", true, BLESSED_WINE)
        .id();
    let names: Vec<String> = (0..20).map(|i| format!("Library card {i}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    scenario.with_library_top(P0, &names);
    scenario.with_library_top(P1, &names);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, wine, bolt, priest)
}

/// 3-player growing μ=2 cascade controlled by P0 (both opponents drain), P1 holding a
/// castable Bolt so the loop is OPTIONAL. The ω growing stack means the winner is confirmed
/// via `loop_states_cover_modulo_growth`, not the constant-depth equality.
fn setup_3p_optional_cascade(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// 3-player MANDATORY, unstoppable, net-progress, NO-LOSS loop: P0 has a self-refilling
/// "whenever you gain life, you gain 1 life" engine. Nobody drains, nobody can break it
/// (opponents have empty hands / no abilities) ⇒ CR 732.4 draw.
fn setup_3p_draw(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Life Engine", 2, 2, SELF_LIFE_ENGINE);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// Drive PassPriority/OrderTriggers beats, accumulating events, until a state OTHER than
/// `Priority`/`OrderTriggers` (a `LoopShortcut`/`RespondToShortcut`/`GameOver`/…) or the
/// cap. Returns accumulated events + the terminal `waiting_for`.
pub(crate) fn drive_collect(runner: &mut GameRunner, cap: usize) -> (Vec<GameEvent>, WaitingFor) {
    let mut all: Vec<GameEvent> = Vec::new();
    for _ in 0..cap {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => match runner.act(GameAction::PassPriority) {
                Ok(r) => all.extend(r.events),
                Err(_) => break,
            },
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                match runner
                    .act(GameAction::OrderTriggers { order })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                {
                    Ok(r) => all.extend(r.events),
                    Err(_) => break,
                }
            }
            _ => break,
        }
    }
    (all, runner.state().waiting_for.clone())
}

// ────────────────────────────── T-OFF ──────────────────────────────

/// T-OFF: the real winning drain under `Off` reaches the natural CR 704.5a SBA death — no
/// ring sampling, no shortcut, no `ResolutionHalted`. Discriminator: the SAME fixture under
/// `Interactive` produces a DIFFERENT outcome (early shortcut, victim positive), proving
/// `Off` runs zero new code.
#[test]
fn off_natural_death_no_shortcut() {
    let (mut runner, kickoff) = setup_2p_drain(LoopDetectionMode::Off);
    let out = runner.cast(kickoff).resolve();
    let mut all: Vec<GameEvent> = out.events().to_vec();
    let (rest, wf) = drive_collect(&mut runner, 2000);
    all.extend(rest);

    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "OFF: the drain still ends the game for P0, via the NATURAL CR 704.5a death"
    );
    // Natural-death signature: the victim actually crossed 0 and was eliminated.
    assert!(
        life(&runner, P1) <= 0 && is_eliminated(&runner, P1),
        "OFF: P1 must have drained to <= 0 and been eliminated (no early shortcut)"
    );
    // Off runs zero new code: the ring is never populated and no shortcut/halt occurs.
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "OFF: the loop-detect ring must be empty (sampler gated off)"
    );
    assert!(
        runner.state().unbounded_resources.is_empty(),
        "OFF: no unbounded axes marked (the detector never ran)"
    );
    assert!(
        !all.iter()
            .any(|e| matches!(e, GameEvent::ResolutionHalted { .. })),
        "OFF: no ResolutionHalted — the natural death ends it cleanly"
    );

    // Discriminator: the SAME fixture under Interactive ends DIFFERENTLY (mandatory
    // winning drain → early auto-win with the victim still at positive life).
    let (mut irunner, ikickoff) = setup_2p_drain(LoopDetectionMode::Interactive);
    let _ = irunner.cast(ikickoff).resolve();
    let (_ievents, iwf) = drive_collect(&mut irunner, 500);
    assert_eq!(
        iwf,
        WaitingFor::GameOver { winner: Some(P0) },
        "Interactive: mandatory winning drain auto-wins for P0"
    );
    assert!(
        life(&irunner, P1) > 0,
        "Interactive: the shortcut fired EARLY — P1 still positive ({}), unlike OFF (<=0)",
        life(&irunner, P1)
    );
}

// ────────────────────────────── T-ON ──────────────────────────────

/// T-ON ⭐: the same lethal drain under `On`, byte-identical to the pre-PR-7 event stream
/// (`GOLDEN_ON`, captured from HEAD before the mode-`match` wrap). Fails if wrapping the
/// body perturbed even one event.
#[test]
fn on_shortcut_byte_identical_to_pre_pr7_golden() {
    let (mut runner, kickoff) = setup_2p_drain(LoopDetectionMode::On);
    let out = runner.cast(kickoff).resolve();
    let mut all: Vec<GameEvent> = out.events().to_vec();
    let (rest, wf) = drive_collect(&mut runner, 500);
    all.extend(rest);

    // The golden covers event ordering and effect payloads from before SpellCast
    // gained its optional cast-time snapshot, before LifeChanged began reporting
    // the life total it left the player on, and before the prepared designation
    // joined `LKISnapshot`/`ZoneChangeRecord`. All are orthogonal fields asserted
    // by their own tests (the Thor quantity tests; `effects::life` and
    // `combat_damage`; the prepared suites), so omit them from this legacy
    // byte-for-byte comparison.
    for event in &mut all {
        match event {
            GameEvent::SpellCast {
                cast_mana_value, ..
            } => *cast_mana_value = None,
            GameEvent::LifeChanged { new_total, .. } => *new_total = LifeTotalReading::default(),
            _ => {}
        }
    }

    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "ON: mandatory winning drain auto-wins for P0"
    );
    assert!(
        life(&runner, P1) > 0,
        "ON: the shortcut fired early (P1 positive)"
    );
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_none_or(|combat| combat.attacked_permanents.is_empty()),
        "the golden fixture has no active permanent attack relation"
    );
    let mut event_stream = format!("{all:?}")
        .replace(", cast_mana_value: None", "")
        .replace(", new_total: LifeTotalReading(None)", "")
        .replace(", was_prepared: false", "")
        // `block_pairs` joined `ZoneChangeRecord` after the golden was cut; no
        // creature in this fixture is in combat, so every record's is empty.
        .replace(", block_pairs: []", "")
        .replace(", departure: Moved", "")
        // `attack_target` was added to the combat status after the golden was cut;
        // no combat is live in this fixture, so its derived value is None (CR 506.3).
        .replace(", attack_target: None", "")
        .replace(", attack_target_identity: None", "")
        .replace(", intensity: 0", "");
    for owner in 0..2 {
        for controller in 0..2 {
            event_stream = event_stream.replace(
                &format!(
                    ", arrival: ArrivalIdentity {{ owner: PlayerId({owner}), controller: PlayerId({controller}) }}"
                ),
                "",
            );
        }
    }
    assert_eq!(
        event_stream, GOLDEN_ON,
        "ON: the accumulated event stream must be byte-identical to the pre-PR-7 golden — \
         wrapping the reconcile body in the mode `match` must not perturb any event"
    );
}

// ─────────────────────────────── T-Vito ───────────────────────────────

/// T-Vito ⭐ (PR-7 gate-relax witness): the REAL escalating TARGETED drain
/// (Vito, Thorn of the Dusk Rose + Sanguine Bond + Bloodthirsty Conqueror, verbatim
/// Oracle text) detects under `Interactive` and auto-wins for P0 with the victim still
/// at POSITIVE life — the shortcut fired EARLY (ω-cover), not a natural CR 704.5a death.
/// Detection requires BOTH item-3 (forced-unique targeted cover) AND item-4 (the
/// `Typed`-target projected refinement). The two per-conjunct revert-probes are measured
/// in the impl report: reverting EITHER conjunct loses detection ⇒ P1 grinds to natural
/// death (life <= 0) rather than an early crown. The two identical drainers exercise the
/// CR 603.3b OrderTriggers cascade beat the loop-detect ring must survive (G2).
#[test]
fn vito_bond_conqueror_2p_determinate_win() {
    let (mut runner, kickoff) = setup_2p_vito(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 2000);

    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: Some(P0) },
        "escalating targeted drain auto-wins for P0"
    );
    // DISCRIMINATOR (flips when either conjunct is reverted): early detection leaves the
    // victim positive; losing detection grinds P1 to <= 0 via natural resolution.
    assert!(
        life(&runner, P1) > 0,
        "the shortcut fired EARLY — P1 still positive ({}); reverting item-3 or item-4 \
         loses detection and P1 reaches natural death (<=0)",
        life(&runner, P1)
    );
}

// ────────────────────────── T-3p-cascade ──────────────────────────

/// T-3p-cascade: a ≥3p growing-cascade OPTIONAL winning loop under `Interactive`. The bridge
/// OFFERS a `LoopShortcut` (not an auto-win); the proposer declares the offered count; both
/// opponents are prompted in APNAP order and Accept ⇒ `GameOver{winner: P0}`, winner via the
/// ω-covering path with the opponents still at positive life.
#[test]
fn interactive_3p_optional_cascade_apnap_accept_win() {
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);

    // The OFFER fired (NOT an auto-win): waiting on the proposer to declare the shortcut.
    assert_eq!(
        wf,
        runner.state().waiting_for.clone(),
        "drive stopped at a non-priority state"
    );
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        ..
    } = wf
    else {
        panic!("Interactive optional cascade must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    assert_eq!(predicted_winner, Some(P0), "the detector predicts P0 wins");
    // Fired early — both opponents alive at positive life (ω shortcut, not natural death).
    assert!(
        life(&runner, P1) > 0 && life(&runner, P2) > 0 && !is_eliminated(&runner, P1),
        "opponents must be alive at positive life when the offer fires"
    );

    // Proposer declares the shortcut.
    runner
        .act(GameAction::DeclareShortcut {
            count: offered(runner.state()).1,
            template: None,
        })
        .expect("P0 declares the shortcut");

    // APNAP fan-out: first opponent prompted, then the second, both in turn order after P0.
    let WaitingFor::RespondToShortcut {
        player: first,
        remaining_players,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("after Declare, the first opponent must be prompted");
    };
    assert_eq!(
        first, P1,
        "APNAP: first responder is the next player after P0"
    );
    assert_eq!(remaining_players, vec![P2], "APNAP: P2 queued after P1");

    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts");

    let WaitingFor::RespondToShortcut { player: second, .. } = runner.state().waiting_for.clone()
    else {
        panic!("after P1 accepts, P2 must be prompted");
    };
    assert_eq!(second, P2, "APNAP: second responder is P2");

    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P2 accepts (last) → take the shortcut");

    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "both accepted ⇒ the shortcut resolves to P0's win"
    );
}

/// #10464: validating a responder's `Accept` must not TAKE the shortcut.
///
/// The last responder's `Accept` is the answer that materializes the whole take, so
/// `SimulationFilter` used to answer "is this legal?" by cloning the state and applying it: the
/// take ran once for the verdict and again for the real answer, holding the game lock for about
/// two takes. This row measures the take drive itself (`take_cost_snapshot`), never a wall clock,
/// and it measures it at the public `legal_actions` boundary — the call the server session makes
/// after every action.
///
/// **Fixture pin:** P1 accepts first, so the window measured is P2's with `remaining_players`
/// empty — the seat whose `Accept` is the materializing one (CR 732.2c: the shortcut is taken once
/// the last player has answered). On a window that still has a queued
/// responder an `Accept` merely advances the poll, which is why the shorter window would pass
/// vacuously. The enumeration reach guard (`exactly one RespondToShortcut`) is what keeps the
/// zero counters from being satisfied by an empty action list.
#[test]
fn shortcut_accept_validation_does_not_take_the_shortcut() {
    // This measures the generic shortcut protocol using the existing composed synthetic loop.
    // Nearest printed anchors: Marauding Blight-Priest and Exquisite Blood; the fixture's
    // fixed-one Test Blood Sipper is not Exquisite Blood's variable-amount printed ability.
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    assert!(
        matches!(wf, WaitingFor::LoopShortcut { .. }),
        "REACH-GUARD: the offer must fire, got {wf:?}"
    );
    runner
        .act(GameAction::DeclareShortcut {
            count: offered(runner.state()).1,
            template: None,
        })
        .expect("P0 declares the shortcut");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts");

    let WaitingFor::RespondToShortcut {
        player,
        remaining_players,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "REACH-GUARD: P2 must be prompted, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P2, "APNAP: the last responder is P2");
    assert!(
        remaining_players.is_empty(),
        "REACH-GUARD: this row needs the seat whose Accept MATERIALIZES the take"
    );

    let before = take_cost_snapshot();
    let validation_before = engine::game::perf_counters::snapshot().raw_validation_state_clones;
    let actions = engine::ai_support::legal_actions(runner.state());
    let during = take_cost_snapshot().since(before);
    let validation_clones =
        engine::game::perf_counters::snapshot().raw_validation_state_clones - validation_before;

    assert!(
        matches!(
            actions.as_slice(),
            [GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept
            }]
        ),
        "REACH-GUARD: the window enumerates exactly the materializing Accept, got {actions:?} — \
         a Shorten at place 0 takes nothing, so the take counters below would not be measuring \
         the take this row exists to remove"
    );
    assert_eq!(
        validation_clones, 0,
        "deciding whether the Accept is legal must not reach the clone-and-apply probe"
    );
    // MEASURED, same fixture, same window: base `bf584d6bd` answers this call with
    // `validation_clones=1, state_copies=50`; head with `0` and `1`. The copies above the one the
    // candidate probe makes are the simulated Accept driving the shortcut — `GameState::clone`
    // accounts every copy, so the take's snapshots land in this counter.
    assert_eq!(
        during.state_copies, 1,
        "one copy, the candidate probe's; the legality verdict must not add a take's snapshot"
    );
}

/// CR 732.2a: a shortcut belongs to the player with priority, not necessarily the player whose loop
/// it is. On P1's turn P1 casts Soothing Balm on P0, who holds Marauding Blight-Priest and
/// Exquisite Blood: the gain (CR 119.9 + CR 603.2c) starts the drain, and every repetition costs
/// P1 a life. P1 holds priority at P0 26 / P1 19 and is offered 18, the bound 19 its own
/// elimination would reach, with no winner, since P1 survives the offered count (CR 104.2a).
#[test]
fn interactive_offer_separates_priority_proposer_from_predicted_winner() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(P0, "Marauding Blight-Priest", 3, 2, MARAUDING_BLIGHT_PRIEST);
    scenario.add_enchantment_from_oracle(P0, "Exquisite Blood", EXQUISITE_BLOOD);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let balm = scenario
        .add_spell_to_hand_from_oracle(P1, "Soothing Balm", true, SOOTHING_BALM)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let _ = runner.cast(balm).target_player(P0).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        schema,
        ..
    } = &wf
    else {
        panic!("P1's priority window must receive a LoopShortcut offer, got {wf:?}");
    };
    assert_eq!(
        (
            *proposer,
            schema.iteration_count.clone(),
            schema.measured_repetition_bound,
            *predicted_winner,
            seat_lives(runner.state()),
        ),
        (
            P1,
            IterationCount::Fixed(18),
            Some(19),
            None,
            vec![(P0, 26), (P1, 19)],
        )
    );
    let offer = runner.state().clone();

    let wrong = apply(
        runner.state_mut(),
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(18),
            template: None,
        },
    );
    assert!(
        matches!(wrong, Err(EngineError::WrongPlayer)),
        "P0 cannot propose while P1 holds priority, got {wrong:?}"
    );

    let mut taken = offer.clone();
    r6a_declare_and_accept_all(&mut taken, P1, 18);
    assert_eq!(
        (taken.waiting_for.clone(), seat_lives(&taken)),
        (WaitingFor::Priority { player: P1 }, vec![(P0, 44), (P1, 1)])
    );

    let mut past = offer;
    r6a_declare_and_accept_all(&mut past, P1, 19);
    assert_eq!(past.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
}

// ─────────────────────────── T-3p-draw ────────────────────────────

/// T-3p-draw: a ≥3p MANDATORY, net-progress, no-loss, unstoppable loop draws under
/// `Interactive` (CR 732.4). Discriminator: the SAME fixture under `Off` does NOT draw (it
/// grinds / halts, never reaching the draw branch), proving the draw is the Interactive path,
/// not a
/// pre-existing outcome.
#[test]
fn interactive_3p_mandatory_no_loss_draw() {
    let (mut runner, kickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    assert_eq!(
        wf,
        WaitingFor::GameOver { winner: None },
        "Interactive: an all-mandatory, no-loss, unstoppable net-progress loop is a CR 732.4 draw"
    );

    // Discriminator: under Off the same fixture never draws via that branch (it grinds to the
    // iteration/growth backstop or keeps going — not GameOver{None} by this branch).
    let (mut orunner, okickoff) = setup_3p_draw(LoopDetectionMode::Off);
    let _ = orunner.cast(okickoff).resolve();
    let (_oevents, owf) = drive_collect(&mut orunner, 500);
    assert_ne!(
        owf,
        WaitingFor::GameOver { winner: None },
        "Off must NOT reach the CR 732.4 net-progress draw (that branch is Interactive-only)"
    );
}

// ────────────────────────── T-Q1-shorten ──────────────────────────

/// T-Q1-shorten ⭐: an OPTIONAL winning drain under `Interactive`. The proposer declares the
/// shortcut; the opponent SHORTENS at a low place ⇒ the shortened drive commits its cycles and
/// the ending point lands on THAT opponent (CR 732.2b/c); the opponent casts removal on an
/// enabler ⇒ the loop breaks (no GameOver, re-detection does not re-confirm). Discriminator:
/// replacing Shorten with Accept runs the same fixture to `GameOver{winner: P0}` — proving the
/// WINDOW stopped it, not an unrelated fizzle.
#[test]
fn interactive_shorten_hands_priority_and_breaks_loop() {
    let (mut runner, wine, bolt, priest) = setup_2p_blight_priest_drain();
    let _ = runner.cast(wine).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);

    let WaitingFor::LoopShortcut {
        proposer, schema, ..
    } = wf
    else {
        panic!("optional drain must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(
        (proposer, schema.iteration_count),
        (P0, IterationCount::Fixed(19))
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(19),
            template: None,
        })
        .expect("P0 declares");

    // Positive reach-guard: the opponent WAS actually prompted before it responds.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P1
        ),
        "P1 must be prompted to respond before shortening"
    );

    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 1 },
        })
        .expect("P1 shortens");

    // CR 732.2b/c: the shortcut was taken to the place P1 named, and P1 holds its ending point.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "the shortening opponent holds the shortened sequence's ending point"
    );
    assert!(
        life(&runner, P1) > 0,
        "P1 is alive — the shortened drive stopped at the place P1 named, short of lethal"
    );

    // P1 casts removal on an enabler ⇒ the loop breaks.
    let _ = runner.cast(bolt).target_object(priest).resolve();
    assert!(
        runner.state().objects.get(&priest).map(|o| o.zone)
            != Some(engine::types::zones::Zone::Battlefield),
        "the drain enabler (Blight-Priest) must have left the battlefield"
    );

    // Re-detection on the next beats does NOT re-confirm the (now-broken) loop.
    let (_r, wf2) = drive_collect(&mut runner, 200);
    assert!(
        !matches!(wf2, WaitingFor::GameOver { winner: Some(_) }),
        "after the enabler is removed, no player is shortcut to a win; got {wf2:?}"
    );
    assert!(
        life(&runner, P1) > 0 && !is_eliminated(&runner, P1),
        "P1 survives — the shorten window genuinely stopped the loop"
    );

    // Discriminator: the SAME fixture with Accept instead of Shorten runs to P0's win.
    let (mut arunner, awine, _abolt, _apriest) = setup_2p_blight_priest_drain();
    let _ = arunner.cast(awine).resolve();
    let (_ae, awf) = drive_collect(&mut arunner, 500);
    assert!(matches!(awf, WaitingFor::LoopShortcut { .. }));
    arunner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(19),
            template: None,
        })
        .expect("declare");
    arunner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    assert_eq!(
        arunner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "Accept (not Shorten) ⇒ the loop resolves to P0's win — proves the window stops it"
    );
}

// ───────────────────── T-Q1-decline (Seam 1) ───────────────────────

/// T-Q1-decline ⭐ (interactive bridge, Seam 1): CR 732.2a — suggesting a shortcut is
/// OPTIONAL, so the proposer may DECLINE the auto-offered optional drain. The engine dismisses
/// the offer and restores ordinary priority to the living seat (P0 here); an ordinary action
/// then resolves and the declined loop is NOT immediately re-offered by the post-return
/// reconcile.
///
/// Non-vacuous revert-probe (measured): the interactive Seam-1 re-offer is suppressed by the
/// `apply_action` deliberate-action ring invalidation (fires for `DeclineShortcut` before the
/// handler runs). The handler therefore does NOT clear the ring itself — a per-action re-clear
/// would distrust that engine-wide invariant — so there is no handler ring-clear to serve as a
/// discriminator here. The load-bearing line for THIS test is the offer dismissal
/// (`state.waiting_for = WaitingFor::Priority { .. }`): deleting it leaves `waiting_for ==
/// LoopShortcut { P0 }` (the reconcile's Priority-gated seams skip a non-Priority state) ⇒ the
/// `Priority { P0 }` assertion (a) flips to fail. Seam-2 independence is proven by the
/// object-growth test.
#[test]
fn interactive_optional_drain_decline_restores_priority_no_reoffer() {
    let (mut runner, kickoff, _bolt, _cleric) =
        setup_2p_optional_drain(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);

    // F2 positive reach-guard: the offer was genuinely reached before we decline. Without this
    // a fixture drift that never offers would let DeclineShortcut hit the apply wildcard and
    // pass assertion (c) vacuously.
    assert!(
        matches!(wf, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "optional drain must OFFER a LoopShortcut to P0, got {wf:?}"
    );

    // CR 732.2a: the proposer (P0) declines the offer.
    let decline = runner
        .act(GameAction::DeclineShortcut)
        .expect("P0 declines the shortcut");

    // (a): the offer is dismissed and ordinary priority is restored to the living seat. Deleting
    // the handler's `state.waiting_for = Priority { .. }` dismissal leaves `waiting_for ==
    // LoopShortcut { P0 }` (the reconcile's Priority-gated seams skip a non-Priority state) ⇒
    // this assertion flips to fail — the load-bearing revert-probe for this interactive seam.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "decline dismisses the offer and restores ordinary priority to the living seat"
    );
    assert_eq!(decline.waiting_for, WaitingFor::Priority { player: P0 });
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "the recurrence ring is empty after decline (invalidated by the deliberate-action clear)"
    );

    // (b) an ordinary action resolves from the restored priority window.
    runner
        .act(GameAction::PassPriority)
        .expect("an ordinary PassPriority resolves after the decline handback");

    // (c) the SAME loop is not instantly re-offered on the immediate next beat (the ring is
    // empty, so it takes several samples to re-detect; a genuine later re-recurrence would then
    // legitimately re-arm the offer — CR 732.2a event-driven re-arm).
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "the declined loop must not be re-offered on the immediate next beat, got {:?}",
        runner.state().waiting_for
    );
}

// ───────────────────── T-declare-roundtrip ─────────────────────────

/// T-declare-roundtrip: each protocol action is accepted only from its authorized actor —
/// `DeclareShortcut` from the proposer, `RespondToShortcut` from the current responder.
/// A wrong actor is rejected with `WrongPlayer`.
#[test]
fn declare_and_respond_authorization() {
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    assert!(matches!(wf, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0));

    let count = offered(runner.state()).1;
    // Wrong actor for DeclareShortcut (an opponent) → rejected.
    let wrong = apply(
        runner.state_mut(),
        P1,
        GameAction::DeclareShortcut {
            count: count.clone(),
            template: None,
        },
    );
    assert!(
        matches!(wrong, Err(EngineError::WrongPlayer)),
        "an opponent may not declare the proposer's shortcut, got {wrong:?}"
    );

    // Correct actor (P0) → accepted; advances to the first responder.
    apply(
        runner.state_mut(),
        P0,
        GameAction::DeclareShortcut {
            count,
            template: None,
        },
    )
    .expect("P0 declares");
    let WaitingFor::RespondToShortcut { player: first, .. } = runner.state().waiting_for.clone()
    else {
        panic!("expected a RespondToShortcut prompt");
    };

    // Wrong actor for RespondToShortcut (the proposer) → rejected.
    let wrong2 = apply(
        runner.state_mut(),
        P0,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        },
    );
    assert!(
        matches!(wrong2, Err(EngineError::WrongPlayer)),
        "the proposer may not answer their own shortcut offer, got {wrong2:?}"
    );

    // Correct actor (the prompted opponent) → accepted.
    apply(
        runner.state_mut(),
        first,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        },
    )
    .expect("the prompted opponent accepts");

    // RIDER-2 — CR 732.2a decline authorization (fresh runner: the flow above consumed the
    // offer). `DeclineShortcut` is a normal protocol action dispatched via the
    // `(waiting_for, action)` match; `game/engine.rs`'s `check_actor_authorization` runs BEFORE
    // `apply_action` and keys on `WaitingFor::LoopShortcut.acting_player` == the proposer.
    // Unlike `Concede`/`Debug`, `DeclineShortcut` is NOT on any pre-match early-return
    // allowlist, so a wrong actor is rejected with the SPECIFIC `WrongPlayer` — proving the
    // decline genuinely routes THROUGH the auth firewall (a vacuous "not accepted" would also
    // pass on an allowlist bypass, which the concrete-variant assert rules out).
    let (mut drunner, dkickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = drunner.cast(dkickoff).resolve();
    let (_de, dwf) = drive_collect(&mut drunner, 500);
    assert!(
        matches!(dwf, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "decline-auth precondition: the offer must be reached with proposer P0, got {dwf:?}"
    );

    // Wrong actor for DeclineShortcut (an opponent) → the concrete WrongPlayer error.
    let wrong_decline = apply(drunner.state_mut(), P1, GameAction::DeclineShortcut);
    assert!(
        matches!(wrong_decline, Err(EngineError::WrongPlayer)),
        "an opponent may not decline the proposer's shortcut, got {wrong_decline:?}"
    );
    // The rejected action left the offer intact (no state mutation on an auth reject).
    assert!(
        matches!(drunner.state().waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "a rejected wrong-actor decline must not disturb the offer, got {:?}",
        drunner.state().waiting_for
    );

    // Correct actor (the proposer P0) → accepted; ordinary priority handed back.
    apply(drunner.state_mut(), P0, GameAction::DeclineShortcut).expect("P0 declines");
    assert!(
        matches!(drunner.state().waiting_for, WaitingFor::Priority { .. }),
        "the proposer's decline hands ordinary priority back, got {:?}",
        drunner.state().waiting_for
    );
}

// ─────────────────── T-variant-housekeeping ────────────────────────

/// T-variant-housekeeping: `WaitingFor::LoopShortcut{proposer}.acting_player()` reads the
/// `proposer` field (routing authorization to the proposer), not a constant.
#[test]
fn loop_shortcut_acting_player_reads_proposer() {
    let cert = LoopCertificate {
        unbounded: vec![],
        rising: vec![],
        win_kind: Some(WinKind::LethalDamage),
        win_kind_at: None,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
        per_cycle: None,
    };
    let wf_a = WaitingFor::LoopShortcut {
        proposer: P1,
        predicted_winner: Some(P0),
        certificate: cert.clone(),
        schema: ShortcutDecisionSchema::default(),
        declaration: None,
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };
    let wf_b = WaitingFor::LoopShortcut {
        proposer: P2,
        predicted_winner: None,
        certificate: cert.clone(),
        schema: ShortcutDecisionSchema::default(),
        declaration: None,
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };
    assert_eq!(wf_a.acting_player(), Some(P1));
    assert_eq!(wf_b.acting_player(), Some(P2));

    // And RespondToShortcut routes to its `player`.
    let proposal = ShortcutProposal {
        proposer: P0,
        predicted_winner: Some(P0),
        count: IterationCount::Fixed(1),
        unbounded: vec![],
        rising: vec![],
        win_kind: Some(WinKind::LethalDamage),
        win_kind_at: None,
        template: None,
        per_cycle: None,
        shortened_by: None,
        published_declaration: None,
        leading_aims: Vec::new(),
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };
    let wf_r = WaitingFor::RespondToShortcut {
        player: P2,
        remaining_players: vec![],
        proposal,
    };
    assert_eq!(wf_r.acting_player(), Some(P2));

    // Turn-control sibling: P0 controls P1's turn, so it is the authorized transport
    // submitter for P1's priority-held offer even though P0 is also the predicted winner.
    // The proposal authority remains P1; only the player who submits P1's choice changes.
    let mut delegated = GameState::new_two_player(42);
    delegated.active_player = P1;
    delegated.priority_player = P0;
    delegated.turn_decision_controller = Some(P0);
    delegated.waiting_for = WaitingFor::LoopShortcut {
        proposer: P1,
        predicted_winner: Some(P0),
        certificate: cert.clone(),
        schema: ShortcutDecisionSchema::default(),
        declaration: None,
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };
    apply(&mut delegated, P0, GameAction::DeclineShortcut)
        .expect("the turn controller may submit the priority holder's decline");
    assert!(
        matches!(delegated.waiting_for, WaitingFor::Priority { player } if player == P1),
        "declining under turn control returns the semantic priority holder P1 to ordinary play"
    );
}

// ─────────────── T-concede-proposer (F1 revert-guard) ────────────────

/// The latched proposer P0 concedes DURING the open APNAP window. `Concede` bypasses the
/// `WaitingFor` dispatch (engine.rs), so `proposal.proposer` is never re-validated, and
/// because the acting player (P1) is still alive the elimination self-heal leaves the stale
/// offer standing. When the last opponent accepts, the proposer-liveness guard in
/// `apply_confirmed_shortcut` (F1) must REFUSE to crown the departed proposer — CR 104.3a (a
/// player who conceded has lost and cannot be crowned), CR 104.2a (the winner must still be
/// in the game), CR 800.4a (the proposer's loop objects have already left the game) — and
/// hand priority back instead. Reverting F1 makes P2's Accept crown
/// `GameOver{winner: Some(P0)}`, a departed winner, which this test forbids.
#[test]
fn interactive_proposer_concede_mid_apnap_does_not_crown_departed() {
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional cascade must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 proposes while it has priority");

    // P0 declares → APNAP window opens on P1, with P2 queued behind.
    runner
        .act(GameAction::DeclareShortcut {
            count: offered(runner.state()).1,
            template: None,
        })
        .expect("P0 declares");
    let WaitingFor::RespondToShortcut {
        player,
        remaining_players,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "after Declare the APNAP window must open, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "window opens on P1");
    assert_eq!(remaining_players, vec![P2], "P2 queued behind P1");

    // The latched proposer P0 concedes MID-window (CR 104.3a: leaves + loses immediately).
    // The acting player is P1 (alive), so the elimination self-heal does NOT prune the
    // stale proposal — the window survives with a now-departed `proposal.proposer`.
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("P0 concedes");
    assert!(is_eliminated(&runner, P0), "P0 has left the game");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P1
        ),
        "the offer survives the conceder (acting P1 is alive), got {:?}",
        runner.state().waiting_for
    );

    // P1 accepts → advance to P2 (still alive).
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P2
        ),
        "after P1 accepts, P2 (alive) is prompted, got {:?}",
        runner.state().waiting_for
    );

    // The other answer CR 732.2b grants this same seat at this same window. A departed proposer
    // is not a fact P2's next answer can change, so refusing at the responder's seam would wedge
    // the seat instead of the answer: the seam TAKES the shortening.
    {
        let mut shortened = runner.state().clone();
        apply(
            &mut shortened,
            P2,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Shorten { at_iteration: 1 },
            },
        )
        .expect("the seam takes a place this engine will drive, whatever the proposer's liveness");
    }

    // P2 accepts (last) → would crown the departed P0 if F1 were reverted.
    let last = runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P2 accepts (last)");

    // F1: the proposer-liveness guard refuses to crown the departed P0 and hands
    // priority back for a later LIVE re-detect. Reverting F1 flips this to
    // GameOver{winner: Some(P0)}.
    assert_ne!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "a departed proposer (P0 conceded) must NOT be crowned (CR 104.2a / 104.3a)"
    );
    match runner.state().waiting_for {
        WaitingFor::Priority { player } => {
            assert!(
                !is_eliminated(&runner, player),
                "F1 must hand priority to a LIVING player (CR 800.4a), not the departed proposer; got Priority {{{player:?}}}"
            );
            assert_ne!(
                player, P0,
                "priority must not return to the conceded proposer P0"
            );
        }
        ref other => panic!("F1 hands priority back (manual fallback), got {other:?}"),
    }
    assert!(
        !last
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::GameOver { winner } if *winner == Some(P0))),
        "no GameOver{{Some(P0)}} event may be emitted for the departed proposer"
    );
}

// ──────────────── T-concede-queued (F2 revert-guard) ────────────────

/// A QUEUED opponent (P2, not yet prompted) concedes AFTER the window opened. `Concede`
/// bypasses the `WaitingFor` dispatch, so `remaining_players` still lists the departed seat.
/// When the prompted opponent (P1) accepts, the liveness filter in
/// `handle_respond_to_shortcut` (F2) must DROP the departed seat and — finding no living
/// remainder — take the shortcut for the still-living proposer P0 instead of advancing onto
/// the departed P2 (CR 800.4a: never wait on a player who has left; F1 then re-validates P0's
/// own liveness before crowning). Reverting F2 makes P1's Accept set
/// `RespondToShortcut{player: P2}` — a permanent wait on a departed player.
#[test]
fn interactive_queued_opponent_concede_no_deadlock() {
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional cascade must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 proposes while it has priority");

    runner
        .act(GameAction::DeclareShortcut {
            count: offered(runner.state()).1,
            template: None,
        })
        .expect("P0 declares");
    let WaitingFor::RespondToShortcut {
        player,
        remaining_players,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "after Declare the APNAP window must open, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P1, "window opens on P1");
    assert_eq!(remaining_players, vec![P2], "P2 queued behind P1");

    // The QUEUED (not-yet-prompted) opponent P2 concedes. Acting player is P1 (alive), so the
    // self-heal leaves the window on P1 — but `remaining_players` still lists the departed P2.
    runner
        .act(GameAction::Concede { player_id: P2 })
        .expect("P2 concedes");
    assert!(is_eliminated(&runner, P2), "P2 has left the game");
    assert!(
        !is_eliminated(&runner, P0) && !is_eliminated(&runner, P1),
        "P0/P1 remain in the game"
    );

    // P1 accepts. F2 drops departed P2 from the queue; no living remainder ⇒ take the
    // shortcut for the still-living P0 — NOT advance onto departed P2.
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts (last living opponent)");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P2
        ),
        "must NOT wait on the departed P2 (CR 800.4a), got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) },
        "the last living opponent accepted ⇒ crown the still-living proposer P0"
    );
}

// ───────────── T-subset-lethal (D2 — nonfallers.len()==1 guard) ─────────────

/// Marauding Blight-Priest and Exquisite Blood under P0 with Blessed Wine in hand, and Platinum
/// Emperion under P2. Returns the runner and Blessed Wine.
fn setup_3p_emperion_drain() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Marauding Blight-Priest", 3, 2, MARAUDING_BLIGHT_PRIEST);
    scenario.add_enchantment_from_oracle(P0, "Exquisite Blood", EXQUISITE_BLOOD);
    scenario.add_creature_from_oracle(P2, "Platinum Emperion", 8, 8, PLATINUM_EMPERION);
    let wine = scenario
        .add_spell_to_hand_from_oracle(P0, "Blessed Wine", true, BLESSED_WINE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, wine)
}

/// CR 104.2a: Marauding Blight-Priest drains each opponent, but Platinum Emperion keeps P2's life
/// fixed (CR 101.2), so P1's crossing is the only elimination a repetition reaches: the offer's
/// count is P1's life at the frame and it names no winner.
#[test]
fn interactive_3p_subset_lethal_does_not_crown() {
    let (mut runner, wine) = setup_3p_emperion_drain();
    let _ = runner.cast(wine).resolve();
    let (events, wf) = drive_collect(&mut runner, 500);

    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        schema,
        ..
    } = &wf
    else {
        panic!("the drain must reach an offer, got {wf:?}");
    };
    let p1 = life(&runner, P1);
    assert!(p1 > 0 && p1 < 20, "the drain ran on P1, life = {p1}");
    assert_eq!(
        (
            *proposer,
            schema.iteration_count.clone(),
            *predicted_winner,
            life(&runner, P2),
        ),
        (P0, IterationCount::Fixed(p1 as u32), None, 20)
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::GameOver { .. })),
        "no game end precedes the offer"
    );
}

// ───────────── PR-7 Combo-UI Stage 2 — E1 drive-and-measure crown ──────────────

/// Vito, Thorn of the Dusk Rose, Sanguine Bond and Bloodthirsty Conqueror under P0 with Blessed
/// Wine in hand; P1 at 6 holding Lightning Bolt off a Mountain. Returns the runner and Blessed
/// Wine.
fn setup_2p_vito_wine() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Vito, Thorn of the Dusk Rose", 1, 3, VITO);
    scenario.add_enchantment_from_oracle(P0, "Sanguine Bond", SANGUINE_BOND);
    scenario.add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 5, 5, BLOODTHIRSTY_CONQUEROR);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let wine = scenario
        .add_spell_to_hand_from_oracle(P0, "Blessed Wine", true, BLESSED_WINE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, wine)
}

/// A synthetic `LethalDamage` offer certificate for injecting a `LoopShortcut`
/// on a loop that never offers naturally (subset-lethal / >2p targeted).
fn synthetic_lethal_cert() -> LoopCertificate {
    LoopCertificate {
        unbounded: vec![],
        rising: vec![],
        win_kind: Some(WinKind::LethalDamage),
        win_kind_at: None,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
        per_cycle: None,
    }
}

/// Accept the shortcut from every remaining living opponent (drain-one-advance APNAP), for
/// injected offers with any opponent count.
fn accept_all_opponents(runner: &mut GameRunner) {
    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("living opponent accepts the shortcut");
    }
}

/// CR 405.5 + CR 608.1: each gain puts two "target opponent" triggers on the stack and the top
/// one resolves before the next pair lands on it, so a repetition costs P1 1: at the frame P0 22 /
/// P1 5 the offer is 5, won by P0.
#[test]
fn vito_2p_optional_offer_declare_crowns() {
    let (mut runner, wine) = setup_2p_vito_wine();
    let _ = runner.cast(wine).resolve();
    let _ = drive_collect(&mut runner, 2000);
    assert_eq!(
        (offered(runner.state()), seat_lives(runner.state())),
        (
            (P0, IterationCount::Fixed(5), Some(P0)),
            vec![(P0, 22), (P1, 5)]
        )
    );
    let offer = runner.state().clone();

    let mut taken = offer.clone();
    r6a_declare_and_accept_all(&mut taken, P0, 5);
    assert_eq!(taken.waiting_for, WaitingFor::GameOver { winner: Some(P0) });

    let mut past = offer;
    r6a_declare_and_accept_all(&mut past, P0, 6);
    assert_eq!(
        (past.waiting_for.clone(), seat_lives(&past)),
        (WaitingFor::Priority { player: P0 }, vec![(P0, 22), (P1, 5)])
    );
}

/// Test E ⭐ (SOUNDNESS #1 firewall, item 3): the declare-time `validate_pins` firewall REJECTS
/// an illegal-value pin (a target outside the slot's offered `legal_targets`) BEFORE APNAP opens
/// (⇒ manual-play Priority), and INGESTS a legal pin (⇒ RespondToShortcut opens). REVERT-PROBE:
/// removing the validate hook lets the illegal pin open the response window (a leak).
#[test]
fn declare_illegal_pin_falls_back_legal_ingests() {
    // A schema exposing ONE Targets slot whose only legal target is Player(P1).
    let source = YieldTarget::ThisObject {
        source_id: ObjectId(1),
        incarnation: None,
        trigger_description: None,
    };
    let slot = DecisionSlot {
        source: source.clone(),
        point: ChoicePoint::AnnouncedTarget,
        index: 0,
    };
    let schema = ShortcutDecisionSchema {
        suggested_declaration: None,
        leading_aims: Vec::new(),
        iteration_count: IterationCount::Fixed(1),
        // This producer measured no CR 704 threshold — `Default` carries the absence and a
        // capacity at the global cap.
        measured_repetition_bound: None,
        deliverable_capacity: ShortcutDecisionSchema::default().deliverable_capacity,
        points: vec![DecisionPoint {
            slot: slot.clone(),
            kind: DecisionPointKind::Targets {
                legal_targets: vec![TargetRef::Player(P1)],
                min_targets: 1,
                max_targets: 1,
                ordered: true,
            },
        }],
        convoke_tappable_count: 0,
    };
    let template_for = |pinned: PlayerId| DecisionTemplate {
        owner: P0,
        decisions: vec![PinnedDecision::Targets {
            slot: slot.clone(),
            targets: vec![TargetPin::Player(pinned)],
        }],
        replay: ReplayMode::Scheduled {
            count: IterationCount::Fixed(1),
        },
        key: DecisionGroupKey::from_sources(
            std::slice::from_ref(&source),
            DecisionKind::LoopChoice,
        ),
    };

    // ILLEGAL half: pin Player(P2), not in the offered legal set ⇒ rejected to Priority.
    let (mut runner, _kickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: synthetic_lethal_cert(),
        schema: schema.clone(),
        declaration: None,
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: Some(template_for(P2)),
        })
        .expect("declare dispatch succeeds (the rejection is a manual-fallback, not an error)");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "an illegal-value pin is REJECTED before APNAP (manual fallback), got {:?}",
        runner.state().waiting_for
    );

    // LEGAL half (reach-guard, not always-reject): pin Player(P1) ⇒ RespondToShortcut opens.
    let (mut runner2, _kickoff2) = setup_3p_draw(LoopDetectionMode::Interactive);
    runner2.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: synthetic_lethal_cert(),
        schema,
        declaration: None,
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };
    runner2
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: Some(template_for(P1)),
        })
        .expect("declare with a legal pin");
    assert!(
        matches!(
            runner2.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "a legal pin is INGESTED — the response window opens, got {:?}",
        runner2.state().waiting_for
    );
}

// ─────────────────── T-B3-materialize (Phase 4b) ───────────────────────

/// Reach `LoopShortcut{P0}` on a fresh `setup_2p_optional_drain(Interactive)` fixture.
/// Returns the runner parked at the offer, `life(P1)` at that instant, and the
/// DRAIN_CLERIC object id (for template pins).
pub(crate) fn reach_2p_optional_drain_offer() -> (GameRunner, i32, ObjectId) {
    reach_drain_offer(setup_2p_optional_drain(LoopDetectionMode::Interactive))
}

/// [`reach_2p_optional_drain_offer`] on [`setup_2p_blight_priest_drain`]'s board; the id is
/// Marauding Blight-Priest's.
pub(crate) fn reach_2p_blight_priest_drain_offer() -> (GameRunner, i32, ObjectId) {
    reach_drain_offer(setup_2p_blight_priest_drain())
}

fn reach_drain_offer(
    (mut runner, kickoff, _bolt, cleric): (GameRunner, ObjectId, ObjectId, ObjectId),
) -> (GameRunner, i32, ObjectId) {
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional drain must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    let l0 = life(&runner, P1);
    (runner, l0, cleric)
}

/// Probe the per-cycle P1 drain constant via an independent `Fixed(1)` materialization
/// of the DRAIN_CLERIC/BLOOD_SIPPER pairing (one recurrence = one full cycle).
fn probe_drain_delta() -> i32 {
    let (mut runner, l0, _cleric) = reach_2p_optional_drain_offer();
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: None,
        })
        .expect("declare Fixed(1)");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");
    let delta = l0 - life(&runner, P1);
    assert!(
        delta > 0,
        "Fixed(1) must materialize a nonzero drain cycle, got delta={delta}"
    );
    delta
}

/// B3-materialize-stop-short ⭐ (N < cycles-to-lethal): P1's life must drop EXACTLY
/// `N*delta` — a NON-ZERO multiple. This is the empirical BLOCKER #2 gate: if the
/// per-cycle recurrence boundary is unseeded (`waiting_for` never re-matches
/// `Priority{active}`), the drive spins to `cycle_beat_cap` every iteration and aborts at
/// 0 complete cycles, so drop==0 and this assertion FAILS; under the pre-4b decline-stub,
/// drop==0 too — both revert targets are caught by the same assertion.
#[test]
fn b3_materialize_stop_short() {
    let delta = probe_drain_delta();
    let (mut runner, l0, _cleric) = reach_2p_optional_drain_offer();
    let n: u32 = 3;
    assert!(
        (n as i32) * delta < l0,
        "test precondition: N*delta must stay short of lethal (l0={l0}, delta={delta})"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        })
        .expect("declare Fixed(N)");
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("accept");

    assert_eq!(
        life(&runner, P1),
        l0 - (n as i32) * delta,
        "P1 life must drop EXACTLY N*delta"
    );
    assert!(
        !is_eliminated(&runner, P1),
        "P1 must remain alive (N below cycles-to-lethal)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "must not reach GameOver, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "materialization stops at Priority{{living_priority_seat}} (P0) — manual fallback, \
         not a wrong-crown or a stuck handback"
    );
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "the ring must be cleared on stop-short (Q3) so the same apply() does not instantly \
         re-offer"
    );
}

/// PR-7 DoS cap (CR 732.2a SAFETY LIMIT): a `Fixed` count over `MAX_SHORTCUT_CYCLES` is
/// handed back to manual play with NO drive. This is the engine-side count cap that stops the
/// catastrophic 4-byte remote vector — `Fixed(u32)` scalar-encodes ~4.3e9 cycles in ~10 bytes,
/// sailing through the WS frame cap (`phase-server`'s `MAX_WS_MESSAGE_BYTES`, 64 KB). The count
/// is HARDCODED as
/// `Fixed(u32::MAX)`; the cap
/// const is private to the engine crate and invisible across this integration-test boundary.
///
/// VACUITY TRAP (PR-7): a handback lands on `WaitingFor::Priority`, and so does the cap-ABSENT
/// stop-short path (a drive commits + stops there too). So `waiting_for` alone is an INVARIANT,
/// not a discriminator. The DISCRIMINATOR is the observable DRIVE: `life(P1) == l0` proves the
/// cap fired before any cycle ran. The revert-probe (delete Edit B's guard body) opens APNAP,
/// Accept drives, and on this life-DRAIN fixture P1 crosses lethal in ~l0/delta cycles (tens —
/// `materialize_fixed_shortcut`'s CrossLethal arm commits + stops, so `u32::MAX` does NOT hang)
/// ⇒ `life(P1) ≤ 0` + GameOver ⇒ `assert_eq!(life(P1), l0)` FAILS.
///
/// Positive reach-guard: `b3_materialize_stop_short` (n=3) proves the harness DOES drive when
/// n ≤ cap, so T1's no-drive is the cap firing, not a dead fixture.
#[test]
fn over_cap_fixed_count_hands_back_with_no_drive() {
    let (mut runner, l0, _cleric) = reach_2p_optional_drain_offer();

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(u32::MAX),
            template: None,
        })
        .expect("declare Fixed(u32::MAX)");

    // Symmetric-across-revert Accept: with Edit B active the declare hands back immediately
    // (APNAP never opens), so the Accept would be illegal — issue it ONLY when actually parked
    // at RespondToShortcut (the cap-absent revert path).
    if matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        runner
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("accept");
    }

    // DRIVE discriminator: the cap fired BEFORE any cycle ran, so P1's life is untouched.
    assert_eq!(
        life(&runner, P1),
        l0,
        "over-cap Fixed hands back with NO drive — P1 life unchanged (the discriminator)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "no crown — the drive never ran, got {:?}",
        runner.state().waiting_for
    );
    // CR 732.2a: the handback is ordinary priority for the living seat.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "handback to living_priority_seat (P0)"
    );
}

// ═══════════════════ Interactive draw gate ═══════════════════

/// Poison rider for the DRAW-gate behavioral test: fires on the SAME "whenever you gain
/// life" event the SELF_LIFE_ENGINE cascade pumps, dripping a poison counter onto each
/// opponent every cycle. Non-targeted (no mid-drive target prompt ⇒ mandatory-preserving).
const POISON_RIDER: &str = "Whenever you gain life, each opponent gets a poison counter.";

/// 3-player MANDATORY self-sustaining lifegain cascade (SELF_LIFE_ENGINE) that ALSO drips
/// poison onto each opponent every cycle (POISON_RIDER, a SEPARATE second trigger), so P0 orders
/// the two triggers each cycle. Nobody loses LIFE, so Path A's `live_mandatory_loop_winner` finds
/// no faller; opponents accrue POISON.
fn setup_3p_poison_draw(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    scenario.add_creature_from_oracle(P0, "Test Life Engine", 2, 2, SELF_LIFE_ENGINE);
    scenario.add_creature_from_oracle(P0, "Test Poison Dripper", 2, 2, POISON_RIDER);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// CR 732.4 + CR 704.5c: the pure lifegain loop is a draw; the same loop with a poison rider is
/// no draw. The loop continues whichever of P0's two triggers resolves first (CR 603.3b), so the
/// order is no decision and P0 wins the mandatory loop outright.
#[test]
fn interactive_recurring_poison_is_not_drawn() {
    // CONTROL (differential anchor): the SHARED pure-lifegain structure reaches the CR 732.4
    // gate and DRAWS — establishes that this fixture shape is one that CAN be certified a draw,
    // so the variant's not-drawing is attributable to the one added line (the poison rider).
    let (mut control, ckickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
    let _ = control.cast(ckickoff).resolve();
    let (_ce, cwf) = drive_collect(&mut control, 500);
    assert_eq!(
        cwf,
        WaitingFor::GameOver { winner: None },
        "control anchor: the pure-lifegain structure IS certified a CR 732.4 draw — so the ONLY \
         fixture change (the poison rider) is what makes the variant below not-draw"
    );

    let (mut runner, kickoff) = setup_3p_poison_draw(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (events, wf) = drive_collect(&mut runner, 500);
    let players = &runner.state().players;
    assert_eq!(wf, WaitingFor::GameOver { winner: Some(P0) });
    assert_eq!(
        players
            .iter()
            .map(|p| p.poison_counters)
            .collect::<Vec<_>>(),
        vec![0, 1, 1]
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::GameOver { winner: None })),
        "no CR 732.4 draw event may be emitted for a poison-dripping loop"
    );
}

/// LOW-2: the AI's `RespondToShortcut` decision self-preserves. Positive: the polled
/// opponent with a meaningful action (a castable Bolt) Shortens rather than Accepting its
/// own loss, and applying that response actually hands it a real priority window.
/// Control: the SAME fixture/flow's second APNAP responder — who holds no meaningful
/// action — gets Accept from the identical `smart_shortcut_response` call.
#[test]
fn low2_smart_shortcut_self_preservation() {
    // Positive: P1 (has the Bolt) self-preserves via Shorten.
    let (mut runner, kickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 500);
    let WaitingFor::LoopShortcut { proposer, .. } = wf else {
        panic!("optional cascade must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0);
    runner
        .act(GameAction::DeclareShortcut {
            count: offered(runner.state()).1,
            template: None,
        })
        .expect("P0 declares");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P1
        ),
        "positive reach-guard: P1 must be prompted before the AI decision is tested"
    );

    let p1_response = engine::ai_support::smart_shortcut_response(runner.state(), P1);
    assert_eq!(
        p1_response,
        ShortcutResponse::Shorten { at_iteration: 1 },
        "P1's Lightning Bolt and Mountain survive one repetition, so the take to place 1 seats \
         P1 with Bolt castable ⇒ smart_shortcut_response self-preserves via Shorten, not Accept \
         of its own loss"
    );
    runner
        .act(GameAction::RespondToShortcut {
            response: p1_response,
        })
        .expect("apply P1's AI decision");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P2
        ),
        "the shortened proposal is still polled to P2; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P2 accepts the shortened proposal");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 },
        "the shortened take's ending point is P1's — it survives"
    );
    assert!(
        life(&runner, P1) > 0,
        "P1 is alive — one repetition does not drain it"
    );

    // Control: the identical fixture/flow, but P1 Accepts (submitted manually, not via the
    // AI, so the APNAP queue advances instead of stopping) so the SECOND responder (P2,
    // who holds no meaningful action) is reached. `smart_shortcut_response` must Accept.
    let (mut crunner, ckickoff) = setup_3p_optional_cascade(LoopDetectionMode::Interactive);
    let _ = crunner.cast(ckickoff).resolve();
    let (_ce, cwf) = drive_collect(&mut crunner, 500);
    assert!(matches!(cwf, WaitingFor::LoopShortcut { .. }));
    crunner
        .act(GameAction::DeclareShortcut {
            count: offered(crunner.state()).1,
            template: None,
        })
        .expect("declare");
    assert!(
        matches!(
            crunner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P1
        ),
        "positive reach-guard: P1 is first in APNAP order"
    );
    crunner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1 accepts (manually, to advance the APNAP queue to P2)");
    assert!(
        matches!(
            crunner.state().waiting_for,
            WaitingFor::RespondToShortcut { player, .. } if player == P2
        ),
        "positive reach-guard: P2 is prompted second"
    );

    let p2_response = engine::ai_support::smart_shortcut_response(crunner.state(), P2);
    assert_eq!(
        p2_response,
        ShortcutResponse::Accept,
        "control: P2 holds no meaningful action ⇒ smart_shortcut_response must Accept \
         (revert-failing: an unconditional-Accept revert makes P1's response above Accept \
         too, which crowns P0's win with P1 still a faller — the Shorten assertion above \
         would fail first)"
    );
}

// ---------------------------------------------------------------------------
// PR-7 Phase 4d-ii — LIVE object-growth detection + offer (the 51st: Witherbloom,
// the Balancer + Sprout Swarm token-growth infinite). Cast-pipeline tests: real
// parsed AST (verbatim Oracle text), driven through `GameRunner::cast(..).resolve()`.
// ---------------------------------------------------------------------------

/// Sprout Swarm's verbatim Oracle text (Scryfall / card-data.json).
const SPROUT_SWARM_ORACLE: &str = "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {1} or one mana of that creature's color.)\nBuyback {3} (You may pay an additional {3} as you cast this spell. If you do, put this card into your hand as it resolves.)\nCreate a 1/1 green Saproling creature token.";

/// Witherbloom's granted-affinity Oracle line (the loop-relevant clause).
const WITHERBLOOM_AFFINITY_ORACLE: &str =
    "Instant and sorcery spells you cast have affinity for creatures.";

/// Build the 51st fixture: Witherbloom (granted affinity) + `n_fodder` untapped green
/// 1/1 Saproling creatures + Sprout Swarm ({1}{G}, Buyback {3}, Convoke) in P0's hand.
/// Returns `(runner, sprout_id, fodder_ids)`. `Interactive` loop-detection ON.
pub(crate) fn sprout_swarm_scenario(n_fodder: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    sprout_swarm_scenario_with_drain(n_fodder, None)
}

/// As [`sprout_swarm_scenario`], but optionally adds a big "Test Drain Engine" permanent whose
/// `drain_oracle` (a `"Whenever you cast a spell, ..."` trigger) fires on EACH recast and drains
/// a resource axis in the LIVE recast body — the N4/N5/N6 no-offer negative controls. The engine
/// is a 9/9 so a self-damage drain does not kill it within the 2-iteration detection drive.
fn sprout_swarm_scenario_with_drain(
    n_fodder: usize,
    drain_oracle: Option<&str>,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Witherbloom, the Balancer",
        5,
        5,
        WITHERBLOOM_AFFINITY_ORACLE,
    );
    if let Some(oracle) = drain_oracle {
        scenario.add_creature_from_oracle(P0, "Test Drain Engine", 9, 9, oracle);
    }
    let mut fodder = Vec::new();
    for _ in 0..n_fodder {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        // The starting fodder must be GREEN so convoke can tap it for the {G} pip, and tokens
        // like the Saprolings the loop mints (CR 111.1).
        for &id in &fodder {
            let object = st.objects.get_mut(&id).unwrap();
            object.color = vec![ManaColor::Green];
            object.is_token = true;
        }
    }
    (runner, sprout, fodder)
}

/// Count real Saproling tokens/creatures on P0's battlefield in a state.
fn saproling_count(state: &GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.name == "Saproling"))
        .count()
}

/// P1 ⭐ — the 51st COVERS and OFFERS. A single real Witherbloom/Sprout-Swarm cast (paying
/// buyback and convoke) settles with an empty stack; the empty-stack hook drives two recast
/// iterations on a clone, confirms the fodder-growth cover and sign-check, and OFFERS the
/// interactive shortcut. Discriminators: the offer reaches `LoopShortcut`; and clone-isolation,
/// exactly ONE real Saproling was created by the single real cast (the drives ran on clones).
#[test]
fn object_growth_51st_sprout_swarm_covers_and_offers() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let before = saproling_count(runner.state());
    let outcome = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3}
        .convoke_with(&[fodder[0]]) // tap one green Saproling for the {G} pip
        .commit()
        .resolve();

    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, predicted_winner, .. }
                if *proposer == P0 && predicted_winner.is_none()
        ),
        "expected LoopShortcut offer to P0, got {:?}",
        outcome.final_waiting_for()
    );
    let WaitingFor::LoopShortcut { certificate, .. } = outcome.final_waiting_for() else {
        unreachable!()
    };
    assert_eq!(
        certificate.win_kind,
        Some(WinKind::Advantage),
        "an inert token-growth loop is a CR 104.4b optional Advantage loop"
    );
    assert!(
        certificate.unbounded.contains(&ResourceAxis::TokensCreated),
        "the unbounded axis must name TokensCreated, got {:?}",
        certificate.unbounded
    );
    // Clone-isolation (risk iii): the two detection drives ran on CLONES and must not
    // leak — exactly 4 starting + 1 from the single real cast = 5 real Saprolings.
    assert_eq!(
        saproling_count(outcome.state()),
        before + 1,
        "the clone drives must not leak real tokens (INV-1)"
    );
    // Sprout Swarm returned to hand (CR 702.27a buyback) — recastable for the loop.
    assert_eq!(outcome.zone_of(sprout), engine::types::zones::Zone::Hand);

    // The live trace holds the real cast's discriminating fields: P0's cast of the recastable
    // Sprout Swarm, by its card identity (CR 400.7).
    let hand_sprout = outcome
        .state()
        .objects
        .values()
        .find(|o| {
            o.name == "Sprout Swarm"
                && o.controller == P0
                && o.zone == engine::types::zones::Zone::Hand
        })
        .expect("Sprout Swarm recastable in hand");
    let cast = engine::game::play_trace_view(outcome.state())
        .and_then(|view| {
            view.entries.into_iter().find_map(|entry| match entry.kind {
                engine::game::EntryKind::Play {
                    action: GameAction::CastSpell { card_id, .. },
                    locus,
                    ..
                } => Some((entry.seat, locus, card_id)),
                _ => None,
            })
        })
        .expect("the buyback + token-creating cast is traced");
    assert_eq!(
        cast,
        (
            P0,
            engine::game::PlayLocus::Cast(hand_sprout.id),
            hand_sprout.card_id
        ),
        "the traced cast is P0's, of the recastable Sprout Swarm"
    );
}

/// **CR 708.2a + CR 732.2a: a face-down permanent moves neither the offer verdict nor either
/// projection leaf's job.**
///
/// The offering board with ONE grafted face-down permanent still reaches `LoopShortcut` under
/// either controller, which is what licenses `proposer_hidden_view`'s `FaceDownRevealed` no-op
/// arm as a rules decision rather than a verdict-moving one — the driven clone may carry no
/// name (CR 708.2a) and the verdict does not depend on which name it carries. The ungrafted
/// arm is the control: the graft is not what produces the offer. The wire projection is the
/// reach guard — in the SAME run the controller's copy carries the back-face name and the
/// other seat's carries the hidden-card name, so the two-sided pair keeps BOTH its entries.
#[test]
fn object_growth_offer_survives_a_face_down_permanent_under_either_controller() {
    use engine::game::game_object::BackFaceData;
    use engine::game::visibility::filter_state_for_viewer;
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    // A typeless inert permanent: the face-down leaves key on `face_down && back_face`, and
    // giving it no card types keeps it out of both the fodder class and every SBA.
    let run = |grafted_controller: Option<PlayerId>| -> (GameRunner, Option<ObjectId>) {
        let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
        let grafted = grafted_controller.map(|controller| {
            let id = create_object(
                runner.state_mut(),
                CardId(900),
                controller,
                "Grafted Permanent".into(),
                Zone::Battlefield,
            );
            let obj = runner
                .state_mut()
                .objects
                .get_mut(&id)
                .expect("just created");
            obj.face_down = true;
            obj.back_face = Some(BackFaceData {
                name: "Grizzly Bears".to_string(),
                power: Some(2),
                toughness: Some(2),
                ..Default::default()
            });
            id
        });
        let outcome = runner
            .cast(sprout)
            .accept_optional()
            .convoke_with(&[fodder[0]])
            .commit()
            .resolve();
        assert!(
            matches!(
                outcome.final_waiting_for(),
                WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
            ),
            "expected a LoopShortcut offer to P0 (grafted controller {grafted_controller:?}), \
             got {:?}",
            outcome.final_waiting_for()
        );
        (runner, grafted)
    };

    // CONTROL: the same board with no face-down permanent offers.
    run(None);

    for controller in [P0, P1] {
        let (runner, grafted) = run(Some(controller));
        let id = grafted.expect("the grafted arm builds a permanent");
        let observer = if controller == P0 { P1 } else { P0 };

        let to_controller = filter_state_for_viewer(runner.state(), controller);
        assert_eq!(
            to_controller.objects.get(&id).map(|o| o.name.as_str()),
            Some("Grizzly Bears"),
            "CR 708.5: the controller's wire copy carries the back-face name"
        );

        let to_observer = filter_state_for_viewer(runner.state(), observer);
        assert_eq!(
            to_observer.objects.get(&id).map(|o| o.name.as_str()),
            Some("Hidden Card"),
            "and the other seat's carries the hidden-card name in the SAME run — the \
             observer-side face-down redaction leaf is what writes this entry"
        );
    }
}

/// Kodama of the East Tree's growing-class-reading trigger (Scryfall / card-data).
/// Its body puts a permanent "with equal or lesser mana value" from hand onto the
/// battlefield — a `ChangeZone` whose target filter reads a mutable board aggregate,
/// so `fire_time_conditions_read_growing_class` flags it IF it is scanned.
const KODAMA_TRIGGER_ORACLE: &str = "Whenever another permanent you control enters, if it wasn't put onto the battlefield with this ability, you may put a permanent card with equal or lesser mana value from your hand onto the battlefield.";

/// The 51st fixture with Kodama relocated into `zone`, where its "another permanent you
/// control enters" trigger cannot function: CR 113.6 puts a permanent's abilities on the
/// battlefield unless the ability declares otherwise (CR 113.6b), and this one declares
/// nothing. Kodama parses ON the battlefield, so the definition is a real parse before it is
/// moved. Everything else is the passing fixture, so the ONLY variable either row below
/// carries is the inert observer's zone.
fn growing_class_observer_outside_the_battlefield(
    zone: engine::types::zones::Zone,
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    use engine::types::zones::Zone;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Witherbloom, the Balancer",
        5,
        5,
        WITHERBLOOM_AFFINITY_ORACLE,
    );
    // Kodama parses ON the battlefield, so its trigger is a real parsed def before the
    // relocation below.
    let kodama = scenario
        .add_creature_from_oracle(P0, "Kodama of the East Tree", 6, 6, KODAMA_TRIGGER_ORACLE)
        .id();
    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
        st.battlefield.retain(|&id| id != kodama);
        let obj = st.objects.get_mut(&kodama).unwrap();
        obj.zone = zone;
        let p0 = st.players.iter_mut().find(|p| p.id == P0).unwrap();
        match zone {
            Zone::Library => p0.library.insert(0, kodama),
            Zone::Graveyard => p0.graveyard.push_back(kodama),
            other => panic!(
                "this fixture places the observer in a library or a graveyard, not {other:?}"
            ),
        }
    }

    // The observer really sits where the caller asked, so an offer must come from correctly
    // IGNORING it and not from its having been removed.
    let kodama_obj = &runner.state().objects[&kodama];
    assert_eq!(
        kodama_obj.zone, zone,
        "the growing-class observer must sit in {zone:?} for the row to discriminate",
    );
    assert_eq!(
        kodama_obj.trigger_definitions.len(),
        1,
        "reach-guard: the observer must have PARSED — a misparse leaves zero trigger \
         defs and the offer below forms for the wrong reason (nothing to ignore); got {}",
        kodama_obj.trigger_definitions.len()
    );

    (runner, sprout, kodama, fodder)
}

/// Drive the fixture's one Sprout Swarm cycle and assert the CR 732.2a offer surfaces naming
/// the token-growth axis — the loop genuinely detected, not an unrelated fall-through.
fn assert_growing_class_observer_is_ignored(
    mut runner: GameRunner,
    sprout: ObjectId,
    fodder: &[ObjectId],
    why: &str,
) {
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();

    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "{why}, got {:?}",
        outcome.final_waiting_for()
    );
    let WaitingFor::LoopShortcut { certificate, .. } = outcome.final_waiting_for() else {
        unreachable!()
    };
    assert!(
        certificate.unbounded.contains(&ResourceAxis::TokensCreated),
        "{why}: the detected loop's unbounded axis must be TokensCreated, got {:?}",
        certificate.unbounded
    );
}

/// A growing-class-reading trigger on a library card (Kodama of the East Tree) does not suppress
/// the loop-shortcut offer.
#[test]
fn object_growth_library_observer_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    let (runner, sprout, _kodama, fodder) =
        growing_class_observer_outside_the_battlefield(Zone::Library);
    assert_growing_class_observer_is_ignored(
        runner,
        sprout,
        &fodder,
        "a growing-class trigger in the LIBRARY must not suppress the offer",
    );
}

/// The same observer in P0's graveyard, its parsed trigger intact, does not suppress the offer
/// either.
#[test]
fn object_growth_public_zone_observer_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    let (runner, sprout, kodama, fodder) =
        growing_class_observer_outside_the_battlefield(Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&kodama].trigger_definitions.len(),
        1,
        "reach-guard: a graveyard is public (CR 404.2), so the definition survives"
    );
    assert_growing_class_observer_is_ignored(
        runner,
        sprout,
        &fodder,
        "a growing-class trigger in a PUBLIC non-battlefield zone must not suppress the offer",
    );
}

/// Find the (single) object named `name` controlled by `player` in `zone`.
fn object_named_in_zone(
    state: &GameState,
    name: &str,
    player: PlayerId,
    zone: engine::types::zones::Zone,
) -> Option<ObjectId> {
    state
        .objects
        .values()
        .find(|o| o.name == name && o.controller == player && o.zone == zone)
        .map(|o| o.id)
}

/// P2 ⭐ (updated 2026-07-18, user directive): ACCEPTING an unbounded object-growth (fodder /
/// token) shortcut MARKS the certificate's ∞ axes via the shared `mark_unbounded_loop` writer
/// and materializes ZERO discrete tokens — the ∞ status IS the applied result (contrast the
/// old O(N) drive, which capped the infinite at N and cost ≈0.4 s/token / 212 s for 500). The
/// finite tokens are minted later, at the CR 500.4 phase/turn boundary, when the player names a
/// finite count for each ∞ status; accept itself only flags the status. Declaring `Fixed(5)`
/// yet getting 0 tokens is itself discriminating — it proves the count is ignored (no drive).
///
/// DISCRIMINATING (revert-probe verified): deleting the `mark_unbounded_loop` call in
/// `materialize_object_growth_shortcut` leaves `unbounded_resources` empty ⇒ the ∞-status
/// assertion FLIPS to fail ("must mark unbounded_resources"); a re-introduced N-iteration drive
/// would break the board-invariance assertion (it would add ≥1 Saproling).
#[test]
fn object_growth_51st_accept_marks_unbounded_and_mints_no_tokens() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    let WaitingFor::LoopShortcut { certificate, .. } = outcome.final_waiting_for() else {
        panic!(
            "P2 precondition: the offer must fire, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert!(certificate.unbounded.contains(&ResourceAxis::TokensCreated));
    assert!(
        runner.state().unbounded_resources.is_empty(),
        "the OFFER must not pre-mark the ∞ status (only accepting does)"
    );
    let at_offer = saproling_count(runner.state());

    // P0 (LoopShortcut.proposer — inferred submitter) declares a Fixed(5) shortcut. The count
    // is ignored for an unbounded loop (the ∞ mark is count-independent); Fixed(5) here is a
    // discriminator that a re-introduced drive would turn into +5 tokens.
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(5),
            template: None,
        })
        .expect("declare shortcut");
    // The lone opponent (P1 — inferred RespondToShortcut submitter) accepts ⇒ mark ∞.
    runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("respond accept");

    // (1) ∞ status APPLIED — the revert-probe target.
    let axes = runner
        .state()
        .unbounded_resources
        .get(&P0)
        .expect("accepting an unbounded loop must mark unbounded_resources for the controller");
    assert!(
        axes.contains(&ResourceAxis::TokensCreated),
        "the marked axis must be TokensCreated, got {axes:?}"
    );
    // (2) ZERO tokens minted at accept — the finite count is named later, at the phase boundary.
    assert_eq!(
        saproling_count(runner.state()),
        at_offer,
        "accepting an unbounded loop must not drive discrete iterations"
    );
    assert!(
        object_named_in_zone(
            runner.state(),
            "Sprout Swarm",
            P0,
            engine::types::zones::Zone::Hand
        )
        .is_some(),
        "CR 702.27a: Sprout Swarm must still be in P0's hand after accept"
    );
    // (3) priority handed back to a living seat — the protocol closed cleanly.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "priority handed back after accept, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().loop_detect_ring.is_empty());
}

/// T-object-growth-decline ⭐ (Seam 2): CR 732.2a — the controller DECLINES the auto-offered
/// object-growth (Sprout Swarm) shortcut. The engine restores ordinary priority, clears the
/// window's trace, an ordinary action resolves, and the loop is NOT re-offered.
#[test]
fn object_growth_sprout_swarm_decline_restores_priority_no_reoffer() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let _ = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3}
        .convoke_with(&[fodder[0]]) // tap one green Saproling for the {G} pip
        .commit()
        .resolve();

    // F2 positive reach-guard: the object-growth offer was genuinely reached, and the trace the
    // decline must clear stands.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { proposer, predicted_winner, .. } if proposer == P0 && predicted_winner.is_none()),
        "Sprout Swarm must OFFER a LoopShortcut to P0, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        engine::game::play_trace_view(runner.state()).is_some(),
        "the object-growth offer stands on the window's trace (the Seam-2 gate)"
    );

    // RIDER-3 (runtime, semantic identity): the engine-owned `convoke_tappable_count` published on
    // the offer schema must equal the sum the DELETED React reduce computed over the same points
    // (Sprout Swarm convokes ⇒ a real nonzero ConvokeTaps offer). Cross-checking the published
    // count against the live ConvokeTaps `tappable` lengths proves the authority-move to the
    // engine changed no displayed value — a wrong/defaulted engine count would fail here.
    if let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for {
        let react_equivalent: usize = schema
            .points
            .iter()
            .filter_map(|p| match &p.kind {
                DecisionPointKind::ConvokeTaps { tappable } => Some(tappable.len()),
                _ => None,
            })
            .sum();
        assert!(
            react_equivalent > 0,
            "Sprout Swarm's object-growth offer must present a real nonzero ConvokeTaps schema"
        );
        assert_eq!(
            schema.convoke_tappable_count, react_equivalent,
            "engine-owned convoke_tappable_count must equal the old React reduce's sum over the same points (RIDER-3)"
        );
    }

    // CR 732.2a: the controller (P0) declines the offer.
    let decline = runner
        .act(GameAction::DeclineShortcut)
        .expect("P0 declines the object-growth shortcut");

    // (a) + (c): ordinary priority restored AND the decliner's plays discarded, so the post-return
    // reconcile does not re-name the loop.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "decline restores ordinary priority; the context-clear suppresses the immediate re-offer, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(decline.waiting_for, WaitingFor::Priority { .. }),
        "the decline result hands priority back"
    );
    assert!(
        !trace_seats(runner.state()).contains(&P0),
        "the decliner's plays were discarded on decline (Seam 2)"
    );
    assert!(runner.state().loop_detect_ring.is_empty());

    // (b) an ordinary action resolves from the restored priority window.
    runner
        .act(GameAction::PassPriority)
        .expect("an ordinary PassPriority resolves after the decline handback");

    // (c) the declined loop is not instantly re-offered on the immediate next beat.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "the declined object-growth loop must not be re-offered, got {:?}",
        runner.state().waiting_for
    );
}

/// N1 — finite-mana REJECTS (B4). Same fixture WITHOUT Witherbloom's affinity granter: each
/// recast must pay the real {1}{G}+buyback{3} = {4}{G}, which 4 untapped green creatures
/// cannot cover by convoke alone (needs 5 taps) ⇒ the injector aborts (UnpayableConvoke) ⇒
/// no offer. Revert-failing paired reach-guard: P1 (with affinity) DOES offer, so the only
/// difference is the affinity reduction feeding the sustainable {G}-only convoke cost.
#[test]
fn object_growth_no_affinity_does_not_offer() {
    // Fixture with NO Witherbloom (no affinity): 4 green Saprolings + Sprout Swarm, plus a
    // pool that funds ONE manual cast of {4}{G} so the first cast still resolves and captures
    // the recast context — isolating the DRIVEN recast's unpayability as the discriminator.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    // Fund the FIRST cast entirely from the pool ({4} generic + {G}); no convoke needed, so
    // the first cast resolves + captures the recast context, isolating the DRIVEN recast's
    // convoke-only unpayability as the sole discriminator.
    let mut mana = vec![ManaUnit::new(ManaType::Colorless, ObjectId(9_999), false, vec![]); 4];
    mana.push(ManaUnit::new(
        ManaType::Green,
        ObjectId(9_999),
        false,
        vec![],
    ));
    scenario.with_mana_pool(P0, mana);
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
    }
    let outcome = runner.cast(sprout).accept_optional().commit().resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no affinity ⇒ the driven recast can't afford {{4}}{{G}} via convoke ⇒ NO offer, got {:?}",
        outcome.final_waiting_for()
    );
}

// ---------------------------------------------------------------------------
// INTERRUPTIBILITY matched pair (combo 2+3): the opponent HOLDS a real Murder.
// Undefused (opponent passes) ⇒ the CR 732.2a object-growth shortcut is GRANTED.
// Defused (opponent Murders Witherbloom in response to Sprout Swarm on the
// stack, CR 601.2i) ⇒ the affinity granter is gone, so the empty-stack hook's
// clone-drive re-derives the recast WITHOUT affinity, convoke alone can't pay
// {4}{G} ⇒ NO grant beyond the current stack. The opponent's pass-vs-respond is
// the SOLE delta and FLIPS the outcome.
// ---------------------------------------------------------------------------

/// Arm `player` with a real castable Murder ({1}{B}{B}, "Destroy target creature.") backed by 3
/// Swamps — the held defuse. Returns the Murder's `ObjectId`.
fn arm_murder(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    for _ in 0..3 {
        scenario.add_basic_land(player, ManaColor::Black);
    }
    let mut murder =
        scenario.add_spell_to_hand_from_oracle(player, "Murder", true, "Destroy target creature.");
    murder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 1,
    });
    murder.id()
}

/// As [`sprout_swarm_scenario`], but ALSO arms P1 with a held Murder (the defuse for the CR 732.2a
/// interruptibility pair). Returns `(runner, sprout, witherbloom, murder, fodder)`.
///
/// R3: pinned at `n_fodder = 4`. The defused negative relies on the driven recast being unpayable
/// once affinity is removed — convoke-only must then pay the full {4}{G} (buyback {3} + base
/// {1}{G}), i.e. 5 taps, while at most 4 untapped green creatures remain at the recast (one fodder
/// is tapped for the real cast's convoke, plus the one fresh Saproling). If a future bump made
/// convoke alone able to pay {4}{G}, the Murder defuse would stop breaking the loop and the defused
/// test would go vacuous — keep this tied to the `object_growth_no_affinity_does_not_offer` math.
fn sprout_swarm_scenario_with_murder(
    n_fodder: usize,
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let witherbloom = scenario
        .add_creature_from_oracle(
            P0,
            "Witherbloom, the Balancer",
            5,
            5,
            WITHERBLOOM_AFFINITY_ORACLE,
        )
        .id();
    let mut fodder = Vec::new();
    for _ in 0..n_fodder {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let murder = arm_murder(&mut scenario, P1);
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
    }
    (runner, sprout, witherbloom, murder, fodder)
}

/// T-object-growth-INT-a ⭐ — INTERRUPTIBILITY, UNDEFUSED: P1 HOLDS a real Murder but PASSES ⇒ the
/// CR 732.2a object-growth shortcut is GRANTED. Sprout Swarm resolves through a genuine response
/// window (P1 auto-passes, CR 601.2i/117.3c), the token-growth loop settles, and the shortcut is
/// OFFERED. Matched with the defused twin: P1's pass-vs-respond is the SOLE delta and FLIPS the
/// outcome. Reach-guards prove the defuse was genuinely held (Murder still in hand, Witherbloom
/// still on the battlefield).
#[test]
fn object_growth_interruptibility_undefused_opponent_passes_grants() {
    let (mut runner, sprout, witherbloom, murder, fodder) = sprout_swarm_scenario_with_murder(4);
    let outcome = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3}
        .convoke_with(&[fodder[0]]) // tap one green Saproling for the {G} pip
        .commit()
        .resolve();

    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0
        ),
        "UNDEFUSED (P1 passes): the object-growth shortcut is OFFERED to P0, got {:?}",
        outcome.final_waiting_for()
    );
    // Reach-guards: the defuse was genuinely HELD (not spent) and the affinity granter survived.
    assert_eq!(
        outcome.state().objects[&murder].zone,
        engine::types::zones::Zone::Hand,
        "P1's Murder is still in hand (held, not cast) — the offer is not vacuous on a spent defuse"
    );
    assert_eq!(
        outcome.state().objects[&witherbloom].zone,
        engine::types::zones::Zone::Battlefield,
        "Witherbloom (the affinity granter) survives when P1 passes"
    );
}

/// T-object-growth-INT-b ⭐ — INTERRUPTIBILITY, DEFUSED: P1 RESPONDS to Sprout Swarm (on the stack,
/// CR 601.2i) by casting Murder on Witherbloom. The affinity granter is destroyed, Sprout resolves
/// (one Saproling made, buyback → hand), and the empty-stack hook's clone-drive re-derives the
/// recast WITHOUT affinity ⇒ convoke-only {4}{G} needs 5 taps but ≤4 untapped greens remain ⇒
/// unpayable ⇒ NO grant beyond the current stack (CR 732.2a). The ONLY delta vs the undefused twin
/// is P1's respond-vs-pass, and the outcome FLIPS (offer → no offer). This is the exact
/// `object_growth_no_affinity_does_not_offer` mechanism, reached at RUNTIME by removing affinity
/// mid-stack instead of omitting the granter from the fixture.
#[test]
fn object_growth_interruptibility_defused_opponent_responds_no_grant() {
    let (mut runner, sprout, witherbloom, murder, fodder) = sprout_swarm_scenario_with_murder(4);
    let before = saproling_count(runner.state());
    let murder_card = runner.state().objects[&murder].card_id;

    // Commit Sprout (buyback + convoke) to the stack WITHOUT resolving — leaving P0 priority with
    // Sprout on the stack. The bare `commit()` temporary is dropped at the `;`, releasing the
    // borrow so the manual drive can continue.
    runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit();
    // P0 passes ⇒ P1 gets priority with Sprout on the stack (the real response window).
    runner.act(GameAction::PassPriority).expect("P0 passes");
    // P1 RESPONDS: Murder destroys Witherbloom in response to Sprout. The reducer surfaces a
    // `TargetSelection` prompt (the action's `targets` field is not consumed), answered below.
    runner
        .act(GameAction::CastSpell {
            object_id: murder,
            card_id: murder_card,
            targets: vec![witherbloom],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 may cast Murder in response (instant speed)");
    // Settle: Murder targets Witherbloom, resolves, destroys it; then Sprout resolves (token +
    // buyback → hand); then the empty-stack hook drives the clone (no affinity ⇒ unpayable ⇒ no
    // offer).
    for _ in 0..60 {
        match runner.state().waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(witherbloom)],
                    })
                    .expect("Murder targets Witherbloom (a legal creature)");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    // Reach-guards: the response LANDED (Witherbloom gone) and the Sprout cast still RESOLVED (a
    // real Saproling was made — the no-offer is the recast-unpayable break, not a fizzled cast).
    assert!(
        runner.state().objects.get(&witherbloom).map(|o| o.zone)
            != Some(engine::types::zones::Zone::Battlefield),
        "reach-guard: P1's Murder destroyed Witherbloom (the response landed)"
    );
    assert_eq!(
        saproling_count(runner.state()),
        before + 1,
        "reach-guard: Sprout still resolved and made one Saproling (the cast did not fizzle)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "DEFUSED (P1 responds): affinity is gone ⇒ the driven recast can't afford {{4}}{{G}} via \
         convoke ⇒ NO grant beyond the current stack, got {:?}",
        runner.state().waiting_for
    );
}

/// The 2p Vito drain made optional by P1's Mountain and Lightning Bolt, ALSO arming P1 with a held
/// Murder (the defuse for the CR 732.2a Vito-drain interruptibility pair) and capturing the
/// Bloodthirsty Conqueror + Murder ids.
///
/// Sanguine Bond is a REDUNDANT drainer: the drain loop is Vito+Conqueror OR Sanguine+Conqueror
/// (either targeted drainer feeds the single closer). Bloodthirsty Conqueror is the SINGLE closer
/// ("Whenever an opponent loses life, you gain that much life") — Murder→Conqueror breaks the loop
/// regardless of the redundant Sanguine (drop-probe-confirmed by the spike). Both drainers still
/// fire per P0 lifegain, so the per-window life decrement is 2 (Vito's 1 + Sanguine's 1).
fn setup_2p_vito_optional_with_murder(
    mode: LoopDetectionMode,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 6);
    scenario.add_creature_from_oracle(P0, "Vito, Thorn of the Dusk Rose", 1, 4, VITO);
    scenario.add_creature_from_oracle(P0, "Sanguine Bond", 2, 2, SANGUINE_BOND);
    let conqueror = scenario
        .add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 3, 4, BLOODTHIRSTY_CONQUEROR)
        .id();
    // The red land + Bolt make the loop OPTIONAL (so it OFFERS instead of auto-crowning); keep it.
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let murder = arm_murder(&mut scenario, P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff, conqueror, murder)
}

/// T-Vito-INT-a ⭐ — INTERRUPTIBILITY, UNDEFUSED: P1 HOLDS a real Murder but PASSES ⇒ the CR 732.2a
/// Vito-drain shortcut is GRANTED. The kickoff resolves, the Vito/Sanguine drains fan out through
/// genuine APNAP priority windows (P1 auto-passes, CR 601.2i/117.3c), the loop settles, and the
/// shortcut is OFFERED to P0. Matched with the defused twin: P1's pass-vs-respond is the SOLE delta
/// and FLIPS the outcome. Reach-guards prove the defuse was genuinely held (Murder still in hand,
/// closer still on the battlefield, P1 alive-positive).
#[test]
fn vito_interruptibility_undefused_opponent_passes_grants() {
    let (mut runner, kickoff, conqueror, murder) =
        setup_2p_vito_optional_with_murder(LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let (_events, wf) = drive_collect(&mut runner, 2000);

    let WaitingFor::LoopShortcut {
        proposer,
        certificate,
        ..
    } = wf
    else {
        panic!("UNDEFUSED (P1 passes): the optional 2p Vito drain must OFFER a LoopShortcut, got {wf:?}");
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    // The Vito drain's deciding win is lethal (the drain kills P1), not an inert Advantage loop.
    assert_eq!(
        certificate.win_kind,
        Some(WinKind::LethalDamage),
        "the Vito drain offer's deciding win_kind is LethalDamage"
    );
    assert!(
        !certificate.mandatory,
        "the loop is OPTIONAL (P1 holds real answers)"
    );
    // Reach-guards: the defuse was genuinely HELD (Murder still in hand, not spent) and the single
    // closer survived — so the offer is not vacuous on a spent defuse / broken loop.
    assert_eq!(
        runner.state().objects[&murder].zone,
        engine::types::zones::Zone::Hand,
        "P1's Murder is still in hand (held, not cast)"
    );
    assert_eq!(
        runner.state().objects[&conqueror].zone,
        engine::types::zones::Zone::Battlefield,
        "Bloodthirsty Conqueror (the single closer) survives when P1 passes"
    );
    assert!(
        life(&runner, P1) > 0 && !is_eliminated(&runner, P1),
        "the offer fires EARLY with P1 alive-positive, life = {}",
        life(&runner, P1)
    );
}

/// T-Vito-INT-b ⭐ — INTERRUPTIBILITY, DEFUSED: P1 RESPONDS at the first pre-offer priority window
/// that has the Vito/Sanguine drains on the stack (CR 603.3b) by casting Murder on Bloodthirsty
/// Conqueror. The single closer is destroyed; the 2 in-flight drains then resolve (P1 loses EXACTLY
/// 2, no Conqueror re-gain) and the stack empties ⇒ NO grant beyond the current stack (CR 732.2a).
/// The ONLY delta vs the undefused twin is P1's respond-vs-pass, and the outcome FLIPS (offer → no
/// offer). Non-vacuity reach-guards: the response LANDED (Conqueror → graveyard), the defuse was
/// spent (Murder left P1's hand), and P1 lost EXACTLY 2 — the precise decrement proves the 2 drains
/// fired before the closer-removal break (so no-offer is the break, not an upstream fizzle).
#[test]
fn vito_interruptibility_defused_opponent_responds_no_grant() {
    let (mut runner, kickoff, conqueror, murder) =
        setup_2p_vito_optional_with_murder(LoopDetectionMode::Interactive);
    let initial_p1_life = life(&runner, P1);
    let murder_card = runner.state().objects[&murder].card_id;

    // Commit the kickoff to the stack WITHOUT resolving — P0 retains priority with it on the stack.
    runner.cast(kickoff).commit();

    // STEP to the FIRST Priority{P1} window whose stack carries a Vito/Sanguine drain trigger
    // (CR 603.3b: the drains sit on the stack after the kickoff resolves, giving P1 a genuine
    // pre-offer response window). Do NOT auto-pass P1 there. Before that window the only stack
    // entry is the kickoff Spell (no TriggeredAbility), so this precisely selects the drain window.
    let mut reached = false;
    for _ in 0..80 {
        let (wf, drain_on_stack) = {
            let st = runner.state();
            (
                st.waiting_for.clone(),
                st.stack
                    .iter()
                    .any(|e| matches!(e.kind, StackEntryKind::TriggeredAbility { .. })),
            )
        };
        match wf {
            WaitingFor::Priority { player } if player == P1 && drain_on_stack => {
                reached = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to advance toward the drain window");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("P0 orders its two simultaneous drain triggers");
            }
            other => panic!("unexpected state before the drain window: {other:?}"),
        }
    }
    assert!(
        reached,
        "must reach a Priority{{P1}} window with a drain trigger on the stack; got {:?}",
        runner.state().waiting_for
    );
    // Reach-guard: at the response window P1 has NOT yet lost life (drains unresolved) and the
    // closer is still live — the loss below is caused by the in-flight drains, not a prior cycle.
    assert_eq!(
        life(&runner, P1),
        initial_p1_life,
        "P1 has not lost life yet at the response window (drains still on the stack)"
    );

    // P1 RESPONDS: Murder destroys the single closer (Bloodthirsty Conqueror) in response to the
    // drains. The reducer surfaces a `TargetSelection` (the action's `targets` is not consumed),
    // answered in the settle loop below.
    runner
        .act(GameAction::CastSpell {
            object_id: murder,
            card_id: murder_card,
            targets: vec![conqueror],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 may cast Murder in response (instant speed)");

    // Settle: Murder resolves (destroys Conqueror), then the 2 drains resolve (P1 -2, no re-gain),
    // then the stack empties. No new triggers fire (the closer is gone) ⇒ no offer.
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } => break,
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(conqueror)],
                    })
                    .expect("Murder targets Bloodthirsty Conqueror (a legal creature)");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    // Reach-guards (non-vacuity): the response LANDED (closer destroyed), the defuse was spent, and
    // the 2 in-flight drains resolved — the EXACT decrement proves the break happened after the
    // drains fired (not an upstream fizzle) and that the closer removal stopped the re-gain.
    assert_eq!(
        runner.state().objects[&conqueror].zone,
        engine::types::zones::Zone::Graveyard,
        "reach-guard: P1's Murder destroyed the closer (Conqueror → graveyard)"
    );
    assert_ne!(
        runner.state().objects[&murder].zone,
        engine::types::zones::Zone::Hand,
        "reach-guard: the defuse was spent (Murder left P1's hand)"
    );
    assert_eq!(
        life(&runner, P1),
        initial_p1_life - 2,
        "reach-guard: the 2 in-flight drains resolved (P1 lost EXACTLY 2, no Conqueror re-gain), \
         life = {}",
        life(&runner, P1)
    );
    // Terminal is NOT a LoopShortcut and IS a plain empty-stack Priority (the break, not a fizzle).
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }),
        "DEFUSED (P1 responds Murder→Conqueror): the single closer is gone ⇒ the drains resolve \
         once and the loop is broken ⇒ NO grant beyond the current stack, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "terminal is a plain empty-stack Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// N3 — no-buyback REJECTS (B3). Sprout Swarm cast WITHOUT paying buyback ⇒ the spell goes to
/// the graveyard, not hand ⇒ the cast cannot recur, so the trace names no period and nothing is
/// offered. Revert-failing paired reach-guard: P1 (buyback paid, card returns to hand) DOES offer.
#[test]
fn object_growth_no_buyback_does_not_offer() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    // Decline buyback; convoke still pays the base {1}{G} (affinity reduces {1}→{0}).
    let outcome = runner
        .cast(sprout)
        .decline_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no buyback ⇒ card to graveyard ⇒ no recast context ⇒ NO offer, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(
        engine::game::play_trace_view(outcome.state()).is_none_or(|view| view.named.is_empty()),
        "B3: the trace names no period when buyback is unpaid"
    );
    // Reach-guard: confirm the cast actually resolved (a real Saproling was made), so the
    // negative above is not vacuous on an aborted cast.
    assert_eq!(
        saproling_count(outcome.state()),
        5,
        "the base cast still created one token"
    );
}

/// FIX 1 (#4603 opt-in gate): the play trace is gated on `loop_detection.samples()`, so
/// DEFAULT/OFF mode never traces. Paired reach-guard: the SAME buyback + token cast in Interactive
/// (sampling) mode IS traced, so the OFF assertion is not vacuous on a cast that is never traced.
#[test]
fn off_mode_capture_leaves_recast_context_none() {
    // OFF (default): flip the fixture's mode back to Off before the identical cast.
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    runner.state_mut().loop_detection = LoopDetectionMode::Off;
    let off = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert!(
        engine::game::play_trace_view(off.state()).is_none(),
        "OFF (#4603): a buyback+token cast must NOT be traced"
    );

    // ON/sampling reach-guard: the same cast is traced (else the OFF assertion is vacuous).
    let (mut on_runner, on_sprout, on_fodder) = sprout_swarm_scenario(4);
    let on = on_runner
        .cast(on_sprout)
        .accept_optional()
        .convoke_with(&[on_fodder[0]])
        .commit()
        .resolve();
    assert!(
        engine::game::play_trace_view(on.state()).is_some(),
        "Interactive/sampling: the same buyback+token cast IS traced"
    );
}

fn sprout_shell_scenario(body: &str) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let oracle = format!(
        "Convoke (Your creatures can help cast this spell. Each creature you tap while casting this spell pays for {{1}} or one mana of that creature's color.)\nBuyback {{3}} (You may pay an additional {{3}} as you cast this spell. If you do, put this card into your hand as it resolves.)\n{body}"
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Witherbloom, the Balancer",
        5,
        5,
        WITHERBLOOM_AFFINITY_ORACLE,
    );
    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, &oracle);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            let object = st.objects.get_mut(&id).unwrap();
            object.color = vec![ManaColor::Green];
            object.is_token = true;
        }
    }
    (runner, sprout, fodder)
}

/// A2 (CR 732.2a determinism gate) — RECAST-BODY randomness NO-OFFER control. The recast spell's
/// own resolution body creates the fodder token AND flips a coin (CR 705.1). The board still grows
/// deterministically by one Saproling per cycle (so the fodder cover + sign-check pass), but the
/// coin makes the loop outcome-dependent ⇒ NOT a legal CR 732.2a shortcut ⇒ NO offer.
///
/// Why this fixture (not an external coin trigger): the fodder cover's
/// `fire_time_conditions_read_growing_class` already rejects a randomness-bearing *permanent*
/// ability (coin/die classify `Axes::CONSERVATIVE`), so an external coin trigger is caught by the
/// cover regardless of A2 — it cannot discriminate A2. The cover does NOT scan the resolving
/// recast *spell's* body, so a coin flip there is exactly the gap A2 closes; MEASURED: with BOTH
/// A2 halves reverted this fixture wrongly OFFERS (the coin advances the RNG 2→6 yet the cover
/// passes).
///
/// Non-vacuity: (1) item-5 — the body parses to `Token` (deterministic growth) + a `FlipCoins`
/// sub-effect (asserted below), so the coin genuinely fires; (2) reach-guard — the SAME shell with
/// a coin-free body offers, isolating the coin (not the shell) as the disqualifier.
#[test]
fn object_growth_random_recast_body_does_not_offer() {
    // item-5: verify the recast body carries a deterministic Token AND a FlipCoins (so the board
    // grows each cycle while the coin advances the RNG — else the fixture would be vacuous).
    let body_def = engine::parser::oracle_effect::parse_effect_chain(
        "Create a 1/1 green Saproling creature token. Flip a coin.",
        engine::types::ability::AbilityKind::Spell,
    );
    assert!(
        matches!(*body_def.effect, Effect::Token { .. }),
        "recast body head must be Token (deterministic growth), got {:?}",
        body_def.effect
    );
    assert!(
        body_def.sub_ability.as_ref().is_some_and(|s| matches!(
            &*s.effect,
            Effect::FlipCoins { flips, .. } if flips.is_single_flip()
        )),
        "recast body must carry a FlipCoins sub-effect (the randomness A2 rejects), got {:?}",
        body_def.sub_ability
    );

    let (mut runner, sprout, fodder) =
        sprout_shell_scenario("Create a 1/1 green Saproling creature token. Flip a coin.");
    let outcome = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    assert!(
        !matches!(outcome.final_waiting_for(), WaitingFor::LoopShortcut { .. }),
        "A2: a recast body bearing a coin flip is outcome-dependent (CR 732.2a) ⇒ must NOT offer, \
         got {:?}",
        outcome.final_waiting_for()
    );

    // Reach-guard: the SAME shell with a coin-free body offers, isolating the coin (not the
    // buyback/convoke shell) as the sole disqualifier — and proving the input reaches the offer path.
    let (mut ok_runner, ok_sprout, ok_fodder) =
        sprout_shell_scenario("Create a 1/1 green Saproling creature token.");
    let ok = ok_runner
        .cast(ok_sprout)
        .accept_optional()
        .convoke_with(&[ok_fodder[0]])
        .commit()
        .resolve();
    assert!(
        matches!(ok.final_waiting_for(), WaitingFor::LoopShortcut { .. }),
        "reach-guard: the same shell with a deterministic (coin-free) body offers"
    );
}

// ── N4 (energy, branch a) + N5 (player-counter, branch b): UNIT + structural-wiring coverage,
// NOT live fixtures — a LIVE per-recast drain on these two axes is architecturally infeasible in
// this harness (team-lead-authorized fallback on GENUINE infeasibility, not convenience):
//   • Energy is only spent via a cost. Adding a per-cast energy cost to the recast breaks
//     Buyback's return-to-hand (measured: the spell does not return ⇒ the loop cannot recur), so
//     any resulting "no offer" comes from NON-RECURRENCE, not the branch-(a) energy sign-check —
//     a vacuous live test. (Revert-probing branch (a) did NOT flip such a fixture, confirming the
//     vacuity; it was removed rather than shipped as false confidence.)
//   • No engine effect decreases Experience/Ticket player-counters (only Rad, an automatic
//     precombat turn action, not a per-cast cost), so branch (b) has no live per-recast drain.
// Both branches are covered by the 4d-i foundation unit tests
// `analysis::resource::sign_check_energy_decrease_rejects` / `_player_counter_decrease_rejects`,
// and the live call-site (`driving_resources_non_decreasing` on the driven frames) is proven
// LOAD-BEARING by N6 above, which vetoes through that same function (branches a/b/d share it).
// The branch-(a)/(b) sign-checks are fail-closed DEFENSIVE guards — live-unreachable in TODAY's
// buyback-recast mechanism, NOT dead code; they fire the moment a future recast mechanism or a
// per-recast energy/player-counter-drain card makes them reachable. Add a live fixture then.

// ---------------------------------------------------------------------------
// Stage 1 — ShortcutDecisionSchema on the LoopShortcut offer (T1/T2/T4/T6).
// ---------------------------------------------------------------------------

/// T1 ⭐: the object-growth (convoke-recast) offer carries exactly ONE `ConvokeTaps`
/// decision-point whose `tappable` is the LIVE offer-time `is_convoke_eligible(P0)` set, and an
/// optional-loop iteration seed EQUAL to the ceiling it publishes. Board-derivation (hostile): the creature TAPPED to
/// pay convoke during the real cast is EXCLUDED; an untapped controlled creature is INCLUDED — a
/// constant/hard-coded set could not track which creature was spent. Revert-probe: a builder
/// that dropped the ConvokeTaps pin (empty points) or hard-coded the set fails these.
#[test]
fn object_growth_offer_schema_has_live_convoke_taps() {
    let (mut runner, sprout, fodder) = sprout_swarm_scenario(4);
    let outcome = runner
        .cast(sprout)
        .accept_optional() // pay buyback {3}
        .convoke_with(&[fodder[0]]) // tap one green Saproling for the {G} pip
        .commit()
        .resolve();

    let WaitingFor::LoopShortcut { schema, .. } = outcome.final_waiting_for() else {
        panic!(
            "expected a LoopShortcut offer, got {:?}",
            outcome.final_waiting_for()
        );
    };

    // Exactly one open decision-point, and it is the convoke tap set (Sprout Swarm has Convoke).
    assert_eq!(
        schema.points.len(),
        1,
        "one open decision-point (convoke), got {:?}",
        schema.points
    );
    let DecisionPointKind::ConvokeTaps { tappable } = &schema.points[0].kind else {
        panic!(
            "expected a ConvokeTaps decision-point, got {:?}",
            schema.points[0].kind
        );
    };
    // CR 732.2a + CR 732.2c: the object-growth mint overwrites the win-kind seed with the budget
    // and measures no threshold, so this offer's suggestion and capacity coincide — the frontend
    // echoes this value verbatim and the accepted count caps the CR 500.5 collapse prompt, so a
    // smaller seed would cap it too.
    assert_eq!(
        schema.iteration_count,
        IterationCount::Fixed(schema.deliverable_capacity)
    );

    // The tappable set is LIVE-derived from the offer-time board: exactly the untapped creatures
    // P0 controls (== is_convoke_eligible(P0)), compared as a set.
    let expected: std::collections::BTreeSet<ObjectId> = outcome
        .state()
        .objects
        .values()
        .filter(|o| o.is_convoke_eligible(P0))
        .map(|o| o.id)
        .collect();
    let got: std::collections::BTreeSet<ObjectId> = tappable.iter().copied().collect();
    assert_eq!(
        got, expected,
        "tappable must equal the live is_convoke_eligible(P0) set"
    );
    assert!(
        !expected.is_empty(),
        "reach-guard: the convoke set is non-empty"
    );

    // Board-derivation (hostile): fodder[0] was TAPPED to pay convoke during the real cast, so it
    // is EXCLUDED from the offer-time tap set, while an untapped controlled creature is INCLUDED.
    assert!(
        outcome.state().objects.get(&fodder[0]).unwrap().tapped,
        "reach-guard: fodder[0] is tapped from paying convoke"
    );
    assert!(
        !got.contains(&fodder[0]),
        "the tapped convoke payer is excluded from the live tap set"
    );
    assert!(
        got.contains(&fodder[1]),
        "an untapped controlled creature is included"
    );

    // The point's slot binds the recast card's CR 400.7 AllCopies identity.
    assert_eq!(
        schema.points[0].slot.source,
        YieldTarget::AllCopies {
            card_id: outcome.state().objects[&sprout].card_id,
            trigger_description: None,
        },
        "the convoke slot binds the recast card identity"
    );
    let _ = sprout;
}

/// T2: a non-targeted drain offer reifies NO per-iteration decision-points (empty schema), and
/// Marauding Blight-Priest's drain from P1's 19 seeds the count that ends at P1's crossing (CR
/// 704.5a). T1's non-empty ConvokeTaps set is the reach-guard against "the schema is always empty".
#[test]
fn drain_offer_schema_is_empty_and_counts_to_the_crossing() {
    let (runner, _l0, _priest) = reach_2p_blight_priest_drain_offer();
    let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for else {
        panic!(
            "expected a LoopShortcut offer, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        schema.points.is_empty(),
        "a non-targeted drain reifies no decision-points, got {:?}",
        schema.points
    );
    assert_eq!(schema.iteration_count, IterationCount::Fixed(19));
}

/// T4 ⭐ (SECURITY): a `LoopShortcut` schema's hidden-info legal targets are redacted per-viewer.
/// The controller (P0) keeps every legal target; a non-controller (P2) loses ONLY the target
/// that is a hidden card in an opponent's hand, retaining the public `Player` and battlefield
/// object targets. Two-directional: the controller-retains half is the reach-guard against an
/// unconditional drop. Revert-probe: deleting the `WaitingFor::LoopShortcut` block in
/// `filter_state_for_viewer` makes P2's view retain the hidden hand card ⇒ this test fails (leak).
#[test]
fn loop_shortcut_schema_redacts_hidden_targets_for_non_controller() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let hidden_hand = scenario.add_bolt_to_hand(P1); // a hidden card in P1's hand
    let battlefield = scenario.add_creature(P0, "Test Ogre", 3, 3).id(); // public battlefield object
    let mut runner = scenario.build();

    let slot = DecisionSlot {
        source: YieldTarget::ThisObject {
            source_id: ObjectId(999),
            incarnation: None,
            trigger_description: None,
        },
        point: ChoicePoint::AnnouncedTarget,
        index: 0,
    };
    let schema = ShortcutDecisionSchema {
        suggested_declaration: None,
        leading_aims: Vec::new(),
        iteration_count: IterationCount::Fixed(1),
        // This producer measured no CR 704 threshold — `Default` carries the absence and a
        // capacity at the global cap.
        measured_repetition_bound: None,
        deliverable_capacity: ShortcutDecisionSchema::default().deliverable_capacity,
        points: vec![DecisionPoint {
            slot,
            kind: DecisionPointKind::Targets {
                legal_targets: vec![
                    TargetRef::Object(hidden_hand),
                    TargetRef::Player(P1),
                    TargetRef::Object(battlefield),
                ],
                min_targets: 1,
                max_targets: 1,
                ordered: true,
            },
        }],
        convoke_tappable_count: 0,
    };
    let cert = LoopCertificate {
        unbounded: vec![],
        rising: vec![],
        win_kind: Some(WinKind::LethalDamage),
        win_kind_at: None,
        mandatory: false,
        residual_board_delta: BoardDelta::default(),
        per_cycle: None,
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: cert,
        schema,
        declaration: None,
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };

    let targets_of = |wf: &WaitingFor| -> Vec<TargetRef> {
        let WaitingFor::LoopShortcut { schema, .. } = wf else {
            panic!("expected LoopShortcut, got {wf:?}");
        };
        let DecisionPointKind::Targets { legal_targets, .. } = &schema.points[0].kind else {
            panic!("expected a Targets point, got {:?}", schema.points[0].kind);
        };
        legal_targets.clone()
    };

    // Controller P0 (reach-guard): keeps ALL three legal targets — the redaction is
    // viewer-scoped, not an unconditional drop.
    let p0_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P0);
    let p0_targets = targets_of(&p0_view.waiting_for);
    assert_eq!(p0_targets.len(), 3, "controller keeps all legal targets");
    assert!(p0_targets.contains(&TargetRef::Object(hidden_hand)));
    assert!(p0_targets.contains(&TargetRef::Player(P1)));
    assert!(p0_targets.contains(&TargetRef::Object(battlefield)));

    // Non-controller P2: drops ONLY the hidden hand Object; retains the public Player + the
    // public battlefield Object.
    let p2_view = engine::game::visibility::filter_state_for_viewer(runner.state(), P2);
    let p2_targets = targets_of(&p2_view.waiting_for);
    assert!(
        !p2_targets.contains(&TargetRef::Object(hidden_hand)),
        "leak: a non-controller must NOT see the hidden hand card as a legal target: {p2_targets:?}"
    );
    assert!(
        p2_targets.contains(&TargetRef::Player(P1)),
        "the public Player target is retained: {p2_targets:?}"
    );
    assert!(
        p2_targets.contains(&TargetRef::Object(battlefield)),
        "the public battlefield object target is retained: {p2_targets:?}"
    );
    assert_eq!(
        p2_targets.len(),
        2,
        "exactly the one hidden target is dropped"
    );
}

/// R0-a ⭐ (SECURITY): the RESPONDER-facing copy of a declared template is redacted too.
///
/// `handle_declare_shortcut` moves the proposer's `DecisionTemplate` VERBATIM onto
/// `ShortcutProposal.template` one state transition after the `LoopShortcut` offer whose
/// `declaration` is redacted by `d5h_a_hidden_object_pin_drops_the_whole_declaration_for_a_non_proposer`
/// (`engine/src/game/visibility.rs`). Before this row, `grep -c RespondToShortcut
/// crates/engine/src/game/visibility.rs` was 0: the identical pin vector was public to every
/// responder and spectator.
///
/// CR 732.2b, ALL-OR-NOTHING: the responder's right is to shorten "by naming a place where they
/// will make a game choice that's different than what's been proposed", so a half-shown pin set
/// would state a proposal that was never made. The hostile fixture pins ONE hidden hand card and
/// ONE public seat, and the assertion is `template.is_none()` — an implementation that merely
/// trimmed the hidden pin would hand back `Some` with a one-pin vector and fail here.
///
/// # Non-vacuity
///
/// The board is FOUR seats so that the pinned card's owner is neither the proposer nor a
/// responder, which separates three properties a 2-seat fixture conflates. Paired positives,
/// because a redactor that simply dropped every template would satisfy the negative for free:
/// (1) the PROPOSER (P0) keeps it — the guard is keyed to the viewer boundary; (2) the OWNER of
/// the pinned hand card (P3) keeps it even though they are not the proposer — so the drop is
/// keyed to what the viewer may actually see, not to "everyone but the proposer"; (3) an all-seat
/// template reaches the responder unchanged and byte-equal to the proposer's. The negative is
/// asserted for the QUEUED responder P2 as well as the current one P1, which is what makes the
/// guard's keying on `proposal.proposer` (not on the prompted `player`) observable.
///
/// REVERT-PROBE: delete the `WaitingFor::RespondToShortcut` arm in `filter_state_for_scope` ⇒
/// P1/P2 see the hidden-hand pin ⇒ the two `is_none()` assertions FAIL while all positives stay
/// green. Flipping the shared `pins_name_hidden_source`'s inner `any` to `all` fails this row AND
/// the pre-existing `LoopShortcut` row D5-h — which is the proof that the extraction left ONE
/// authority behind rather than two copies.
///
/// *What wrong implementation would still pass this row?* One that redacts `proposal.template`
/// but leaks the same identity through the `LoopShortcut` offer it came from — that surface is
/// covered by D5-h and by `loop_shortcut_schema_redacts_hidden_targets_for_non_controller` above.
#[test]
fn respond_to_shortcut_template_redacts_a_hidden_pin_for_non_proposers() {
    const P3: PlayerId = PlayerId(3);

    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // The pinned card sits in a hand belonging to NEITHER the proposer (P0) nor either
    // responder (P1 current, P2 queued), so "cannot see it" and "is not the proposer" are
    // distinguishable properties of a viewer on this one board.
    let hidden_hand = scenario.add_bolt_to_hand(P3);
    let runner = scenario.build();

    let source = YieldTarget::ThisObject {
        source_id: ObjectId(999),
        incarnation: None,
        trigger_description: None,
    };
    let window = |pins: Vec<TargetPin>| -> GameState {
        let mut state = runner.state().clone();
        state.waiting_for = WaitingFor::RespondToShortcut {
            player: P1,
            remaining_players: vec![P2],
            proposal: ShortcutProposal {
                proposer: P0,
                predicted_winner: Some(P0),
                count: IterationCount::Fixed(3),
                unbounded: vec![],
                rising: vec![],
                win_kind: Some(WinKind::LethalDamage),
                win_kind_at: None,
                template: Some(DecisionTemplate {
                    owner: P0,
                    decisions: vec![PinnedDecision::Targets {
                        slot: DecisionSlot {
                            source: source.clone(),
                            point: ChoicePoint::AnnouncedTarget,
                            index: 0,
                        },
                        targets: pins,
                    }],
                    replay: ReplayMode::Scheduled {
                        count: IterationCount::Fixed(3),
                    },
                    key: DecisionGroupKey::from_sources(
                        std::slice::from_ref(&source),
                        DecisionKind::LoopChoice,
                    ),
                }),
                per_cycle: None,
                shortened_by: None,
                published_declaration: None,
                leading_aims: Vec::new(),
                road: engine::analysis::loop_check::OfferRoad::Ring,
                counted_take: Default::default(),
                period: Default::default(),
            },
        };
        state
    };
    let projected = |state: &GameState, viewer: PlayerId| -> Option<DecisionTemplate> {
        match engine::game::visibility::filter_state_for_viewer(state, viewer).waiting_for {
            WaitingFor::RespondToShortcut { proposal, .. } => proposal.template,
            other => panic!("the fixture parks on the CR 732.2b response window, got {other:?}"),
        }
    };

    // ── the hostile arm: one hidden hand card + one public seat in the SAME pin vector ──
    let hidden_state = window(vec![
        TargetPin::ByIdentity(YieldTarget::ThisObject {
            source_id: hidden_hand,
            incarnation: None,
            trigger_description: None,
        }),
        TargetPin::Player(P1),
    ]);
    assert!(
        projected(&hidden_state, P0).is_some(),
        "reach-guard + positive: the PROPOSER's own projection keeps the template, so the drops \
         below are keyed to the viewer boundary rather than to the fixture"
    );
    assert!(
        projected(&hidden_state, P1).is_none(),
        "CR 732.2b: the CURRENT responder must not receive a proposal whose pin names a card in \
         a hand they cannot see — and all-or-nothing means the public seat pin goes with it"
    );
    assert!(
        projected(&hidden_state, P2).is_none(),
        "the QUEUED responder is projected the same way: the guard keys on `proposal.proposer`, \
         not on the prompted `player`"
    );
    assert!(
        projected(&hidden_state, P3).is_some(),
        "positive: the OWNER of the pinned hand card keeps the template even though they are not \
         the proposer — the drop is keyed to what this viewer may actually see, so a redactor \
         that dropped the template for every non-proposer fails here"
    );

    // ── the paired positive: an all-seat template carries no hidden identity ──
    let public_state = window(vec![TargetPin::Player(P1)]);
    assert_eq!(
        projected(&public_state, P1),
        projected(&public_state, P0),
        "an all-seat template reaches the responder UNCHANGED — without this arm a redactor that \
         dropped every template would pass the negatives above"
    );
    assert!(
        projected(&public_state, P1).is_some(),
        "and it is genuinely present, not two matching `None`s"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// R3-c — the `DecisionTemplate` carrier census over `WaitingFor`
// ─────────────────────────────────────────────────────────────────────────────────────────

/// HOW a `WaitingFor` variant reaches a `DecisionTemplate`: in its own body, or through a
/// field type that carries one. The intermediate type is NAMED rather than flattened to a
/// bool — "via which type" is a real axis (`RespondToShortcut` reaches the template through
/// `ShortcutProposal`) and a bool would erase it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CarrierKind {
    Direct,
    Via(String),
}

use syn::{GenericArgument, Item, PathArguments, Type};

/// Every type identifier `ty` names, outermost first: `Option<Vec<Foo>>` ⇒
/// `["Option", "Vec", "Foo"]`.
///
/// A NAME list rather than a `contains(marker)` over rendered text: the latter answers yes for
/// `DecisionTemplateAudit` and for the word inside a doc comment, and both would be carriers
/// this census invented.
fn type_names(ty: &Type) -> Vec<String> {
    fn walk(ty: &Type, out: &mut Vec<String>) {
        match ty {
            Type::Path(p) => {
                for seg in &p.path.segments {
                    out.push(seg.ident.to_string());
                    if let PathArguments::AngleBracketed(args) = &seg.arguments {
                        for arg in &args.args {
                            if let GenericArgument::Type(inner) = arg {
                                walk(inner, out);
                            }
                        }
                    }
                }
            }
            Type::Reference(r) => walk(&r.elem, out),
            Type::Slice(s) => walk(&s.elem, out),
            Type::Array(a) => walk(&a.elem, out),
            Type::Group(g) => walk(&g.elem, out),
            Type::Paren(p) => walk(&p.elem, out),
            Type::Tuple(t) => t.elems.iter().for_each(|e| walk(e, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(ty, &mut out);
    out
}

/// Every item in `items`, INCLUDING the ones nested in inline `mod` blocks — a holder does not
/// stop being a holder for living inside a module.
fn flatten<'a>(items: &'a [Item], out: &mut Vec<&'a Item>) {
    for item in items {
        out.push(item);
        if let Item::Mod(m) = item {
            if let Some((_, inner)) = &m.content {
                flatten(inner, out);
            }
        }
    }
}

/// `(variant name, its field types)` for `enum_name`, in declaration order.
fn enum_variants(src: &str, enum_name: &str) -> Vec<(String, Vec<Type>)> {
    let parsed =
        syn::parse_file(src).unwrap_or_else(|e| panic!("parse the `{enum_name}` source: {e}"));
    let mut items = Vec::new();
    flatten(&parsed.items, &mut items);
    items
        .iter()
        .find_map(|item| match item {
            Item::Enum(e) if e.ident == enum_name => Some(
                e.variants
                    .iter()
                    .map(|v| {
                        (
                            v.ident.to_string(),
                            v.fields.iter().map(|f| f.ty.clone()).collect(),
                        )
                    })
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{enum_name}` must be declared in the parsed source"))
}

/// Every type declaration in the walked corpus whose own body names `marker` — struct, enum or
/// alias, at any visibility, generic or not, nested in a `mod` or not. THE DEPTH-1 STEP, and
/// the whole of it — see the row's disclosed limitation.
///
/// # Why `syn` and not string ops
///
/// This step used to scan for `pub struct <Ident> {` at column 0, which is a small fraction of
/// the declarations that can hold a `marker`. MEASURED over this walk root: that shape matches
/// **491** declarations, while **654** `pub enum`, **108** `pub(crate) struct` and **12**
/// generic `pub struct` heads are invisible to it — and a carrier the depth-1 step cannot see
/// is scored as a clean GREEN, which is the one failure mode a census must not have. `syn` is
/// already a `[dev-dependencies]` entry of this crate and already the instrument
/// `deterministic_game_state_serde` parses production sources with, so this is reuse and not a
/// new dependency. `PLANT 5` below is the arm that holds the four recovered forms.
///
/// Computed ONCE per corpus rather than re-scanned per candidate identifier: the walk is 500+
/// files and the enum is 138 variants, so the per-identifier form is quadratic in the corpus
/// for no extra signal.
fn types_carrying(corpus: &[(String, String)], marker: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    for (file, src) in corpus {
        // A SOUND prefilter, not a shortcut: a declaration can only NAME `marker` if its file
        // text contains `marker`, so this skips ~500 parses without narrowing the answer.
        if !src.contains(marker) {
            continue;
        }
        let parsed = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {file}: {e}"));
        let mut items = Vec::new();
        flatten(&parsed.items, &mut items);
        for item in items {
            let (name, field_types): (String, Vec<&Type>) = match item {
                Item::Struct(s) => (
                    s.ident.to_string(),
                    s.fields.iter().map(|f| &f.ty).collect(),
                ),
                Item::Enum(e) => (
                    e.ident.to_string(),
                    e.variants
                        .iter()
                        .flat_map(|v| v.fields.iter().map(|f| &f.ty))
                        .collect(),
                ),
                Item::Type(t) => (t.ident.to_string(), vec![t.ty.as_ref()]),
                _ => continue,
            };
            if field_types
                .iter()
                .flat_map(|t| type_names(t))
                .any(|n| n == marker)
            {
                out.insert(name);
            }
        }
    }
    out
}

/// Every variant of `enum_name` in `enum_src` that reaches `marker`, DIRECTLY or through one
/// field type found in `corpus`.
///
/// SOURCE-PARAMETERIZED on purpose: every probe below plants into an in-memory `String`, so the
/// census's own discrimination costs no compile and mutates no worktree file. Mirrors
/// `super::loop_shortcut_offer_writer_census::classify`'s `(src, needle, file)` shape.
fn carriers_in_source(
    enum_src: &str,
    enum_name: &str,
    corpus: &[(String, String)],
    marker: &str,
    walk_via: bool,
) -> Vec<(String, CarrierKind)> {
    let via_types = types_carrying(corpus, marker);
    let mut out = Vec::new();
    for (name, field_types) in enum_variants(enum_src, enum_name) {
        // Outermost-first across the variant's fields in declaration order, so `Via` names the
        // holder a reader would name.
        let named: Vec<String> = field_types.iter().flat_map(type_names).collect();
        if named.iter().any(|n| n == marker) {
            out.push((name, CarrierKind::Direct));
            continue;
        }
        if !walk_via {
            continue;
        }
        if let Some(via) = named.into_iter().find(|n| via_types.contains(n)) {
            out.push((name, CarrierKind::Via(via)));
        }
    }
    out
}

/// Does the shared privacy authority's body carry an `if let WaitingFor::<name>` dispatch arm?
fn redaction_arm_present(visibility_src: &str, name: &str) -> bool {
    let needle = format!("if let {}::{name}", "WaitingFor");
    let mut depth = 0usize;
    for line in visibility_src.lines() {
        if depth == 0 {
            if !line.starts_with("fn filter_state_for_scope(") {
                continue;
            }
            depth = line
                .matches('{')
                .count()
                .saturating_sub(line.matches('}').count());
            continue;
        }
        depth = depth
            .saturating_add(line.matches('{').count())
            .saturating_sub(line.matches('}').count());
        if depth == 0 {
            break;
        }
        if line.contains(&needle) {
            return true;
        }
    }
    false
}

/// Splice `injected` in immediately above `enum_name`'s closing brace, on a COPY of `src`.
fn plant_into_enum(src: &str, enum_name: &str, injected: &str) -> String {
    let open = format!("pub enum {enum_name} {{");
    let mut out = String::new();
    let mut inside = false;
    let mut planted = false;
    for line in src.lines() {
        if !planted {
            if inside && line == "}" {
                out.push_str(injected);
                planted = true;
            } else if !inside && line.trim_start().starts_with(&open) {
                inside = true;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    assert!(
        planted,
        "the plant anchor `{open}` … `}}` must exist in the copy"
    );
    out
}

/// **R3-c** — exactly TWO `WaitingFor` variants carry a `DecisionTemplate`, and both have a
/// redaction arm inside the shared `filter_state_for_scope` authority.
///
/// MEASURED at this tip: `{(LoopShortcut, Direct), (RespondToShortcut, Via(ShortcutProposal))}`
/// out of 138 variants; the VIA target is `analysis::loop_check::ShortcutProposal`'s
/// `pub template: Option<DecisionTemplate>`.
///
/// # Why a census exists here at all
///
/// The REDACTION DISPATCH in `filter_state_for_scope` is two `if let`s, not a `match`, so a
/// THIRD carrier gets no compile error THERE — that is the gap this row closes. It is not true
/// elsewhere: **at least 9** exhaustive `match`es on `WaitingFor` would fail E0004, spread over
/// two crates (`engine`, `phase-ai`) — measured by whole-workspace AST enumeration over
/// `crates/`, which is a **lower bound**, not a total. Those matches make a new variant hard to
/// ADD; not one of them makes it hard to add UNREDACTED. **The site list is deliberately not
/// enumerated here** — a frozen list in a doc comment is a claim no test defends, and it rots
/// the moment a crate is added.
///
/// # Why the probes plant into a `String` instead of adding a variant
///
/// MEASURED: a real third variant yields 6 E0004 under `cargo check -p phase-engine --lib` and
/// 7 with `--features test-support`, and BOTH runs abort at the lib — so the dependent crates
/// and this ~4 800-row integration target are never type-checked, and the repair list is
/// neither stable nor bounded. Source injection has neither problem and needs no build.
///
/// # Discrimination — five plant arms, all RUN, all over in-memory copies
///
/// * a 3rd DIRECT and a 4th VIA carrier planted into a copy of the enum source ⇒ the set
///   assertion fails NAMING both new variants (`n = 4`);
/// * the depth-1 VIA step disabled ⇒ the set shrinks to `{LoopShortcut}` ⇒ fails, so the
///   transitive step is load-bearing rather than decoration;
/// * a synthetic enum with no carrier at all ⇒ `n = 0`, so the classifier cannot only ever
///   return the answer this row wants;
/// * the `RespondToShortcut` arm deleted from a copy of `visibility.rs` ⇒ the redaction half
///   flips to `false` for that carrier while `LoopShortcut` stays `true`. The mutation is on a
///   COPY, so the shipped row `respond_to_shortcut_template_redacts_a_hidden_pin_for_non_proposers`
///   above is not perturbed;
/// * a `pub(crate)`, a GENERIC, an ENUM, an ALIAS and a `mod`-NESTED holder planted at once ⇒
///   all five are NAMED as `Via` carriers, and a non-carrier planted beside them is not. These
///   are the forms the retired `pub struct <Ident> {{` string scan could not see, and each was
///   a false GREEN rather than a false alarm.
///
/// # DISCLOSED LIMITATIONS
///
/// 1. **The walk is DEPTH-1.** A `WaitingFor` field whose type reaches a `DecisionTemplate` two
///    levels down is invisible to it. Measured today: zero such types exist. That is a latent
///    gap, not a covered case.
/// 2. **The E0004 figure above is a LOWER BOUND from a named instrument**, not a total, and this
///    row must never be "helpfully" upgraded into a site list.
#[test]
fn exactly_two_waiting_for_variants_carry_a_decision_template_and_both_are_redacted() {
    use super::loop_shortcut_offer_writer_census::rs_files;

    // Assembled at runtime for the same reason both sibling censuses assemble their anchors:
    // an instrument that can count its own needle after a future move is one that lies about
    // the surface it measures.
    let marker = format!("{}{}", "Decision", "Template");
    let engine_src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let corpus: Vec<(String, String)> = rs_files(&engine_src)
        .into_iter()
        .map(|path| {
            let src =
                std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
            let rel = path
                .strip_prefix(&engine_src)
                .expect("walked path is under its root")
                .to_string_lossy()
                .replace('\\', "/");
            (rel, src)
        })
        .collect();
    let file_of = |suffix: &str| -> String {
        corpus
            .iter()
            .find(|(f, _)| f.ends_with(suffix))
            .unwrap_or_else(|| panic!("the walk must reach {suffix}"))
            .1
            .clone()
    };
    let enum_src = file_of("types/game_state.rs");
    let visibility_src = file_of("game/visibility.rs");

    // ── the classifier's own reach-guard: the enum was actually found ──
    let total = enum_variants(&enum_src, "WaitingFor").len();
    assert_eq!(
        total, 147,
        "`WaitingFor` has 147 variants at this tip, read off the `syn` parse. This number is \
         pinned so a variant REMOVED is as visible as one added; if you added a variant and it \
         carries no `DecisionTemplate`, update this number. A wildly different count means the \
         reader lost its anchor, and every assertion below would then be measuring an empty enum"
    );
    // 132 -> 133 is adjudicated: `ResolutionOptionalPaymentChoice` carries no
    // `DecisionTemplate`; it is a direct server-authored branch list, so the
    // two template carriers asserted below remain unchanged.
    // 128 ⇒ 129 is ADJUDICATED, not bumped: upstream #7336 ("make dig entries attack") added
    // `EntryAttackTargetChoice { player, object_id, valid_targets }`. Measured, because the
    // number alone cannot say it: that variant carries NO `DecisionTemplate` (zero matches in
    // its body), so it is not a third carrier and the assertion below is unchanged by it. The
    // count moved for a reason that does not touch this row's subject — which is exactly the
    // case this reach-guard exists to make visible rather than silent.
    // 129 ⇒ 130 is ADJUDICATED on the same terms: upstream #7382 ("choose pre-entry opponent
    // controller") added `EntryControllerChoice { player: PlayerId, candidates: Vec<PlayerId> }`
    // (CR 614.12a). Measured, not inferred from the diff: that body holds NO `DecisionTemplate`,
    // so it is not a third carrier, and both the carrier vec and the redaction loop below are
    // unchanged by it. The count itself was read from THIS assertion's own failure (`left: 130`)
    // rather than from a hand-written variant counter — one was tried and returned 49 while
    // contradicting itself, and a second instrument that disagrees with the `syn` parse is worth
    // less than no second instrument. The reach-guard did its whole job here: this drift produced
    // no merge conflict and could not have, so CI was the only thing between it and shipping.
    // 130 ⇒ 132 is ADJUDICATED: ResolveAllConsent and ResolveAllReady are control-protocol states
    // with no DecisionTemplate payload, so neither expands the carrier set nor the redaction duty.
    // 133 ⇒ 135 is ADJUDICATED: Ripple's two-decision model (CR 702.60a / 608.2d) added
    // `RippleRevealChoice { player, source_id, count }` (the optional "you may reveal") and
    // `RippleBottomOrder { player, source_id, cards, final_cast }` (the "in any order" bottom
    // placement). Measured, not inferred: neither body holds a `DecisionTemplate` — both are
    // resolution-choice prompts handled in `engine_resolution_choices`, not shortcut-style
    // templates — so the carrier vec and the `filter_state_for_scope` redaction loop below are
    // unchanged.
    // 135 ⇒ 136 is ADJUDICATED: the CR 706.6 die-roll ignore model (Barbarian Class, Pixie
    // Guide, Wyll) added `DieKeepChoice { player, results, ignorable_indices, ignore_count }`.
    // Measured, not inferred from the diff: that body holds NO `DecisionTemplate` (zero matches),
    // so it is not a third carrier and the carrier assertion below is unchanged by it. It is
    // also deliberately absent from the `filter_state_for_scope` redaction loop. That omission is
    // engine convention carrying no CR annotation — no Comprehensive Rule states that die results
    // are public information — and mirrors `CoinFlipKeepChoice`, which is likewise unredacted.
    // 136 ⇒ 137 is ADJUDICATED: CR 601.2f's caster-elected cost-reduction ordering added
    // `OrderCostReductions { player, reductions, outcomes, pending_cast }`. Measured, not inferred
    // from the diff: that body holds NO `DecisionTemplate` (zero matches), so it is not a third
    // carrier and the carrier assertion below is unchanged by it. It is also deliberately absent
    // from the `filter_state_for_scope` redaction loop: the spell is already announced and every
    // snapshotted reduction comes from a face-up battlefield permanent, so the prompt carries no
    // private information — the same reasoning that leaves `OrderTriggers` unredacted.
    // 137 ⇒ 138 is ADJUDICATED: CR 401.4's library-order choice added
    // `RevealUntilBottomOrder { player, source_id, cards, clear_markers, emit_reveal_until_resolved, reveal_until_hit_snapshot }`.
    // Measured, not inferred: holds no `DecisionTemplate` (resolution-choice prompt handled in
    // `engine_resolution_choices`), so the carrier assertion is unchanged. It is also explicitly
    // included in `accepts_freeform_card_order` and carries public card identifiers.
    // 138 ⇒ 139 is ADJUDICATED: CR 701.71a's Empower Jace choice (CR 608.2d) added
    // `EmpowerJaceChoice { player, source_id, valid_targets }`. Measured, not inferred from the
    // diff: that body holds NO `DecisionTemplate` (zero matches), so it is not a third carrier and
    // the carrier assertion below is unchanged by it.
    // 139 ⇒ 140 is ADJUDICATED: DigRestSplitChoice carries no DecisionTemplate;
    // its hidden card identities are filtered by the Dig visibility path.
    // 140 ⇒ 141 is ADJUDICATED: CR 405.3's copy-batch order added
    // `SpellCopyOrderChoice { player, source_id, choices }`. Its body holds no
    // `DecisionTemplate`, and its choices are spells on the stack, which are
    // public, so neither the carrier vec nor the redaction loop changes.
    // 141 ⇒ 142 is ADJUDICATED: `ManaAbilityManaPayment { player, pending_mana_ability }`
    // holds no `DecisionTemplate`, so neither the carrier vec nor the redaction loop changes.
    // 142 ⇒ 143 is ADJUDICATED: the one-time-boon token-host choice (CR 608.2d,
    // Dunbarrow Revivalist) added `ChooseTokenHost { player, source_id,
    // legal_targets, pending_ability }`. Measured, not inferred from the diff:
    // that body holds NO `DecisionTemplate` (zero matches) — it is a
    // resolution-choice prompt answered by `GameAction::ChooseTarget`, not a
    // shortcut-style template — so it is not a third carrier and the carrier
    // assertion below is unchanged by it.
    // 143 ⇒ 144 is ADJUDICATED: the Eye to Eye / Mouth to Mouth contest added
    // `PhysicalContest { state: ContestState }`. Measured, not inferred from the diff:
    // that body holds NO `DecisionTemplate` (zero matches) — it is an actorless
    // real-time physical clicking contest driven by the host engine contest driver,
    // not a shortcut-style template — so it is not a third carrier and the carrier
    // assertion below is unchanged by it.
    // 144 ⇒ 145 is ADJUDICATED: the conspiracy agenda prompt added
    // `SecretAgendaChoice { player, object_id, name_count }`. That body holds NO
    // `DecisionTemplate`, so it is not a third carrier either.
    // 145 ⇒ 146 is ADJUDICATED: Petals of Insight's bottom ordering added
    // `DigBottomOrder { player, source_id, cards }`. Its body holds NO
    // `DecisionTemplate` — it is a card-permutation prompt answered by
    // `SelectCards` — so it is not a third carrier and the carrier assertion
    // below is unchanged by it.
    // 146 ⇒ 147 is ADJUDICATED: Turtle Team-Up added `HeroEventChoice` (hero-choice
    // Events answered with `GameAction::SelectCards`, handled in
    // `engine_resolution_choices`). Measured, not inferred from the diff: that body
    // holds NO `DecisionTemplate` (zero matches) — it is a resolution-choice prompt,
    // not a shortcut-style template — so it is not a third carrier and the carrier
    // assertion below is unchanged by it.

    let carriers = carriers_in_source(&enum_src, "WaitingFor", &corpus, &marker, true);
    assert_eq!(
        carriers,
        vec![
            ("LoopShortcut".to_string(), CarrierKind::Direct),
            (
                "RespondToShortcut".to_string(),
                CarrierKind::Via("ShortcutProposal".to_string())
            ),
        ],
        "CR 732.2a / CR 723.4: exactly TWO `WaitingFor` variants carry a `DecisionTemplate` — \
         `LoopShortcut` directly and `RespondToShortcut` through `ShortcutProposal` — and both \
         are named rather than counted, because a census asserting `>= 1 carrier` is vacuous. A \
         THIRD carrier is a new per-viewer redaction obligation: the dispatch in \
         `filter_state_for_scope` is `if let`s, so nothing will fail to compile. got {carriers:?}"
    );

    // ── the redaction half, read from the production source ──
    for (name, kind) in &carriers {
        assert!(
            redaction_arm_present(&visibility_src, name),
            "CR 723.4: every `DecisionTemplate` carrier needs its own dispatch arm inside \
             `filter_state_for_scope`; `{name}` ({kind:?}) has none"
        );
    }

    // ── PLANT 1 — a 3rd DIRECT and a 4th VIA carrier, into COPIES ──
    let planted_src = plant_into_enum(
        &enum_src,
        "WaitingFor",
        &format!(
            "    ProbeThirdCarrier {{\n        template: Option<{marker}>,\n    }},\n\
             \x20   ProbeFourthCarrier {{\n        holder: ProbeViaHolder,\n    }},\n"
        ),
    );
    let mut planted_corpus = corpus.clone();
    planted_corpus.push((
        "planted.rs".to_string(),
        format!("pub struct ProbeViaHolder {{\n    pub template: Option<{marker}>,\n}}\n"),
    ));
    let planted = carriers_in_source(&planted_src, "WaitingFor", &planted_corpus, &marker, true);
    assert_eq!(
        planted
            .iter()
            .map(|(n, k)| (n.as_str(), k.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("LoopShortcut", CarrierKind::Direct),
            (
                "RespondToShortcut",
                CarrierKind::Via("ShortcutProposal".to_string())
            ),
            ("ProbeThirdCarrier", CarrierKind::Direct),
            (
                "ProbeFourthCarrier",
                CarrierKind::Via("ProbeViaHolder".to_string())
            ),
        ],
        "ANTI-VACUITY: the census must NAME a planted third (direct) and fourth (transitive) \
         carrier, else the two-carrier answer above is what a dead instrument returns. \
         got {planted:?}"
    );

    // ── PLANT 2 — the depth-1 VIA step disabled ──
    let no_via = carriers_in_source(&enum_src, "WaitingFor", &corpus, &marker, false);
    assert_eq!(
        no_via,
        vec![("LoopShortcut".to_string(), CarrierKind::Direct)],
        "the transitive step is LOAD-BEARING: without it `RespondToShortcut` is invisible and \
         the shipped set assertion above would be a one-element claim. got {no_via:?}"
    );

    // ── PLANT 3 — a synthetic enum with no carrier ──
    let synthetic = "pub enum WaitingFor {\n    Alpha {\n        player: PlayerId,\n    },\n    \
                     Beta {\n        x: u32,\n    },\n}\n";
    let none = carriers_in_source(synthetic, "WaitingFor", &corpus, &marker, true);
    assert!(
        none.is_empty(),
        "a carrier-free enum must classify as carrier-free; an instrument that can only ever \
         return {{LoopShortcut, RespondToShortcut}} is what this arm forecloses. got {none:?}"
    );

    // ── PLANT 4 — the redaction arm deleted, on a COPY of `visibility.rs` ──
    let mutated_visibility = visibility_src.replace(
        &format!("if let {}::RespondToShortcut", "WaitingFor"),
        &format!("if let {}::ZzzDeletedArm", "WaitingFor"),
    );
    assert!(
        redaction_arm_present(&mutated_visibility, "LoopShortcut")
            && !redaction_arm_present(&mutated_visibility, "RespondToShortcut"),
        "the redaction half must FLIP when its arm is deleted from the copy, and only for the \
         carrier whose arm was deleted — otherwise the `for` loop above asserts nothing"
    );

    // ── PLANT 5 — the four holder FORMS the retired string scan could not see, plus a
    //    non-carrier control. Each is a real declaration shape from this walk root: over
    //    `crates/engine/src` the old `pub struct <Ident> {` shape matched 491 declarations
    //    while 654 `pub enum`, 108 `pub(crate) struct` and 12 generic `pub struct` heads were
    //    invisible — every one of them a carrier that would have scored a clean GREEN. ──
    let forms = format!(
        "pub(crate) struct CrateVisHolder {{\n    pub template: Option<{marker}>,\n}}\n\
         pub struct GenericHolder<'a, T> {{\n    pub template: &'a {marker},\n    pub t: T,\n}}\n\
         pub enum EnumHolder {{\n    WithTemplate({marker}),\n    Without,\n}}\n\
         pub type AliasHolder = Option<{marker}>;\n\
         pub struct NotAHolder {{\n    pub seat: PlayerId,\n}}\n\
         mod inner {{\n    pub struct NestedHolder {{\n        pub template: \
         Option<super::{marker}>,\n    }}\n}}\n"
    );
    let mut forms_corpus = corpus.clone();
    forms_corpus.push(("forms.rs".to_string(), forms));
    let forms_src = plant_into_enum(
        &enum_src,
        "WaitingFor",
        "    ProbeCrateVis {\n        h: CrateVisHolder,\n    },\n\
         \x20   ProbeGeneric {\n        h: GenericHolder<'static, u8>,\n    },\n\
         \x20   ProbeEnumHolder {\n        h: EnumHolder,\n    },\n\
         \x20   ProbeAlias {\n        h: AliasHolder,\n    },\n\
         \x20   ProbeNested {\n        h: NestedHolder,\n    },\n\
         \x20   ProbeNonCarrier {\n        h: NotAHolder,\n    },\n",
    );
    let by_forms: Vec<(String, CarrierKind)> =
        carriers_in_source(&forms_src, "WaitingFor", &forms_corpus, &marker, true)
            .into_iter()
            .filter(|(n, _)| n.starts_with("Probe"))
            .collect();
    assert_eq!(
        by_forms,
        vec![
            (
                "ProbeCrateVis".to_string(),
                CarrierKind::Via("CrateVisHolder".to_string())
            ),
            (
                "ProbeGeneric".to_string(),
                CarrierKind::Via("GenericHolder".to_string())
            ),
            (
                "ProbeEnumHolder".to_string(),
                CarrierKind::Via("EnumHolder".to_string())
            ),
            (
                "ProbeAlias".to_string(),
                CarrierKind::Via("AliasHolder".to_string())
            ),
            (
                "ProbeNested".to_string(),
                CarrierKind::Via("NestedHolder".to_string())
            ),
        ],
        "the depth-1 step must see a `pub(crate)`, a GENERIC, an ENUM, an ALIAS and a \
         `mod`-NESTED holder alike — and `ProbeNonCarrier` must be ABSENT, because an \
         instrument that reports every variant is as useless as one that reports too few. \
         got {by_forms:?}"
    );
}

/// CR 732.2a: the answers a standing offer's confirmed period replays — which may name a card a
/// viewer cannot see — reach no viewer: the period is stripped from every filtered state, the
/// proposer's included, while the unfiltered state keeps it for the take.
#[test]
fn recorded_loop_pins_are_redacted_for_a_viewer_who_cannot_see_the_pinned_object() {
    let Some(db) = super::support::shared_card_db() else {
        return;
    };
    let mut rig = super::loop_shortcut_mana_engine::setup(
        true,
        engine::types::game_state::LoopDetectionMode::Interactive,
        db,
    );
    let mana = super::loop_shortcut_mana_engine::mana_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt publishes its mana ability");
    let untap =
        super::loop_shortcut_mana_engine::untap_ability_index(rig.runner.state(), rig.basalt)
            .expect("Basalt publishes its untap ability");
    super::loop_shortcut_mana_engine::drive_one_period(&mut rig, mana, untap);
    let carried = |state: &GameState| match &state.waiting_for {
        WaitingFor::LoopShortcut { period, .. } => !period.is_empty(),
        other => panic!("expected an offer, got {other:?}"),
    };
    let state = rig.runner.state();
    assert!(
        carried(state),
        "positive: the unfiltered offer carries its period"
    );
    for viewer in state.players.iter().map(|player| player.id) {
        assert!(
            !carried(&engine::game::visibility::filter_state_for_viewer(
                state, viewer
            )),
            "{viewer:?}'s view carries no period"
        );
    }
}

/// T6 (serde): the schema rides the `WaitingFor::LoopShortcut` serialization as `data.schema`
/// (tag/content) and round-trips equal — the FE contract that lets the frontend read the offer's
/// decision schema off the wire without any engine-side special casing.
#[test]
fn loop_shortcut_serializes_schema_under_data() {
    let (runner, _l0, _cleric) = reach_2p_optional_drain_offer();
    let WaitingFor::LoopShortcut { schema, .. } = &runner.state().waiting_for else {
        panic!(
            "expected a LoopShortcut offer, got {:?}",
            runner.state().waiting_for
        );
    };
    let v = serde_json::to_value(&runner.state().waiting_for).expect("serialize WaitingFor");
    assert!(
        v["data"]["schema"].is_object(),
        "WaitingFor::LoopShortcut must serialize data.schema, got {v}"
    );
    let schema_back: ShortcutDecisionSchema =
        serde_json::from_value(v["data"]["schema"].clone()).expect("deserialize schema");
    assert_eq!(&schema_back, schema, "the schema round-trips off the wire");
}

// ---------------------------------------------------------------------------
// Battlefield-entry-ledger observers beside the object-growth loop.
// ---------------------------------------------------------------------------

/// Park Heights Pegasus, verbatim (Scryfall / MTGJSON `AtomicCards.json`). Its trigger `execute`
/// body carries the CR 608.2i `QuantityRef::BattlefieldEntriesThisTurn` read.
const PARK_HEIGHTS_PEGASUS_ORACLE: &str = "Flying, trample\nWhenever this creature deals combat damage to a player, draw a card if you had two or more creatures enter the battlefield under your control this turn.";

/// ANTI-VACUITY CONTROL: the same board shape with a trigger that reads NOTHING
/// board-mutable. Granted in BOTH builds, which is what proves the veto below
/// comes from the ledger clause and not from the bystander's mere presence.
const PLAIN_DRAW_TRIGGER_ORACLE: &str =
    "Flying, trample\nWhenever this creature deals combat damage to a player, draw a card.";

/// The passing 51st Sprout Swarm / Witherbloom object-growth row plus exactly ONE
/// extra battlefield permanent, controlled by `bystander_controller`, carrying
/// `bystander_oracle`. Returns the final `WaitingFor` plus the bystander's id, so the
/// caller can reach-guard its zone.
///
/// `phase` parameterises the loop window's step (CR 500.1 / CR 506.1), which is what
/// the CR 510.2 phase-unreachability rows key on; `bystander_controller` parameterises
/// the observer's controller, which is what the CR 117.1b sole-driver rows key on. Both
/// axes exist so a row can move exactly ONE variable against its own control.
fn object_growth_with_bystander_at(
    phase: Phase,
    bystander_controller: PlayerId,
    bystander_oracle: &str,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(phase);
    scenario.add_creature_from_oracle(
        P0,
        "Witherbloom, the Balancer",
        5,
        5,
        WITHERBLOOM_AFFINITY_ORACLE,
    );
    let bystander = scenario
        .add_creature_from_oracle(
            bystander_controller,
            "BBFU10 Bystander",
            2,
            2,
            bystander_oracle,
        )
        .id();
    let mut fodder = Vec::new();
    for _ in 0..4 {
        fodder.push(scenario.add_creature(P0, "Saproling", 1, 1).id());
    }
    let sprout = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Sprout Swarm", true, SPROUT_SWARM_ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        b.id()
    };
    let mut runner = scenario.build();
    {
        let st = runner.state_mut();
        st.loop_detection = LoopDetectionMode::Interactive;
        for &id in &fodder {
            st.objects.get_mut(&id).unwrap().color = vec![ManaColor::Green];
        }
    }
    // The cast pipeline settles the loop verdict into `state().waiting_for`, which
    // is what the caller reads (the borrowed `CastOutcome` is dropped here).
    runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    (runner, bystander)
}

/// The shipped two-call-site shape: P0's own bystander, precombat main.
fn object_growth_with_bystander(bystander_oracle: &str) -> (GameRunner, ObjectId) {
    object_growth_with_bystander_at(Phase::PreCombatMain, P0, bystander_oracle)
}

/// Park Heights Pegasus, whose combat-damage ledger trigger cannot fire in a precombat-main window,
/// does not suppress the object-growth offer.
#[test]
fn object_growth_phase_unreachable_ledger_observer_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    // (2) ANTI-VACUITY CONTROL first: an otherwise byte-identical board whose
    // bystander reads nothing board-mutable still gets the offer.
    let (control_runner, control_bystander) =
        object_growth_with_bystander(PLAIN_DRAW_TRIGGER_ORACLE);
    match &control_runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "(2) control: the detected loop's unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "(2) anti-vacuity control: a plain draw-trigger bystander must NOT suppress \
             the offer, got {other:?}"
        ),
    }
    assert_eq!(
        control_runner.state().objects[&control_bystander].zone,
        Zone::Battlefield,
        "(3) control reach-guard: the bystander is a functioning battlefield permanent"
    );

    // The subject: the SAME board with Park Heights Pegasus instead.
    let (runner, bystander) = object_growth_with_bystander(PARK_HEIGHTS_PEGASUS_ORACLE);

    // (3) reach-guard — the observer is on the battlefield and carries exactly one trigger.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "(3) reach-guard: the ledger observer must be a functioning battlefield permanent"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "(3) reach-guard: exactly one trigger definition carries the ledger read"
    );

    // (1) THE OFFER.
    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut {
            certificate,
            predicted_winner,
            ..
        } => {
            assert!(
                certificate.unbounded.contains(&ResourceAxis::TokensCreated),
                "(1) the detected loop's unbounded axis must be TokensCreated, got {:?}",
                certificate.unbounded
            );
            assert_eq!(
                *predicted_winner, None,
                "(1) this is an Advantage offer, not a predicted win"
            );
        }
        other => panic!(
            "(1) CR 510.2 / CR 506.1: Park Heights Pegasus's combat-damage trigger cannot \
             fire inside a PreCombatMain loop window, so it must NOT suppress the \
             CR 732.2a object-growth offer; got {other:?}"
        ),
    }
}

/// Drives the object-growth board with `oracle`'s bystander and asserts the offer stands with the
/// bystander on the battlefield carrying `(triggers, abilities)` definitions.
fn offers_beside_observer(
    phase: Phase,
    controller: PlayerId,
    oracle: &str,
    surfaces: (usize, usize),
) -> (GameRunner, ObjectId) {
    let (runner, bystander) = object_growth_with_bystander_at(phase, controller, oracle);
    let obj = &runner.state().objects[&bystander];
    assert_eq!(obj.zone, engine::types::zones::Zone::Battlefield);
    assert_eq!(
        (obj.trigger_definitions.len(), obj.abilities.len()),
        surfaces,
        "reach-guard: the observer carries its named surfaces"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "got {:?}",
        runner.state().waiting_for
    );
    (runner, bystander)
}

/// The object-growth loop offers beside Park Heights Pegasus in the combat damage step; whether it
/// reads the growing class is the loop's own replay to show (CR 732.2a).
#[test]
fn combat_damage_step_ledger_observer_still_suppresses_offer() {
    offers_beside_observer(Phase::CombatDamage, P0, PARK_HEIGHTS_PEGASUS_ORACLE, (1, 0));
}

/// Smuggler's Share, verbatim (Scryfall `cards/named?exact=`), behind the harness's
/// shared `"Flying, trample\n"` keyword prefix so subject and control differ ONLY in
/// the ledger clause. Its trigger is `TriggerMode::Phase` with `phase: End`.
const SMUGGLERS_SHARE_ORACLE: &str = "Flying, trample\nAt the beginning of each end step, draw a card for each opponent who drew two or more cards this turn, then create a Treasure token for each opponent who had two or more lands enter the battlefield under their control this turn.";

/// Smuggler's Share's end-step observer does not suppress the offer in a precombat-main loop
/// window.
#[test]
fn smugglers_share_end_step_observer_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    // (2) ANTI-VACUITY CONTROL, granted in BOTH builds.
    let (control_runner, _) = object_growth_with_bystander(PLAIN_DRAW_TRIGGER_ORACLE);
    assert!(
        matches!(
            control_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "(2) control: a plain draw-trigger bystander must not suppress the offer"
    );

    let (runner, bystander) = object_growth_with_bystander(SMUGGLERS_SHARE_ORACLE);

    // (3) reach-guards.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "(3) reach-guard: exactly one trigger definition carries the ledger read"
    );

    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "(1) unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "(1) CR 500.1 / CR 506.1: an end-step observer cannot fire inside a \
             precombat-main loop window, so it must not suppress the offer; got {other:?}"
        ),
    }
}

/// The object-growth loop offers beside Smuggler's Share in an end-step window; whether it reads
/// the growing class is the loop's own replay to show (CR 732.2a).
#[test]
fn end_step_window_end_step_observer_still_suppresses_offer() {
    offers_beside_observer(Phase::End, P0, SMUGGLERS_SHARE_ORACLE, (1, 0));
}

/// The Prydwen, Steel Flagship, verbatim (Scryfall `cards/named?exact=`), behind the
/// harness's shared keyword prefix. Its ETB matcher is `nontoken artifact you control`,
/// which is triple-disjoint from a P0 Saproling creature TOKEN.
const PRYDWEN_ORACLE: &str = "Flying, trample\nFlying\nWhenever another nontoken artifact you control enters, create a 2/2 white Human Knight creature token with \"This token gets +2/+2 as long as an artifact entered the battlefield under your control this turn.\"\nCrew 2";

/// The SAME card with its ETB matcher widened from `nontoken artifact` to `creature`,
/// which genuinely DOES match the loop's Saproling fodder.
const PRYDWEN_BROAD_ORACLE: &str = "Flying, trample\nFlying\nWhenever another creature you control enters, create a 2/2 white Human Knight creature token with \"This token gets +2/+2 as long as an artifact entered the battlefield under your control this turn.\"\nCrew 2";

/// A Prydwen whose ETB matcher is disjoint from the Saproling fodder does not suppress the offer;
/// widened to `creature`, its trigger fires on each Saproling's entry and no offer stands.
#[test]
fn prydwen_artifact_matcher_bystander_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    let (runner, bystander) = object_growth_with_bystander(PRYDWEN_ORACLE);

    // (3) reach-guards, ALL BEFORE the offer match. On a positive (offer-forming) row a
    // parse failure yields NO observer at all, which would make the offer trivially green —
    // these guards are what make that vacuity mode loud. `Crew` is a keyword, not an
    // `abilities[]` entry, so the ONE surface here is the ETB trigger.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: the observer is on the battlefield"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "reach-guard: the ETB observer must have PARSED — a misparse leaves zero trigger \
         defs and the offer below forms for the wrong reason; got {}",
        obj.trigger_definitions.len()
    );
    assert!(
        obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        obj.abilities.len(),
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>(),
    );

    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "K3-1: unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "K3-1 CR 603.6a: an ETB matcher provably disjoint from the fodder class must \
             not suppress the offer; got {other:?}"
        ),
    }

    // HF-K3: the genuinely-matching sibling keeps its veto.
    let (broad_runner, broad_bystander) = object_growth_with_bystander(PRYDWEN_BROAD_ORACLE);

    // (3) reach-guards on the BROAD half — this is a veto row, so the veto must be
    // attributable to the ONE named trigger surface and not to any other.
    let broad_obj = &broad_runner.state().objects[&broad_bystander];
    assert_eq!(
        broad_obj.zone,
        Zone::Battlefield,
        "reach-guard: the observer is on the battlefield"
    );
    assert_eq!(
        broad_obj.trigger_definitions.len(),
        1,
        "reach-guard: this row's claim is about ONE named trigger surface; got {}",
        broad_obj.trigger_definitions.len()
    );
    assert!(
        broad_obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        broad_obj.abilities.len(),
        broad_obj
            .abilities
            .iter()
            .map(|a| a.kind)
            .collect::<Vec<_>>(),
    );

    assert!(
        !matches!(
            broad_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "HF-K3: an ETB matcher that DOES match the Saproling fodder must keep vetoing; \
         got {:?}",
        broad_runner.state().waiting_for
    );
}

/// A non-mana activated ability whose body reads a live board aggregate
/// (`QuantityRef::ObjectCount`).
const AGGREGATE_ACTIVATED_ORACLE: &str =
    "Flying, trample\n{2}: Draw a card for each creature you control.";

/// Circle of Dreams Druid's mana ability, verbatim (Scryfall `cards/named?exact=`),
/// behind the shared keyword prefix — the same `ObjectCount` aggregate read on a MANA
/// ability, which CR 605.3a keeps activatable without priority.
const AGGREGATE_MANA_ORACLE: &str = "Flying, trample\n{T}: Add {G} for each creature you control.";

/// A second, non-activated class-reading surface on the same object: a trigger whose body carries
/// the same `ObjectCount` aggregate.
const AGGREGATE_TWO_SURFACE_ORACLE: &str = "Flying, trample\n{2}: Draw a card for each creature you control.\nWhenever this creature attacks, draw a card for each creature you control.";

/// The object-growth loop offers beside the driver's own class-counting activated ability, alone
/// or with a class-counting trigger beside it; whether it reads the growing class is the loop's own
/// replay to show (CR 732.2a).
#[test]
fn driver_own_unproposed_activated_ability_is_relieved() {
    offers_beside_observer(Phase::PreCombatMain, P0, AGGREGATE_ACTIVATED_ORACLE, (0, 1));
    offers_beside_observer(
        Phase::PreCombatMain,
        P0,
        AGGREGATE_TWO_SURFACE_ORACLE,
        (1, 1),
    );
}

/// The object-growth loop offers beside an opponent's class-counting mana ability (CR 605.3a);
/// whether it reads the growing class is the loop's own replay to show (CR 732.2a).
#[test]
fn foreign_mana_ability_still_vetoes() {
    let (runner, bystander) =
        offers_beside_observer(Phase::PreCombatMain, P1, AGGREGATE_MANA_ORACLE, (0, 1));
    assert!(engine::game::mana_abilities::is_mana_ability(
        &runner.state().objects[&bystander].abilities[0]
    ));
}

/// The object-growth loop offers beside an opponent's permanent with a class-counting activated
/// ability and attack trigger; whether it reads the growing class is the loop's own replay to show
/// (CR 732.2a).
#[test]
fn foreign_object_second_surface_still_vetoes_after_x1() {
    offers_beside_observer(
        Phase::PreCombatMain,
        P1,
        AGGREGATE_TWO_SURFACE_ORACLE,
        (1, 1),
    );
}

// ===========================================================================
// PR-7 Phase 1b — CR 732.2a loop-detect ring retention across a FORCED
// pre-priority window and the action that answers it.
//
// Both rows LOAD a committed real 4-player dump through the production restore
// chokepoint (`PersistedGameState::Raw(..).into_game_state()`, the same path the
// server's `from_persisted` and WASM's `decode_restored_game_state` funnel
// through) and DRIVE through the public `apply()` boundary. Synthetic
// `GameScenario` boards are deliberately NOT used here: the property under test
// is an accumulation across dozens of real beats.
// ===========================================================================

pub(crate) fn gunzip_dump(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
    engine::types::game_state::record_captured_onsets_json(&json)
}

pub(crate) fn restore_dump(json: &str) -> GameState {
    let envelope: serde_json::Value =
        serde_json::from_str(json).expect("dump envelope parses as JSON");
    // Decode AS `PersistedGameState` rather than decoding a bare `GameState` and wrapping
    // it in `Raw`: only the former runs `reject_legacy_raw_prompt_authority` and
    // `decode_persisted_resolution_state`, which is the rest of the production chokepoint
    // — including the CR 732.2a load-seam bound invariant `w15_*` below pins.
    // The test unwraps the fallible persistence boundary after asserting this fixture decodes.
    serde_json::from_value::<engine::types::game_state::PersistedGameState>(
        envelope["gameState"].clone(),
    )
    .expect("gameState deserializes through the production decoder")
    .into_game_state()
    .expect("persisted test snapshot satisfies the checked restore contract")
}

/// The migrated dellian dump's `gameState`, as a raw `serde_json::Value`.
fn dellian_game_state_value() -> serde_json::Value {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    envelope["gameState"].clone()
}

/// R0c — the migrated fixture decodes through BOTH decoders, and an un-migrated one
/// through NEITHER.
///
/// This is the row that converts the next upstream save-compat break into a named
/// regression instead of four unrelated-looking red rows. Upstream #6718 (`0468df1f4`)
/// added `TargetSelectionSlot::effect_kind` with no `#[serde(default)]`; every dump
/// fixture captured before it became undecodable, and nothing said so in one place.
///
/// The negative arm IS the positive arm's anti-vacuity control: without it, an
/// `assert!(ok)` pair would pass on any value at all, including one where the field was
/// never consulted. Both arms operate on the SAME value, differing only by the presence
/// of `effect_kind`, so the verdict is attributable to that field and nothing else.
#[test]
fn migrated_dump_decodes_through_both_decoders_and_unmigrated_through_neither() {
    let migrated = dellian_game_state_value();

    // Reach-guard: prove the mutation below has something to remove. A value with no
    // `effect_kind` key would make the negative arm's `Err` unattributable.
    let slots = migrated["waiting_for"]["data"]["target_slots"]
        .as_array()
        .expect("the dellian dump publishes a target_slots array");
    assert_eq!(
        slots.len(),
        1,
        "the dellian dump publishes exactly one slot"
    );
    assert!(
        slots[0].get("effect_kind").is_some(),
        "the MIGRATED fixture must carry effect_kind — if this fails, the migration script \
         was not run, and the negative arm below would pass for the wrong reason"
    );

    // POSITIVE: both the direct `GameState` decode and the production `PersistedGameState`
    // decode accept the migrated value.
    assert!(
        serde_json::from_value::<GameState>(migrated.clone()).is_ok(),
        "migrated fixture must decode as a bare GameState"
    );
    assert!(
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(migrated.clone())
            .is_ok(),
        "migrated fixture must decode through the PRODUCTION decoder"
    );

    // R8 — THE ROUTING CANONICALIZES PERSISTED PROVENANCE. The six loaders stopped decoding
    // a bare `GameState` and wrapping it in `PersistedGameState::Raw`, and started decoding AS
    // `PersistedGameState`: a different type, a different `Deserialize`, and a different
    // conversion (`decode_persisted_resolution_state`, which injects
    // `resolution_state_version` and decodes the resolution state as `ResolutionStateWire`).
    // The persistence boundary assigns the current-turn occurrence namespace to the old
    // ledger and matching live stack events; direct-current raw decode deliberately does not.
    // Every other serialized surface must remain unchanged.
    //
    // `GameState` has no `PartialEq`, so compare its serialized forms recursively.
    // Hash-backed owners now serialize canonically at their field boundaries; arrays are
    // therefore always order-sensitive here, including libraries, stacks, and trigger order.
    // Sorting below is diagnostic only: it distinguishes reordering from changed membership
    // without ever accepting either difference.

    /// Collect every path at which `a` and `b` differ in a way that set-ordering cannot
    /// explain. Returns an empty vec iff the two states are equal modulo set order.
    fn differences(
        a: &serde_json::Value,
        b: &serde_json::Value,
        path: &str,
        out: &mut Vec<String>,
    ) {
        use serde_json::Value;
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
                keys.sort();
                keys.dedup();
                for key in keys {
                    match (x.get(key), y.get(key)) {
                        (Some(l), Some(r)) => differences(l, r, &format!("{path}.{key}"), out),
                        _ => out.push(format!("{path}.{key} (present on one side only)")),
                    }
                }
            }
            (Value::Array(x), Value::Array(y)) if x == y => {}
            (Value::Array(x), Value::Array(y)) => {
                let (mut xs, mut ys) = (x.clone(), y.clone());
                let key = |v: &Value| v.to_string();
                xs.sort_by_key(key);
                ys.sort_by_key(key);
                if xs == ys {
                    out.push(format!("{path} (REORDERED)"));
                } else {
                    out.push(format!("{path} (different elements)"));
                }
            }
            _ if a == b => {}
            _ => out.push(path.to_string()),
        }
    }

    let serialized = |state: &GameState| serde_json::to_value(state).expect("GameState serializes");
    let legacy_restored = {
        let raw: GameState = serde_json::from_value(migrated.clone())
            .expect("the pre-routing loader form: a bare GameState decode");
        engine::types::game_state::PersistedGameState::Raw(Box::new(raw))
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract")
    };
    let routed_restored =
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(migrated.clone())
            .expect("the routed loader form: decode AS PersistedGameState")
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract");
    let routed_value = serialized(&routed_restored);
    let mut diffs = Vec::new();
    differences(
        &serialized(&legacy_restored),
        &routed_value,
        "state",
        &mut diffs,
    );
    assert_eq!(
        diffs,
        vec![
            "state.stack (different elements)".to_string(),
            "state.zone_changes_this_turn (different elements)".to_string(),
        ],
        "the production restore changes only the persisted zone-change provenance surfaces"
    );

    let stack_zone_change_keys = |state: &GameState| {
        state
            .stack
            .iter()
            .filter_map(|entry| {
                let StackEntryKind::TriggeredAbility {
                    trigger_event: Some(GameEvent::ZoneChanged { record, .. }),
                    ..
                } = &entry.kind
                else {
                    return None;
                };
                Some((record.recorded_turn_number, record.turn_zone_change_index))
            })
            .collect::<Vec<_>>()
    };
    let legacy_stack_keys = stack_zone_change_keys(&legacy_restored);
    let routed_stack_keys = stack_zone_change_keys(&routed_restored);
    assert!(
        !legacy_stack_keys.is_empty(),
        "reach-guard: the fixture has live stack ZoneChanged trigger contexts"
    );
    assert!(
        legacy_restored
            .zone_changes_this_turn
            .iter()
            .all(|record| record.recorded_turn_number == 0)
            && legacy_stack_keys.iter().all(|(turn, _)| *turn == 0),
        "direct-current raw decode retains the historical zero provenance defaults"
    );
    assert_eq!(
        routed_restored
            .zone_changes_this_turn
            .iter()
            .enumerate()
            .map(|(index, record)| (
                record.recorded_turn_number,
                record.turn_zone_change_index,
                index
            ))
            .collect::<Vec<_>>(),
        (0..routed_restored.zone_changes_this_turn.len())
            .map(|index| (routed_restored.turn_number, index, index))
            .collect::<Vec<_>>(),
        "production restore stamps each retained ledger row in its current-turn namespace"
    );
    assert!(
        routed_stack_keys
            .iter()
            .all(|(turn, _)| *turn == routed_restored.turn_number),
        "production restore stamps every matching live stack event with the current turn"
    );
    assert_eq!(
        routed_stack_keys
            .iter()
            .map(|(_, index)| *index)
            .collect::<Vec<_>>(),
        legacy_stack_keys
            .iter()
            .map(|(_, index)| *index)
            .collect::<Vec<_>>(),
        "production restore preserves each stack event's ledger occurrence identity"
    );

    // The perturbation IS that assertion's reach-guard: without it, a comparison that
    // compared a value to itself — or explained every difference away as set order — would
    // pass on any two states at all. Perturb ONE scalar; the comparison must SEE it, and
    // must name the field it saw.
    let mut perturbed = legacy_restored.clone();
    perturbed.turn_number += 1;
    let mut perturbed_diffs = Vec::new();
    differences(
        &serialized(&perturbed),
        &serialized(&legacy_restored),
        "state",
        &mut perturbed_diffs,
    );
    assert_eq!(
        perturbed_diffs,
        vec!["state.turn_number".to_string()],
        "the comparison must see a one-scalar difference AND name it; if it cannot, the \
         canonicalization-surface comparison above proves nothing"
    );

    // NEGATIVE (the anti-vacuity control): strip the field back out and both must reject.
    let mut unmigrated = migrated;
    unmigrated["waiting_for"]["data"]["target_slots"]
        .as_array_mut()
        .expect("target_slots is an array")
        .iter_mut()
        .for_each(|slot| {
            slot.as_object_mut()
                .expect("each slot is an object")
                .remove("effect_kind")
                .expect("each slot carried effect_kind before removal");
        });
    assert!(
        serde_json::from_value::<GameState>(unmigrated.clone()).is_err(),
        "an un-migrated save must NOT decode as a bare GameState"
    );
    assert!(
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(unmigrated)
            .is_err(),
        "an un-migrated save must NOT decode through the production decoder — the strict \
         decoder is the point; a #[serde(default)] shim would silently accept it"
    );
}

/// R0d — CR 732.2a: a persisted `LoopShortcut` offer whose WIRE bound is `0` must fail the
/// load, and one whose bound is `5` must not.
///
/// The defect this pins (W15) is real and was measured before the fix: a wire
/// count of `0` deserialized clean, satisfied `is_bounded()`, and reached
/// `ai_support/candidates.rs`, which echoed it as a declared `IterationCount::Fixed(0)` —
/// so the engine opened the CR 732.2b response window for an offer that admits no legally
/// takeable sequence. The offer was corrupt one beat BEFORE any count was declared.
///
/// ⚠ THE FIXTURE CHOICE IS LOAD-BEARING — this row uses TENACITY, not the dellian dump the
/// dual-decode row uses. The dellian value is `TriggerTargetSelection` and carries no
/// `schema` object at all, so the legacy `…schema.max_iterations` key cannot even be written
/// onto it: both
/// arms would decode identically, for a reason having nothing to do with the invariant.
/// The tenacity dump is the only in-tree `LoopShortcut` capture.
///
/// The key is ABSENT in the fixture, so the mutation CREATES it — asserted below, because a
/// mutation that silently failed to apply would make the `0` arm's verdict meaningless.
///
/// ⚠ NON-VACUITY DEPENDS ON THE ROUTED LOADER. This row decodes AS `PersistedGameState`;
/// the pre-5c loader form (`PersistedGameState::Raw(Box::new(bare_decode))`) bypasses
/// `PersistedGameState`'s own `Deserialize` and therefore never runs
/// `decode_persisted_resolution_state` at all, so the same assertions written against that
/// form could not fail. REVERT-PROBE: delete `reject_zero_bound_shortcut_offer`'s body (or
/// its call) ⇒ the `0` arm decodes `Ok` ⇒ FAILS. The wire-`5` arm is the anti-vacuity half:
/// it proves the mutation instrument reaches the field and that the invariant refuses `0`
/// specifically rather than refusing every mutated save.
/// A PRE-SPLIT persisted offer decodes to the boundedness verdict it had, through both
/// production raw-JSON ingresses, plus the zero the load still refuses.
///
/// The legacy key answered two questions in one integer, writing the engine's budget to mean
/// "measured nothing". `migrate_legacy_shortcut_repetition_bound` is the inverse of that
/// encoding, and this row drives it through the ingresses rather than calling it: a legacy value
/// BELOW the budget decodes bounded with the capacity that value, a legacy value AT the budget
/// decodes UNBOUNDED (the hostile sibling — a migration moving the value into the measured field
/// unconditionally would mint a measured threshold for a save whose producer measured none), and
/// a legacy zero re-encodes to a zero capacity the load still refuses.
///
/// REVERT-PROBE: delete the migration ⇒ the narrowed arm's capacity defaults to the budget and
/// its verdict flips to unbounded ⇒ FAILS, while the zero arm stops failing the load at all.
#[test]
fn a_wire_zero_shortcut_bound_fails_the_load_and_a_wire_five_does_not() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let base = envelope["gameState"].clone();

    // Reach-guards on the fixture itself.
    assert_eq!(
        base["waiting_for"]["type"].as_str(),
        Some("LoopShortcut"),
        "R0d must run on a LoopShortcut capture — the invariant is scoped to that variant"
    );
    assert!(
        base["waiting_for"]["data"]["schema"].is_object(),
        "the tenacity offer carries a schema object for the bound to live on"
    );
    let schema = &base["waiting_for"]["data"]["schema"];
    assert!(
        schema.get("max_iterations").is_none()
            && schema.get("measured_repetition_bound").is_some()
            && schema.get("deliverable_capacity").is_some(),
        "the save carries the split pair, which the mutation below replaces with the legacy key \
         the migration consumes"
    );

    let with_bound = |n: u64| {
        let mut v = base.clone();
        let schema = v["waiting_for"]["data"]["schema"]
            .as_object_mut()
            .expect("the offer carries a schema object");
        schema.remove("measured_repetition_bound");
        schema.remove("deliverable_capacity");
        schema.insert("max_iterations".to_string(), serde_json::json!(n));
        // Anti-vacuity: prove the write landed before drawing any conclusion from it.
        assert_eq!(
            v["waiting_for"]["data"]["schema"]["max_iterations"].as_u64(),
            Some(n),
            "the mutation must reach schema.max_iterations"
        );
        v
    };

    // CR 732.2a: a proposal must describe "a sequence of game choices ... that may be
    // legally taken based on the current game state". A bound of 0 admits none.
    let zero =
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(with_bound(0));
    let message = zero
        .expect_err("a legacy wire bound of 0 must fail the load, not revive a corrupt offer")
        .to_string();
    assert!(
        message.contains("deliverable_capacity 0"),
        "the rejection must NAME the invariant it enforces, got: {message}"
    );

    // The control: same fixture, same instrument, same mutated key, a legal bound — and the
    // MIGRATED verdict, which is what tells "recovered from the legacy key" apart from
    // "defaulted". A legacy 5 was a measured threshold, so the offer reads bounded at 5.
    let five =
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(with_bound(5))
            .expect("a legacy wire bound of 5 is a legal bound and must still load");
    let engine::types::game_state::PersistedGameState::Raw(five) = five else {
        panic!("the tenacity envelope restores as a raw persisted state");
    };
    let WaitingFor::LoopShortcut { schema, .. } = &five.waiting_for else {
        panic!("the restored capture must still be parked on its offer");
    };
    assert_eq!(schema.measured_repetition_bound, Some(5));
    assert_eq!(schema.deliverable_capacity, 5);
    assert!(schema.is_bounded());

    // THE HOSTILE SIBLING: the legacy value AT the budget meant "measured nothing", so it must
    // decode UNBOUNDED at a capacity of the budget — never bounded at the budget.
    let sentinel = crate::fantastic_four_bounded_loop::MAX_SHORTCUT_CYCLES_MIRROR;
    let at_budget = serde_json::from_value::<engine::types::game_state::PersistedGameState>(
        with_bound(u64::from(sentinel)),
    )
    .expect("a legacy wire bound at the budget is the un-narrowed encoding and must load");
    let engine::types::game_state::PersistedGameState::Raw(at_budget) = at_budget else {
        panic!("the tenacity envelope restores as a raw persisted state");
    };
    let WaitingFor::LoopShortcut { schema, .. } = &at_budget.waiting_for else {
        panic!("the restored capture must still be parked on its offer");
    };
    assert_eq!(
        schema.measured_repetition_bound, None,
        "the budget was the OLD encoding's 'measured nothing', so nothing was measured"
    );
    assert!(!schema.is_bounded());
    assert_eq!(schema.deliverable_capacity, sentinel);

    // ── THE SECOND INGRESS ──────────────────────────────────────────────────────────
    // CR 732.2a again, through the OTHER decode entry point. Upstream #6933 split the
    // decode surface in two: `GameStateDecode::decode_persisted_resolution_state` (which
    // the arms above reach) deserializes `ResolutionStateWire` itself and NEVER calls
    // `GameStateDecode::decode`. A bare `GameState` decode routes through `decode` with
    // `GameStateDecodeMode::DirectCurrentRaw` instead (see `impl Deserialize for
    // GameState`), so hosting the bound guard on only one of them leaves this path able
    // to revive exactly the corrupt offer the arms above refuse.
    //
    // REVERT-PROBE: delete the `reject_zero_bound_shortcut_offer` call in
    // `GameStateDecode::decode` ONLY — leaving the one in
    // `decode_persisted_resolution_state` — and every arm above still passes while this
    // one flips to `Ok`. That single-site revert is why this row exists and why the
    // arms above cannot stand in for it.
    //
    // REACH-GUARD FIRST: `DirectCurrentRaw` deliberately SKIPS only the MODE-GATED legacy
    // migrations; those registered outside that guard — the repetition-bound split the
    // `Some(5)` arm below reads among them — run on every mode. So if this fixture could not
    // decode bare at all, the `Err` below would prove nothing about the bound.
    assert!(
        serde_json::from_value::<GameState>(base.clone()).is_ok(),
        "reach-guard: the unmutated fixture must decode through the bare-GameState \
         ingress, or the zero-bound Err below is not attributable to the bound"
    );

    let bare_zero = serde_json::from_value::<GameState>(with_bound(0));
    let bare_message = bare_zero
        .expect_err("the bare-GameState ingress must refuse a legacy wire bound of 0 too")
        .to_string();
    assert!(
        bare_message.contains("deliverable_capacity 0"),
        "the bare-ingress rejection must NAME the same invariant, got: {bare_message}"
    );
    // The second ingress recovers the verdict too: a missed registration at either call site is
    // otherwise silent, because the compiler sees neither.
    let bare_five = serde_json::from_value::<GameState>(with_bound(5))
        .expect("a legal bound must still load through the bare-GameState ingress");
    let WaitingFor::LoopShortcut { schema, .. } = &bare_five.waiting_for else {
        panic!("the restored capture must still be parked on its offer");
    };
    assert_eq!(schema.measured_repetition_bound, Some(5));
    assert_eq!(schema.deliverable_capacity, 5);
}

/// R0e — a persisted `LoopShortcut` offer on the ring road carrying a confirmed period fails the
/// load whatever its bound; the recorded road's offer carrying its period loads, bounded or not. The period is a production-serialized value taken from a live
/// offer.
#[test]
fn a_wire_ring_road_offer_carrying_a_period_fails_the_load() {
    let Some(db) = super::support::shared_card_db() else {
        return;
    };
    let mut rig = super::loop_shortcut_mana_engine::setup(
        true,
        engine::types::game_state::LoopDetectionMode::Interactive,
        db,
    );
    let mana = super::loop_shortcut_mana_engine::mana_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt publishes its mana ability");
    let untap =
        super::loop_shortcut_mana_engine::untap_ability_index(rig.runner.state(), rig.basalt)
            .expect("Basalt publishes its untap ability");
    super::loop_shortcut_mana_engine::drive_one_period(&mut rig, mana, untap);
    let WaitingFor::LoopShortcut { period, .. } = &rig.runner.state().waiting_for else {
        panic!(
            "REACH-GUARD: the mana engine offers, got {:?}",
            rig.runner.state().waiting_for
        );
    };
    let donor_period = serde_json::to_value(period).expect("a period serializes");
    assert!(
        donor_period["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "REACH-GUARD: the donated period is non-empty, or every period arm is vacuous"
    );

    let json = gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let base = envelope["gameState"].clone();
    assert_eq!(
        base["waiting_for"]["type"].as_str(),
        Some("LoopShortcut"),
        "the invariant is scoped to the one variant that carries a schema AND a period"
    );
    assert!(
        base["waiting_for"]["data"]["schema"]
            .get("deliverable_capacity")
            .is_some()
            && base["waiting_for"]["data"].get("period").is_some(),
        "the save carries a bound and a period, so each splice REPLACES the key it tests"
    );

    let spliced = |mut v: serde_json::Value,
                   road: &str,
                   bound: Option<u64>,
                   period: Option<&serde_json::Value>| {
        let data = v["waiting_for"]["data"]
            .as_object_mut()
            .expect("the offer carries its data");
        data.remove("period");
        let schema = data["schema"]
            .as_object_mut()
            .expect("the offer carries a schema object");
        schema.remove("measured_repetition_bound");
        schema.remove("deliverable_capacity");
        v["waiting_for"]["data"]["road"] = serde_json::json!(road);
        if let Some(n) = bound {
            v["waiting_for"]["data"]["schema"]["max_iterations"] = serde_json::json!(n);
        }
        if let Some(period) = period {
            v["waiting_for"]["data"]["period"] = period.clone();
        }
        v
    };
    let decode_persisted = |value: serde_json::Value| {
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(value)
    };

    // ── A1 — THE GUARD FIRES on a bounded ring offer.
    let message = decode_persisted(spliced(base.clone(), "Ring", Some(5), Some(&donor_period)))
        .expect_err("a ring offer carrying a confirmed period must fail the load")
        .to_string();
    assert!(
        message.contains("ring road carries a confirmed period"),
        "the rejection must NAME the invariant it enforces and must not be either sibling zero \
         guard firing instead, got: {message}"
    );

    // ── A6 — ORDERING PROBE: a legacy zero migrates to a measured zero, so the zero check must
    // keep answering first.
    let message = decode_persisted(spliced(base.clone(), "Ring", Some(0), Some(&donor_period)))
        .expect_err("a zero bound must still fail the load when a period rides with it")
        .to_string();
    assert!(
        message.contains("deliverable_capacity 0"),
        "ORDERING: hoisting the period check above the zero-bound check relabels a corrupt zero \
         with the wrong invariant, got: {message}"
    );

    // ── A2 — THE PERIOD CONJUNCT: a bounded ring offer with no period is the bounded mint.
    assert!(
        decode_persisted(spliced(base.clone(), "Ring", Some(5), None)).is_ok(),
        "a narrowed bound with no period is exactly what the bounded mint publishes"
    );

    // ── A3 — THE ROAD, NOT THE BOUND, keys the refusal.
    assert!(
        decode_persisted(spliced(base.clone(), "Ring", None, Some(&donor_period))).is_err(),
        "an unbounded ring offer carrying a period routes away from the drain too"
    );
    for bound in [None, Some(5)] {
        assert!(
            decode_persisted(spliced(
                base.clone(),
                "RecordedPeriod",
                bound,
                Some(&donor_period)
            ))
            .is_ok(),
            "a recorded offer carrying its period must load, bound {bound:?}"
        );
    }

    // ── A4 — ANY SEAT: the take routes on the period whoever made its plays.
    let foreign_period = {
        let mut period = donor_period.clone();
        for item in period["items"]
            .as_array_mut()
            .expect("a period lists its items")
        {
            item["seat"] = serde_json::json!(1);
        }
        period
    };
    assert!(
        decode_persisted(spliced(
            base.clone(),
            "Ring",
            Some(5),
            Some(&foreign_period)
        ))
        .is_err(),
        "a period of another seat routes the agreed cycles away from the drain too"
    );

    // ── A5 — THE OTHER GUARDED INGRESS: `GameStateDecode::decode` through
    // `impl Deserialize for GameState`, on the same save's bare state.
    assert!(
        serde_json::from_value::<GameState>(spliced(
            base.clone(),
            "Ring",
            Some(5),
            Some(&donor_period)
        ))
        .is_err(),
        "the bare ingress refuses the pair too"
    );
    assert!(
        serde_json::from_value::<GameState>(spliced(
            base,
            "RecordedPeriod",
            Some(5),
            Some(&donor_period)
        ))
        .is_ok(),
        "and loads the bounded recorded offer carrying its period"
    );
}

/// Opponents the ENGINE considers living. `Player::is_eliminated` is the authority the
/// CR 732.2a detector uses when it builds its `living` set — `eliminated_players` and
/// `life > 0` are not sufficient on their own, so this reads the field the detector reads.
fn engine_live_opponents(state: &GameState, of: PlayerId) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| p.id != of && !p.is_eliminated)
        .map(|p| p.id)
        .collect()
}

/// On the COMMITTED-SAVE population, what each dump's own raw JSON spells is what its
/// decoded pair says, and the legacy key's OTHER receivers are left alone.
///
/// Three classes, each read off the raw payload rather than named by hand: saves that carry the
/// split pair (the verdict is BOUNDED at that value), a save whose pair is spliced out (it
/// defaults — unbounded at the budget), and saves whose `RepeatContinuation::WhileCondition`
/// spells the legacy key for its own ceiling (a name-keyed walk would rewrite or drop those,
/// turning a bounded while-condition into an unbounded one). Each class has a member on BOTH
/// raw-JSON ingresses.
///
/// REVERT-PROBE: key the walk on the name instead of the receiver ⇒ the third class's multisets
/// diverge ⇒ FAILS.
#[test]
fn committed_saves_decode_to_the_boundedness_verdict_their_raw_json_states() {
    let mirror = crate::fantastic_four_bounded_loop::MAX_SHORTCUT_CYCLES_MIRROR;

    // The raw `schema` object of a payload parked on a `LoopShortcut` offer.
    fn raw_schema(state: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
        assert_eq!(
            state["waiting_for"]["type"].as_str(),
            Some("LoopShortcut"),
            "this class is scoped to saves parked on an offer"
        );
        state["waiting_for"]["data"]["schema"]
            .as_object()
            .expect("an offer carries a schema object")
    }

    // Values at every `max_iterations` key under `objects` — i.e. the key's NON-schema receivers,
    // which are `RepeatContinuation::WhileCondition` ceilings.
    fn foreign_ceilings(objects: &serde_json::Value) -> Vec<u64> {
        fn walk(value: &serde_json::Value, out: &mut Vec<u64>) {
            match value {
                serde_json::Value::Object(map) => {
                    if let Some(n) = map
                        .get("max_iterations")
                        .and_then(serde_json::Value::as_u64)
                    {
                        out.push(n);
                    }
                    for v in map.values() {
                        walk(v, out);
                    }
                }
                serde_json::Value::Array(items) => {
                    for v in items {
                        walk(v, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        walk(objects, &mut out);
        out.sort_unstable();
        out
    }

    // ── CLASS 1 — the split pair is present and BELOW the budget: it decodes as spelled. ────
    for (label, gz) in [
        (
            "lethal_lifegain_loss_4p",
            &include_bytes!("../fixtures/lethal_lifegain_loss_4p.json.gz")[..],
        ),
        (
            "weird_drain_4p",
            &include_bytes!("../fixtures/weird_drain_4p.json.gz")[..],
        ),
    ] {
        let json = gunzip_dump(gz);
        let envelope: serde_json::Value =
            serde_json::from_str(&json).expect("dump envelope parses as JSON");
        let raw = envelope["gameState"].clone();
        let raw_schema = raw_schema(&raw);
        assert!(
            !raw_schema.contains_key("max_iterations"),
            "[{label}] REACH-GUARD: the save carries no legacy key"
        );
        let measured = raw_schema
            .get("measured_repetition_bound")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_else(|| panic!("[{label}] this class is the saves that CARRY the pair"))
            as u32;
        assert!(
            measured > 0 && measured < mirror,
            "[{label}] BOARD CLASS: a narrowed value, so 'decoded' and 'defaulted' differ; got \
             {measured}"
        );

        let state = restore_dump(&json);
        let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
            panic!("[{label}] the save restores at its offer");
        };
        assert_eq!(schema.measured_repetition_bound, Some(measured));
        assert!(schema.is_bounded(), "[{label}] and the verdict is BOUNDED");
        assert_eq!(
            raw_schema["deliverable_capacity"].as_u64(),
            Some(u64::from(schema.deliverable_capacity)),
            "[{label}] the capacity is the ceiling that encoding published"
        );
        assert_eq!(
            schema.iteration_count,
            IterationCount::Fixed(measured),
            "[{label}] and the suggestion the save carries still agrees with it"
        );
    }

    // ── CLASS 2 — NEITHER key: the defaults answer, on both ingresses. ────────────────────
    // This is what tells a decoded save apart from a defaulted one, and it buys the claim that a
    // per-field `#[serde(default)]` cannot recover a bounded verdict on its own.
    let tenacity = gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    ));
    let mut tenacity_envelope: serde_json::Value =
        serde_json::from_str(&tenacity).expect("dump envelope parses as JSON");
    let schema_raw = tenacity_envelope["gameState"]["waiting_for"]["data"]["schema"]
        .as_object_mut()
        .expect("the tenacity save is parked on an offer");
    assert!(
        schema_raw.remove("measured_repetition_bound").is_some()
            && schema_raw.remove("deliverable_capacity").is_some()
            && !schema_raw.contains_key("max_iterations"),
        "REACH-GUARD: the splice removes the pair the save carries and leaves no count key"
    );
    let envelope_state = restore_dump(&tenacity_envelope.to_string());
    let bare_state: GameState = serde_json::from_value(tenacity_envelope["gameState"].clone())
        .expect("the bare ingress decodes the spliced save");
    for (label, state) in [("envelope", &envelope_state), ("bare", &bare_state)] {
        let WaitingFor::LoopShortcut { schema, .. } = &state.waiting_for else {
            panic!("[{label}] the tenacity save restores at its offer");
        };
        assert_eq!(schema.measured_repetition_bound, None, "[{label}]");
        assert!(!schema.is_bounded(), "[{label}]");
        assert_eq!(schema.deliverable_capacity, mirror, "[{label}]");
        // A restored save may carry ANY suggestion against ANY capacity: agreement is an
        // invariant of freshly minted offers only.
        assert_eq!(
            schema.iteration_count,
            IterationCount::Fixed(15),
            "[{label}]"
        );
        assert!(15 < schema.deliverable_capacity, "[{label}]");
    }

    // ── CLASS 3 — the key's OTHER receiver, on both ingresses. ────────────────────────────
    let precast = gunzip_dump(include_bytes!(
        "../fixtures/combo_infinite_pile_4p_untapped_precast.json.gz"
    ));
    let precast_raw: serde_json::Value =
        serde_json::from_str(&precast).expect("the precast capture is BARE and parses as JSON");
    let precast_state: GameState =
        serde_json::from_str(&precast).expect("the bare ingress decodes the precast capture");
    let dina = gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_phase5_no_offer_4p.json.gz"
    ));
    let dina_raw: serde_json::Value = serde_json::from_str(&dina)
        .map(|v: serde_json::Value| v["gameState"].clone())
        .expect("dump envelope parses as JSON");
    let dina_state = restore_dump(&dina);

    for (label, raw, decoded) in [
        ("precast (bare ingress)", &precast_raw, &precast_state),
        ("dina (envelope ingress)", &dina_raw, &dina_state),
    ] {
        let before = foreign_ceilings(&raw["objects"]);
        assert!(
            !before.is_empty(),
            "[{label}] REACH-GUARD: this save must actually carry while-condition ceilings, \
             else the comparison below is vacuous"
        );
        let reserialized =
            serde_json::to_value(decoded).expect("a decoded state re-serializes for projection");
        assert_eq!(
            foreign_ceilings(&reserialized["objects"]),
            before,
            "[{label}] the migration names the LoopShortcut receiver, not the key: every \
             while-condition ceiling reads what this save's own raw JSON spells"
        );
    }
}

/// Actions a dump driver must never take: they end the game or bypass the reducer, and a
/// generic "first legal action" driver otherwise picks them and fakes a result.
fn dump_driver_forbids(a: &GameAction) -> bool {
    matches!(a, GameAction::Concede { .. } | GameAction::Debug(_))
}

/// The seat that can act on this beat plus its legal actions, read through the same
/// per-viewer enumerator the multiplayer transport uses. `WaitingFor::acting_player` is
/// the engine's own answer to "whose beat is this", so it is tried first; the all-seat
/// scan is the fallback and costs roughly 4x per beat.
fn dump_beat_actor(state: &GameState) -> Option<(PlayerId, Vec<GameAction>)> {
    if let Some(p) = state.waiting_for.acting_player() {
        let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, p);
        if !actions.is_empty() {
            return Some((p, actions));
        }
    }
    for p in state.players.iter().map(|p| p.id) {
        let (actions, _costs, _grouped) = engine::ai_support::legal_actions_for_viewer(state, p);
        if !actions.is_empty() {
            return Some((p, actions));
        }
    }
    None
}

/// One beat of the drain-loop drive policy: at `Priority` ALWAYS pass (the mandatory
/// triggers resolve and re-trigger — that IS the loop; casting here wanders off it), and
/// answer every other prompt, preferring a target choice aimed at `pin`.
///
/// Returns the beat's `GameEvent`s so a caller can key on what the beat actually DID
/// (`CombatDamageDealtToPlayer` / `DamageDealt` for the CR 510.2 rows) instead of
/// inferring it from phase and life deltas. Callers that only need liveness ignore it.
///
/// ⚠ THE `pin` PREFERENCE IS INERT ON EVERY TRACKED DUMP, and that is MEASURED, not
/// inferred from this function's body. Driving all five tracked 4p dumps for 60 beats each:
/// only `dellian_emblem_conqueror_4p` reaches a `WaitingFor::TriggerTargetSelection` window
/// at all (seven of them, at beats 0/9/18/27/36/45/54, all on `ObjectId(541)`), and every
/// one of those windows enumerates **`GameAction::ChooseTarget` ×3 and
/// `GameAction::SelectTargets` ×0** — so the `SelectTargets` preference above never fires
/// and the fallback answers with a `ChooseTarget`. `dina`, `tenacity`,
/// `witherbloom_sprout_lumaret` and `witherbloom_sprout_lumaret_simple` reach NO
/// `TriggerTargetSelection` window in 60 beats. This is the same trap
/// `fantastic_four_bounded_loop.rs` already records for the F4 dump, and it means NO TRACKED
/// FIXTURE CAN EXERCISE THE `SelectTargets` REDUCER ARM at the wire tier — see
/// [`c2a_row_t1b_both_trigger_target_selection_arms_route_through_the_single_writer`].
fn dump_drive_one_beat(
    state: &mut GameState,
    pin: Option<PlayerId>,
) -> Result<Vec<GameEvent>, String> {
    // CR 117.3d: at a priority window this policy always passes, so dispatch the pass instead of
    // enumerating the whole per-viewer candidate set to find it — on the 152-entry `dellian` stack
    // that scan is a real per-beat cost. This reproduces both halves of the enumerator's hatch —
    // its structural predicate and the submitter identity it authorizes (CR 723.5) — so this arm
    // stays inside the subset that hatch asserts equivalent to a simulated pass; every other shape
    // falls through to the enumerating path below.
    if let WaitingFor::Priority { player } = state.waiting_for {
        if engine::game::priority::pass_priority_structurally_legal(state, player) {
            let actor = engine::game::turn_control::authorized_submitter_for_player(state, player);
            return apply(state, actor, GameAction::PassPriority)
                .map(|r| r.events)
                .map_err(|e| format!("apply err (PassPriority): {e:?}"));
        }
    }
    let Some((who, actions)) = dump_beat_actor(state) else {
        return Err(format!("no legal actor at {:?}", state.waiting_for));
    };
    let chosen = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        actions
            .iter()
            .find(|a| matches!(a, GameAction::PassPriority))
            .cloned()
    } else {
        pin.and_then(|t| {
            actions.iter().find(|a| {
                matches!(a, GameAction::SelectTargets { targets }
                    if targets.iter().any(|r| matches!(r, TargetRef::Player(p) if *p == t)))
            })
        })
        .or_else(|| {
            actions
                .iter()
                .find(|a| !matches!(a, GameAction::PassPriority) && !dump_driver_forbids(a))
        })
        .or_else(|| actions.iter().find(|a| !dump_driver_forbids(a)))
        .cloned()
    };
    let Some(action) = chosen else {
        return Err(format!("empty action list at {:?}", state.waiting_for));
    };
    let actor = engine::game::turn_control::authorized_submitter_for_player(state, who);
    apply(state, actor, action.clone())
        .map(|r| r.events)
        .map_err(|e| format!("apply err ({action:?}): {e:?}"))
}

/// PR-7 Phase 5a — CR 732.2a per-iteration pin enumeration, on a REAL 4p board.
///
/// `bounded_cycle_pin_slots` is the single authority for the choice slots a bounded cycle
/// offer must publish. Dump B is the acceptance population: obj **541** is a CR 114.2
/// emblem (command zone, "both owned and controlled by that player") whose triggered
/// ability drains `target opponent`, i.e. the
/// `Typed{type_filters: [], controller: Opponent, properties: []}` player shape.
///
/// **TWO ARMS, and the pairing is the whole point.** The dump ships with the prompt UP
/// (`TriggerTargetSelection` carrying already-materialized `legal_targets`), so arm ⓐ alone
/// would pass against a prompt-READING implementation — which returns ZERO slots at the
/// real offer beat, where `waiting_for` is `Priority`. Arm ⓑ is the identical state with
/// exactly one field reassigned.
///
/// REVERT-PROBES (both must flip):
/// * ⓘ narrow the AST predicate so `Typed{[], Opponent, []}` is rejected ⇒ BOTH arms return
///   zero ⇒ FAILS.
/// * ⓙ re-implement the enumerator to read `state.waiting_for`'s
///   `target_slots[..].legal_targets` ⇒ arm ⓐ still passes, arm ⓑ returns zero ⇒ FAILS.
/// * ⓐ' delete the `entry.controller != proposer` filter ⇒ the bystander-proposer
///   assertion FAILS. (On THIS board every one of the 152 stack entries is P0-controlled —
///   measured — so the honest form of that probe is a bystander proposer, not a rising
///   count for P0.)
///
/// MUST-NOT-FLIP: `bounded_cycle_pin_slots(..).is_empty()` on the shipped
/// `b3_materialize_stop_short` offer board — asserted, not assumed. That zero is the
/// byte-identity pin for every shipped `Fixed(N)` drive, and pairing it with dump B's
/// non-zero in the SAME row proves the instrument returns both values.
#[test]
fn bounded_cycle_pin_slots_enumerates_the_emblem_slot() {
    use engine::game::engine::bounded_cycle_pin_slots;
    use engine::types::zones::Zone;

    const EMBLEM: ObjectId = ObjectId(541);
    const P3: PlayerId = PlayerId(3);

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    )));

    // ── reach guards, all derived from the loaded board, none from the predicate ──
    let emblem = state
        .objects
        .get(&EMBLEM)
        .expect("reach-guard: dump B carries the emblem object");
    assert_eq!(
        emblem.zone,
        Zone::Command,
        "reach-guard: CR 114.2 puts the emblem in the command zone — the whole reason this \
         row exists (a battlefield-only slot builder would be untested by it)"
    );
    let emblem_incarnation = emblem.incarnation;
    let emblem_entries = state.stack.iter().filter(|e| e.source_id == EMBLEM).count();
    assert_eq!(
        emblem_entries, 1,
        "reach-guard, measured off the loaded 152-deep stack: the emblem has exactly one \
         live entry here, so this row's COUNT carries no claim about the per-SOURCE dedupe \
         — that is \
         `bounded_cycle_pin_slots_publishes_one_point_per_source_not_per_entry`'s job"
    );
    assert_eq!(
        engine_live_opponents(&state, P0),
        vec![P1, P2, P3],
        "reach-guard: three living opponents, so the per-iteration choice is REAL"
    );

    let expected_slot = DecisionSlot {
        source: YieldTarget::ThisObject {
            source_id: EMBLEM,
            incarnation: Some(emblem_incarnation),
            trigger_description: None,
        },
        point: ChoicePoint::AnnouncedTarget,
        index: 0,
    };
    let expected_legal = vec![
        TargetRef::Player(P1),
        TargetRef::Player(P2),
        TargetRef::Player(P3),
    ];

    // ── arm ⓐ: the shipped board, prompt UP ──
    assert!(
        matches!(state.waiting_for, WaitingFor::TriggerTargetSelection { .. }),
        "arm ⓐ precondition: the dump ships AT the prompt; got {:?}",
        state.waiting_for
    );
    let with_prompt = bounded_cycle_pin_slots(&state, P0);
    // The CR 115.2 TARGETS half — this row's subject. Filtered rather than counted whole
    // because the mint also admits shape (B) (may-only, no announcement choice), whose
    // points are asserted separately below; before shape (B) existed the two coincided.
    let targets: Vec<_> = with_prompt
        .iter()
        .filter(|p| matches!(p.kind, DecisionPointKind::Targets { .. }))
        .collect();
    assert_eq!(
        targets.len(),
        emblem_entries,
        "one TARGETS point per qualifying SOURCE; on this board that count coincides with \
         the emblem's single entry, which is asserted above rather than assumed"
    );
    for point in &targets {
        assert_eq!(
            point.slot, expected_slot,
            "the slot names the CR 114.2 command-zone emblem at its CR 400.7 incarnation"
        );
        assert_eq!(
            point.kind,
            DecisionPointKind::Targets {
                legal_targets: expected_legal.clone(),
                min_targets: 1,
                max_targets: 1,
                ordered: false,
            },
            "the legal set is `find_legal_targets`' native output (CR 115.2), not a \
             declaration echo"
        );
    }
    // ── the shape-(B) half, measured on the SAME board rather than asserted elsewhere ──
    // CR 603.5: three P0-controlled optional no-target triggers (sources 126, 208, 274)
    // publish a `MayChoice` gate each. 126 and 208 carry 34 stack entries apiece here, so
    // this count is ALSO the per-source dedupe on a real board: without it the shipped
    // dump would publish 69 may points, not 3.
    let may_sources: Vec<_> = with_prompt
        .iter()
        .filter(|p| p.kind == DecisionPointKind::MayChoice)
        .map(|p| match &p.slot.source {
            YieldTarget::ThisObject { source_id, .. } => *source_id,
            other => panic!("a mint slot names an object source; got {other:?}"),
        })
        .collect();
    assert_eq!(
        may_sources,
        vec![ObjectId(126), ObjectId(208), ObjectId(274)],
        "CR 603.5: the may-only (shape (B)) sources this board publishes, deduped per \
         SOURCE across their 34/34/1 stack entries"
    );
    for src in [ObjectId(126), ObjectId(208)] {
        assert!(
            state.stack.iter().filter(|e| e.source_id == src).count() > 1,
            "reach-guard: {src:?} must carry MORE than one entry, or the dedupe assertion \
             above is vacuous"
        );
    }

    // ── arm ⓑ: the SAME state at the real offer beat — one field reassigned ──
    state.waiting_for = WaitingFor::Priority { player: P0 };
    assert_eq!(
        bounded_cycle_pin_slots(&state, P0),
        with_prompt,
        "arm ⓑ: byte-for-byte the same slot set with NO prompt to read. Both production \
         call sites run at `WaitingFor::Priority`, so an implementation that reads \
         `target_slots[..].legal_targets` publishes nothing when it matters"
    );

    // ── ⓐ': a bystander proposer specifies none of these choices (CR 732.2a) ──
    assert!(
        state.stack.iter().all(|e| e.controller == P0),
        "measured: every dump-B stack entry is P0-controlled, so the controller filter is \
         probed from the PROPOSER side"
    );
    for bystander in [P1, P2, P3] {
        assert!(
            bounded_cycle_pin_slots(&state, bystander).is_empty(),
            "a bystander ({bystander:?}) controls none of these entries"
        );
    }

    // ── must-NOT-flip: the shipped Fixed(N) drive board publishes NOTHING ──
    let (shipped, _l0, _cleric) = reach_2p_optional_drain_offer();
    assert!(
        bounded_cycle_pin_slots(shipped.state(), P0).is_empty(),
        "the untargeted `each opponent loses 1 life` drain reifies no per-iteration player \
         choice — every shipped Fixed(N) drive must stay byte-identical"
    );
}

/// Phase 1b crown-safety row: a window that crowns must still crown after the pause-retention
/// exemption. Dump C is the population where that is measurable: it ships
/// with exactly ONE living opponent, which is the only shape `loop_check`'s
/// `nonfallers.len() == 1` crown gate admits.
///
/// ARM CHOICE (this is the anti-vacuity decision, stated explicitly): the fixture ships AT
/// `WaitingFor::LoopShortcut`, so loading it and asserting the saved payload would drive
/// **0 beats** and prove nothing — the assertion would read the offer the dump was saved
/// with. This row therefore DECLINES the saved offer first, forcing the detector to
/// RE-DERIVE the crown from live beats, and asserts a non-zero driven beat count before
/// asserting anything about the payload. `revive_decline` (reviving P1/P2 to three living
/// opponents) is FORBIDDEN here: at three living opponents the crown gate short-circuits
/// and there is no crown left to assert.
///
/// REVERT-PROBE: implement the withdrawn "count + clear_loop_detect_ring + Path-A
/// early-return" remedy ⇒ C's crown disappears as soon as a `TriggerTargetSelection`
/// enters the accumulation — this row is the measured reason that remedy stays withdrawn.
#[test]
fn dump_c_still_crowns_at_one_living_opponent_after_pause_retention() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/tenacity_exquisite_blood_4p.json.gz"
    ));
    let mut state = restore_dump(&json);

    assert!(
        state.loop_detection.samples(),
        "reach-guard: sampling mode required; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        engine_live_opponents(&state, P0),
        vec![PlayerId(3)],
        "reach-guard: dump C ships with EXACTLY ONE living opponent (P3) — the only \
         population the CR 732.2a crown gate admits"
    );
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for.clone() else {
        panic!(
            "reach-guard: dump C ships AT a saved offer; got {:?}",
            state.waiting_for
        )
    };
    assert_eq!(proposer, P0);

    // Discard the saved offer so the detector must re-derive it from live beats.
    apply(&mut state, proposer, GameAction::DeclineShortcut).expect("decline the saved offer");

    let pin = engine_live_opponents(&state, P0).first().copied();
    let mut beats = 0usize;
    for _ in 0..200 {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        if let Err(why) = dump_drive_one_beat(&mut state, pin) {
            panic!("drive stopped after {beats} beats: {why}");
        }
        beats += 1;
    }

    // ANTI-VACUITY CONTROL: a zero here means the row asserted the dump's saved offer
    // instead of a re-derived one, and both revert-probes above would be inert. The
    // exact count is reported (not just `> 0`) so a change in how far the detector has
    // to drive to re-derive the crown surfaces as a diff rather than passing silently.
    assert_eq!(
        beats, 4,
        "the `c decline` arm re-derives the crown after a measured 4 driven beats — a 0 here \
         would mean the row read the dump's saved offer back instead of re-deriving one, \
         which is what makes both revert-probes above live"
    );

    let WaitingFor::LoopShortcut {
        predicted_winner,
        certificate,
        schema,
        ..
    } = state.waiting_for.clone()
    else {
        panic!(
            "the crown must survive pause retention; after {beats} beats waiting_for was {:?}",
            state.waiting_for
        )
    };
    assert_eq!(predicted_winner, Some(P0), "the crown still names P0");
    assert_eq!(certificate.win_kind, Some(WinKind::LethalDamage));
    assert_eq!(
        certificate.unbounded,
        vec![ResourceAxis::Life(P0), ResourceAxis::Life(PlayerId(3))],
        "the re-derived certificate names the same two life axes as BASE"
    );
    assert!(
        schema.points.is_empty(),
        "a choice-free drain publishes no decision points"
    );
    // CR 119.3 + CR 704.5a + CR 104.2a: P3, the one living opponent, loses 1 per repetition from
    // 14, so the fourteenth crowns P0.
    assert_eq!(
        (schema.iteration_count, state.players[3].life),
        (IterationCount::Fixed(14), 14)
    );
}

/// A one-item confirmed period, for offers built by hand.
pub(crate) fn one_item_period() -> engine::game::period_confirm::ConfirmedPeriod {
    serde_json::from_value(serde_json::json!({ "items": [{
        "seat": 0,
        "action": { "type": "PassPriority" },
        "play": null,
        "mandatory_answer": false,
        "next_object_id": 1,
        "minted_since": 1,
        "cost_move": null,
    }] }))
    .expect("a one-item period deserializes")
}

/// Seam D (CR 732.2a): against a NON-EMPTY schema on an offer that published no declaration of
/// its own, a `template: None` declaration validates no pins. That is legitimate only when the
/// offer carries the confirmed period the take replays, so the guard's two halves differ only in
/// the offer's period, on ONE fixture and ONE schema:
///
/// * EMPTY period ⇒ fail-closed manual-play handback (Priority), APNAP never opens.
/// * NON-EMPTY period ⇒ APNAP opens — the reach-guard proving the guard is not a blanket
///   "reject every `template: None`".
///
/// REVERT-PROBE: delete the guarded `None` arm ⇒ the first half opens `RespondToShortcut` and
/// FAILS. Drop its period conjunct instead ⇒ the second half FAILS.
#[test]
fn template_none_against_a_pin_consuming_schema_falls_back_to_manual_play() {
    let source = YieldTarget::ThisObject {
        source_id: ObjectId(1),
        incarnation: None,
        trigger_description: None,
    };
    let schema = ShortcutDecisionSchema {
        suggested_declaration: None,
        leading_aims: Vec::new(),
        iteration_count: IterationCount::Fixed(1),
        measured_repetition_bound: None,
        deliverable_capacity: ShortcutDecisionSchema::default().deliverable_capacity,
        points: vec![DecisionPoint {
            slot: DecisionSlot::first(source, ChoicePoint::AnnouncedTarget),
            kind: DecisionPointKind::Targets {
                legal_targets: vec![TargetRef::Player(P1)],
                min_targets: 1,
                max_targets: 1,
                ordered: true,
            },
        }],
        convoke_tappable_count: 0,
    };

    let declare_with_period =
        |period: engine::game::period_confirm::ConfirmedPeriod| -> WaitingFor {
            let (mut runner, _kickoff) = setup_3p_draw(LoopDetectionMode::Interactive);
            runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
                proposer: P0,
                predicted_winner: Some(P0),
                certificate: synthetic_lethal_cert(),
                schema: schema.clone(),
                declaration: None,
                road: engine::analysis::loop_check::OfferRoad::Ring,
                counted_take: Default::default(),
                period,
            };
            runner
                .act(GameAction::DeclareShortcut {
                    count: IterationCount::Fixed(1),
                    template: None,
                })
                .expect(
                    "declare dispatch succeeds (a rejection is a manual fallback, not an error)",
                );
            runner.state().waiting_for.clone()
        };

    let without_period = declare_with_period(Default::default());
    assert!(
        matches!(without_period, WaitingFor::Priority { .. }),
        "CR 732.2a: a pin-consuming schema declared with NO template and NO confirmed period \
         must fail closed to manual play, not open APNAP; got {without_period:?}"
    );

    let with_period = declare_with_period(one_item_period());
    assert!(
        matches!(with_period, WaitingFor::RespondToShortcut { .. }),
        "reach-guard: an offer carrying its confirmed period keeps opening APNAP — the guard is \
         two-conjunct, not a blanket template-None rejection; got {with_period:?}"
    );
}

/// A loop-free board that carries the CR 732.2a ring ACROSS TURN BOUNDARIES and still
/// refuses to offer — the no-false-positive control for widening
/// [`WaitingFor::is_forced_cascade_window`] to the CR 703.1 turn-based actions.
///
/// CR 732.2a says a proposed shortcut "may even cross multiple turns", which is why the
/// class now retains across CR 502.3 untap / CR 508.1 declare attackers / CR 509.1
/// declare blockers / CR 514.1 cleanup discard. Retention is NECESSARY BUT NOT YET
/// SUFFICIENT for that: `loop_states_equal` still compares `turn_number`, so no
/// cross-turn pair certifies today — which is exactly what the ATTRIBUTION half below
/// measures. The risk that widening introduces is the
/// mirror image of the bug it fixes: a ring that now survives turn cycling might
/// accumulate on an ordinary board and certify a loop that isn't there.
///
/// FIXTURE (loop-free by construction, and hostile on purpose): P0 has an upkeep ticker
/// ("At the beginning of your upkeep, you gain 1 life"), a drain cleric (gain ⇒ each
/// opponent loses 1) and a "may draw" scribe (opponent loses life ⇒ optional draw). Each
/// of P0's upkeeps runs a FINITE 3-deep cascade — nothing re-triggers the ticker — yet the
/// per-turn shape is drain-like (P1 loses 1 every other turn), which is exactly the shape
/// a naive detector would mistake for a loop. The cascade ends at the scribe's CR 603.5
/// "may" pause, which leaves the stack already popped, so the sampler's clear arm never
/// fires on the tail resolution and the accumulated frames survive into the rest of the
/// turn. That is what makes cross-turn retention observable on a board with no loop at
/// all.
///
/// NON-VACUITY, both halves, measured (300 beats, 23 turns):
/// * POSITIVE — the ring really is retained across turn boundaries: at beat 58 the drive
///   sits at a `DeclareAttackers` window in turn 6 holding 4 frames whose OLDEST was
///   sampled in turn 4. Without the widening that frame cannot exist: BASE clears at
///   `DeclareAttackers`. So the widening is demonstrably LIVE on this board.
/// * DISCRIMINANT GUARD — `OptionalEffectChoice` is a PRE-EXISTING member of the class and
///   also occurs on this board (10 times). The retention witness is therefore required to
///   be one of the NEWLY exempt turn-based windows; an `OptionalEffectChoice` witness
///   would satisfy the row without the widening being consulted at all.
/// * NEGATIVE — no `LoopShortcut` / `RespondToShortcut` is ever raised, even after the ring
///   saturates at all 16 frames (measured: reached by turn 22, spanning ~9 turn boundaries).
/// * ATTRIBUTION — the decline is a MEASURED comparison failure, not an absent one. The
///   engine's own recurrence gate `loop_states_equal_modulo_resources` reports FALSE on the
///   oldest/newest retained pair, while reporting TRUE on the oldest against itself (the
///   positive control that the comparator is live on this data, trap 7). The monotone axis
///   is named and asserted: each turn's CR 504.1 draw strictly shrinks the library, so no
///   two retained frames can be the same position. Measured at the witness beat: P0's
///   library 60 → 59, P1's 59 → 58. (Honest scope: equalizing library and hand alone does
///   NOT flip the gate to true — turn number and life differ too. The library shrink is
///   asserted as a monotone non-recurrence witness, not as the sole cause.)
///
/// REVERT-PROBE (measured, not predicted): delete the CR 703.1 turn-based members from
/// `is_forced_cascade_window` and the POSITIVE half fails — the retention witness is never
/// found, because `apply_action` clears the ring on the very first `DeclareAttackers` of
/// each turn.
#[test]
fn drawgo_ring_spans_turns_but_never_offers() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(
        P0,
        "Test Upkeep Ticker",
        2,
        2,
        "At the beginning of your upkeep, you gain 1 life.",
    );
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(
        P0,
        "Test May Scribe",
        2,
        2,
        "Whenever an opponent loses life, you may draw a card.",
    );
    // CR 504.1: both players draw every turn, so the libraries must outlast the drive —
    // a deck-out would end the game and silently truncate every assertion below.
    let names: Vec<String> = (0..60).map(|i| format!("Filler {i}")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    scenario.with_library_top(P0, &refs);
    scenario.with_library_top(P1, &refs);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let mut state = runner.state().clone();

    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring, making every \
         assertion below vacuous; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the board must start with an EMPTY ring — every frame below is \
         accumulated by this drive"
    );

    // The cross-turn retention witness: (window name, live turn, oldest frame's turn,
    // ring before the answer, ring after it, oldest frame, newest frame).
    let mut witness: Option<(String, u32, u32, usize, usize, GameState, GameState)> = None;
    let mut offer_at: Option<(usize, String)> = None;
    let mut turns_seen: Vec<u32> = Vec::new();

    for beat in 0..300usize {
        if !turns_seen.contains(&state.turn_number) {
            turns_seen.push(state.turn_number);
        }
        let name = state.waiting_for.variant_name().to_string();
        if matches!(
            state.waiting_for,
            WaitingFor::LoopShortcut { .. } | WaitingFor::RespondToShortcut { .. }
        ) {
            offer_at = Some((beat, name));
            break;
        }
        // The witness must be a NEWLY exempt CR 703.1 turn-based window, never the
        // pre-existing `OptionalEffectChoice` member (see DISCRIMINANT GUARD above).
        let turn_based = matches!(
            state.waiting_for,
            WaitingFor::UntapChoice { .. }
                | WaitingFor::ChooseUntapSubset { .. }
                | WaitingFor::DeclareAttackers { .. }
                | WaitingFor::ExertChoice { .. }
                | WaitingFor::EnlistChoice { .. }
                | WaitingFor::DeclareBlockers { .. }
                | WaitingFor::DiscardToHandSize { .. }
        );
        let before = state.loop_detect_ring.len();
        let pair = (witness.is_none() && turn_based && before >= 4)
            .then(|| {
                // The witness reports on the basis-B turn conjunct, which reads the
                // CR 104.4b comparand half.
                let front = state.loop_detect_ring.front()?.normalized.clone();
                let back = state.loop_detect_ring.back()?.normalized.clone();
                (front.turn_number < state.turn_number).then_some((front, back))
            })
            .flatten();
        let live_turn = state.turn_number;
        if dump_drive_one_beat(&mut state, None).is_err() {
            break;
        }
        if let Some((front, back)) = pair {
            witness = Some((
                name,
                live_turn,
                front.turn_number,
                before,
                state.loop_detect_ring.len(),
                front.clone(),
                back.clone(),
            ));
        }
    }

    assert!(
        turns_seen.len() >= 3,
        "reach-guard: the drive must cross at least 2 full turn boundaries for a \
         cross-turn claim to mean anything; saw turns {turns_seen:?}"
    );

    let (window, live_turn, frame_turn, before, after, oldest, newest) = witness.expect(
        "POSITIVE HALF: no CR 703.1 turn-based window (CR 502.3 / CR 508.1 / CR 509.1 / \
             CR 514.1) was ever reached holding >= 4 frames whose oldest was sampled in an \
             EARLIER turn. That is precisely BASE behaviour — dropping the turn-based \
             members from `is_forced_cascade_window` reproduces this failure, because \
             `apply_action` then clears the ring at the first declare-attackers of every \
             turn. Without this witness the no-offer assertion below is vacuous.",
    );
    assert!(
        after >= before,
        "answering the forced turn-based window {window} must not discard the ring \
         (CR 703.1 + CR 117.3a: no player had priority there, so the answer is not a \
         deliberate break); ring went {before} -> {after}"
    );
    assert!(
        frame_turn < live_turn,
        "the retained frame must predate the live turn; frame turn {frame_turn}, live \
         turn {live_turn}"
    );

    assert!(
        offer_at.is_none(),
        "NO-FALSE-POSITIVE: this board has no loop — each upkeep runs a FINITE cascade and \
         nothing re-triggers the ticker — so no CR 732.2a shortcut may ever be offered, \
         however many frames the widened class lets the ring carry across turns. Got an \
         offer at {offer_at:?} (witness: {window} in turn {live_turn} held a turn-{frame_turn} frame)"
    );

    // ATTRIBUTION: the decline is a measured comparison FAILURE on a live comparator,
    // not an absent comparison.
    assert!(
        loop_states_equal_modulo_resources(&oldest, &oldest),
        "positive control (trap 7): the engine's recurrence gate must report TRUE on a \
         retained frame against itself, else the FALSE asserted next is an inert \
         instrument rather than a measured non-recurrence"
    );
    assert!(
        !loop_states_equal_modulo_resources(&oldest, &newest),
        "the turn-{frame_turn} and turn-{} frames must NOT compare recurrent — that \
         comparison failing is WHY no offer forms",
        newest.turn_number
    );
    for (i, (old_p, new_p)) in oldest.players.iter().zip(newest.players.iter()).enumerate() {
        assert!(
            new_p.library.len() < old_p.library.len(),
            "CR 504.1: every turn's draw strictly shrinks each library, which is the \
             monotone axis that makes two retained frames un-recurrable. P{i} went \
             {} -> {} across the retained window",
            old_p.library.len(),
            new_p.library.len()
        );
    }
}

// ===========================================================================
// CR 510.2 EVENT-KEYED loop-ring invalidation.
//
// `WaitingFor::AssignCombatDamage` / `AssignBlockerDamage` are excluded from
// `is_forced_cascade_window` because CR 510.2 deals the assigned damage with no
// intervening priority. That WINDOW-keyed exclusion is necessary but NOT
// sufficient: the window opens only when a damage DIVISION choice is required
// (`game::combat_damage`: "Auto-assign for unblocked, single blocker, or
// blocked-but-no-current-blockers"). An UNBLOCKED attacker moves a life total
// with NO window to exclude — and with the CR 703.1 turn-based members now in
// the class, `DeclareAttackers` / `DeclareBlockers` no longer clear the ring
// either, so the ring rides straight through the life change.
//
// The sufficient guard is `GameState::invalidate_loop_ring_on_unobserved_life_move`,
// called at the end of `apply_combat_damage` — the CR 510.2 batch itself.
// ===========================================================================

fn player_life(state: &GameState, p: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|pl| pl.id == p)
        .map(|pl| pl.life)
        .expect("seat exists")
}

/// `dump_drive_one_beat`, but it actually fights.
///
/// MEASURED, and the reason this helper exists: at a CR 508.1 / CR 509.1 declaration the
/// generic driver takes the FIRST legal action, and that is the EMPTY declaration —
/// 14 `DeclareAttackers` windows over 400 beats produced
/// `DeclareAttackers { attacks: [], bands: [] }` every time and ZERO combat damage. A row
/// about CR 510.2 driven by that policy is vacuous by construction. Here the largest
/// non-empty declaration wins, so the attack and the block both really happen; every
/// other window keeps the shared policy.
fn combat_drive_one_beat(state: &mut GameState) -> Result<Vec<GameEvent>, String> {
    if matches!(
        state.waiting_for,
        WaitingFor::DeclareAttackers { .. } | WaitingFor::DeclareBlockers { .. }
    ) {
        if let Some((who, actions)) = dump_beat_actor(state) {
            let biggest = actions
                .iter()
                .filter_map(|a| match a {
                    GameAction::DeclareAttackers { attacks, .. } => Some((attacks.len(), a)),
                    GameAction::DeclareBlockers { assignments } => Some((assignments.len(), a)),
                    _ => None,
                })
                .max_by_key(|(n, _)| *n)
                .filter(|(n, _)| *n > 0)
                .map(|(_, a)| a.clone());
            if let Some(action) = biggest {
                let actor = engine::game::turn_control::authorized_submitter_for_player(state, who);
                return apply(state, actor, action.clone())
                    .map(|r| r.events)
                    .map_err(|e| format!("apply err ({action:?}): {e:?}"));
            }
        }
    }
    dump_drive_one_beat(state, None)
}

/// The shared board for both CR 510.2 rows. P0 runs the same loop-FREE upkeep cascade
/// `drawgo_ring_spans_turns_but_never_offers` uses (ticker → drain cleric → "may" scribe),
/// which is what accumulates a CR 732.2a ring at all; the trio carries Defender so the
/// only creature that can attack is the dedicated 3/3, making the combat shape of each
/// row a deliberate fixture property rather than an artifact of which creature the driver
/// happened to declare.
///
/// `p1_wall` gives P1 a single 0/20 blocker. With it, the attack is BLOCKED and CR 510.2
/// moves no player's life (creature-only damage). Without it, the attacker is UNBLOCKED
/// and CR 510.2 moves P1's life with no assignment window — CR 510.1c's window needs 2+
/// blockers to divide damage among.
fn combat_ring_board(p1_wall: bool) -> GameState {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // Deep life totals on BOTH seats: the drive must outlast several turn cycles of
    // combat damage plus the cleric's drain, and a CR 704.5a death would end the game
    // and silently truncate every assertion below.
    scenario.with_life(P0, 400);
    scenario.with_life(P1, 400);
    scenario.add_creature_from_oracle(
        P0,
        "Test Upkeep Ticker",
        2,
        2,
        "Defender\nAt the beginning of your upkeep, you gain 1 life.",
    );
    scenario.add_creature_from_oracle(
        P0,
        "Test Drain Cleric",
        2,
        2,
        &format!("Defender\n{DRAIN_CLERIC}"),
    );
    scenario.add_creature_from_oracle(
        P0,
        "Test May Scribe",
        2,
        2,
        "Defender\nWhenever an opponent loses life, you may draw a card.",
    );
    scenario.add_creature(P0, "Test Lone Attacker", 3, 3);
    if p1_wall {
        // 0 power so the trade kills nothing and the block repeats every turn cycle;
        // toughness 20 so the 3/3 never kills it either. Defender is load-bearing, not
        // flavour: without it the wall attacks on P1's turn, is still TAPPED on P0's, and
        // cannot block — measured, the attack then went through unblocked and the row
        // silently became a duplicate of the unblocked one.
        scenario.add_creature_from_oracle(P1, "Test Wall", 0, 20, "Defender");
    }
    // CR 504.1: both players draw every turn, so the libraries must outlast the drive —
    // a deck-out would end the game and truncate the row.
    let names: Vec<String> = (0..60).map(|i| format!("Filler {i}")).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    scenario.with_library_top(P0, &refs);
    scenario.with_library_top(P1, &refs);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state().clone()
}

/// HIGH-1: the CR 510.2 damage EVENT clears the CR 732.2a ring even though no
/// `AssignCombatDamage` window ever opens.
///
/// FIXTURE: P0 attacks with one unblocked 3/3 into an empty board while the loop-free
/// upkeep cascade keeps the ring populated. CR 510.1c's assignment window needs a
/// division choice, so on this board it never opens at all — which is exactly the hole
/// the window-keyed exclusion leaves and the reason the fence has to be event-keyed.
///
/// ASSERTIONS, in the order they discharge each other:
/// 1. REACH-GUARD — the drive reaches a CR 510.2 beat that damaged P1 while the ring
///    already carried >= 2 frames. Without that, "the ring is empty afterwards" is
///    unobservable: an already-empty ring would satisfy it.
/// 2. DISCRIMINANT GUARD — no `AssignCombatDamage` / `AssignBlockerDamage` window is
///    observed anywhere in the drive. This row's whole point is that the damage lands
///    with NO window; a fixture drift that introduces a division choice would make the
///    window-keyed exclusion sufficient and the row vacuous, so it fails loudly instead.
/// 3. LIFE-MOVE WITNESS — P1's life strictly decreased across that beat, so assertion 4
///    is about a real CR 119.3 / CR 120.3a life movement.
/// 4. THE DELIVERABLE — the ring is empty immediately after the beat.
///
/// REVERT-PROBE (measured, recorded in the handoff report): delete the
/// `invalidate_loop_ring_on_unobserved_life_move` call from `apply_combat_damage` ⇒
/// assertion 4 FAILS while 1–3 still PASS, which is what proves 4 is the discriminator.
#[test]
fn unblocked_attacker_damage_clears_the_loop_ring_with_no_window() {
    let mut state = combat_ring_board(false);

    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: every frame below is accumulated by this drive, not preloaded"
    );

    let mut assignment_window: Option<String> = None;
    // (beat, ring before, ring after, P1 life before, P1 life after, combat damage)
    let mut witness: Option<(usize, usize, usize, i32, i32, u32)> = None;
    let mut max_ring = 0usize;
    let mut damage_beats = 0usize;

    for beat in 0..400usize {
        if matches!(
            state.waiting_for,
            WaitingFor::AssignCombatDamage { .. } | WaitingFor::AssignBlockerDamage { .. }
        ) {
            assignment_window.get_or_insert_with(|| state.waiting_for.variant_name().to_string());
        }
        let before = state.loop_detect_ring.len();
        max_ring = max_ring.max(before);
        let life_before = player_life(&state, P1);
        let Ok(events) = combat_drive_one_beat(&mut state) else {
            break;
        };
        let dealt: u32 = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CombatDamageDealtToPlayer {
                    player_id,
                    total_damage,
                    ..
                } if *player_id == P1 => Some(*total_damage),
                _ => None,
            })
            .sum();
        if dealt > 0 {
            damage_beats += 1;
            if witness.is_none() && before >= 2 {
                witness = Some((
                    beat,
                    before,
                    state.loop_detect_ring.len(),
                    life_before,
                    player_life(&state, P1),
                    dealt,
                ));
                // The evidence is complete; the rest of the drive only costs time. The
                // window guard has already covered every beat up to here, and the loop
                // below re-checks the settled window once more.
                break;
            }
        }
    }
    if matches!(
        state.waiting_for,
        WaitingFor::AssignCombatDamage { .. } | WaitingFor::AssignBlockerDamage { .. }
    ) {
        assignment_window.get_or_insert_with(|| state.waiting_for.variant_name().to_string());
    }

    let (beat, before, after, life_before, life_after, dealt) = witness.unwrap_or_else(|| {
        panic!(
            "reach-guard: no CR 510.2 beat dealt combat damage to P1 while the ring held \
             >= 2 frames, so the clear below would be unobservable. Combat-damage beats \
             seen: {damage_beats}; max ring: {max_ring}. ATTRIBUTION: if \
             `is_forced_cascade_window` no longer exempts \
             `DeclareAttackers`/`DeclareBlockers`, the ring is wiped before combat and \
             this guard reds first — read that as a class-membership regression, not as a \
             failure of the combat-damage fence below"
        )
    });

    assert!(
        assignment_window.is_none(),
        "DISCRIMINANT GUARD: this row exists because an UNBLOCKED attacker deals CR 510.2 \
         damage with NO window — CR 510.1c's assignment window opens only for a division \
         choice. A {assignment_window:?} window means the fixture drifted into the case \
         the window-keyed exclusion already covers, making the row vacuous."
    );
    assert!(
        life_after < life_before,
        "LIFE-MOVE WITNESS: CR 120.3a — {dealt} combat damage to P1 must have reduced its \
         life; got {life_before} -> {life_after} at beat {beat}"
    );
    assert!(
        before >= 2,
        "reach-guard: the ring must carry >= 2 frames INTO the damage beat; got {before}"
    );
    assert_eq!(
        after, 0,
        "CR 510.2 + CR 704.5a: the damage batch moved a life total with no intervening \
         priority, so the ring accumulated before it may not be compared across it — it \
         must be EMPTY after the beat. Ring went {before} -> {after} at beat {beat} \
         (P1 {life_before} -> {life_after}). Deleting the \
         `invalidate_loop_ring_on_unobserved_life_move` call from `apply_combat_damage` \
         reproduces this failure: with `DeclareAttackers` / `DeclareBlockers` in the \
         forced-cascade class and no assignment window ever opening, nothing else clears \
         here."
    );
}

/// The matched negative: CR 510.2 damage that moves NO player's life leaves the ring
/// alone. Same board, but P1 fields a single 0/20 wall, so the 3/3 is blocked and the
/// whole batch is creature-to-creature.
///
/// This pins the `p.life != before` predicate as load-bearing. Replacing it with an
/// unconditional `clear()` — "clear on every combat damage" — still passes the row above
/// but flips this one to FAIL, and would be a needless retention regression on every
/// board where creatures merely trade.
///
/// NON-VACUITY: the witness requires a beat that BOTH dealt combat damage to a creature
/// (`DamageDealt { target: Object, is_combat: true }`) AND left every player's life
/// unchanged, with the ring already carrying >= 2 frames. A beat where no combat happened
/// cannot satisfy it, so the row cannot pass by the attack never occurring. The single
/// blocker also keeps CR 510.1c's division window shut, matching the row above.
#[test]
fn creature_only_combat_damage_leaves_the_loop_ring_intact() {
    let mut state = combat_ring_board(true);

    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring; got {:?}",
        state.loop_detection
    );

    // (beat, ring before, ring after, creature damage dealt)
    let mut witness: Option<(usize, usize, usize, u32)> = None;
    let mut max_ring = 0usize;
    let mut creature_damage_beats = 0usize;

    for beat in 0..400usize {
        let before = state.loop_detect_ring.len();
        max_ring = max_ring.max(before);
        let lives_before: Vec<i32> = state.players.iter().map(|p| p.life).collect();
        let Ok(events) = combat_drive_one_beat(&mut state) else {
            break;
        };
        let to_creatures: u32 = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::DamageDealt {
                    target: TargetRef::Object(_),
                    amount,
                    is_combat: true,
                    ..
                } => Some(*amount),
                _ => None,
            })
            .sum();
        let lives_after: Vec<i32> = state.players.iter().map(|p| p.life).collect();
        if to_creatures > 0 && lives_after == lives_before {
            creature_damage_beats += 1;
            if witness.is_none() && before >= 2 {
                witness = Some((beat, before, state.loop_detect_ring.len(), to_creatures));
                break;
            }
        }
    }

    let (beat, before, after, dealt) = witness.unwrap_or_else(|| {
        panic!(
            "reach-guard: no beat dealt CR 510.2 damage to a creature with every player's \
             life unchanged while the ring held >= 2 frames. Creature-damage beats seen: \
             {creature_damage_beats}; max ring: {max_ring}. ATTRIBUTION: if \
             `is_forced_cascade_window` no longer exempts \
             `DeclareAttackers`/`DeclareBlockers`, the ring is wiped before combat and \
             this guard reds first — read that as a class-membership regression, not as a \
             failure of the combat-damage fence below"
        )
    });

    assert!(
        after >= before,
        "CR 119.3: no player's life moved in this CR 510.2 batch ({dealt} damage, all of it \
         to creatures), so there is nothing for the loop-ring prohibition to fence and the \
         accumulated ring must SURVIVE. Ring went {before} -> {after} at beat {beat}. \
         Replacing the `p.life != before` predicate in \
         `invalidate_loop_ring_on_unobserved_life_move` with an unconditional `clear()` \
         reproduces this failure."
    );
}

// ===========================================================================
// X1-1 — CR 117.1b on a REAL 4-player dump.
// ===========================================================================

/// Sprout Swarm in P0's hand in the dump-A capture.
const X1_SPROUT: ObjectId = ObjectId(64);
/// An untapped P0 fodder Saproling to convoke for the {G}.
const X1_FODDER: ObjectId = ObjectId(421);

/// X1-1: on the real 4-player Witherbloom / Sprout Swarm / Lumaret capture, P0's Saproling loop
/// offers beside three opponents' class-reading utility lands and P0's own Jadar.
#[test]
fn witherbloom_lumaret_4p_offers_with_opponent_utility_lands() {
    use engine::types::game_state::LoopDetectionMode;
    use engine::types::zones::Zone;

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/witherbloom_sprout_lumaret_4p.json.gz"
    )));
    state.loop_detection = LoopDetectionMode::On;

    // ── fixture preconditions (hold in BOTH revert modes ⇒ the offer is non-vacuous) ──
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "fixture precondition: ordinary P0 priority pre-cast, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state
            .objects
            .get(&X1_SPROUT)
            .map(|o| (o.name.as_str(), o.zone)),
        Some(("Sprout Swarm", Zone::Hand)),
        "fixture precondition: Sprout Swarm is in P0's hand"
    );
    let fodder = state.objects.get(&X1_FODDER).expect("fodder present");
    assert!(
        fodder.name == "Saproling" && fodder.controller == P0 && !fodder.tapped,
        "fixture precondition: an untapped P0 Saproling to convoke"
    );
    // The X1 class is really present: opponents control battlefield permanents.
    let foreign_permanents = state
        .battlefield
        .iter()
        .filter(|id| state.objects.get(id).is_some_and(|o| o.controller != P0))
        .count();
    assert!(
        foreign_permanents >= 3,
        "fixture precondition: the dump carries opponent-controlled permanents (the X1 \
         class); got {foreign_permanents}"
    );

    let jadar = state
        .objects
        .get(&engine::types::identifiers::ObjectId(75))
        .expect("fixture precondition: object 75 is present");
    assert_eq!(
        (jadar.name.as_str(), jadar.zone, jadar.controller),
        ("Jadar, Ghoulcaller of Nephalia", Zone::Battlefield, P0),
        "fixture precondition: the driver-side observer this row names"
    );
    assert_eq!(
        jadar.trigger_definitions.len(),
        1,
        "fixture precondition: Jadar carries exactly one trigger definition; got {}",
        jadar.trigger_definitions.len()
    );

    let outcome = GameRunner::from_state(state)
        .cast(X1_SPROUT)
        .accept_optional()
        .convoke_with(&[X1_FODDER])
        .commit()
        .resolve();

    // ── reach-guard: the cast really resolved and grew the class ──
    assert_eq!(
        outcome.zone_of(X1_SPROUT),
        Zone::Hand,
        "reach-guard: Buyback returned Sprout Swarm to P0's hand"
    );

    // ── DISCRIMINATOR: the CR 732.2a offer surfaces ──
    match outcome.final_waiting_for() {
        WaitingFor::LoopShortcut {
            proposer,
            predicted_winner,
            certificate,
            ..
        } => {
            assert_eq!(*proposer, P0, "the driver proposes");
            assert_eq!(
                *predicted_winner, None,
                "an Advantage offer has no predicted winner"
            );
            assert_eq!(
                certificate.win_kind,
                Some(WinKind::Advantage),
                "CR 732.2a: this is a beneficial (advantage) loop, not a mandatory win"
            );
            assert!(
                certificate.unbounded.contains(&ResourceAxis::TokensCreated),
                "the unbounded axes must include TokensCreated, got {:?}",
                certificate.unbounded
            );
        }
        other => panic!(
            "X1-1: the opponents' utility-land abilities must not suppress the offer; got {other:?}"
        ),
    }
}

// ===========================================================================
// K4 — CR 608.2i + CR 608.2j ledger observers one entry-filter noun apart.
// Every fixture carries the harness's shared `"Flying, trample\n"` keyword prefix, so
// subject and control differ ONLY in the ledger clause.
// ===========================================================================

/// FIXTURE C (PRIMARY) — measured `mode=DamageDone`, `phase=null`, `damage_kind=Any`,
/// `constraint=null`.
const LEDGER_ARTIFACT_FILTER_ORACLE: &str = "Flying, trample\nWhenever this creature deals damage to a player, draw a card if you had two or more artifacts enter the battlefield under your control this turn.";

/// FIXTURE D (PRIMARY) — fixture C with one Oracle noun changed. Measured: the two
/// serialized trigger definitions are 984 bytes each and differ at exactly TWO token
/// positions — `filter.type_filters[0]` and the humanized `description` string — both
/// projections of that ONE noun, and `description` is a display string no scan predicate
/// reads.
const LEDGER_CREATURE_FILTER_ORACLE: &str = "Flying, trample\nWhenever this creature deals damage to a player, draw a card if you had two or more creatures enter the battlefield under your control this turn.";

/// FIXTURE A (CORROBORATING) — a DIFFERENT `TriggerMode`. Measured `mode=Phase`,
/// `phase=PreCombatMain`, `damage_kind=Any`, `constraint=OnlyDuringYourTurn`.
const PHASE_LEDGER_ARTIFACT_FILTER_ORACLE: &str = "Flying, trample\nAt the beginning of your precombat main phase, draw a card if you had two or more artifacts enter the battlefield under your control this turn.";

/// FIXTURE B (CORROBORATING) — fixture A one Oracle noun apart.
const PHASE_LEDGER_CREATURE_FILTER_ORACLE: &str = "Flying, trample\nAt the beginning of your precombat main phase, draw a card if you had two or more creatures enter the battlefield under your control this turn.";

/// K4-N1: a damage ledger observer whose entry filter cannot count the Saproling fodder does not
/// suppress the offer (CR 608.2j).
#[test]
fn noncombat_damage_ledger_observer_whose_filter_excludes_the_class_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    // (2) ANTI-VACUITY CONTROL, granted in BOTH builds.
    let (control_runner, _) = object_growth_with_bystander(PLAIN_DRAW_TRIGGER_ORACLE);
    assert!(
        matches!(
            control_runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "(2) control: a plain draw-trigger bystander must not suppress the offer"
    );

    let (runner, bystander) = object_growth_with_bystander(LEDGER_ARTIFACT_FILTER_ORACLE);

    // (3) reach-guards.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "(3) reach-guard: exactly one trigger definition carries the ledger read"
    );

    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "(1) unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "(1) CR 608.2j: a `Typed{{Artifact}}` entry filter cannot count a Saproling \
             creature token, so the observer must not suppress the offer; got {other:?}"
        ),
    }
}

/// The object-growth loop offers beside a noncombat-damage ledger observer whose filter counts the
/// Saprolings; whether it reads the growing class is the loop's own replay to show (CR 732.2a).
#[test]
fn noncombat_damage_ledger_observer_whose_filter_matches_the_class_still_suppresses_offer() {
    offers_beside_observer(
        Phase::PreCombatMain,
        P0,
        LEDGER_CREATURE_FILTER_ORACLE,
        (1, 0),
    );
}

/// K4-N4a: the same through a phase trigger.
#[test]
fn phase_reachable_ledger_observer_whose_filter_excludes_the_class_does_not_suppress_offer() {
    use engine::types::zones::Zone;

    let (runner, bystander) = object_growth_with_bystander(PHASE_LEDGER_ARTIFACT_FILTER_ORACLE);

    // (3) reach-guards, ALL BEFORE the offer match. This is a POSITIVE row: a parse failure
    // yields no observer at all and the offer would form trivially, so these guards are what
    // make that vacuity mode loud.
    let obj = &runner.state().objects[&bystander];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach-guard: the observer is on the battlefield"
    );
    assert_eq!(
        obj.trigger_definitions.len(),
        1,
        "reach-guard: the ledger observer must have PARSED — a misparse leaves zero trigger \
         defs and the offer below forms for the wrong reason; got {}",
        obj.trigger_definitions.len()
    );
    assert!(
        obj.abilities.is_empty(),
        "reach-guard: this row's claim is about ONE named TRIGGER surface; the bystander \
         also carries {} ability def(s) {:?}",
        obj.abilities.len(),
        obj.abilities.iter().map(|a| a.kind).collect::<Vec<_>>(),
    );

    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { certificate, .. } => assert!(
            certificate.unbounded.contains(&ResourceAxis::TokensCreated),
            "K4-N4a: unbounded axis must be TokensCreated, got {:?}",
            certificate.unbounded
        ),
        other => panic!(
            "K4-N4a CR 608.2j: a phase-REACHABLE observer whose entry filter excludes the \
             fodder must not suppress the offer; got {other:?}"
        ),
    }
}

/// The object-growth loop offers beside a phase-reachable ledger observer whose filter counts the
/// Saprolings; whether it reads the growing class is the loop's own replay to show (CR 732.2a).
#[test]
fn phase_reachable_ledger_observer_whose_filter_matches_the_class_still_suppresses_offer() {
    offers_beside_observer(
        Phase::PreCombatMain,
        P0,
        PHASE_LEDGER_CREATURE_FILTER_ORACLE,
        (1, 0),
    );
}

// ===========================================================================
// R6a — the ∞ badge stays up while a collapse is merely SCHEDULED (the engine defers APPLYING
// an accepted shortcut's growth to the CR 500.5 boundary, while advancing to the proposal's
// ending point per CR 732.2c), and CR 732.2c bounds the boundary prompt by the accepted count.
// ===========================================================================

/// Sprout Swarm in P0's hand in the `witherbloom_sprout_lumaret_simple_4p` capture.
const R6A_SPROUT: ObjectId = ObjectId(405);
/// The one untapped P0 Saproling in that capture — the {G} convoke fodder.
const R6A_FODDER: ObjectId = ObjectId(1412);

/// Load the simple 4p Witherbloom/Sprout capture and drive one real buyback+convoke
/// recast through the cast pipeline, returning the state AT the CR 732.2a offer.
fn r6a_offer_state() -> GameState {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/witherbloom_sprout_lumaret_simple_4p.json.gz"
    )));
    state.loop_detection = LoopDetectionMode::On;
    let outcome = GameRunner::from_state(state)
        .cast(R6A_SPROUT)
        .accept_optional()
        .convoke_with(&[R6A_FODDER])
        .commit()
        .resolve();
    outcome.state().clone()
}

/// The real 4p Sprout board whose take batches: its accept stashes a `Tokens` collapse for the
/// CR 500.5 boundary, where the Witherbloom board above now performs at the take.
fn batched_offer_state() -> GameState {
    crate::combo_infinite_pile::offer_state()
}

/// Proposer declares `Fixed(n)`; every living opponent accepts (APNAP).
fn r6a_declare_and_accept_all(state: &mut GameState, proposer: PlayerId, n: u32) {
    apply(
        state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        },
    )
    .expect("the proposer declares the object-growth shortcut");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
}

/// Pass priority through the real production path until the CR 500.5 step/phase
/// boundary surfaces a non-`Priority` prompt (the `LoopCollapse` pay-amount) or the
/// phase advances with no prompt. Bounded so a wedge fails loudly.
fn r6a_drive_to_boundary(state: &mut GameState) {
    let start_phase = state.phase;
    for _ in 0..64 {
        let WaitingFor::Priority { player } = state.waiting_for.clone() else {
            return;
        };
        apply(state, player, GameAction::PassPriority)
            .expect("pass priority toward the next phase boundary");
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || state.phase != start_phase {
            return;
        }
    }
    panic!("r6a_drive_to_boundary: no phase boundary within 64 passes");
}

/// R6a-1 (PRIMARY), INVERTED to option (B). Accepting the batched Sprout loop writes
/// `unbounded_resources = {P0: [TokensCreated]}` plus a non-empty ∞ pile, and
/// registers a finite collapse. The COUNT is fixed at accept (`pending_materialization_count`,
/// which bounds the boundary prompt per CR 732.2c); what this engine defers is APPLYING it, until
/// the CR 500.5 boundary (`game::turns`), while the game advances to the proposal's ending point
/// (CR 732.2c; `types::game_state`'s `scheduled_collapse_axes` doc has the full reading). This test
/// pins what the engine DOES during that window: the marks are still live, so the
/// projection KEEPS every `∞` surface. CR 732.2c bounds the collapse; it never licensed hiding a
/// mark the store still carries, which is what the BASE gate used it for.
///
/// NEVER HIDE — and never filter the store either. The store must still carry the mark
/// (the boundary reads it until it applies the growth), so this row asserts store AND wire.
///
/// NON-VACUITY: every wire assertion here is a NON-emptiness paired with the store's own
/// non-emptiness at (1), so a projection that returned nothing fails immediately.
///
/// ASSERTION ORDER inside the viewer loop is PILE FIRST, then rows: RP-1 (pile guard) and
/// RP-1d (row guard) each panic on their own line, so whichever comes first hides the other.
/// Pile-first buys RP-1d an in-test rows→pile control; the rows control RP-1 loses here is
/// supplied out-of-loop by `unregistered_axis_still_renders_its_infinity_badge`, whose
/// pre-clear rows assertion is green under RP-1 and red under RP-1d.
///
/// REVERT-PROBES (RUN):
/// ⓐ RP-1 — restore `if collapse_scheduled(controller, &TokensCreated) { continue; }` in
///    `derive_views`' pile loop ⇒ the PILE assertion below FAILS; the rows assertion is
///    unreached here and is controlled out-of-loop (above).
/// ⓑ RP-1d — restore `if collapse_scheduled(controller, &axis) { continue; }` in the resource
///    row loop ⇒ the ROWS assertion below FAILS while the PILE assertion above it passes.
#[test]
fn scheduled_collapse_still_renders_the_unbounded_badge() {
    let mut state = batched_offer_state();

    // (0) reach-guard: the real cast reached the CR 732.2a offer.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the buyback+convoke recast must surface P0's offer, got {:?}",
        state.waiting_for
    );

    // BASELINE, captured BEFORE the accept so the unmaterialized claim below is falsifiable.
    let saprolings = |s: &GameState| {
        s.battlefield
            .iter()
            .filter(|id| {
                s.objects
                    .get(id)
                    .is_some_and(|o| o.controller == P0 && o.name == "Saproling")
            })
            .count()
    };
    let saprolings_before = saprolings(&state);

    r6a_declare_and_accept_all(&mut state, P0, 200);

    // (1) POSITIVE CONTROL — the accept really marked the ∞ axes in the STORE. Without
    // this the emptiness in (3) would be vacuous.
    let marked = state
        .unbounded_resources
        .get(&P0)
        .expect("accept must mark P0's ∞ axes in the store")
        .clone();
    assert!(
        marked.contains(&ResourceAxis::TokensCreated),
        "the accept marks TokensCreated ∞, got {marked:?}"
    );
    assert!(
        !state.unbounded_loop_pile.is_empty(),
        "the object-growth accept writes a non-empty ∞ pile (store still populated)"
    );
    assert_eq!(
        state.pending_unbounded_materialization.len(),
        1,
        "exactly one controller has a scheduled collapse"
    );
    // The growth is UNMATERIALIZED: the accepted count has not been applied, so P0's Saprolings
    // are EXACTLY what they were before the accept; a premature materialization of the accepted
    // 200 mints moves this number.
    assert_eq!(
        saprolings(&state),
        saprolings_before,
        "the ∞-badged token axis must be UNMATERIALIZED at this point"
    );

    // (2) FAIL-CLOSED CONTROL, in the SAME state: every ∞ axis the accept scheduled is
    // covered by the shared authority. An axis it does not name keeps its badge (R6a-2/-3).
    let scheduled = state.scheduled_collapse_axes(
        state
            .pending_unbounded_materialization
            .get(&P0)
            .expect("stash present"),
    );
    assert!(
        marked.iter().all(|a| scheduled.contains(a)),
        "every marked axis on this board is scheduled; marked={marked:?} scheduled={scheduled:?}"
    );

    // (3) DISCRIMINATOR — on the WIRE, for EVERY viewer (and the spectator view), the ∞ pile and
    // both ∞ rows still project. No ∞ surface consults the collapse schedule, so the HUD can never
    // show a card group's ∞ while hiding its resource badge. The pile's own positive row is
    // `combo_infinite_pile::real_4p_object_growth_accept_writes_infinite_pile`, so a regression on
    // ONE surface stays visible even though this row covers pile + rows at once.
    for viewer in [None, Some(P0), Some(P1), Some(P2), Some(PlayerId(3))] {
        let views = engine::game::derived_views::derive_views(&state, viewer);
        assert!(
            !views.unbounded_pile.is_empty(),
            "the scheduled collapse still projects the ∞ pile (viewer {viewer:?})"
        );
        let axes: Vec<ResourceAxis> = views.unbounded_resources.iter().map(|r| r.axis).collect();
        assert!(
            axes.contains(&ResourceAxis::TokensCreated),
            "...and the ∞ row beside it (viewer {viewer:?}), got {axes:?}"
        );
    }

    // (3b) A TRIPWIRE, NOT A SECOND PRODUCER. Multiplayer broadcasts (`phase-server`) and the
    // WASM `wrap_filtered` getter go through `derive_filtered_views`, which CALLS
    // `derive_views(filtered_state, viewer)` and then overrides only
    // `unique_authorized_submitter` and `blocker_assignment_pairs`. It WRAPS; it does not
    // bypass. So `derive_views` alone decides what the broadcast path shows — there is no other
    // producer of these three fields, and this row costs zero production code.
    //
    // What it DOES guard is the INPUT: `filter_state_for_viewer` is a clone-and-redact with
    // ZERO `unbounded` references today, so a filtered viewer sees the same ∞ surfaces the
    // hot-seat viewer does. If a future redaction ever dropped the ∞ stores from the filtered
    // clone, the broadcast path alone would go dark — remote players would lose the ∞ pile and
    // rows while the local viewer kept them. That asymmetry is the regression this row catches.
    // EXACT MEMBERSHIP, not non-emptiness. A non-empty check passes a PARTIAL projection that
    // drops one of the two axes or loses pile members, which is precisely the regression described
    // above. These rows pin the SET and the FULL membership, both compared against the store so the
    // expectation cannot drift away from what the accept actually wrote.
    let expected_axes: std::collections::BTreeSet<ResourceAxis> = marked.iter().copied().collect();
    // CR 110.1: the projection emits only pile members still ON THE BATTLEFIELD, so the oracle
    // must model that filter. A legitimately stale STORED id (the store is deliberately
    // unfiltered) is the projection being RIGHT, not a regression — without this filter the
    // equality below would indict the correct behaviour. On this fixture every stored member is
    // still on the battlefield here, so the filter is a no-op TODAY: it is latent correctness,
    // and `stale_pile_member_is_omitted_from_the_wire_but_kept_in_the_store` below is the case
    // that actually makes the stale/live distinction bite.
    let expected_pile: std::collections::BTreeSet<ObjectId> = state
        .unbounded_loop_pile
        .values()
        .flat_map(|ids| ids.iter().copied())
        .filter(|id| state.battlefield.contains(id))
        .collect();
    assert!(
        !expected_axes.is_empty() && !expected_pile.is_empty(),
        "control: the expectations themselves must be non-trivial, got {expected_axes:?} / \
         {} pile members",
        expected_pile.len()
    );
    for viewer in [P0, P1, P2, PlayerId(3)] {
        let filtered = engine::game::visibility::filter_state_for_viewer(&state, viewer);
        let views =
            engine::game::derived_views::derive_filtered_views(&state, &filtered, Some(viewer));
        let got_axes: std::collections::BTreeSet<ResourceAxis> =
            views.unbounded_resources.iter().map(|r| r.axis).collect();
        assert_eq!(
            got_axes, expected_axes,
            "the viewer-FILTERED broadcast path must project EVERY marked ∞ axis, not merely some \
             (viewer {viewer:?})"
        );
        let got_pile: std::collections::BTreeSet<ObjectId> =
            views.unbounded_pile.iter().copied().collect();
        assert_eq!(
            got_pile, expected_pile,
            "the viewer-FILTERED broadcast path must project the FULL ∞ pile membership \
             (viewer {viewer:?})"
        );

        // NOTE ON WHAT COVERS "flagging must not FILTER rows": the `got_axes == expected_axes`
        // assertion above is exact-set equality against every marked axis, so it already fails if
        // the scheduled flag ever suppressed a row. A separate "a TokensCreated row exists" pin
        // used to sit here and was strictly subsumed by it — no mutation could red the pin without
        // first reding the equality — so it is gone rather than left as decoration. (Rows CAN be
        // dropped, by `object_growth_backing`, for a TOKEN axis whose whole registered pile left
        // the battlefield; this fixture keeps its backing intact, pinned by `expected_pile`.)
        //
        // R2 — the SCHEDULE survives the viewer-filtered broadcast path. Read off
        // `unbounded_families`, the channel that replaced the per-row `scheduled` flag; what
        // matters here is that a scheduled family reaches every viewer.
        let scheduled_families: Vec<UnboundedFamily> = views
            .unbounded_families
            .iter()
            .filter(|f| matches!(f.state, FamilyCollapseState::Scheduled { .. }))
            .map(|f| f.family)
            .collect();
        assert!(
            scheduled_families.contains(&UnboundedFamily::Tokens),
            "R2/filtered: the filtered broadcast path reports the scheduled family (viewer \
             {viewer:?}), got {:?}",
            views.unbounded_families
        );
        // R1 — the channel survives serialize→deserialize, and a POPULATED channel is EMITTED.
        // `unbounded_families` is `skip_serializing_if = "Vec::is_empty"`, so the emission half is
        // what catches a state that is computed correctly and then silently dropped on the wire.
        let json = serde_json::to_string(&views).expect("serialize");
        let back: engine::game::derived_views::DerivedViews =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            back.unbounded_families, views.unbounded_families,
            "R1/roundtrip: the family collapse states survive serialize→deserialize (viewer \
             {viewer:?})"
        );
        assert!(
            json.contains("\"unbounded_families\"") && json.contains("\"Scheduled\""),
            "R1/emitted: a populated family channel is EMITTED, not skipped (viewer {viewer:?})"
        );
    }

    // (4) THE STORE IS UNTOUCHED — the projection read, it did not mutate.
    assert_eq!(
        state.unbounded_resources.get(&P0),
        Some(&marked),
        "the ∞ store must survive the projection (the boundary still reads it)"
    );
    assert!(
        !state.unbounded_loop_pile.is_empty(),
        "the ∞ pile must survive the projection too"
    );
}

/// MED-2 (CR 732.2a + CR 110.1): a pile member that has LEFT the battlefield is omitted from
/// the WIRE while the STORE keeps it. Sibling of the oracle filter added to
/// `scheduled_collapse_still_renders_the_unbounded_badge` above, which is a no-op on that
/// fixture (nothing is stale there) — this test is what makes the distinction bite.
///
/// The store/wire split is the whole contract: `unbounded_loop_pile` must stay unfiltered
/// because the boundary collapse reads it,
/// so the liveness filter has to live in the projection.
///
/// MUTATIONS (RUN, measured over the 164-test loop/∞ blast radius):
/// - Delete `if state.battlefield.contains(id)` from `derive_views`' pile loop ⇒ row (1) reds
///   here ("the wire omits the stale member (viewer None)") and NOTHING else moves — 1 failed
///   / 163 passed. That zero collateral is the finding, not a footnote: before this test, the
///   pile loop's liveness filter had no runnable guard at all. In particular the oracle filter
///   added to `scheduled_collapse_still_renders_the_unbounded_badge` stays GREEN under this
///   mutation, because no member is stale on that fixture — the filter there is latent
///   correctness, and THIS test is what makes the distinction bite.
/// - "Fix" it by pruning the STORE instead of the wire (drop the departing id from
///   `unbounded_loop_pile` in `zones::apply_zone_exit_cleanup`) ⇒ rows (1), (2) and (4) go
///   GREEN and only row (3) reds. Row (3) is the discriminator against that wrong fix.
#[test]
fn stale_pile_member_is_omitted_from_the_wire_but_kept_in_the_store() {
    use engine::game::zones::move_to_zone;
    use engine::types::zones::Zone;
    use std::collections::BTreeSet;

    let mut state = batched_offer_state();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        state.waiting_for
    );
    r6a_declare_and_accept_all(&mut state, P0, 200);

    let stored: BTreeSet<ObjectId> = state
        .unbounded_loop_pile
        .get(&P0)
        .expect("the object-growth accept registers a ∞ pile")
        .clone();
    assert!(
        stored.len() >= 2,
        "reach-guard: this rig's pile has >= 2 members, so removing ONE leaves a non-empty \
         wire — the case is about a STALE member, not about the whole backing set dying. The \
         whole-set case is `accepted_object_growth_row_survives_losing_its_entire_pile`, which \
         asserts the row SURVIVES it, because that rig's collapse has been accepted (CR 732.2c); \
         got {}",
        stored.len()
    );
    assert!(
        stored.iter().all(|id| state.battlefield.contains(id)),
        "reach-guard: BEFORE the departure every stored member is on the battlefield, so the \
         wire/store divergence below is caused by the departure and nothing else"
    );

    let departed = *stored.iter().next().unwrap();
    let mut events: Vec<GameEvent> = Vec::new();
    move_to_zone(&mut state, departed, Zone::Graveyard, &mut events);
    assert!(
        !state.battlefield.contains(&departed),
        "the departure really happened (CR 110.1: it stopped being a permanent)"
    );

    // (3) THE STORE IS NOT FILTERED. This is the discriminator against a "fix" that prunes
    //     `unbounded_loop_pile` itself: that mutation satisfies (1), (2) and (4) and reds only
    //     this row. The store must survive because the boundary collapse reads it.
    assert!(
        state
            .unbounded_loop_pile
            .get(&P0)
            .is_some_and(|pile| pile.contains(&departed)),
        "(3) the STORE must still carry the departed member — only the wire filters"
    );

    let expected: BTreeSet<ObjectId> = stored
        .iter()
        .copied()
        .filter(|id| state.battlefield.contains(id))
        .collect();
    assert_eq!(
        expected.len(),
        stored.len() - 1,
        "control: exactly ONE stored member became stale"
    );

    for viewer in [None, Some(P0), Some(P1), Some(P2), Some(PlayerId(3))] {
        let views = engine::game::derived_views::derive_views(&state, viewer);
        let wire: BTreeSet<ObjectId> = views.unbounded_pile.iter().copied().collect();
        assert!(
            !wire.contains(&departed),
            "(1) the wire omits the stale member (viewer {viewer:?})"
        );
        assert_eq!(
            wire, expected,
            "(2) EXACT membership: the wire is stored ∩ battlefield, so a projection that \
             dropped EXTRA members fails here too (viewer {viewer:?})"
        );
        assert_eq!(
            views.unbounded_pile.len(),
            stored.len() - 1,
            "(4) exactly one member is lost between store and wire (viewer {viewer:?})"
        );
    }

    // The ROW survives: the rest of the pile still backs the axis. This is the `Some(true)`
    // arm of `derived_views::object_growth_backing` — a partial departure is not a revocation.
    let axes: Vec<ResourceAxis> = engine::game::derived_views::derive_views(&state, None)
        .unbounded_resources
        .iter()
        .map(|row| row.axis)
        .collect();
    assert!(
        axes.contains(&ResourceAxis::TokensCreated),
        "one stale member leaves live backing behind, so the ∞ row persists, got {axes:?}"
    );
}

/// R6a-3 (FAIL-CLOSED), under option (B). ONE rig, TWO arms that fail on DIFFERENT wrong
/// implementations — the pair is what pins "the ∞ rows do not depend on the collapse schedule at
/// all", which is strictly stronger than either arm alone.
///
/// 1. PRE-CLEAR arm (stash PRESENT) — kills any STASH-KEYED hide filter. This is also the
///    load-bearing rows control for the sibling test above: it is green under the pile-guard probe
///    (RP-1) and red under the row-guard probe (RP-1d), i.e. it discriminates in BOTH directions,
///    which is what lets that test assert pile-first.
/// 2. POST-CLEAR arm (stash DROPPED, marks kept) — kills any LABELLABILITY-KEYED hide filter.
///    `LoopCollapseAxis::from_resource_axis` maps `TokensCreated` / `Counter(..)` / `Life(..)` to
///    a label, so "hide every axis that has a collapse label" is a one-liner that passes arm 1's
///    sibling rows and still hides an axis nothing will ever collapse. Arm 2 is the only row in
///    this file that reds it.
///
/// REVERT-PROBE (RP-1d, RUN): restore `if collapse_scheduled(controller, &axis) { continue; }` in
/// `derive_views`' resource-row loop ⇒ arm 1 FAILS (stash present ⇒ rows hidden) while arm 2 stays
/// green (stash cleared ⇒ nothing scheduled ⇒ rows project). That asymmetry is why both arms
/// exist.
///
/// REVERT-PROBE (RP-4, RUN): hide rows matching
/// `matches!(axis, ResourceAxis::TokensCreated | ResourceAxis::Counter(..) | ResourceAxis::Life(_))`
/// — a *transcription* of the three `Some` arms of `LoopCollapseAxis::from_resource_axis`
/// (`types/game_state.rs`), inlined because that fn is declared as a bare module-private
/// `fn` and `game::derived_views` is a sibling module, so calling it is
/// `error[E0624]: associated function from_resource_axis is private`. Do not widen its
/// visibility. ⇒ BOTH arms FAIL, and arm 2 is the one that is unreachable by any stash-keyed
/// probe, because the stash is already gone when it runs.
#[test]
fn unregistered_axis_still_renders_its_infinity_badge() {
    let mut state = batched_offer_state();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        state.waiting_for
    );
    r6a_declare_and_accept_all(&mut state, P0, 200);

    // Both arms need the STORE marks present; arm 1 additionally needs the registration present.
    let marked = state
        .unbounded_resources
        .get(&P0)
        .expect("accept marked the ∞ axes")
        .clone();
    assert!(
        marked.contains(&ResourceAxis::TokensCreated),
        "reach-guard: the labellable token axis is marked, got {marked:?}"
    );
    assert_eq!(
        state.pending_unbounded_materialization.len(),
        1,
        "reach-guard for arm 1: the registration is present, so a stash-keyed filter would fire"
    );

    // ARM 1, BEFORE the drop: while the collapse is merely SCHEDULED the ∞ rows stay projected.
    let scheduled_rows =
        engine::game::derived_views::derive_views(&state, None).unbounded_resources;
    let scheduled_axes: Vec<ResourceAxis> = scheduled_rows.iter().map(|r| r.axis).collect();
    assert!(
        scheduled_axes.contains(&ResourceAxis::TokensCreated),
        "a merely-SCHEDULED collapse still projects the ∞ row, got {scheduled_axes:?}"
    );

    // R3 PRE-CLEAR positive control — without it the post-clear "every family Unscheduled" below
    // is VACUOUS: a mutant that never reports a schedule at all satisfies the post-clear
    // assertion, so the pair only discriminates because this arm proves a schedule CAN be reported
    // on this same state.
    {
        let v = engine::game::derived_views::derive_views(&state, None);
        let j = serde_json::to_string(&v).unwrap();
        assert!(
            v.unbounded_families
                .iter()
                .any(|f| matches!(f.state, FamilyCollapseState::Scheduled { .. }))
                && j.contains("\"Scheduled\""),
            "R3/pre-clear: a registered materialization SCHEDULES a family AND emits it, got {:?}",
            v.unbounded_families
        );
    }

    // ARM 2: drop the registrations, keep the marks — an ∞ axis that is collapsible-LABELLED but
    // has nothing scheduled to collapse it.
    state.pending_unbounded_materialization.clear();

    // R3 — with nothing scheduled EVERY family is `Unscheduled`, which is deliberately NOT the
    // same as "the channel disappears": the ∞ badges are still on screen, they just promise
    // nothing. The rows must therefore still be there, and the `Scheduled` encoding must be gone
    // from the wire entirely.
    {
        let v = engine::game::derived_views::derive_views(&state, None);
        let j = serde_json::to_string(&v).unwrap();
        assert!(
            !v.unbounded_families.is_empty()
                && v.unbounded_families
                    .iter()
                    .all(|f| f.state == FamilyCollapseState::Unscheduled),
            "R3/post-clear: with nothing scheduled every family is Unscheduled — and the channel \
             is still populated, so this is not vacuous; got {:?}",
            v.unbounded_families
        );
        assert!(
            !j.contains("\"Scheduled\""),
            "R3/post-clear: no Scheduled state reaches the wire, got {j}"
        );
        let back: engine::game::derived_views::DerivedViews = serde_json::from_str(&j).unwrap();
        assert_eq!(
            back.unbounded_families, v.unbounded_families,
            "R3/default: the Unscheduled channel round-trips unchanged"
        );
        assert!(
            !back.unbounded_resources.is_empty(),
            "R3/default reach: …and it still carries the rows, so the line above is not vacuous"
        );
    }

    let rows = engine::game::derived_views::derive_views(&state, None).unbounded_resources;
    let axes: Vec<ResourceAxis> = rows.iter().map(|r| r.axis).collect();
    assert!(
        axes.contains(&ResourceAxis::TokensCreated),
        "FAIL-CLOSED: a collapsible-LABELLED axis with NO registered materialization is \
         still unbounded and must keep its ∞ badge, got {axes:?}"
    );
}

/// R4-C4b (CR 732.2c). "Once the last player has either accepted or shortened the shortcut
/// proposal, the shortcut is taken" — its ending point is fixed at the accepted N, so the
/// CR 500.5 boundary collapse prompt may not offer a WIDER range than the table agreed to.
/// BASE re-asked with `max = MAX_SHORTCUT_CYCLES` (1000), letting a controller who proposed
/// 7 cycles walk away with 1000.
///
/// REVERT-PROBE (RUN): restore `max: crate::game::engine::MAX_SHORTCUT_CYCLES` ⇒ `max`
/// reads 1000 ⇒ FAILS. `min: 0` is asserted unchanged (a collapse-to-nothing stays legal).
#[test]
fn accepted_fixed_count_bounds_the_boundary_collapse_prompt() {
    let mut state = batched_offer_state();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        state.waiting_for
    );
    r6a_declare_and_accept_all(&mut state, P0, 7);
    assert!(
        state.pending_unbounded_materialization.contains_key(&P0),
        "reach-guard: the accept scheduled a collapse, so the boundary WILL prompt"
    );

    r6a_drive_to_boundary(&mut state);

    match &state.waiting_for {
        WaitingFor::PayAmountChoice {
            player,
            resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
            min,
            max,
            ..
        } => {
            assert_eq!(*player, P0, "the loop controller is prompted");
            assert_eq!(
                *max, 7,
                "CR 732.2c: the accepted Fixed(7) bounds the collapse prompt (BASE: 1000)"
            );
            assert_eq!(*min, 0, "a collapse-to-nothing stays legal");
        }
        other => {
            panic!("the CR 500.5 boundary must prompt P0 for the collapse count, got {other:?}")
        }
    }

    // REJECTION DISCRIMINATOR — the bound is ENFORCED by the reducer, not merely advertised
    // in the prompt. This is the control a widened-`max` BASE cannot pass: with
    // `max = MAX_SHORTCUT_CYCLES` a submit of 8 is ACCEPTED, so this assertion is what makes
    // the range assertion above load-bearing rather than cosmetic.
    let over = apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 8 });
    assert!(
        matches!(&over, Err(EngineError::InvalidAction(msg)) if msg.contains("[0, 7]")),
        "CR 732.2c: collapsing PAST the accepted count must be rejected, got {over:?}"
    );

    // The bound is honored end-to-end: submitting exactly N is still accepted.
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 7 })
        .expect("collapsing at exactly the accepted count is legal");
}

// ===========================================================================
// A replay take's per-cycle history work does not grow with its count.
// ===========================================================================

/// A per-turn history vector a replay take appends to.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TakeHistoryVector {
    JournalEntries,
    ProducedMana,
    SpentMana,
    BattlefieldEntries,
    CountersAdded,
}

impl TakeHistoryVector {
    /// Whether `original` is inline, and whether `copy` shares its chunks.
    fn inline_and_shared(self, original: &GameState, copy: &GameState) -> (bool, bool) {
        fn of<A: Clone>(original: &im::Vector<A>, copy: &im::Vector<A>) -> (bool, bool) {
            (original.is_inline(), original.ptr_eq(copy))
        }
        let (journal, copied) = (
            &original.resolved_rules_journal,
            &copy.resolved_rules_journal,
        );
        match self {
            Self::JournalEntries => of(journal.entries(), copied.entries()),
            Self::ProducedMana => of(journal.produced_mana(), copied.produced_mana()),
            Self::SpentMana => of(journal.spent_mana(), copied.spent_mana()),
            Self::BattlefieldEntries => of(
                &original.battlefield_entries_this_turn,
                &copy.battlefield_entries_this_turn,
            ),
            Self::CountersAdded => of(
                &original.counter_added_this_turn,
                &copy.counter_added_this_turn,
            ),
        }
    }
}

/// A map-backed per-turn history field.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TakeHistoryMap {
    AbilityResolutions,
    ActivatedAbilities,
    TrackedObjectSets,
    TrackedSetMemberCauses,
    LkiCopiableValues,
    ActivatedAbilitiesThisGame,
}

impl TakeHistoryMap {
    fn len(self, state: &GameState) -> usize {
        match self {
            Self::AbilityResolutions => state.ability_resolutions_this_turn.len(),
            Self::ActivatedAbilities => state.activated_abilities_this_turn.len(),
            Self::TrackedObjectSets => state.tracked_object_sets.len(),
            Self::TrackedSetMemberCauses => state.tracked_set_member_causes.len(),
            Self::LkiCopiableValues => state.lki_copiable_values.len(),
            Self::ActivatedAbilitiesThisGame => state.activated_abilities_this_game.len(),
        }
    }
}

/// The one take the meter recorded since its last reset.
fn only_take(n: u32) -> TakeCostRecord {
    let records = take_cost_records();
    assert_eq!(
        records.len(),
        1,
        "the take at {n} drives the replay exactly once"
    );
    records.into_iter().next().expect("one take record")
}

fn history_copies(cycle: &TakeCostCounters) -> u64 {
    cycle.history_entries_copied + cycle.history_map_entries_unshared
}

/// Drives `take` on a fresh board at a small count and at `large`, and checks that the history
/// entries copied per cycle stay within twice the small take's maximum, that every keyed journal
/// read examines at most the record it finds, that each take snapshots the state once, and that a
/// state copy after the large take shares every history structure it grew (`grown`, `maps`).
/// `take` builds its board, calls `perf_counters::reset`, performs the take and returns the state.
pub(crate) fn assert_take_history_work_is_flat(
    large: u32,
    grown: &[TakeHistoryVector],
    maps: &[TakeHistoryMap],
    mut take: impl FnMut(u32) -> GameState,
) {
    const SMALL: u32 = 3;
    take(SMALL);
    let small = only_take(SMALL);
    let state = take(large);
    let large_take = only_take(large);

    for (record, n) in [(&small, SMALL), (&large_take, large)] {
        assert_eq!(
            (record.count, record.delivered, record.cycles.len()),
            (n, n, n as usize),
            "reach: the take at {n} delivers every cycle"
        );
        assert_eq!(
            record.whole_take.drive_snapshots, 1,
            "the take at {n} snapshots the state once"
        );
        for (cycle, counts) in record.cycles.iter().enumerate() {
            assert!(
                counts.journal_records_examined <= counts.journal_keyed_reads,
                "cycle {cycle} of the take at {n}: each keyed journal read examines at most the \
                 record it finds ({counts:?})"
            );
        }
    }
    assert!(
        small
            .cycles
            .iter()
            .map(|c| c.journal_keyed_reads)
            .sum::<u64>()
            > 0,
        "reach: the small take reads the journal by key"
    );
    let most = |record: &TakeCostRecord| record.cycles.iter().map(history_copies).max();
    let (small_most, large_most) = (most(&small).unwrap_or(0), most(&large_take).unwrap_or(0));
    assert!(
        small_most > 0,
        "reach: the small take copies history entries"
    );
    assert!(
        large_most <= 2 * small_most,
        "history entries copied per cycle grow with the count: {large_most} at {large} against \
         {small_most} at {SMALL}"
    );

    let before = take_cost_snapshot();
    let copy = state.clone();
    let copied = take_cost_snapshot().since(before);
    for vector in grown {
        let (inline, shared) = vector.inline_and_shared(&state, &copy);
        assert!(!inline, "reach: {vector:?} outgrew its inline chunk");
        assert!(shared, "a state copy shares {vector:?}");
    }
    for map in maps {
        assert!(map.len(&state) > 0, "reach: {map:?} holds entries");
    }
    assert_eq!(copied.state_copies, 1, "reach: one state copy was taken");
    assert_eq!(
        copied.history_map_entries_unshared, 0,
        "a state copy shares every map-backed history entry"
    );
}

/// Drives `take` on a fresh board at a small count and at `large`, and checks that the mana-pool
/// entries walked per cycle are the same at both counts.
pub(crate) fn assert_take_pool_walk_is_flat(large: u32, mut take: impl FnMut(u32) -> GameState) {
    const SMALL: u32 = 3;
    take(SMALL);
    let small = only_take(SMALL);
    take(large);
    let large_take = only_take(large);

    for (record, n) in [(&small, SMALL), (&large_take, large)] {
        assert_eq!(
            (record.count, record.delivered, record.cycles.len()),
            (n, n, n as usize),
            "reach: the take at {n} delivers every cycle"
        );
    }
    let most = |record: &TakeCostRecord| {
        record
            .cycles
            .iter()
            .map(|cycle| cycle.pool_entries_walked)
            .max()
            .unwrap_or(0)
    };
    let (small_most, large_most) = (most(&small), most(&large_take));
    assert!(small_most > 0, "reach: the small take walks the pool");
    assert_eq!(
        large_most, small_most,
        "pool entries walked per cycle grow with the count: {large_most} at {large} against \
         {small_most} at {SMALL}"
    );
}

/// The Witherbloom/Sprout take's per-cycle history work does not grow with its count.
#[test]
fn sprout_take_history_work_is_flat_per_cycle() {
    assert_take_history_work_is_flat(
        9,
        &[
            TakeHistoryVector::JournalEntries,
            TakeHistoryVector::ProducedMana,
            TakeHistoryVector::SpentMana,
            TakeHistoryVector::BattlefieldEntries,
        ],
        &[TakeHistoryMap::AbilityResolutions],
        |n| {
            let mut state = r6a_offer_state();
            engine::game::perf_counters::reset();
            r6a_declare_and_accept_all(&mut state, P0, n);
            state
        },
    );
}

/// A Smaug + Mayhem Devil Treasure-sacrifice take shares the LKI copiable values and per-game
/// activation counts each cycle grows, so its per-cycle copy work does not grow with its count.
#[test]
fn treasure_sacrifice_take_history_work_is_flat_per_cycle() {
    use crate::play_trace::{ability, activate, cast, is_offer, names, settle};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::zones::Zone;

    let db = crate::support::shared_card_db().expect("the committed card fixture loads");
    let offer = || {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let smaug = scenario.add_real_card(P0, "Smaug the Impenetrable", Zone::Battlefield, db);
        let devil = scenario.add_real_card(P0, "Mayhem Devil", Zone::Battlefield, db);
        let breaker =
            scenario.add_real_card(P0, "Professional Face-Breaker", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, db);
        let shock = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
        for _ in 0..60 {
            scenario.add_real_card(P0, "Swamp", Zone::Library, db);
            scenario.add_real_card(P1, "Swamp", Zone::Library, db);
        }
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        let score = |a: &GameAction| 2 * names(&[smaug])(a) + names(&[devil, breaker])(a);
        cast(&mut runner, shock, vec![smaug], CastPaymentMode::Auto);
        settle(&mut runner, &score);
        for _ in 0..8 {
            let state = runner.state();
            let treasure = state
                .battlefield
                .iter()
                .copied()
                .find(|id| state.objects[id].name == "Treasure" && !state.objects[id].tapped)
                .expect("an untapped Treasure to sacrifice");
            let index = ability(state, treasure, true);
            activate(&mut runner, treasure, index);
            settle(&mut runner, &score);
            if is_offer(runner.state()) {
                return runner.state().clone();
            }
        }
        panic!("reach-guard: the Treasure loop surfaces a CR 732.2a offer within 8 cycles");
    };
    assert_take_history_work_is_flat(
        32,
        &[
            TakeHistoryVector::JournalEntries,
            TakeHistoryVector::BattlefieldEntries,
        ],
        &[
            TakeHistoryMap::LkiCopiableValues,
            TakeHistoryMap::ActivatedAbilitiesThisGame,
        ],
        |n| {
            let mut state = offer();
            engine::game::perf_counters::reset();
            r6a_declare_and_accept_all(&mut state, P0, n);
            let copy = state.clone();
            assert!(state.lki_copiable_values.ptr_eq(&copy.lki_copiable_values));
            assert!(state
                .activated_abilities_this_game
                .ptr_eq(&copy.activated_abilities_this_game));
            state
        },
    );
}

/// R6a FIX-2 (CR 732.2c). MEASURED DEFECT in the first cut of the collapse bound: the stash
/// `register_pending_materialization` APPENDS ("two accepts by the same controller, coexist"),
/// but the bound was written with a bare `insert`, i.e. it OVERWROTE. A controller who accepts
/// `Fixed(1)` and then, in the SAME phase, accepts `Fixed(1000)` therefore ends up with a
/// two-item stash bounded at 1000 — and since the boundary applies ONE submitted amount to
/// EVERY item, the first accept's loop would materialize 1000 times though the table agreed to
/// exactly one.
///
/// SHIPPED SEMANTICS, PINNED HERE EXPLICITLY: the bound is the MINIMUM of the accepted counts.
/// The second accept's agreed 1000 is UNDER-delivered down to 1. That is still a divergence
/// from what the table agreed to — but it is the safe polarity: no accept in the stash can ever
/// be over-materialized, which is the CR 732.2c violation ("the shortcut is taken" at the count
/// the last player accepted, not at some later, larger one). The exact per-accept bound needs
/// the flat stash to become accept-grouped and is deliberately NOT smuggled in here; the
/// boundary's pause-safety `sort_by_key` reorders that flat list, so a positional parallel
/// bound vector is not a valid shortcut to it.
///
/// REVERT-PROBE (RUN): restore `pending_materialization_count.insert(proposal.proposer, n)` in
/// `materialize_fixed_shortcut` ⇒ the bound reads 1000, the prompt offers `max == 1000`, and the
/// out-of-range submit is ACCEPTED ⇒ assertions (4), (5) and (6) FAIL.
#[test]
fn two_accepts_in_one_phase_bound_the_collapse_to_the_smallest_accepted_count() {
    let mut state = batched_offer_state();

    // (1) reach-guard: the first real cast reached the CR 732.2a offer.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the first buyback+convoke recast must surface P0's offer, got {:?}",
        state.waiting_for
    );
    let phase_at_first_accept = state.phase;
    r6a_declare_and_accept_all(&mut state, P0, 1);
    assert_eq!(
        state.pending_materialization_count.get(&P0).copied(),
        Some(1),
        "the first accept records its own Fixed(1) bound"
    );

    // (2) The buyback returned Sprout Swarm to hand and priority came back, so a SECOND real
    // cast is available in the SAME phase — this is what makes the append reachable at all.
    let fodder = *state
        .battlefield
        .iter()
        .find(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == P0 && !o.tapped && o.name.contains("Saproling"))
        })
        .expect("an untapped P0 Saproling remains to convoke the second cast");
    let sprout = state
        .objects
        .values()
        .find(|o| o.name == "Sprout Swarm" && o.zone == engine::types::zones::Zone::Hand)
        .map(|o| o.id)
        .expect("the buyback returned Sprout Swarm to P0's hand");
    let mut state = GameRunner::from_state(state)
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder])
        .commit()
        .resolve()
        .state()
        .clone();

    // (3) reach-guard: the second cast really produced a second offer, in the same phase.
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the second recast must surface a second offer, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.phase, phase_at_first_accept,
        "both accepts land in ONE phase, so they share ONE stash and ONE boundary prompt"
    );
    r6a_declare_and_accept_all(&mut state, P0, 1000);

    // (4) THE PREMISE + THE FIX. The stash APPENDED (two items, one boundary amount for both),
    // and the bound is the MINIMUM — not the latest write.
    assert_eq!(
        state
            .pending_unbounded_materialization
            .get(&P0)
            .map(Vec::len),
        Some(2),
        "premise: the two accepts coexist in ONE stash, so ONE amount will scale BOTH"
    );
    assert_eq!(
        state.pending_materialization_count.get(&P0).copied(),
        Some(1),
        "CR 732.2c: min(1, 1000) — the later Fixed(1000) may NOT re-scale the Fixed(1) accept \
         (BASE overwrite: 1000)"
    );

    let p0_permanents = |s: &GameState| {
        s.battlefield
            .iter()
            .filter(|id| s.objects.get(id).is_some_and(|o| o.controller == P0))
            .count()
    };
    let permanents_before = p0_permanents(&state);

    r6a_drive_to_boundary(&mut state);

    // (5) The prompt advertises the minimum.
    let WaitingFor::PayAmountChoice {
        player,
        resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
        max,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "the CR 500.5 boundary must prompt P0 for the collapse count, got {:?}",
            state.waiting_for
        )
    };
    assert_eq!(*player, P0, "the loop controller is prompted");
    assert_eq!(
        *max, 1,
        "CR 732.2c: the prompt is bounded by the SMALLEST accepted count (BASE: 1000)"
    );

    // (6) And the reducer ENFORCES it — the second accept's agreed 1000 is unreachable.
    let over = apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 1000 });
    assert!(
        matches!(&over, Err(EngineError::InvalidAction(msg)) if msg.contains("[0, 1]")),
        "CR 732.2c: the later accept's 1000 cannot be collapsed at, got {over:?}"
    );

    // (7) WHAT B'S 1000 ACTUALLY BECOMES: exactly 1. Each of the two stashed collapses mints
    // ONCE — one new token per collapse — so the first accept keeps precisely the single cycle the
    // table agreed to, and the second is capped down to the same.
    //
    // NOT the BASE discriminator, and deliberately not claimed as one: this submits
    // `amount: 1`, which the BASE overwrite ALSO materializes as Δ2. A bare-`insert` revert
    // probe was RUN and fails only at assertion (5) (`left: Some(1000)`). What BASE gets
    // wrong is that it ADVERTISES `max: 1000` and PERMITS a 1000× submit — assertions (4),
    // (5) and (6) are the rows that catch that. This row exists to pin the post-collapse
    // board, i.e. that the enforced bound is also the delivered one.
    apply(&mut state, P0, GameAction::SubmitPayAmount { amount: 1 })
        .expect("collapsing at the minimum accepted count is legal");
    assert_eq!(
        p0_permanents(&state) - permanents_before,
        2,
        "one materialized cycle per stashed accept, never 1000"
    );
}

/// CR 732.2c: a count of ZERO performs nothing and TAKES the shortcut — on either response.
/// "The game advances to the last proposed ending point, with all game choices contained in the
/// shortcut proposal having been taken": a place of zero names the sequence's start, so arriving
/// there means performing no iteration. The board is unchanged, nothing is stashed, the CR 500.5
/// boundary mints no collapse prompt, the trace offers the period no more, and the beat that follows
/// is a priority window rather than a fresh offer of the shortcut just taken.
///
/// One rule, both responses, so neither is the only one it is bought on: the `Accept` leg drives
/// a real cast, a real `Fixed(0)` declaration and real APNAP accepts on the sprout board; the
/// `Shorten` leg names place 0 on the mana engine, the subclass that stashes nothing at all.
///
/// The `Fixed(1)` control is the instrument: on the same rig an accept at one still registers a
/// stash and still mints the prompt, so the zero leg's absences are a property of the count and
/// not of a fixture that never materializes. The consumed period and the priority beat are
/// assertions rather than remarks — route the zero back through the per-cycle drive and every
/// other assertion still passes while this same `apply()` re-offers the shortcut it just took.
#[test]
fn a_count_of_zero_performs_nothing_and_takes_the_shortcut_on_either_response() {
    let board = |s: &GameState| {
        (
            s.battlefield
                .iter()
                .filter(|id| s.objects.get(id).is_some_and(|o| o.controller == P0))
                .count(),
            s.players.iter().find(|p| p.id == P0).unwrap().life,
        )
    };

    // ── The control: the same rig at ONE registers and prompts.
    let mut one = batched_offer_state();
    r6a_declare_and_accept_all(&mut one, P0, 1);
    assert_eq!(
        one.pending_unbounded_materialization
            .get(&P0)
            .map_or(0, Vec::len),
        1,
        "control: an accept at a NON-ZERO count still registers a deferred collapse"
    );
    r6a_drive_to_boundary(&mut one);
    assert!(
        matches!(
            one.waiting_for,
            WaitingFor::PayAmountChoice {
                resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
                ..
            }
        ),
        "control: that accept reaches the CR 500.5 boundary prompt, got {:?}",
        one.waiting_for
    );

    // ── The Accept leg, on the subclass that registers.
    let mut zero = batched_offer_state();
    assert!(
        matches!(zero.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: at the offer, got {:?}",
        zero.waiting_for
    );
    let at_offer = board(&zero);
    assert!(
        matches!(&zero.waiting_for, WaitingFor::LoopShortcut { period, .. } if !period.is_empty()),
        "reach-guard: the offer's routing signal — its confirmed period — is live going in"
    );
    r6a_declare_and_accept_all(&mut zero, P0, 0);

    assert_eq!(
        board(&zero),
        at_offer,
        "CR 732.2c: a sequence performed zero times leaves the board where it found it"
    );
    assert!(
        zero.pending_unbounded_materialization.is_empty()
            && zero.pending_materialization_count.is_empty(),
        "nothing was performed, so nothing may be stashed and no bound may be written"
    );
    assert!(
        no_offered_span(&zero),
        "the trace offers the period no more — a shortcut that leaves it standing is re-offered \
         by the same action that took it"
    );
    assert!(
        matches!(zero.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: the taking ends at a priority point, never at a fresh offer of the loop it \
         just answered; got {:?}",
        zero.waiting_for
    );
    r6a_drive_to_boundary(&mut zero);
    assert!(
        !matches!(
            zero.waiting_for,
            WaitingFor::PayAmountChoice {
                resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
                ..
            }
        ),
        "nothing was stashed, so the boundary mints no collapse prompt; got {:?}",
        zero.waiting_for
    );

    // ── The Shorten leg, on the subclass that registers nothing.
    let Some(db) = super::support::shared_card_db() else {
        return;
    };
    let mut rig = super::loop_shortcut_mana_engine::setup(
        true,
        engine::types::game_state::LoopDetectionMode::Interactive,
        db,
    );
    let mana = super::loop_shortcut_mana_engine::mana_ability_index(rig.runner.state(), rig.basalt)
        .expect("Basalt publishes its mana ability");
    let untap =
        super::loop_shortcut_mana_engine::untap_ability_index(rig.runner.state(), rig.basalt)
            .expect("Basalt publishes its untap ability");
    super::loop_shortcut_mana_engine::drive_one_period(&mut rig, mana, untap);
    assert!(
        matches!(
            rig.runner.state().waiting_for,
            WaitingFor::LoopShortcut { .. }
        ),
        "reach-guard: the mana engine must OFFER, got {:?}",
        rig.runner.state().waiting_for
    );
    arm_responders(rig.runner.state_mut());
    rig.runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(4),
            template: None,
        })
        .expect("the proposer declares");
    let pool_before = rig.runner.state().players[0]
        .mana_pool
        .count_color(ManaType::Colorless);
    let WaitingFor::RespondToShortcut { player, .. } = rig.runner.state().waiting_for else {
        panic!(
            "the declaration opens a responder window, got {:?}",
            rig.runner.state().waiting_for
        );
    };
    rig.runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 0 },
        })
        .expect("place 0 is in range on a four-repetition proposal");

    let after = rig.runner.state();
    assert_eq!(
        after.players[0].mana_pool.count_color(ManaType::Colorless),
        pool_before,
        "CR 732.2c: zero periods were performed, so the pool did not move"
    );
    assert!(
        after.unbounded_resources.is_empty() && after.pending_unbounded_materialization.is_empty(),
        "a sequence performed zero times grants no unbounded advance and stashes nothing"
    );
    assert!(
        no_offered_span(after),
        "the trace offers the period no more on the shortening ingress too"
    );
    assert_eq!(
        after.waiting_for,
        WaitingFor::Priority { player },
        "CR 732.2b: the shortener named the place, so the shortener holds its ending point"
    );
}

/// The real 4p Sprout board at its offer, with Relic of Progenitus ("{T}: Target player exiles a
/// card from their graveyard.") on P1's battlefield, so the first responder holds a game choice at
/// every place it may name. `None` without the card DB.
fn relic_offer_state() -> Option<(GameState, ObjectId)> {
    let db = super::support::shared_card_db()?;
    let mut state = batched_offer_state();
    let relic = crate::play_trace::place(&mut state, P1, "Relic of Progenitus", db);
    Some((state, relic))
}

/// Relic of Progenitus ("{T}: Target player exiles a card from their graveyard.") on the
/// battlefield of every living opponent of the offer's proposer, so each seat the offer polls holds
/// a game choice at every place it may name (CR 732.2b).
pub(crate) fn arm_responders(state: &mut GameState) {
    let WaitingFor::LoopShortcut { proposer, .. } = state.waiting_for else {
        panic!(
            "responders are armed at the offer, got {:?}",
            state.waiting_for
        );
    };
    let db = super::support::shared_card_db().expect("the integration card fixture");
    let responders: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| p.id != proposer && !p.is_eliminated)
        .map(|p| p.id)
        .collect();
    for seat in responders {
        crate::play_trace::place(state, seat, "Relic of Progenitus", db);
    }
}

/// The shortener's different choice (CR 732.2c): activate Relic of Progenitus at a player whose
/// graveyard is empty, so the ability resolves without a prompt of its own.
fn diverge_with_relic(state: &mut GameState, shortener: PlayerId, relic: ObjectId) {
    let ability_index = crate::play_trace::ability(state, relic, false);
    apply(
        state,
        shortener,
        GameAction::ActivateAbility {
            source_id: relic,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        },
    )
    .expect("the shortener activates Relic of Progenitus");
    if matches!(state.waiting_for, WaitingFor::TargetSelection { .. }) {
        let seat = state
            .players
            .iter()
            .find(|p| p.graveyard.is_empty())
            .map(|p| p.id)
            .expect("a player with an empty graveyard");
        apply(
            state,
            shortener,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(seat)),
            },
        )
        .expect("the shortener targets that player");
    }
    assert_eq!(
        (state.stack.len(), state.waiting_for.clone()),
        (1, WaitingFor::Priority { player: shortener }),
        "reach: the Relic ability is on the stack and the shortener holds priority"
    );
}

/// CR 732.2c on the object-growth subclass that REGISTERS: a shortened proposal is PERFORMED,
/// not elided. The elision defers the growth to a CR 500.5 boundary collapse under a ceiling the
/// accept wrote; a responder who named a place accepted no unbounded advance, so the named
/// number of periods is delivered instead — the growth the elision would have deferred, standing
/// on the board.
///
/// The Accept control is the instrument, and the stash and the prompt are the discriminators
/// because they are exactly what the elision produces and performance does not. The two
/// shortening lengths separate performing the named number from performing one. P1 holds Relic of
/// Progenitus, so each place it names is one where it has a different game choice (CR 732.2b).
#[test]
fn a_relic_holders_shortened_sprout_loop_is_performed_while_an_accepted_one_is_stashed() {
    let Some((offer, relic)) = relic_offer_state() else {
        return;
    };
    let board = |s: &GameState| {
        (
            s.battlefield
                .iter()
                .filter(|id| s.objects.get(id).is_some_and(|o| o.controller == P0))
                .count() as i64,
            s.players.iter().find(|p| p.id == P0).unwrap().life as i64,
        )
    };
    let at_offer = board(&offer);

    // Control: ACCEPTED at a non-zero count — the growth is deferred, not delivered.
    let mut accepted = offer.clone();
    r6a_declare_and_accept_all(&mut accepted, P0, 2);
    assert_eq!(
        board(&accepted),
        at_offer,
        "control: the elision defers the growth, so the accept itself moves no board"
    );
    assert_eq!(
        accepted
            .pending_unbounded_materialization
            .get(&P0)
            .map_or(0, Vec::len),
        1,
        "control: the accept registers the deferred collapse"
    );
    r6a_drive_to_boundary(&mut accepted);
    assert!(
        matches!(
            accepted.waiting_for,
            WaitingFor::PayAmountChoice {
                resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
                ..
            }
        ),
        "control: the stash mints the CR 500.5 collapse prompt, got {:?}",
        accepted.waiting_for
    );

    for place in [1u32, 2] {
        let mut state = offer.clone();
        apply(
            &mut state,
            P0,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(3),
                template: None,
            },
        )
        .expect("the proposer declares");
        let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for else {
            panic!(
                "the declaration opens a responder window, got {:?}",
                state.waiting_for
            )
        };
        let shortener = player;
        let mut first = true;
        while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
            let response = if std::mem::take(&mut first) {
                ShortcutResponse::Shorten {
                    at_iteration: place,
                }
            } else {
                ShortcutResponse::Accept
            };
            apply(
                &mut state,
                player,
                GameAction::RespondToShortcut { response },
            )
            .expect("each seat answers");
        }

        assert_eq!(
            board(&state),
            (at_offer.0 + i64::from(place), at_offer.1),
            "CR 732.2c at place {place}: that many real periods stand on the board"
        );
        assert!(
            state.pending_unbounded_materialization.is_empty()
                && state.pending_materialization_count.is_empty(),
            "performance defers nothing, so there is no stash and no ceiling to write"
        );
        assert!(
            no_offered_span(&state),
            "the trace offers the period no more after the taking"
        );
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority { player: shortener },
            "the drive reached the place the responder named, so they hold the ending point"
        );
        assert_eq!(shortener, P1, "reach: P1, the Relic's holder, shortened");
        let pass = apply(&mut state.clone(), shortener, GameAction::PassPriority);
        assert!(
            matches!(pass, Err(EngineError::ActionNotAllowed(_))),
            "CR 732.2c: the shortener owes a different choice than a pass; got {pass:?}"
        );
        diverge_with_relic(&mut state, shortener, relic);

        r6a_drive_to_boundary(&mut state);
        assert!(
            !matches!(
                state.waiting_for,
                WaitingFor::PayAmountChoice {
                    resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
                    ..
                }
            ),
            "nothing was stashed, so the boundary mints no collapse prompt; got {:?}",
            state.waiting_for
        );
    }
}

/// CR 732.2b: a responder shortens by "naming a place where they will make a game choice that's
/// different than what's been proposed". On the real 4p Sprout board P1 has no legal action but a
/// pass at the place it names, so its `Shorten` is refused and its window survives; the same place
/// on the same board with Relic of Progenitus on P1's battlefield is admitted.
#[test]
fn a_shorten_by_a_seat_with_no_other_choice_at_its_place_is_refused() {
    let mut state = batched_offer_state();
    apply(
        &mut state,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        },
    )
    .expect("the proposer declares");
    let WaitingFor::RespondToShortcut {
        player, proposal, ..
    } = state.waiting_for.clone()
    else {
        panic!("the declaration opens a responder window");
    };
    assert_eq!(player, P1, "reach: P1 answers first");
    assert_eq!(
        shorten_refusal(&state, &proposal, P1, 1),
        Some(ShortenRefusal::NoDifferentChoice)
    );
    let shorten_at_one = GameAction::RespondToShortcut {
        response: ShortcutResponse::Shorten { at_iteration: 1 },
    };
    assert!(
        !engine::ai_support::structurally_valid_prompt_answer(&state, &shorten_at_one),
        "the AI legality hatch refuses the place the reducer refuses"
    );
    let refused = apply(
        &mut state,
        P1,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 1 },
        },
    );
    assert!(
        matches!(&refused, Err(EngineError::InvalidAction(message))
            if message.contains(ShortenRefusal::NoDifferentChoice.reason())),
        "the refusal names its cause: {refused:?}"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { player, .. } if player == P1),
        "the refusal writes nothing, so P1 can still answer"
    );

    let Some((mut relic_board, _)) = relic_offer_state() else {
        return;
    };
    apply(
        &mut relic_board,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        },
    )
    .expect("the proposer declares on the Relic board");
    let WaitingFor::RespondToShortcut { proposal, .. } = relic_board.waiting_for.clone() else {
        panic!("the declaration opens a responder window on the Relic board");
    };
    assert_eq!(shorten_refusal(&relic_board, &proposal, P1, 1), None);
    assert!(
        engine::ai_support::structurally_valid_prompt_answer(&relic_board, &shorten_at_one),
        "the AI legality hatch admits the place the reducer admits"
    );
}

/// CR 732.2b: the published shortening range is a superset of the places `apply()` admits, so a
/// place inside it that names no different choice is refused at the reducer with its typed reason,
/// and the window survives for the same responder to accept.
#[test]
fn a_refused_place_inside_the_published_range_leaves_the_window_to_accept() {
    let mut state = batched_offer_state();
    apply(
        &mut state,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(3),
            template: None,
        },
    )
    .expect("the proposer declares");
    engine::game::interaction::bind_interaction_authority(
        &mut state,
        engine::types::interaction::InteractionSessionId("range".into()),
    )
    .expect("valid interaction authority binding");
    let filtered = engine::game::visibility::filter_state_for_viewer(&state, P1);
    let published = engine::game::interaction::derive_viewer_interaction(&state, &filtered, P1)
        .opportunities
        .iter()
        .find_map(|opportunity| match &opportunity.response {
            engine::types::interaction::InteractionOpportunityResponse::Schema {
                spec:
                    engine::types::interaction::InteractionResponseSpec::ShortcutReply {
                        min_iteration,
                        max_iteration,
                        ..
                    },
                ..
            } => Some(*min_iteration..=*max_iteration),
            _ => None,
        })
        .expect("P1's window publishes a shortcut-reply range");
    assert!(
        published.contains(&1),
        "place 1 is published: {published:?}"
    );

    let refused = apply(
        &mut state,
        P1,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Shorten { at_iteration: 1 },
        },
    );
    assert!(
        matches!(&refused, Err(EngineError::InvalidAction(message))
            if message.contains(ShortenRefusal::NoDifferentChoice.reason())),
        "the published place is refused with its typed reason: {refused:?}"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { player, .. } if player == P1),
        "the refusal leaves P1's window open"
    );
    apply(
        &mut state,
        P1,
        GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        },
    )
    .expect("P1 accepts in the window the refusal left");
}

/// The AI responder never shortens at place 0 and never sends a `Shorten` the engine refuses. P1
/// holds Relic of Progenitus on the real 4p Sprout board: against three repetitions it shortens at
/// one and the engine admits it, and against one repetition, where no place but 0 is admitted, it
/// accepts.
#[test]
fn the_ai_responder_shortens_only_at_an_admitted_place_past_zero() {
    let Some((offer, _)) = relic_offer_state() else {
        return;
    };
    for (count, expected) in [
        (3, ShortcutResponse::Shorten { at_iteration: 1 }),
        (1, ShortcutResponse::Accept),
    ] {
        let mut state = offer.clone();
        apply(
            &mut state,
            P0,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(count),
                template: None,
            },
        )
        .expect("the proposer declares");
        let response = engine::ai_support::smart_shortcut_response(&state, P1);
        assert_eq!(response, expected, "against Fixed({count})");
        apply(&mut state, P1, GameAction::RespondToShortcut { response })
            .expect("the engine admits the AI's answer");
    }
}

/// The `LoopCollapse` clamp, on the ingress that outlives the swallow above: a DECODE, not a
/// mint. After a zero-count shortcut performs nothing and stashes nothing, no live path writes a
/// zero into `pending_materialization_count` — but a save written BEFORE that carries one, and
/// the production restore admits it. The restored state then drives to the CR 500.5 boundary,
/// mints the prompt at a zero-width range, and the production candidate generator answers it.
///
/// The stash is the reach-guard: the boundary picks the prompt's controller by walking
/// `pending_unbounded_materialization` for a non-empty entry and only then reads the count, so a
/// save carrying the bound alone mints no prompt and this row would assert nothing. The live
/// control is the same drive at a bound of ONE, which is what separates a restored zero from an
/// absent bound — an absent bound is not a zero at this prompt, it is the engine-wide ceiling.
///
/// REVERT-PROBE (RUN, MEASURED): restore `GameAction::SubmitPayAmount { amount: 1 }` in
/// `ai_support::candidates` ⇒ `legal_actions` returns `[]`. The generator validates its
/// candidates against the reducer, so the illegal `amount: 1` is not merely rejected on submit —
/// it is dropped, leaving the AI with NO legal action at this prompt.
#[test]
fn the_collapse_candidate_is_clamped_to_a_bound_restored_from_an_older_save() {
    // A save written before the swallow: a registered stash AND the bound its accept wrote.
    let saved = |bound: u32| {
        let mut state = batched_offer_state();
        r6a_declare_and_accept_all(&mut state, P0, 1);
        assert_eq!(
            state
                .pending_unbounded_materialization
                .get(&P0)
                .map_or(0, Vec::len),
            1,
            "reach-guard: the save must carry the pending-materialization STASH, or the boundary \
             selects no controller and mints no prompt at all"
        );
        state.pending_materialization_count.insert(P0, bound);
        let payload = serde_json::to_value(&state).expect("the saved board serializes");
        serde_json::from_value::<engine::types::game_state::PersistedGameState>(payload)
            .expect("it decodes through the production restore chokepoint")
            .into_game_state()
            .expect("persisted test snapshot satisfies the checked restore contract")
    };

    for bound in [0u32, 1] {
        let mut state = saved(bound);
        assert_eq!(
            state.pending_materialization_count.get(&P0).copied(),
            Some(bound),
            "the restored bound survives the decode — without this the row measures something else"
        );
        r6a_drive_to_boundary(&mut state);

        let WaitingFor::PayAmountChoice {
            player,
            resource: engine::types::game_state::PayableResource::LoopCollapse { .. },
            min,
            max,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "bound={bound}: the restored stash must reach the boundary prompt, got {:?}",
                state.waiting_for
            )
        };
        assert_eq!(
            (*player, *min, *max),
            (P0, 0, bound),
            "CR 732.2c: the prompt is bounded by the count the save carried"
        );

        let candidates = engine::ai_support::legal_actions(&state);
        assert_eq!(
            candidates,
            vec![GameAction::SubmitPayAmount {
                amount: bound.min(1)
            }],
            "bound={bound}: the sole collapse candidate is clamped to what the reducer admits"
        );
        apply(&mut state, P0, candidates[0].clone())
            .expect("the generated candidate must be one the reducer accepts");
    }
}

// ===========================================================================
// PR-7 Phase 5b — CR 732.2a BOUNDED cycle fast-forward, on a REAL 4p dump.
//
// The class Path A and the recorded road's mandatory consumer both refuse: a drain lethal
// to SOME opponents leaves a second non-faller, so CR 104.2a determinacy (`loop_check`'s
// crown gate) will not crown,
// and a life-loss axis is not a CR 732.4 no-loss draw. Every row below LOADS
// `dina_conqueror_4p.json.gz` through the production restore chokepoint and DRIVES real
// beats through `apply()` — the offer is an accumulation across dozens of them, which no
// synthetic `GameScenario` reproduces.
// ===========================================================================

/// Drive the loaded dump until the engine itself writes its first loop-shortcut offer, and
/// return the beat. Reads `state.waiting_for`, never an out-of-band call to the offer predicate.
pub(crate) fn drive_to_bounded_offer(state: &mut GameState, cap: usize) -> Option<usize> {
    let pin = engine_live_opponents(state, P0).first().copied();
    for beat in 0..cap {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            return Some(beat);
        }
        if dump_drive_one_beat(state, pin).is_err() {
            return None;
        }
    }
    None
}

fn bounded_offer_parts(
    state: &GameState,
) -> (
    PlayerId,
    &engine::analysis::loop_check::LoopCertificate,
    &engine::analysis::decision_template::ShortcutDecisionSchema,
) {
    match &state.waiting_for {
        WaitingFor::LoopShortcut {
            proposer,
            predicted_winner: _,
            certificate,
            schema,
            declaration: _,
            road: _,
            counted_take: _,
            period: _,
        } => (*proposer, certificate, schema),
        other => panic!("expected a bounded LoopShortcut offer, got {other:?}"),
    }
}

/// PR-7 Phase 5b acceptance — the bounded offer FIRES on the real 4-player Dina/Conqueror
/// drain, at three living opponents, with a bound computed from the offer-beat board.
///
/// EVERY NUMBER IS COMPUTED IN-TEST from the offer-beat state. The chain's reported "32"
/// is not a fixture fact: it drifts with how many beats the drive takes to accumulate, and
/// this row recomputes `min over living opponents of (life - 1) / per-cycle loss` at the
/// beat the offer actually appeared.
///
/// REVERT-PROBES (each must FLIP to FAIL):
/// * remove `elimination_bounds`' `p.life as i64 - 1` headroom term ⇒ the recomputed bound
///   and the published one diverge ⇒ FAILS.
#[test]
fn dina_untargeted_drain_4p_offers_at_three_live_opponents() {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));

    // ── reach-guards on the loaded board; every assertion below is meaningless without them
    assert!(
        state.loop_detection.samples(),
        "reach-guard: a non-sampling mode never populates the ring, so no offer could ever \
         be raised and this row would be vacuous; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring — every frame the offer certifies \
         against was accumulated by THIS drive, not restored"
    );
    assert_eq!(
        engine_live_opponents(&state, P0).len(),
        3,
        "reach-guard: the whole point of this class is that Path A cannot crown, which needs \
         >= 2 non-fallers, i.e. three living opponents here"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }),
        "reach-guard: the dump must NOT ship at a saved offer — the offer is this row's \
         deliverable, not its input"
    );

    let beat = drive_to_bounded_offer(&mut state, 400).expect(
        "CR 732.2a: the bounded offer must FIRE on this real 4p drain. BASE (no Path D) drove \
         326 beats on this same dump and reached zero LoopShortcut beats, so a failure here is \
         the offer never being raised, not a fixture accident.",
    );

    let (proposer, certificate, schema) = bounded_offer_parts(&state);

    // ── the two binding field-value discriminators ──
    assert_eq!(
        proposer, state.active_player,
        "CR 732.2a: the proposer is the priority holder, and step (2) requires that to be the \
         active player the ring sampler gates on"
    );
    assert!(
        matches!(&state.waiting_for, WaitingFor::LoopShortcut { period, .. } if !period.is_empty()),
        "the recorded offer carries the confirmed period its take performs (beat {beat})"
    );
    assert!(
        schema.points.is_empty(),
        "the UNTARGETED class publishes no per-iteration choice, so the schema exposes no \
         decision points; got {:?}",
        schema.points
    );

    // ── the per-period signature, bound FROM the value ──
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .expect("a bounded offer publishes the per-period signature its bound was divided by");
    assert!(
        per_cycle.victim_slot.is_empty(),
        "no slot is published, so nothing is charged to a declared victim — the victims are \
         already visible in `delta.life`; got {:?}",
        per_cycle.victim_slot
    );
    assert!(
        per_cycle.delta != engine::analysis::resource::ResourceVector::default(),
        "a zero-delta cycle states no CR 704 threshold and must never be offered"
    );

    // ── the bound, RECOMPUTED from the offer-beat board ──
    let living_opponents: Vec<PlayerId> = engine_live_opponents(&state, proposer);
    assert_eq!(
        living_opponents.len(),
        3,
        "three living opponents must still be the population AT THE OFFER BEAT ({beat}), not \
         only at load — otherwise the bound below is computed over the wrong seats"
    );
    let mut losses: Vec<(PlayerId, i64, i64)> = vec![];
    for p in state.players.iter().filter(|p| !p.is_eliminated) {
        let loss = -per_cycle.delta.life.get(&p.id).copied().unwrap_or(0);
        losses.push((p.id, p.life as i64, loss));
    }
    let opponent_losses: Vec<i64> = losses
        .iter()
        .filter(|(id, _, _)| *id != proposer)
        .map(|(_, _, loss)| *loss)
        .collect();
    assert!(
        opponent_losses.iter().all(|&l| l > 0),
        "REACH-GUARD against a degenerate fixture: every living opponent must actually be \
         LOSING life per cycle, else the CR 704.5a headroom term never narrows and the bound \
         below would be the safety cap for the wrong reason; measured {losses:?}"
    );
    // The life axis is the only one this re-derivation reads, so a period that also consumed a
    // seat's library or poison is a board it cannot answer for.
    for p in state.players.iter().filter(|p| !p.is_eliminated) {
        assert_eq!(
            (
                per_cycle
                    .delta
                    .library_delta
                    .get(&p.id)
                    .copied()
                    .unwrap_or(0),
                per_cycle.delta.poison.get(&p.id).copied().unwrap_or(0),
            ),
            (0, 0),
            "FIXTURE GAP: the recomputation below narrows on CR 704.5a life alone, and {:?} \
             carries another consuming axis",
            p.id
        );
    }
    let strict: Vec<i64> = losses
        .iter()
        .filter(|(_, _, loss)| *loss > 0)
        .map(|(_, life, loss)| (life - 1) / loss)
        .collect();
    assert!(
        !strict.is_empty(),
        "at least one seat is losing life, asserted above"
    );
    let proposer_strict = losses
        .iter()
        .find(|(id, _, _)| *id == proposer)
        .filter(|(_, _, loss)| *loss > 0)
        .map(|(_, life, loss)| (life - 1) / loss);
    let expected_bound = crate::loop_shortcut_drain_boards::cascade_count_from_strict(
        &strict,
        proposer_strict,
        i64::from(crate::fantastic_four_bounded_loop::MAX_SHORTCUT_CYCLES_MIRROR),
    );
    assert_eq!(
        schema.measured_repetition_bound.map(i64::from),
        Some(expected_bound),
        "CR 704.5a: the published bound spans the WHOLE cascade this period drives — the last \
         repetition at which a living seat crosses, not the first. Recomputed here from the \
         offer-beat board {losses:?} at beat {beat}"
    );
    assert!(
        strict.iter().min() != strict.iter().max(),
        "REACH-GUARD: the consumed seats must cross on DIFFERENT repetitions, or the cascade's \
         last entry coincides with its first and the assertion above cannot tell the two \
         quantifiers apart; measured {losses:?}"
    );
    assert_eq!(
        schema.iteration_count,
        engine::analysis::decision_template::IterationCount::Fixed(schema.deliverable_capacity),
        "CR 732.1b: the offer's own declaration drives the whole cascade here, and with no \
         charged slot there is nothing for a witness declaration to re-aim, so the SUGGESTION \
         and the CEILING coincide on this class"
    );
    assert_eq!(
        (
            &schema.iteration_count,
            offered(&state).2,
            seat_lives(&state)
        ),
        (
            &IterationCount::Fixed(37),
            Some(P0),
            vec![(P0, 47), (P1, 36), (P2, 32), (P3, 37)]
        ),
        "CR 119.3 + CR 104.2a: each opponent loses 1 a repetition, so the last, P3 at 37, leaves \
         at the 37th and P0 wins"
    );
    assert!(
        schema.is_bounded(),
        "the whole claim of this producer is that it MEASURED a repetition threshold; \
         measured bound = {:?}",
        schema.measured_repetition_bound
    );

    // ── siblings: nothing terminal happened, and no unbounded mark was written ──
    assert_eq!(
        certificate.win_kind,
        Some(engine::analysis::loop_check::WinKind::LethalDamage),
        "CR 704.5a: a life drain is not `Advantage`, which is the conjunct that keeps this \
         seam disjoint from Path C's class"
    );
    assert!(
        state.unbounded_resources.is_empty(),
        "an OFFER is not a grant: no unbounded mark may be written by raising a bounded \
         offer; got {:?}",
        state.unbounded_resources
    );

    // ── F6: THE PREVIEW REACHES A REAL GAME ──
    //
    // Every other row that asserts anything about the CR 732.2a count preview hand-builds a
    // `WaitingFor` and projects it. That leaves one hole none of them can see: the preview is
    // published only when the offer pairs `per_cycle: Some` with a FINITE count, and the
    // producer that pairs them is the bounded one. This row closes that by reading the preview
    // off the real 4p dump the engine itself raised the offer on.
    //
    // WHAT WRONG IMPLEMENTATION WOULD STILL PASS THIS ROW? One that publishes the preview only
    // on this fixture's exact per-period shape (the hand-built rows cover the shape space), and
    // one that previews the right numbers for a count the player did not pick — the reserved
    // per-selected-count question, deliberately not answered here.
    let suggested = i64::from(schema.deliverable_capacity);
    let life_deltas: Vec<(PlayerId, i64)> = per_cycle
        .delta
        .life
        .iter()
        .filter(|(_, delta)| **delta != 0)
        .map(|(seat, delta)| (*seat, *delta))
        .collect();
    assert!(
        life_deltas.len() >= 2,
        "reach-guard: the preview's per-seat fold only discriminates when the period moves \
         MORE THAN ONE seat's life — a single-seat period is satisfiable by an implementation \
         that keys every entry to the proposer; measured {life_deltas:?}"
    );

    engine::game::interaction::bind_interaction_authority(
        &mut state,
        engine::types::interaction::InteractionSessionId("dina-preview".to_string()),
    )
    .expect("the offer beat binds an interaction authority");
    let filtered = engine::game::visibility::filter_state_for_viewer(&state, proposer);
    let view = engine::game::interaction::derive_viewer_interaction(&state, &filtered, proposer);
    let engine::types::interaction::InteractionOpportunityResponse::Schema {
        spec:
            engine::types::interaction::InteractionResponseSpec::Shortcut {
                count,
                points,
                preview,
                ..
            },
        ..
    } = &view.opportunities[0].response
    else {
        panic!("the bounded offer publishes a shortcut schema to its proposer");
    };
    let engine::types::interaction::InteractionShortcutCountSpec::Fixed { min, max, .. } = count
    else {
        panic!("a bounded offer publishes a Fixed count window, got {count:?}");
    };
    let published: Vec<u32> = preview.iter().map(|element| element.count).collect();
    for endpoint in [*min, *max] {
        assert!(
            published.contains(&endpoint),
            "CR 732.2a: the window's own {endpoint} must be published; got {published:?}"
        );
    }

    // This board announces NO decision point at all, so there is nothing to allocate a count
    // over — the negative half of the emptiness rule, on a real game rather than a synthetic.
    assert!(
        points.is_empty(),
        "reach-guard: this drain announces no decision point, which is what makes the empty \
         allocations below a property of the offer rather than of the producer; got {points:?}"
    );
    assert!(
        preview.iter().all(|element| element.allocation.is_empty()),
        "an offer announcing no target publishes no split"
    );

    // Every published element states ITS OWN count's product, so the per-seat fold is checked
    // across the whole sample and not only at the suggested count.
    for element in preview {
        let mut expected: Vec<(Option<u8>, i32)> = life_deltas
            .iter()
            .map(|(seat, delta)| (Some(seat.0), (delta * i64::from(element.count)) as i32))
            .collect();
        let mut stated: Vec<(Option<u8>, i32)> = element
            .entries
            .iter()
            .filter(|entry| {
                entry.family == engine::types::interaction::InteractionShortcutPreviewFamily::Life
            })
            .map(|entry| (entry.player, entry.amount))
            .collect();
        expected.sort_unstable();
        stated.sort_unstable();
        assert_eq!(
            stated, expected,
            "CR 119.3: with no announced slot to charge, every life seat keeps the seat \
             `payload_seat` gave it at the raw product of its own count {}",
            element.count
        );
    }

    let preview = preview
        .iter()
        .find(|element| i64::from(element.count) == suggested)
        .expect(
            "CR 732.2a: the offer the engine raised on a REAL 4p drain must publish what its \
             suggested count does. An absent element means every preview has vanished from \
             every real game while the hand-built projection rows stayed green.",
        );

    let mut expected: Vec<(Option<u8>, i32)> = life_deltas
        .iter()
        .map(|(seat, delta)| (Some(seat.0), (delta * suggested) as i32))
        .collect();
    let mut published: Vec<(Option<u8>, i32)> = preview
        .entries
        .iter()
        .filter(|entry| {
            entry.family == engine::types::interaction::InteractionShortcutPreviewFamily::Life
        })
        .map(|entry| (entry.player, entry.amount))
        .collect();
    expected.sort_unstable();
    published.sort_unstable();
    assert_eq!(
        published, expected,
        "CR 119.3: every seat the certified period moves life on is previewed at that seat, \
         multiplied out by the declared count — recomputed here from the offer-beat certificate"
    );
    for (seat, _, loss) in losses.iter().filter(|(id, _, _)| *id != proposer) {
        assert!(
            published.contains(&(Some(seat.0), (-loss * suggested) as i32)),
            "CR 119.3: victim seat {seat:?} loses {loss} per cycle, so its previewed life \
             entry must be the NEGATIVE finished magnitude on that seat's own key — a \
             proposer-keyed subject map publishes it on the wrong HUD; got {published:?}"
        );
    }
}

/// Drive a `GameScenario`-built board until the ENGINE writes a bounded offer, declining
/// nothing and injecting nothing. Returns the beat, or `None` if the cap ran out. Reads
/// `state.waiting_for` — the production Path D write — never an out-of-band predicate call.
fn drive_scenario_to_bounded_offer(runner: &mut GameRunner, cap: usize) -> Option<usize> {
    for beat in 0..cap {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::LoopShortcut {
                predicted_winner: None,
                ..
            }
        ) {
            return Some(beat);
        }
        if dump_drive_one_beat(runner.state_mut(), None).is_err() {
            return None;
        }
    }
    None
}

/// PR-7 Phase 5b — the MULTIPLAYER offer control: the untargeted every-opponent drain raises a
/// bounded offer at THREE and at FOUR players, not only at two.
///
/// FIXTURE — the pure life↔life cascade, both halves verbatim real-card Oracle text verified
/// against the Scryfall API: Marauding Blight-Priest (*"Whenever you gain life, each opponent
/// loses 1 life."*) plus Exquisite Blood (*"Whenever an opponent loses life, you gain that much
/// life."*). Untargeted and every-opponent, which is the true full-multiplayer drain and the
/// same class as the real 4p `dina_conqueror_4p` dump.
///
/// UNEQUAL OPPONENT LIFE IS LOAD-BEARING, not decoration. With every opponent falling, the
/// living partition is `nonfallers == {P0}`, and at EQUAL life `live_mandatory_loop_winner`'s
/// CR 704.3 simultaneity floor passes and Path A crowns P0 while the kick-off is still
/// resolving — measured, at 2, 3 and 4 players. Staggering the totals fails
/// `fallers_lives_pairwise_equal`, which is what leaves the board in the "lethal to some,
/// crowns nobody" regime the bounded offer exists to serve.
#[test]
fn multiplayer_pure_life_drain_offers_at_three_and_four_players() {
    /// Marauding Blight-Priest, verbatim (Scryfall).
    const BLIGHT_PRIEST: &str = "Whenever you gain life, each opponent loses 1 life.";
    /// Exquisite Blood, verbatim (Scryfall).
    const EXQUISITE_BLOOD: &str = "Whenever an opponent loses life, you gain that much life.";

    /// Build the cascade at `seats` players with staggered opponent life, cast the kick-off,
    /// and return the runner plus the seats the engine considers living opponents of P0.
    fn cascade(seats: u8) -> GameRunner {
        let mut scenario = GameScenario::new_n_player(seats, 7);
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        for (i, seat) in (1..seats).map(PlayerId).enumerate() {
            // Staggered: pairwise-UNEQUAL absolute life, equal per-cycle delta.
            scenario.with_life(seat, 1000 + 50 * i as i32);
        }
        scenario.add_creature_from_oracle(P0, "Test Blight Priest", 2, 2, BLIGHT_PRIEST);
        scenario.add_creature_from_oracle(P0, "Test Exquisite Blood", 2, 2, EXQUISITE_BLOOD);
        let kickoff = scenario
            .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
            .id();
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        let _ = runner.cast(kickoff).resolve();
        runner
    }

    for seats in [3u8, 4] {
        let mut runner = cascade(seats);
        let opponents = engine_live_opponents(runner.state(), P0);
        assert_eq!(
            opponents.len(),
            usize::from(seats) - 1,
            "reach-guard at {seats} players: every opponent must still be living at the offer \
             beat, else the bound below is computed over the wrong seats"
        );
        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
            "reach-guard at {seats} players: the staggered life totals must keep Path A from \
             crowning while the kick-off resolves — at EQUAL life it does, and then there is \
             no board left to offer on; got {:?}",
            runner.state().waiting_for
        );

        let beat = drive_scenario_to_bounded_offer(&mut runner, 200).unwrap_or_else(|| {
            panic!(
                "CR 732.2a: the untargeted every-opponent drain must raise a bounded offer at \
                 {seats} players. This is the multiplayer half of the claim — a 2-player-only \
                 detector is not what this lane ships."
            )
        });

        let state = runner.state();
        let (proposer, certificate, schema) = bounded_offer_parts(state);
        assert_eq!(
            proposer, state.active_player,
            "{seats}p: CR 732.2a step (2) — the proposer is the active priority holder"
        );
        assert!(
            schema.points.is_empty(),
            "{seats}p: the UNTARGETED class exposes no decision points; got {:?}",
            schema.points
        );

        let per_cycle = certificate
            .per_cycle
            .as_ref()
            .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
            .expect("a bounded offer publishes its per-period signature");
        assert_eq!(
            (&schema.iteration_count, offered(state).2),
            (&IterationCount::Fixed(1000), None),
            "{seats}p: the count is the shortcut budget, short of every opponent's crossing, so \
             nobody is crowned"
        );

        // EVERY living opponent loses life every cycle — the multiplayer content of the claim.
        // A 2-player-shaped detector that only ever charges one seat fails here.
        let losses: Vec<(PlayerId, i64)> = opponents
            .iter()
            .map(|p| (*p, -per_cycle.delta.life.get(p).copied().unwrap_or(0)))
            .collect();
        assert!(
            losses.iter().all(|(_, loss)| *loss > 0),
            "{seats}p: the published per-cycle δ must charge EVERY living opponent, which is \
             what makes this the untargeted multiplayer class; measured {losses:?} at beat \
             {beat}"
        );

        // The bound, RECOMPUTED from the offer-beat board.
        let strict: Vec<i64> = state
            .players
            .iter()
            .filter(|p| !p.is_eliminated)
            .filter_map(|p| {
                let loss = -per_cycle.delta.life.get(&p.id).copied().unwrap_or(0);
                (loss > 0).then(|| (p.life as i64 - 1) / loss)
            })
            .collect();
        assert!(
            !strict.is_empty(),
            "{seats}p: at least one seat is losing life, asserted above"
        );
        let proposer_strict = {
            let loss = -per_cycle.delta.life.get(&proposer).copied().unwrap_or(0);
            let life = state
                .players
                .iter()
                .find(|p| p.id == proposer)
                .expect("the proposer is on the board")
                .life as i64;
            (loss > 0).then(|| (life - 1) / loss)
        };
        let expected_bound = crate::loop_shortcut_drain_boards::cascade_count_from_strict(
            &strict,
            proposer_strict,
            i64::from(crate::fantastic_four_bounded_loop::MAX_SHORTCUT_CYCLES_MIRROR),
        );
        assert_eq!(
            schema.measured_repetition_bound.map(i64::from),
            Some(expected_bound),
            "{seats}p: CR 704.5a — the published bound spans the whole cascade: the LAST \
             repetition at which a living seat crosses. The staggered life totals are what make \
             that a different number from the first crossing; recomputed here from the \
             offer-beat board"
        );
        assert!(
            schema.is_bounded(),
            "{seats}p: this producer's whole claim is that it NARROWED the bound; \
             measured bound = {:?}",
            schema.measured_repetition_bound
        );
    }
}

/// PR-7 Phase 5b — a declared count ABOVE the offered bound is handed back fail-closed.
///
/// **TEST-ONLY ROW, ZERO NEW PRODUCTION CODE.** The guard already ships
/// (`handle_declare_shortcut`'s `Fixed(n) if *n > offer.schema.deliverable_capacity` arm). It was
/// unbuildable before this phase because no producer narrowed the bound below
/// `MAX_SHORTCUT_CYCLES`, so the comparison was inert; the bounded offer is the first
/// producer that can exercise it. Do not read this row as new mechanism.
///
/// REVERT-PROBE: delete that arm ⇒ the over-bound count is accepted, APNAP opens, and the
/// proposal drives past a CR 704.5a threshold INSIDE the proposal ⇒ the zero-elimination
/// assertion FAILS.
/// MUST-NOT-FLIP: `over_cap_fixed_count_hands_back_with_no_drive` (the global-cap arm) and
/// every unbounded offer's acceptance of any `Fixed(n <= MAX)`.
///
/// ⚠ WHAT SEPARATES THE ARMS, and why this row was NON-DISCRIMINATING until fix round 1.
/// `lives_after == lives_before` + zero eliminations is the handback observation — but at
/// `c6d834040` the ACCEPTED, within-bound path produced the identical observation on this same
/// dump, because `materialize_fixed_shortcut` aborted at cycle 0 and committed nothing. The row
/// therefore could not tell "handed back" from "accepted and driven", and would have stayed
/// green with the guard deleted. It discriminates now because the accepted path MOVES LIFE:
/// `bounded_fixed_count_commits_exactly_n_periods` measures `n × δ` committed on this exact
/// dump (`n=1` → `[50,34,30,35]`, `n=3` → `[52,32,28,33]` from `[49,35,31,36]`). That row is
/// this one's positive control; the `WaitingFor::Priority` + no-`RespondToShortcut` assertions
/// below are what separate a handback from a completed drive, since both end at priority.
#[test]
fn declared_count_above_the_offered_bound_is_handed_back() {
    use engine::analysis::decision_template::IterationCount;

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400)
        .expect("the bounded offer must fire; see the acceptance row");
    let (proposer, _, schema) = bounded_offer_parts(&state);
    let bound = schema.deliverable_capacity;
    assert!(
        schema.is_bounded(),
        "REACH-GUARD: this row is about the PER-OFFER bound, so the offer must have narrowed \
         one — at `MAX_SHORTCUT_CYCLES` the global-cap arm would answer instead and the row \
         would test the wrong guard; got {bound}"
    );
    let lives_before: Vec<i32> = state.players.iter().map(|p| p.life).collect();
    let eliminated_before = state.players.iter().filter(|p| p.is_eliminated).count();

    let result = apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(bound + 1),
            template: None,
        },
    )
    .expect("the declare is a legal action; it is REFUSED by being handed back, not by Err");

    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: an over-bound count contains a conditional action, so it is handed back to \
         ordinary priority — no APNAP window, no drive; got {:?}",
        result.waiting_for
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "the CR 732.2b response window must never open for a rejected declaration"
    );
    assert_eq!(
        state.players.iter().filter(|p| p.is_eliminated).count(),
        eliminated_before,
        "ZERO eliminations: the whole reason the bound exists is that a count above it crosses \
         a CR 704.5a threshold inside the proposal"
    );
    assert_eq!(
        state.players.iter().map(|p| p.life).collect::<Vec<_>>(),
        lives_before,
        "zero committed cycles ⇒ no life moved"
    );
}

// ─────────────── PR-7 Phase 5c — the MANDATORY-DRAW cascade, ≥3 players ───────────────

/// Psychosis Crawler, verbatim (Scryfall).
const PSYCHOSIS_CRAWLER: &str = "Psychosis Crawler's power and toughness are each equal to the number of cards in your hand.\nWhenever you draw a card, each opponent loses 1 life.";

/// `bloodloop` at N players: Psychosis Crawler and Kefka, Ruler of Ruin ("Whenever an opponent
/// loses life during your turn, you draw that many cards.") with five cards in P0's hand, a
/// within-turn mandatory drain cascade on P0's turn whose every cycle also draws, so the board
/// never recurs (a card moves library to hand each cycle).
fn bloodloop_state(players: u8) -> GameState {
    let mut scenario = GameScenario::new_n_player(players, 7);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..players {
        scenario.with_life(PlayerId(i), 20);
    }
    scenario.add_creature_from_oracle(P0, "Psychosis Crawler", 0, 0, PSYCHOSIS_CRAWLER);
    scenario.add_creature_from_oracle(P0, "Kefka, Ruler of Ruin", 5, 7, KEFKA_RULER_OF_RUIN);
    for i in 0..5 {
        scenario.add_card_to_hand(P0, &format!("Held {i}"));
    }
    let names: Vec<String> = (0..60).map(|i| format!("Filler {i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    for i in 0..players {
        scenario.with_library_top(PlayerId(i), &refs);
    }
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner.state().clone()
}

/// PR-7 Phase 5c ACCEPTANCE — the bounded CR 732.2a offer fires on a MANDATORY-DRAW
/// cascade at 2, 3 and 4 players.
///
/// # Why the beats are pinned literals
///
/// The drive is deterministic — fixed scenario seed, `dump_drive_one_beat`'s policy is
/// total (always pass at `Priority`, first legal answer otherwise), and no RNG is on the
/// path. Re-run identical across runs. An offer beat is observable behaviour that this
/// commit CHANGED; pinning it is what stops a later change from moving it silently.
#[test]
fn bloodloop_mandatory_draw_cascade_offers_at_2p_3p_and_4p() {
    for (players, expected_beat, expected_turn) in [(2u8, 31usize, 4u32), (3, 68, 5), (4, 118, 6)] {
        let mut state = bloodloop_state(players);
        let beat = drive_to_bounded_offer(&mut state, 400).unwrap_or_else(|| {
            panic!(
                "{players}p mandatory-draw cascade must raise a bounded offer; a `None` here is \
                 the pre-widening behaviour (step (6) refusing every draw-bearing stack)"
            )
        });
        assert_eq!(beat, expected_beat, "{players}p offer beat");
        assert_eq!(state.turn_number, expected_turn, "{players}p offer turn");

        let (proposer, certificate, schema) = bounded_offer_parts(&state);
        assert_eq!(
            proposer, P0,
            "{players}p: the cascade's controller proposes"
        );
        assert!(
            schema.is_bounded(),
            "{players}p: an unbounded schema would take a different declare arm"
        );
        assert_eq!(
            (
                &schema.iteration_count,
                schema.measured_repetition_bound,
                offered(&state).2
            ),
            (&IterationCount::Fixed(19), Some(19), Some(P0)),
            "{players}p: CR 704.3 + CR 104.2a: every opponent, at 19, leaves together at 19, so \
             P0's library crossing never comes"
        );
        let per_cycle = certificate
            .per_cycle
            .as_ref()
            .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
            .expect("a bounded offer states its per-period signature");

        // The multiplayer property the ≥3p rows exist for: ONE cycle charges EVERY
        // opponent, not just the first. A 2p-only guard could not see this.
        let opponents: Vec<PlayerId> = state
            .players
            .iter()
            .map(|p| p.id)
            .filter(|p| *p != P0)
            .collect();
        assert_eq!(opponents.len(), usize::from(players) - 1);
        for opponent in &opponents {
            assert_eq!(
                per_cycle.delta.life.get(opponent).copied(),
                Some(-1),
                "{players}p: one cycle drains {opponent:?}"
            );
        }
        assert_eq!(
            per_cycle.delta.life.get(&P0).copied().unwrap_or(0),
            0,
            "{players}p: the cascade's controller loses no life"
        );
    }
}

/// Verbatim Oracle text of Leyline of Sanctity.
const LEYLINE_OF_SANCTITY: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nYou have hexproof. (You can't be the target of spells or abilities your opponents control.)";

/// CR 601.2c + CR 603.3d + CR 119.3: with P2 hexproof, Sanguine Bond's "target opponent" has the
/// one legal target P1, so each repetition charges P1 its loss once. P1 at 3 leaves at the third
/// (CR 704.5a), and the next trigger has no legal target, so P2 stays in the game.
/// `Test Lifegain Kickoff` ("You gain 1 life.") only starts the drain; no printed card has exactly
/// that text, and the nearest is Blessed Wine ("You gain 1 life. Draw a card at the beginning of
/// the next turn's upkeep.").
#[test]
fn a_drain_whose_only_target_is_forced_counts_each_loss_once() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 4);
    scenario.with_life(P2, 5);
    scenario.add_enchantment_from_oracle(P0, "Sanguine Bond", SANGUINE_BOND);
    scenario.add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 3, 4, BLOODTHIRSTY_CONQUEROR);
    scenario.add_enchantment_from_oracle(P2, "Leyline of Sanctity", LEYLINE_OF_SANCTITY);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    let _ = runner.cast(kickoff).commit();
    let mut state = runner.state().clone();

    drive_to_bounded_offer(&mut state, 50).expect("reach-guard: the drain is offered");
    let (proposer, _, schema) = bounded_offer_parts(&state);
    assert_eq!(
        seat_lives(&state)[1..],
        [(P1, 3), (P2, 5)],
        "reach-guard: the offer frame"
    );
    assert_eq!(
        schema.iteration_count,
        IterationCount::Fixed(3),
        "CR 704.5a: P1 at 3 losing 1 per repetition leaves at the third"
    );

    r6a_declare_and_accept_all(&mut state, proposer, 3);
    assert_eq!(
        (
            state
                .players
                .iter()
                .map(|p| p.is_eliminated)
                .collect::<Vec<_>>(),
            state.players[2].life
        ),
        (vec![false, true, false], 5),
        "CR 704.5a + CR 800.4: P1 leaves and the game goes on with P2; got {:?}",
        state.waiting_for
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "no winner: P2 is still in the game; got {:?}",
        state.waiting_for
    );
}

// ═══════════════ FIX ROUND 1 — the declared count is CONSUMABLE ═══════════════
//
// The rows below bind `n` to the OBSERVED board delta and to the OFFER's own published
// signature, never to a literal.

/// Declare `Fixed(n)` on the bounded offer `state` is parked at, accept with every living
/// opponent through `apply()`, and return the per-seat life delta the drive COMMITTED
/// alongside the signature and bound the offer published.
///
/// Everything is read off the production state: the signature comes from the offer the ENGINE
/// wrote, the delta from `Player::life` before and after. Nothing is recomputed by the test.
fn accept_bounded_fixed(
    state: &mut GameState,
    n: u32,
) -> (
    Vec<(PlayerId, i64)>,
    engine::analysis::resource::PeriodicDelta,
    u32,
) {
    let (proposer, certificate, schema) = bounded_offer_parts(state);
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes the per-period signature its bound was divided by");
    let bound = schema.deliverable_capacity;
    let before: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .map(|p| (p.id, p.life as i64))
        .collect();
    r6a_declare_and_accept_all(state, proposer, n);
    let committed: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .zip(&before)
        .map(|(p, (id, l0))| {
            assert_eq!(
                p.id, *id,
                "the seat vector is positional and never reordered"
            );
            (p.id, p.life as i64 - l0)
        })
        .collect();
    (committed, per_cycle, bound)
}

/// FIX ROUND 1 PRIMARY (HIGH-1/HIGH-2/HIGH-3) — an accepted `Fixed(n)` within the offered
/// bound commits **exactly `n` copies of the published per-period delta**, on the real 4p
/// Dina/Conqueror dump and on the synthetic mandatory-draw cascade at 3 and 4 players.
///
/// # What the assertion is bound to
///
/// `n * per_cycle.delta.life[seat]`, derived from the certificate the ENGINE published at the
/// offer beat. No literal life total appears in an equality. A fixture whose drain rate drifts
/// moves both sides together; a drive that commits the wrong NUMBER of periods moves only one.
///
/// # Why this is not vacuous
///
/// * Reach-guards below establish that the published δ is non-zero, that at least two seats
///   carry a non-zero term (so a 2-player-shaped bug cannot hide), and that the bound leaves
///   room for `n = 3` — without which the `n`-scaling is untestable.
/// * The `n = 1` vs `n = 3` boards are asserted DIFFERENT on the same fixture. That single
///   assertion is the direct antidote to the defect: at `c6d834040` they were byte-identical.
#[test]
fn bounded_fixed_count_commits_exactly_n_periods() {
    /// Rebuilt from scratch per `n` — a driven trajectory is not replayable from a used state.
    fn build(name: &str) -> GameState {
        match name {
            "dina_conqueror_4p" => restore_dump(&gunzip_dump(include_bytes!(
                "../fixtures/dina_conqueror_4p.json.gz"
            ))),
            "bloodloop3" => bloodloop_state(3),
            "bloodloop4" => bloodloop_state(4),
            other => panic!("unknown fixture {other}"),
        }
    }

    for name in ["dina_conqueror_4p", "bloodloop3", "bloodloop4"] {
        let mut boards: Vec<Vec<(PlayerId, i64)>> = vec![];
        for n in [1u32, 3] {
            let mut state = build(name);
            drive_to_bounded_offer(&mut state, 400).unwrap_or_else(|| {
                panic!("{name}: the bounded offer must fire; see the acceptance row")
            });

            let (committed, per_cycle, bound) = accept_bounded_fixed(&mut state, n);

            // ── reach-guards: without these the equality below can pass degenerately ──
            assert!(
                per_cycle.delta != engine::analysis::resource::ResourceVector::default(),
                "{name}: a zero-delta period makes `n * δ` zero for every `n`, so the scaling \
                 assertion would hold for a drive that committed nothing"
            );
            assert!(
                per_cycle.delta.life.values().filter(|v| **v != 0).count() >= 2,
                "{name}: fewer than two seats with a non-zero life term is a 2-player shape; \
                 the whole class exists because a MULTIPLAYER drain crowns nobody. got {:?}",
                per_cycle.delta.life
            );
            assert!(
                bound >= 3,
                "{name}: `n = 3` must be WITHIN the offered bound, else the declaration is \
                 handed back and this row silently tests the rejection arm; bound = {bound}"
            );

            // ── THE PROPERTY: committed delta == n × published per-period delta ──
            for (seat, delta) in &committed {
                assert_eq!(
                    *delta,
                    i64::from(n) * per_cycle.delta.life.get(seat).copied().unwrap_or(0),
                    "{name} n={n}: {seat:?}'s committed life delta must be exactly `n` copies \
                     of the period the offer published ({:?}); committed {committed:?}",
                    per_cycle.delta.life
                );
            }

            // ── the bound's own contract: a count strictly BELOW the bound crosses no
            //    threshold at all, and the bound itself crosses at most one — this sweep runs
            //    at `n = 1..3` under a `bound >= 3` reach-guard, so every seat survives ──
            assert_eq!(
                state.players.iter().filter(|p| p.is_eliminated).count(),
                0,
                "{name} n={n}: CR 704.5a — a count strictly below the published bound leaves \
                 every seat inside its own headroom, and the bound is measured far above the \
                 counts this sweep drives"
            );
            assert!(
                state.players.iter().all(|p| p.life > 0),
                "{name} n={n}: every seat is above the CR 704.5a threshold; lives {:?}",
                state.players.iter().map(|p| p.life).collect::<Vec<_>>()
            );
            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "{name} n={n}: a completed finite drive hands back to ordinary priority, \
                 not a terminal state; got {:?}",
                state.waiting_for
            );

            boards.push(committed);
        }

        // ── THE DISCRIMINATOR. At `c6d834040` these two were identical for every fixture.
        assert_ne!(
            boards[0], boards[1],
            "{name}: `Fixed(1)` and `Fixed(3)` must produce MEASURABLY different boards — \
             identical outcomes are exactly the defect this round fixes"
        );
    }
}

/// ITEM 2 (CR 732.2a) — the ACCEPT side: an opponent's play in the window's trace must not divert
/// an accepted bounded grant away from the drain materializer.
///
/// **SITE F IS NOT ON THIS PATH, and that is asserted rather than assumed** — dina's bounded offer
/// publishes an EMPTY point set, so the `template: None` declaration this row makes falls to the
/// admitting `None` arm and never reaches the declare-seam period test.
///
/// **THE PROPERTY**: the committed life delta is exactly `n ×` the published per-period delta —
/// i.e. the drain materializer ran. Positive control on the same fixture and same helper:
/// [`bounded_fixed_count_commits_exactly_n_periods`], whose reach-guards (non-zero δ, ≥ 2 seats
/// moving, bound ≥ 3) are repeated here because without them `n × δ` is satisfied by a drive that
/// committed nothing.
#[test]
fn an_accepted_bounded_grant_drains_even_with_a_foreign_period_in_state() {
    const N: u32 = 3;

    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400)
        .expect("the bounded offer must fire; see the acceptance row");

    let (proposer, _, schema) = bounded_offer_parts(&state);
    assert!(
        schema.points.is_empty(),
        "REACH-GUARD / SCOPE: this row isolates the MATERIALIZE dispatch. A non-empty point set \
         would drag `handle_declare_shortcut`'s declare-seam arm into the same measurement and \
         the outcome would no longer attribute to one site; got {:?}",
        schema.points
    );
    let opp = *engine_live_opponents(&state, proposer)
        .first()
        .expect("REACH-GUARD: the foreign period needs a living opponent to belong to");

    // An opponent's play, standing in the window's trace at the moment the grant is accepted.
    let any_object = *state.objects.keys().next().expect("the dump has objects");
    engine::game::install_plays_for_tests(
        &mut state,
        &[(opp, engine::game::PlayLocus::Cast(any_object))],
    );
    assert_ne!(
        opp, proposer,
        "REACH-GUARD: the injected play is another seat's"
    );

    let (committed, per_cycle, bound) = accept_bounded_fixed(&mut state, N);

    // ── reach-guards: without these `n × δ` holds degenerately for a drive that did nothing ──
    assert!(
        per_cycle.delta != engine::analysis::resource::ResourceVector::default(),
        "a zero-delta period makes `n × δ` zero for every `n`, so the equality below would hold \
         for a drive that committed nothing — which is the exact failure mode this row exists to \
         catch"
    );
    assert!(
        per_cycle.delta.life.values().filter(|v| **v != 0).count() >= 2,
        "fewer than two seats with a non-zero life term is a 2-player shape; got {:?}",
        per_cycle.delta.life
    );
    assert!(
        bound >= N,
        "`n = {N}` must be WITHIN the offered bound, else the declaration is handed back and \
         this row silently tests the rejection arm; bound = {bound}"
    );

    // ── THE PROPERTY: the DRAIN materializer ran, not the object-growth one ──
    for (seat, delta) in &committed {
        assert_eq!(
            *delta,
            i64::from(N) * per_cycle.delta.life.get(seat).copied().unwrap_or(0),
            "CR 732.2a: with a FOREIGN play in the trace the accepted grant must still commit \
             exactly `n` copies of the published per-period delta ({:?}); a zero here is a \
             misroute away from the drain. {seat:?} committed {committed:?}",
            per_cycle.delta.life
        );
    }
}

/// CR 732.2a — the DECLINE seam: one seat's decline discards only its own plays from the window's
/// trace, never another seat's.
///
/// | arm | trace at the decline | assertion |
/// |---|---|---|
/// | FOREIGN | an opponent's play | survives, no re-offer |
/// | OWN | the proposer's play | discarded, no re-offer |
///
/// The decline is driven through the production `apply()` reducer, so the post-return reconcile
/// runs too: a re-nag would leave `waiting_for` on a `LoopShortcut`.
#[test]
fn declining_a_shortcut_discards_only_the_decliners_own_driving_period() {
    // One real driven bounded offer, re-derived per arm so neither arm inherits the other's board.
    let offer_state = || {
        let mut state = restore_dump(&gunzip_dump(include_bytes!(
            "../fixtures/dina_conqueror_4p.json.gz"
        )));
        drive_to_bounded_offer(&mut state, 400)
            .expect("the bounded offer must fire; see the acceptance row");
        state
    };
    let decline = |mut state: GameState, plays: &[(PlayerId, engine::game::PlayLocus)]| {
        let (proposer, _, _) = bounded_offer_parts(&state);
        engine::game::install_plays_for_tests(&mut state, plays);
        assert_eq!(
            trace_seats(&state).len(),
            plays.len(),
            "REACH-GUARD: the arm is vacuous unless a period is actually accumulating when the \
             decline lands"
        );
        apply(&mut state, proposer, GameAction::DeclineShortcut)
            .expect("the proposer may always decline their own offer (CR 732.2a)");
        state
    };

    // ── FOREIGN: seat B's period is mid-accumulation when seat A declines ──
    let state = offer_state();
    let (proposer, _, _) = bounded_offer_parts(&state);
    let opp = *engine_live_opponents(&state, proposer)
        .first()
        .expect("REACH-GUARD: the foreign period needs a living opponent to belong to");
    assert_ne!(
        opp, proposer,
        "REACH-GUARD: a period injected for the PROPOSER would be the OWN arm"
    );
    let any_object = *state.objects.keys().next().expect("the dump has objects");
    let state = decline(state, &[(opp, engine::game::PlayLocus::Cast(any_object))]);
    assert_eq!(
        trace_seats(&state),
        vec![opp],
        "CR 732.2a: {proposer:?} declining their own offer must leave {opp:?}'s accumulating \
         period intact"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: preserving {opp:?}'s foreign period must not re-offer {proposer:?}'s declined \
         shortcut within the same `apply()`; got {:?}",
        state.waiting_for
    );

    // ── OWN: the must-not-flip half. The Seam-2 suppressor is load-bearing for the decliner. ──
    let state = decline(
        offer_state(),
        &[(proposer, engine::game::PlayLocus::Cast(any_object))],
    );
    assert!(
        trace_seats(&state).is_empty(),
        "CR 732.2a: the decliner's OWN period must still be discarded; seats = {:?}",
        trace_seats(&state)
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "and the declined offer must not have been re-raised within the same `apply()`; got {:?}",
        state.waiting_for
    );
}

/// The seats of the window trace's plays and resolutions, in order.
fn trace_seats(state: &GameState) -> Vec<PlayerId> {
    use engine::game::EntryKind;
    engine::game::play_trace_view(state).map_or_else(Vec::new, |view| {
        view.entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    EntryKind::Play { .. } | EntryKind::Resolution { .. }
                )
            })
            .map(|entry| entry.seat)
            .collect()
    })
}

/// FIX ROUND 1 MIRROR (A1.2) — the drive STOPS AT the first lethal cycle. It does not commit
/// `n` cycles blindly and reconcile the deaths afterwards.
///
/// # Why the offer has to be doctored, and why that is the honest construction
///
/// `elimination_bounds` admits a CR 704.5a crossing only as the sequence's FINAL iteration, and
/// only when exactly ONE seat sits at the strict floor. The fixture below is SYMMETRIC, so that
/// conjunct never fires and its published bound stays strictly short — `n * loss <= life - 1`
/// for every seat at every legal `n`. Such a board cannot reach its own CROSS-LETHAL arm through
/// an undoctored offer, and a mirror row built on one would be unbuildable rather than merely
/// weak.
///
/// So this row is a HOSTILE fixture: it widens the published capacity on the offer the engine
/// wrote — simulating a producer whose ceiling is WRONG — and then declares a count that arithmetic
/// says must kill. Everything downstream is production: `apply()`'s declare handler, the APNAP
/// window, `apply_confirmed_shortcut`, `materialize_fixed_shortcut`. The question it answers is
/// the one that matters when a certificate is unsound: does the drive stop at the boundary, or
/// does it drive through it?
///
/// CR 704.3: state-based actions are checked whenever a player would get priority, and the
/// drive's every beat goes through `pass_priority_once_with_pipeline`, so CR 704.5a ("if a
/// player has 0 or less life, that player loses the game") is applied INSIDE the drive.
///
/// # SCOPE — this row covers the TOTAL-WIPE arm ONLY
///
/// bloodloop3 seats its two opponents at EQUAL life at the offer beat, so they would cross 0 on
/// the SAME cycle and CR 104.2a would crown. The fixture is
/// structurally incapable of a terminal single-seat crossing: a symmetric fixture collapses
/// every such case into a total one. That symmetry is also what holds this board's PUBLISHED
/// BOUND still — with two seats at the binding value the relief's one-faller conjunct does not
/// fire, so the bound stays at the strict headroom value, which is why ⓐ's doctoring below is
/// still a no-op here. The other arm — one seat crosses while ≥2 players survive, no
/// `GameOver`, the crossing cycle COMMITTING and that one seat
/// leaving the game while the survivors keep an intact loop — behaves DIFFERENTLY
/// and has its own row, [`bounded_fixed_drive_commits_the_terminal_cycle_that_eliminates_one_seat`], which
/// carries the arm-asymmetry table. On the bound side the two boards differ too: two tied
/// seats here hold the relief off, while a unique binding seat there carries the count to its
/// own crossing.
///
/// # The MATCHED PAIR, on the same doctored offer
///
/// * ⓐ `n = cycles_to_lethal - 1` — the drive runs to completion, every seat survives at
///   exactly one point of life, nobody is eliminated.
/// * ⓑ `n = published ceiling + 1` — RE-ADJUDICATED. This arm used to drive and stop at the
///   first crossing cycle. The consumption ceiling now refuses an over-bound count before any
///   cycle runs, so the arm states that refusal instead: not one seat's life moves. What it
///   used to witness — a drive reaching the cross-lethal arm — is reached at an HONEST count by
///   [`the_honest_count_reaches_the_cross_lethal_arm_when_the_crossing_takes_the_last_opponent`],
///   whose board's single opponent makes the crossing a CR 104.2a win.
///
/// ⓐ is what keeps the pair non-vacuous: it drives on this very board, so ⓑ's unmoved life
/// vector is the ceiling firing rather than a fixture that drives nothing.
///
/// ⓐ DOCTORS NOTHING. The published ceiling spans the whole cascade, so one period short of the
/// first crossing is a count the offer already admits and the drive runs on the offer as
/// published. That it lies inside the ceiling is asserted rather than assumed.
///
/// # What flips
///
/// * delete the per-offer disjunct from `apply_confirmed_shortcut`'s guard ⇒ ⓑ drives again and
///   its unmoved-life assertion FAILS while ⓐ stays green.
#[test]
fn bounded_fixed_drive_stops_at_the_first_lethal_cycle() {
    let mut state = bloodloop_state(3);
    drive_to_bounded_offer(&mut state, 400).expect("the bounded offer must fire at 3 players");

    let (proposer, certificate, schema) = bounded_offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");
    let bound = schema.deliverable_capacity;
    let lives_before: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .map(|p| (p.id, p.life as i64))
        .collect();

    // Per-seat loss the offer published; the stop point is derived from it, never from a literal.
    let loss = |seat: &PlayerId| -per_cycle.delta.life.get(seat).copied().unwrap_or(0);
    let victims: Vec<PlayerId> = lives_before
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| loss(id) > 0)
        .collect();
    assert_eq!(
        victims.len(),
        2,
        "REACH-GUARD: this row is about a MULTI-seat partial wipe, so both opponents must be \
         losing life per period; published δ {:?}",
        per_cycle.delta.life
    );

    // The smallest count that drives some living seat to 0 or less: `ceil(life / loss)`.
    let cycles_to_lethal = lives_before
        .iter()
        .filter(|(id, _)| loss(id) > 0)
        .map(|(id, l0)| l0.div_euclid(loss(id)) + i64::from(l0.rem_euclid(loss(id)) != 0))
        .min()
        .expect("at least one seat is losing life, asserted above");
    // One above the published ceiling: the smallest count the consumption seam must refuse, and
    // derived from that ceiling rather than from a multiple of the crossing, which the cascade
    // bound now reaches on its own.
    let n = bound + 1;
    assert!(
        i64::from(bound) >= cycles_to_lethal,
        "REACH-GUARD: the published ceiling must CONTAIN the first lethal cycle, or ⓑ's refusal \
         would be of a count that is merely lethal rather than one above the ceiling; ceiling \
         {bound} vs first crossing {cycles_to_lethal}"
    );

    // ⓐ SURVIVING ARM — one period short of the first crossing. Same doctored offer, so the
    //   only difference between the arms is `n` itself.
    {
        let mut survive = state.clone();
        let survivor_n = u32::try_from(cycles_to_lethal - 1).expect("fits");
        assert!(
            survivor_n < bound,
            "REACH-GUARD: one period short of the first crossing must lie INSIDE the published \
             ceiling, or ⓐ is refused at consumption and states a refusal rather than a \
             completed drive; {survivor_n} vs ceiling {bound}"
        );
        r6a_declare_and_accept_all(&mut survive, proposer, survivor_n);
        assert_eq!(
            survive.players.iter().filter(|p| p.is_eliminated).count(),
            0,
            "ⓐ CR 704.5a: one period short of the crossing, every seat is still above 0; \
             lives {:?}",
            survive.players.iter().map(|p| p.life).collect::<Vec<_>>()
        );
        for (seat, l0) in &lives_before {
            let life_now = survive.players.iter().find(|p| p.id == *seat).unwrap().life as i64;
            assert_eq!(
                l0 - life_now,
                i64::from(survivor_n) * loss(seat),
                "ⓐ {seat:?}: exactly `n` periods committed, no more — the drive must stop \
                 because `n` ran out, not because something died"
            );
        }
        assert!(
            matches!(survive.waiting_for, WaitingFor::Priority { .. }),
            "ⓐ a completed finite drive hands back to priority; got {:?}",
            survive.waiting_for
        );
    }

    // ⓑ THE WIDENED COUNT, RE-ADJUDICATED. It used to drive and stop at the first crossing;
    //   the consumption ceiling now refuses it before any cycle runs, so what this arm states
    //   is the refusal. The stop-at-the-boundary behaviour it used to witness is reached at an
    //   HONEST count by
    //   `the_honest_count_reaches_the_cross_lethal_arm_when_the_crossing_takes_the_last_opponent`,
    //   whose board's unique predicted seat takes the last surviving opponent.
    let WaitingFor::LoopShortcut { schema, .. } = &mut state.waiting_for else {
        unreachable!("bounded_offer_parts already matched the offer")
    };
    schema.deliverable_capacity = n;

    r6a_declare_and_accept_all(&mut state, proposer, n);

    for (seat, l0) in &lives_before {
        assert_eq!(
            state.players.iter().find(|p| p.id == *seat).unwrap().life as i64,
            *l0,
            "ⓑ CR 704.5a: a count above the consumption ceiling is refused where the proposal \
             is spent, so {seat:?}'s life has not moved at all"
        );
    }
    assert_eq!(
        state.players.iter().filter(|p| p.is_eliminated).count(),
        0,
        "ⓑ CR 800.4a: nothing was driven, so nobody left the game"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player }
            if !state.players.iter().any(|p| p.id == player && p.is_eliminated)),
        "ⓑ CR 732.2a: the refusal ends at a place where a player has priority; got {:?}",
        state.waiting_for
    );
}

/// THE TERMINAL CYCLE. A crossing that eliminates ONE seat while **≥2 players survive** raises
/// no `GameOver` (CR 104.2a crowns nobody), so the drive does not cross-lethal. It COMMITS that
/// cycle, and the published ceiling being that crossing's own repetition is what leaves the drive
/// at the priority window the removal was observed at — CR 732.2a's ending point, reached rather
/// than manufactured, because CR 704.3 + CR 117.5 run the state-based sweep every time a player
/// would get priority.
///
/// # The arm asymmetry, stated so a future drive learns it from the doc and not by accident
///
/// | arm | trigger | outcome |
/// |---|---|---|
/// | **total wipe** | every remaining opponent crosses 0 on the same cycle ⇒ `WaitingFor::GameOver` | the crossing cycle COMMITS and the game ends when the proposal named that winner or named nobody; otherwise it is dropped whole |
/// | **terminal crossing** | one seat crosses 0 while **≥2** players survive ⇒ no `GameOver` | the crossing cycle COMMITS when `departure_verdict` names it, that seat is eliminated, and the drive continues; where that crossing is the last one the accepted count contains, continuing IS the loop's end and priority is handed back at a living seat |
/// | **abort** | beat cap, unpinned prompt, engine error | that cycle rolls back whole, prior conforming cycles stay committed |
///
/// The terminal crossing is IN CONTRACT for a legitimately-derived bound, and that is what this
/// row is about: `elimination_bounds` admits a CR 704 crossing as the sequence's FINAL
/// iteration, so the honest bound IS the first crossing and the honest count is what eliminates
/// the seat. The counts PAST it are no longer driven here at all — the consumption ceiling
/// refuses them before any cycle runs, and that refusal is
/// [`an_over_bound_count_is_refused_at_consumption`]'s claim on this same board.
///
/// The atomic per-cycle property survives as **no half-applied period except the terminal one,
/// whose remainder is unmakeable**. Measured on this board, the terminal cycle IS partial: the
/// drained seats take the full count of losses while the proposer's own lifelink gain lands one
/// fewer time, because that trigger is still on the stack at the ending point. That is what
/// CR 732.2a asks for — the sequence stops at a priority window and what is unresolved is live
/// for manual play.
///
/// # Why this row had to exist separately — the fixture-symmetry trap
///
/// [`the_honest_count_reaches_the_cross_lethal_arm_when_the_crossing_takes_the_last_opponent`]
/// is the mirror for the total-wipe arm, but its board seats **a single opponent**, so that
/// seat's crossing always takes the last one and it can only ever exhibit that arm. A fixture
/// whose crossing leaves no opponent standing — a symmetric board whose opponents cross
/// together, or a two-seat board with only one to cross — collapses every terminal case into a
/// total case. This row's dina 4p dump is ASYMMETRIC by measurement, and the reach-guards below
/// FAIL if that ever drifts into symmetry, which is what stops this row from silently becoming
/// a second copy of the mirror.
///
/// # What is asserted, and what is deliberately NOT
///
/// Every quantity is derived from the certificate the ENGINE published and the offer-beat board;
/// no count is pinned as a literal. The row asserts the OBSERVABLE outcome: the published ceiling
/// spans the cascade and contains the first crossing, an accept AT that crossing eliminates
/// exactly one seat and it is the unique first crosser, every
/// survivor is above 0, the detection window is cleared, and the drive hands back to ordinary
/// priority rather than ending the game.
///
/// It does NOT assert "the conformance check never fired". The terminal arm does not run one, so
/// there is nothing to distinguish from the state.
///
/// # REVERT-PROBES
///
/// * derive the consumption prediction on the LAST COMMITTED board rather than on the pre-drive
///   one ⇒ the spent headroom relieves to a remaining-cycles figure of 1 while the drive's
///   terminal index is the crossing ⇒ `departure_verdict`'s repetition comparison refuses ⇒ the
///   elimination assertion reads an empty set and FAILS.
/// * name any seat other than the reduction's own argmin ⇒ that authority's set comparison
///   refuses the terminal cycle for the same reason.
/// * drop the `+ 1` that turns a seat's strict headroom into its crossing repetition ⇒ the
///   published ceiling lands one below the cascade's last entry ⇒ the bound assertion FAILS.
#[test]
fn bounded_fixed_drive_commits_the_terminal_cycle_that_eliminates_one_seat() {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400).expect("the bounded offer must fire on the 4p dump");

    let (proposer, certificate, schema) = bounded_offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");
    let honest_bound = schema.deliverable_capacity;
    let lives_before: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .map(|p| (p.id, p.life as i64))
        .collect();
    let loss = |seat: &PlayerId| -per_cycle.delta.life.get(seat).copied().unwrap_or(0);

    // `ceil(life / loss)` — the first cycle index at which each drained seat crosses 0.
    let crossings: Vec<(PlayerId, i64)> = lives_before
        .iter()
        .filter(|(id, _)| loss(id) > 0)
        .map(|(id, l0)| {
            (
                *id,
                l0.div_euclid(loss(id)) + i64::from(l0.rem_euclid(loss(id)) != 0),
            )
        })
        .collect();
    assert!(
        crossings.len() >= 2,
        "REACH-GUARD: a PARTIAL wipe needs at least two drained seats, else 'one crosses while \
         others survive' is unconstructible; published δ {:?}",
        per_cycle.delta.life
    );
    let first_crossing = crossings
        .iter()
        .map(|(_, c)| *c)
        .min()
        .expect("at least two drained seats, asserted above");

    // ── THE ASYMMETRY REACH-GUARD. This is the guard the mirror row could not have had.
    let first_victims: Vec<PlayerId> = crossings
        .iter()
        .filter(|(_, c)| *c == first_crossing)
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        first_victims.len(),
        1,
        "REACH-GUARD: this row is about the PARTIAL arm, so exactly ONE seat may cross first. \
         Equal crossings would make the whole table die together, take the cross-lethal arm \
         and silently re-test the mirror row's total-wipe arm instead; crossings {crossings:?}"
    );
    let survivors = state.players.iter().filter(|p| !p.is_eliminated).count() - first_victims.len();
    assert!(
        survivors >= 2,
        "REACH-GUARD: with fewer than two survivors CR 104.2a crowns and `WaitingFor::GameOver` \
         routes the drive to the CrossLethal arm; got {survivors} survivors at the first crossing"
    );

    // The published ceiling SPANS the cascade, so the crossing driven below is a count it
    // contains. Asserted, not assumed — it is what makes the elimination below the behaviour of a
    // count the offer itself admits rather than a doctored one, and it is what the `over > 0` arms
    // are widened from.
    let last_crossing = crossings
        .iter()
        .map(|(_, c)| *c)
        .max()
        .expect("at least two drained seats, asserted above");
    assert_eq!(
        i64::from(honest_bound),
        last_crossing,
        "`elimination_cascade` carries the count to its LAST entry — the widest crossing this \
         period drives — so the ceiling contains every crossing before it; bound {honest_bound}, \
         crossings {crossings:?}"
    );
    assert!(
        first_crossing < last_crossing,
        "REACH-GUARD: the first and last crossings must DIFFER, else the assertion above cannot \
         tell the cascade's span from its first entry; crossings {crossings:?}"
    );

    // The detection window is LIVE at the offer beat, so the emptiness asserted after the drive
    // is a CLEARED ring and not an absent one.
    assert!(
        !state.loop_detect_ring.is_empty(),
        "REACH-GUARD: the offer this drive produced was certified against a populated ring"
    );

    // V4's REACH-GUARD, and it is what makes this row discriminate the BOARD the prediction is
    // derived on: with at least two cycles committing before the terminal one, the repetition
    // the reduction names is the drive's FINAL index rather than 1. Derived on the LAST
    // COMMITTED board instead of the pre-drive one, the already-spent headroom would relieve to
    // a remaining-cycles figure of 1, the terminal cycle's index comparison would refuse, and
    // (a) below would report an empty eliminated set.
    assert!(
        first_crossing >= 3,
        "REACH-GUARD: the terminal cycle must be preceded by at least two committed cycles;          first crossing at {first_crossing}"
    );

    // The HONEST count. The counts past it are no longer this row's: the consumption ceiling
    // refuses them before any cycle is driven, which is
    // `an_over_bound_count_is_refused_at_consumption`'s claim on this same board.
    {
        let mut doctored = state.clone();
        let n = u32::try_from(first_crossing).expect("fits");

        let result = take_recorded(&mut doctored, n);

        // CR 732.1b + CR 704.5a: each repetition is Dina, Soul Steeper's "Whenever you gain life,
        // each opponent loses 1 life." answered by Bloodthirsty Conqueror's "Whenever an opponent
        // loses life, you gain that much life."; all `n` commit, the n-th taking the first victim
        // to 0, and the record keeps the first and the n-th.
        assert_eq!(
            markers(&result.events),
            [(proposer, 1), (proposer, n)],
            "n={n}"
        );

        // (a) EXACTLY the unique first crosser is eliminated. Not zero (the terminal cycle
        //     COMMITS) and not the whole table (that is the mirror row's arm).
        assert_eq!(
            doctored
                .players
                .iter()
                .filter(|p| p.is_eliminated)
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            first_victims,
            "n={n}: CR 704.5a + CR 800.4a — the sequence's final iteration carries exactly the \
             seat the arithmetic named past its threshold, and the drive stops there; lives \
             {:?}",
            doctored.players.iter().map(|p| p.life).collect::<Vec<_>>()
        );
        assert!(
            doctored
                .players
                .iter()
                .filter(|p| !p.is_eliminated)
                .all(|p| p.life > 0),
            "n={n}: every SURVIVING seat is above the CR 704.5a threshold, which is the \
             one-crossing lemma the relief rests on; lives {:?}",
            doctored.players.iter().map(|p| p.life).collect::<Vec<_>>()
        );

        // (b) EXACTLY `first_crossing` periods of drain committed on every drained seat: every
        //     cycle up to and including the crossing one. Derived from the published δ, never
        //     from a literal.
        for (seat, l0) in &lives_before {
            let loss = -per_cycle.delta.life.get(seat).copied().unwrap_or(0);
            if loss <= 0 {
                continue;
            }
            let committed = l0
                - doctored
                    .players
                    .iter()
                    .find(|p| p.id == *seat)
                    .unwrap()
                    .life as i64;
            assert_eq!(
                committed,
                first_crossing * loss,
                "n={n} {seat:?}: the drive commits every period up to AND INCLUDING the \
                 crossing cycle, then stops at that cycle's own priority window; lives {:?}",
                doctored.players.iter().map(|p| p.life).collect::<Vec<_>>()
            );
        }

        // (c) NOT the CrossLethal arm. `GameOver` here would mean a terminal crossing crowned
        //     someone, which is the confusion this row exists to keep separate.
        assert!(
            matches!(doctored.waiting_for, WaitingFor::Priority { player }
                if !doctored.players.iter().any(|p| p.id == player && p.is_eliminated)),
            "n={n}: CR 104.2a — a player wins only once ALL their opponents have left, and this \
             crossing eliminates one of three, so there is no winner to crown; CR 800.4a seats \
             the handback at a living player. got {:?}",
            doctored.waiting_for
        );

        // (d) R3-a's TERMINAL ARM — the drive-end seam is the CR 732.2a ending point for this
        //     entry path too, and it discards the detection window before handing back. The
        //     offer-beat guard above is what makes the emptiness a CLEARED ring rather than an
        //     absent one. The populated-JOURNAL half of the same seam is pinned on the f4 dump
        //     by
        //     `fantastic_four_bounded_loop::r3a_the_accepted_drive_ends_at_the_priority_point_with_the_window_cleared`,
        //     the only fixture measured reaching this seam with answers recorded.
        assert!(
            doctored.loop_detect_ring.is_empty(),
            "n={n}: CR 732.2a — the drive ends at the priority handback with the detection \
             window DISCARDED, so a later beat re-detects genuinely instead of this same \
             `apply()` re-offering the interrupted loop; ring still carries {} sample(s)",
            doctored.loop_detect_ring.len()
        );
        assert_eq!(
            doctored.loop_answers_recorded(),
            0,
            "n={n}: CR 603.5 — the recorded `may` answers describe the window that just ended, \
             and the same seam drops them together with the ring. ⚠ FORWARD TRIPWIRE, not a \
             co-equal half of that claim: the journal already reads 0 when this fixture reaches \
             the seam, so a mutant neutering only `loop_answer_journal = None` leaves this \
             clause green while the f4 row fails. It earns its place by failing if a future \
             writer ever populates the journal on this entry path and the seam stops clearing \
             it; the DISCRIMINATING statement of the journal half is the f4 row named above"
        );
    }
}

/// The committed 4-player Dina/Conqueror drain, driven to its live bounded offer.
///
/// Returned whole — board, proposer, certificate and the count the engine published — because
/// every consumption-seam row below re-derives its own quantities from the certificate rather
/// than pinning a literal.
fn dina_bounded_offer() -> (GameState, PlayerId, LoopCertificate, u32) {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_conqueror_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 400)
        .expect("the bounded offer must fire on the committed 4p drain");
    let (proposer, certificate, schema) = bounded_offer_parts(&state);
    (
        state.clone(),
        proposer,
        certificate.clone(),
        schema.deliverable_capacity,
    )
}

/// CR 732.2b: reach the confirmed-shortcut seam through the **untrusted restore ingress** — a
/// constructed `WaitingFor::RespondToShortcut` carrying whatever signature a wire may deliver,
/// then every living opponent accepting through the production `apply()` handler.
///
/// This is the ingress the consumption-time re-derivation exists for. The two LIVE ingresses
/// run the declare firewall on their way in; a restored response window is plain serde and has
/// run nothing, so the proposal's count and its per-period signature are both attacker-shaped.
fn accept_restored_proposal(state: &mut GameState, proposer: PlayerId, proposal: ShortcutProposal) {
    let responders: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.id != proposer)
        .map(|p| p.id)
        .collect();
    let (first, rest) = responders
        .split_first()
        .expect("the response window needs a living opponent");
    state.waiting_for = WaitingFor::RespondToShortcut {
        player: *first,
        remaining_players: rest.to_vec(),
        proposal,
    };
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
}

/// A proposal for the restore ingress: the offer's own certificate with `per_cycle` replaced by
/// whatever signature the row is about. `template: None` keeps the seam's own `template.owner`
/// firewall out of the way, so a refusal below is attributable to the count or the signature.
fn restored_proposal(
    certificate: &LoopCertificate,
    proposer: PlayerId,
    n: u32,
    per_cycle: PeriodicDelta,
) -> ShortcutProposal {
    ShortcutProposal {
        proposer,
        predicted_winner: None,
        count: IterationCount::Fixed(n),
        unbounded: certificate.unbounded.clone(),
        rising: certificate.rising.clone(),
        win_kind: certificate.win_kind,
        win_kind_at: None,
        template: None,
        per_cycle: Some(PeriodSignature::EveryRepetition(per_cycle)),
        shortened_by: None,
        published_declaration: None,
        leading_aims: Vec::new(),
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    }
}

fn seat_lives(state: &GameState) -> Vec<(PlayerId, i32)> {
    state.players.iter().map(|p| (p.id, p.life)).collect()
}

fn eliminated_seats(state: &GameState) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .filter(|p| p.is_eliminated)
        .map(|p| p.id)
        .collect()
}

/// **V2 — CR 704.5a + CR 732.2a: a count above the consumption ceiling is refused where the
/// proposal is spent.**
///
/// The ceiling is RE-DERIVED at the seam from the proposal's own signature and the board the
/// drive would run on, never read off a schema that is gone by then and never copied onto the
/// proposal. CR 732.2a admits only a sequence that "may be legally taken based on the current
/// game state and the predictable results of the sequence of choices", and CR 704.3 runs the
/// CR 704.5a check at every priority beat inside it, so a count that would carry a seat past a
/// threshold before its final iteration is not a legal shortcut whatever schema states it.
///
/// # Non-vacuity
///
/// The paired positive is the SAME board and the SAME doctored schema at a count INSIDE the
/// ceiling: it drives and moves life. Without it, "no life moved" is also what an engine that
/// refuses everything produces, and also what a dead fixture produces.
///
/// REVERT-PROBE: delete the per-offer disjunct from `apply_confirmed_shortcut`'s guard ⇒ the
/// over-count drives and the life vector moves ⇒ the identity assertion FAILS.
#[test]
fn an_over_bound_count_is_refused_at_consumption() {
    let (state, proposer, _certificate, honest) = dina_bounded_offer();
    let before = seat_lives(&state);
    assert!(
        honest >= 2,
        "REACH-GUARD: the published count must leave room BOTH above it (the refused arm) and \
         below it (the paired positive); got {honest}"
    );

    // ⓐ THE REFUSAL — a count comfortably past the ceiling, declared onto a widened schema so
    //   the declare site cannot be what refuses it.
    let over = honest + 3;
    let mut refused = state.clone();
    let WaitingFor::LoopShortcut { schema, .. } = &mut refused.waiting_for else {
        unreachable!("bounded_offer_parts already matched the offer")
    };
    schema.deliverable_capacity = over;
    r6a_declare_and_accept_all(&mut refused, proposer, over);
    assert_eq!(
        seat_lives(&refused),
        before,
        "CR 704.5a: an over-bound sequence is refused BEFORE any cycle is driven, so not one \
         seat's life has moved"
    );
    assert_eq!(
        eliminated_seats(&refused),
        Vec::<PlayerId>::new(),
        "CR 800.4a: nobody left the game, because nothing was driven"
    );
    assert!(
        matches!(refused.waiting_for, WaitingFor::Priority { player }
            if !refused.players.iter().any(|p| p.id == player && p.is_eliminated)),
        "CR 732.2a: the refusal ends at a place where a player has priority; got {:?}",
        refused.waiting_for
    );

    // ⓑ THE PAIRED POSITIVE — the same board and the same doctored schema, one count inside
    //   the ceiling, which drives.
    let inside = honest - 1;
    let mut driven = state.clone();
    let WaitingFor::LoopShortcut { schema, .. } = &mut driven.waiting_for else {
        unreachable!("bounded_offer_parts already matched the offer")
    };
    schema.deliverable_capacity = over;
    r6a_declare_and_accept_all(&mut driven, proposer, inside);
    assert_ne!(
        seat_lives(&driven),
        before,
        "PAIRED POSITIVE: a count inside the ceiling still drives on this very board, so ⓐ's \
         unmoved life vector is the ceiling firing rather than an engine that refuses \
         everything"
    );
}

/// CR 704.5a + CR 800.4a: the cascade of CR 704 threshold crossings an offer's published data
/// implies, RE-DERIVED here — every living seat's first crossing under the declaration this offer
/// publishes, grouped by the repetition it falls on, in departure order.
///
/// Independent of the reduction under test in the part that matters: the grouping, the ordering and
/// the proposer truncation are assembled here, and only the per-repetition CHARGE comes from the
/// engine — `PeriodicDelta::declared_seat_life_charges`, the published authority, because a
/// test-side copy of the net/dip charge model would be a second charge model to argue equal rather
/// than an independent check.
///
/// The horizon is the sum of the living seats' life totals, derived from the BOARD and never from
/// the published count, so a row comparing a published count against this mirror is not comparing
/// the count against itself.
pub(crate) fn published_cascade(state: &GameState) -> Vec<(u32, Vec<PlayerId>)> {
    let (proposer, certificate, schema) = bounded_offer_parts(state);
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");
    let declaration = match &state.waiting_for {
        WaitingFor::LoopShortcut { declaration, .. } => declaration.clone(),
        other => panic!("bounded_offer_parts already matched the offer, got {other:?}"),
    };
    cascade_from(
        state,
        proposer,
        &per_cycle,
        declaration.as_ref(),
        &schema.points,
    )
}

/// [`published_cascade`] over an EXPLICIT signature and declaration, for the rows that drive a
/// perturbed one through the restore ingress and have to state what cascade that perturbation
/// implies.
pub(crate) fn cascade_from(
    state: &GameState,
    proposer: PlayerId,
    per_cycle: &PeriodicDelta,
    declaration: Option<&engine::analysis::decision_template::DecisionTemplate>,
    points: &[engine::analysis::decision_template::DecisionPoint],
) -> Vec<(u32, Vec<PlayerId>)> {
    let living: Vec<(PlayerId, i32)> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| (p.id, p.life))
        .collect();
    let horizon: u32 = living
        .iter()
        .map(|&(_, life)| u32::try_from(i64::from(life).max(1)).unwrap_or(u32::MAX))
        .fold(1, u32::saturating_add);

    let mut crossings: Vec<(u32, PlayerId)> = living
        .iter()
        .filter_map(|&(seat, life)| {
            engine::analysis::resource::PeriodicDelta::first_life_crossing(
                // The PUBLISHED cascade, so the ceiling direction — what this mirror compares a
                // published count against.
                per_cycle.declared_seat_life_charges(
                    seat,
                    declaration,
                    declaration,
                    points,
                    state,
                    ChargeBound::Ceiling,
                    engine::analysis::resource::AnnouncedLead::None,
                ),
                i64::from(life),
                horizon,
            )
            .map(|repetition| (repetition, seat))
        })
        .collect();
    crossings.sort_unstable();
    // CR 800.4a: once the proposer leaves, their objects leave the game with them and the loop
    // does not continue, so their crossing BOUNDS the cascade.
    let bound = crossings
        .iter()
        .find(|&&(_, seat)| seat == proposer)
        .map(|&(repetition, _)| repetition);
    let mut entries: Vec<(u32, Vec<PlayerId>)> = Vec::new();
    for (repetition, seat) in crossings {
        if bound.is_some_and(|last| repetition > last) {
            break;
        }
        match entries.last_mut() {
            Some((at, seats)) if *at == repetition => seats.push(seat),
            _ => entries.push((repetition, vec![seat])),
        }
    }
    entries
}

/// **ROW 2 + ROW 3 + ROW 7a — CR 704.5a + CR 104.2a: one accept at the published ceiling crosses
/// EVERY threshold that ceiling contains, in departure order, and the last one ends the game.**
///
/// The two committed untargeted 4p boards, one from each drive arm a crossing cycle can reach: on
/// one the crossing cycle's published period completes at the forced-window ANSWER beat and the
/// drive classifies it a RECURRENCE, on the other it completes at the settle beat after the removal
/// and the drive classifies it `SeatLeft`. A discriminator or a re-scope living in only one arm is
/// one the other board silently skips, which is why one board is not the population here.
///
/// # Everything quantified over is re-derived from the offer's own published data
///
/// The count is the offer's published capacity; the cascade is `published_cascade`'s, whose charge
/// comes from the published per-period signature under the declaration this offer publishes and
/// whose horizon comes from the board. No count, seat or repetition is written down.
///
/// # Reach guards, asserted before the claim
///
/// The cascade carries MORE THAN ONE ENTRY and its entries' repetitions STRICTLY INCREASE, so
/// "every threshold" has more than one member and the departure order is a real order rather than a
/// single beat. The guard is about the ENTRIES and not about the seats: two seats grouped into one
/// entry satisfies it, which is row 7a's own subject, and a pairwise-distinct-seats guard would
/// refuse the very board whose last entry is a tie.
///
/// # Non-vacuity — the paired SUB-CASCADE accept, on each board, in the same invocation
///
/// The first entry's repetition — the count the single-crossing reduction published before this
/// phase — eliminates exactly that entry's seats and hands priority back. So the multi-entry
/// elimination below is the CEILING moving, not the drive losing its stop; and on the board whose
/// grouped entry is its last, that same leg is row 7a's hostile sibling: one below the tie's
/// repetition takes only the earlier entry's seat.
///
/// REVERT-PROBE: publish the first crossing as the ceiling (the single-crossing reduction) ⇒ the
/// ceiling accept becomes the sub-cascade accept ⇒ the "more than one eliminated" assertion FAILS
/// on both boards while the sub-cascade leg stays green. Route the departure verdict only from the
/// `SeatLeft` arm ⇒ the board whose crossing cycle lands in `Recurred` stops at its first entry.
#[test]
fn one_accept_crosses_every_threshold_the_published_ceiling_contains() {
    for fixture in [
        &include_bytes!("../fixtures/dina_noff_turn5_4p.json.gz")[..],
        &include_bytes!("../fixtures/dina_conqueror_4p.json.gz")[..],
    ] {
        let mut at_offer = restore_dump(&gunzip_dump(fixture));
        drive_to_bounded_offer(&mut at_offer, 600)
            .expect("the bounded offer must fire on this committed 4p drain");
        let (proposer, _, schema) = bounded_offer_parts(&at_offer);
        let published = schema.deliverable_capacity;
        let cascade = published_cascade(&at_offer);
        let living_opponents = at_offer
            .players
            .iter()
            .filter(|p| !p.is_eliminated && p.id != proposer)
            .count();

        // ── REACH GUARDS, on the CASCADE's entries ──
        assert!(
            cascade.len() > 1,
            "REACH-GUARD: the cascade must carry more than one entry, or 'every threshold' has \
             one member and this row is the single-crossing row again; got {cascade:?}"
        );
        assert!(
            cascade.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "REACH-GUARD: the entries' repetitions must strictly increase, or the departure ORDER \
             below is a single beat; got {cascade:?}"
        );
        let (last_repetition, last_seats) = cascade.last().cloned().expect("asserted non-empty");
        assert_eq!(
            published, last_repetition,
            "the published ceiling is the LAST entry's repetition — the whole cascade and not its \
             first crossing; cascade {cascade:?}"
        );
        let (first_repetition, first_seats) = cascade.first().cloned().expect("asserted non-empty");
        let predicted: Vec<PlayerId> = {
            let mut all: Vec<PlayerId> = cascade
                .iter()
                .flat_map(|(_, seats)| seats.clone())
                .collect();
            all.sort_unstable();
            all
        };
        assert_eq!(
            predicted.len(),
            living_opponents,
            "REACH-GUARD: this board's cascade must name every living opponent, so the last entry \
             takes THE LAST one and CR 104.2a is what ends the drive; cascade {cascade:?}"
        );
        assert!(
            living_opponents > 2,
            "REACH-GUARD: more than two opponents must be alive, or the FIRST crossing already \
             takes the last one and the sub-cascade control below cannot hand priority back"
        );

        // ── ⓐ THE PUBLISHED CEILING — every threshold the count contains, in order ──
        let mut driven = at_offer.clone();
        r6a_declare_and_accept_all(&mut driven, proposer, published);
        assert_eq!(
            eliminated_seats(&driven),
            predicted,
            "CR 704.5a: one accept at the published ceiling takes every seat the cascade named; \
             cascade {cascade:?}, lives {:?}",
            seat_lives(&driven)
        );
        // ROW 3 — CR 104.2a: the last crossing takes the last remaining opponent, so the game ends
        // immediately rather than handing priority back. Asserted on the TERMINAL STATE and not on
        // the arm, so the row is true on either route into it.
        assert!(
            matches!(
                driven.waiting_for,
                WaitingFor::GameOver { winner } if winner == Some(proposer)
            ),
            "CR 104.2a: a player still in the game wins immediately once every opponent has left; \
             got {:?} with lives {:?}",
            driven.waiting_for,
            seat_lives(&driven)
        );

        // ── ⓑ THE SUB-CASCADE ACCEPT — the first entry's repetition, on this same board ──
        let mut sub = at_offer.clone();
        r6a_declare_and_accept_all(&mut sub, proposer, first_repetition);
        assert_eq!(
            eliminated_seats(&sub),
            first_seats,
            "SUB-CASCADE: the first crossing eliminates exactly its own entry's seats — which is \
             ROW 7a's hostile sibling where that entry precedes a GROUPED one; cascade \
             {cascade:?}, lives {:?}",
            seat_lives(&sub)
        );
        assert!(
            matches!(sub.waiting_for, WaitingFor::Priority { .. }),
            "SUB-CASCADE: opponents survive it, so CR 732.2a's ending point is a priority window; \
             got {:?}",
            sub.waiting_for
        );
        assert!(
            last_seats.len() > first_seats.len() || cascade.len() > 2,
            "REACH-GUARD: the two legs must differ in more than a name — either the last entry \
             GROUPS more seats than the first (row 7a's board) or the cascade has a middle entry; \
             got {cascade:?}"
        );
    }
}

/// **ROW 7a — CR 704.3 + CR 704.5a: the reduction admits a TIE as one entry's seat set, and the
/// accept at the resulting ceiling takes every seat in it.**
///
/// CR 704.3 runs the state-based sweep whenever a player would get priority, so one repetition
/// taking two seats to their thresholds removes both at the same beat. The reduction answers that
/// one way — admitted as one entry holding both — and the drive's set comparison agrees.
///
/// Separated from the row above because the SUBJECT is the grouping rather than the ordering: this
/// row asserts the tie exists on this board, that it is a LATER entry, and that the accept takes
/// both its seats together.
///
/// # Reach guards, asserted before the claim
///
/// A strictly earlier entry precedes the grouped one, so it is a later entry and not a degenerate
/// first; and the tied seats' crossings are asserted EQUAL, off the mirror, so the grouping is a
/// measured tie rather than an artefact of the assertion's shape.
///
/// # Discrimination
///
/// Refuse the grouping — answer the tie with the segment's floor instead of the entry's repetition,
/// which is what `ResourceVector::elimination_bounds` does — and the published ceiling stops one
/// repetition short of the entry, so neither of its seats leaves and both assertions fail. The
/// accept one below that repetition is in the row above, in its own invocation, and eliminates only
/// the earlier entry's seat.
#[test]
fn a_tied_entry_is_one_seat_set_and_the_accept_takes_all_of_it() {
    let mut at_offer = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_noff_turn5_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut at_offer, 600)
        .expect("the bounded offer must fire on this committed 4p drain");
    let (proposer, _, schema) = bounded_offer_parts(&at_offer);
    let published = schema.deliverable_capacity;
    let cascade = published_cascade(&at_offer);

    let grouped = cascade
        .iter()
        .position(|(_, seats)| seats.len() > 1)
        .unwrap_or_else(|| {
            panic!(
                "REACH-GUARD (live instrument): this board's cascade must carry an entry whose \
                 seats cross on ONE repetition, or there is no tie to admit; got {cascade:?}"
            )
        });
    assert!(
        grouped > 0 && cascade[grouped - 1].0 < cascade[grouped].0,
        "REACH-GUARD: a strictly EARLIER entry must precede the grouped one, so it is a later \
         entry and not a degenerate first; got {cascade:?}"
    );
    let (repetition, tied) = cascade[grouped].clone();
    assert_eq!(
        repetition, published,
        "on this board the grouped entry is the cascade's last, so the published ceiling IS its \
         repetition; got {cascade:?}"
    );

    let mut driven = at_offer.clone();
    r6a_declare_and_accept_all(&mut driven, proposer, published);
    for seat in &tied {
        assert!(
            eliminated_seats(&driven).contains(seat),
            "CR 704.3 + CR 704.5a: every seat in the tied entry leaves on that one repetition; \
             {seat:?} did not, with cascade {cascade:?} and lives {:?}",
            seat_lives(&driven)
        );
    }
}

/// **ROW 5 + ROW 5b — CR 732.2a: the published SUGGESTION is a count the offer's own declaration
/// drives, and the published CEILING is never below it.**
///
/// The population is the committed dumps this walk reaches a bounded offer on.
///
/// THE LEG EACH BOARD IS DRIVEN ON, stated because it changes what is being verified: two of these
/// dumps restore ALREADY AT an offer whose published capacity is DESERIALIZED rather than re-
/// derived
/// by this phase's producer, and `drive_to_bounded_offer` returns at beat 0 on them. The row drives
/// whichever leg that walk reaches and asserts the pair the offer actually published, which is the
/// right question for both legs — a deserialized capacity the handler will enforce is as load-
/// bearing
/// as a freshly derived one.
///
/// The first observable is the BOARD MOVING. Declaring `schema.iteration_count` verbatim with
/// `template: None` opens the CR 732.2b response window (the reach guard, which separates "declare
/// refused" from "drive aborted"), and the accept then has to commit a change rather than hand a
/// zero-cycle priority back — which is what publishing a suggestion the consumption seam's
/// drivability gate refuses would produce.
///
/// The second observable is the pair `deliverable_capacity >= iteration_count`, asserted on every
/// board in the population, with the boards where the two COINCIDE asserted to coincide in the same
/// invocation so the row is not satisfied by a difference that exists everywhere.
///
/// # A live instrument
///
/// The population is asserted non-empty, and every board is asserted to publish a `Fixed`
/// suggestion, so a walk that reached no offer fails rather than passing by absence.
///
/// REVERT-PROBE: publish the WITNESS count as the suggestion ⇒ the allocated boards' own
/// declaration cannot drive it, the declare opens the window and the accept commits zero cycles ⇒
/// row 5's board-moved assertion FAILS. Publish the SUGGESTION as the ceiling ⇒ row 5b's pair still
/// holds but the differ leg below finds no board, and its own guard fires.
#[test]
fn every_live_bounded_offer_publishes_a_suggestion_its_own_declaration_drives() {
    let boards: &[(&str, &[u8])] = &[
        (
            "dina_noff_turn5_4p",
            &include_bytes!("../fixtures/dina_noff_turn5_4p.json.gz")[..],
        ),
        (
            "dina_conqueror_4p",
            &include_bytes!("../fixtures/dina_conqueror_4p.json.gz")[..],
        ),
        (
            "dina_conqueror_phase5_no_offer_4p",
            &include_bytes!("../fixtures/dina_conqueror_phase5_no_offer_4p.json.gz")[..],
        ),
        (
            "lethal_lifegain_loss_4p",
            &include_bytes!("../fixtures/lethal_lifegain_loss_4p.json.gz")[..],
        ),
        (
            "weird_drain_4p",
            &include_bytes!("../fixtures/weird_drain_4p.json.gz")[..],
        ),
    ];
    let mut reached: Vec<&str> = Vec::new();
    let mut coincide: Vec<&str> = Vec::new();
    for &(label, gz) in boards {
        let mut at_offer = restore_dump(&gunzip_dump(gz));
        if drive_to_bounded_offer(&mut at_offer, 600).is_none() {
            continue;
        }
        let (proposer, _, schema) = bounded_offer_parts(&at_offer);
        reached.push(label);
        let IterationCount::Fixed(suggestion) = schema.iteration_count else {
            panic!("[{label}] a bounded offer publishes a `Fixed` suggestion, got {schema:?}");
        };
        let capacity = schema.deliverable_capacity;

        // ── ROW 5b — the pair, on every board ──
        assert!(
            capacity >= suggestion && suggestion >= 1,
            "[{label}] CR 732.2a: the declare handler refuses any count above the capacity, so a \
             ceiling below the suggestion would have it refuse the offer's own suggestion; got \
             capacity {capacity} vs suggestion {suggestion}"
        );
        if capacity == suggestion {
            coincide.push(label);
        }

        // ── ROW 5 — the declaration opens the window, and the accept MOVES the board ──
        let before = seat_lives(&at_offer);
        let mut driven = at_offer.clone();
        apply(
            &mut driven,
            proposer,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(suggestion),
                template: None,
            },
        )
        .unwrap_or_else(|e| {
            panic!("[{label}] the declare handler must accept {suggestion}: {e:?}")
        });
        assert!(
            matches!(driven.waiting_for, WaitingFor::RespondToShortcut { .. }),
            "[{label}] REACH-GUARD: the declaration must OPEN the CR 732.2b response window, which \
             is what separates 'declare refused' from 'drive aborted'; got {:?}",
            driven.waiting_for
        );
        while let WaitingFor::RespondToShortcut { player, .. } = driven.waiting_for.clone() {
            apply(
                &mut driven,
                player,
                GameAction::RespondToShortcut {
                    response: ShortcutResponse::Accept,
                },
            )
            .unwrap_or_else(|e| panic!("[{label}] each living opponent accepts: {e:?}"));
        }
        assert_ne!(
            seat_lives(&driven),
            before,
            "[{label}] CR 732.2c: the accepted sequence is TAKEN — a suggestion the consumption \
             seam's drivability gate refuses commits zero cycles and hands priority back with the \
             board unmoved"
        );
    }
    assert!(
        reached.len() > 1,
        "LIVE INSTRUMENT: the walk must reach a live bounded offer on more than one committed \
         board, or every assertion above is an absence-shaped pass; reached {reached:?}"
    );
    assert!(
        !coincide.is_empty(),
        "LIVE INSTRUMENT: at least one board must publish a capacity EQUAL to its suggestion, so \
         the inequality above is not satisfied by a difference that exists everywhere; reached \
         {reached:?}"
    );
}

/// **CR 800.4a + CR 704.5a — an accept that reaches a crossing commits the cycle that crossing
/// falls on, and that seat leaves the game.**
///
/// On this committed 4p drain the crossing cycle's published period completes at the forced-window
/// answer beat, so the drive classifies it as a RECURRENCE and reaches the per-cycle conformance
/// check rather than the departure arm. The departing seat's own library leaves the game with them
/// (CR 800.4a), which the observed period reports on an axis the published period is silent on, so
/// a whole-population comparison refuses the very cycle the published count was derived to reach:
/// the accept commits one cycle fewer and eliminates nobody. Scoping that comparison to the seats
/// still in the game is what makes the published count deliver the crossing it named.
///
/// # Everything quantified over is re-derived from the offer's own published data
///
/// The count driven is the FIRST crossing the published cascade contains, and the seat it takes:
/// both come from `PeriodicDelta::first_life_crossing` under the declaration this offer publishes,
/// over the live lives at the offer beat, with the published ceiling as the horizon. The ceiling
/// reaches further — the whole cascade — which is asserted here and driven by
/// `one_accept_crosses_every_threshold_the_published_ceiling_contains`; this row is about the
/// conformance scope at ONE crossing cycle, so it drives the first one and keeps its paired
/// one-below leg. No count and no seat is written down here.
///
/// # Reach guards, asserted before the claim
///
/// More than two opponents are alive at the offer beat, so the crossing does not take the last one
/// and CR 104.2a is not what ends the drive. Exactly ONE living opponent has a crossing inside the
/// published count while the others have none — which is both the "more than one member" guard's
/// negative half and this instrument's live control: a `first_life_crossing` that answered `Some`
/// for everything, or `None` for everything, fails that count.
///
/// # Non-vacuity — the PAIRED SUB-CASCADE ACCEPT, on this board, in this invocation
///
/// One count below the published one drives on the same board and eliminates nobody, leaving the
/// crossing seat alive at exactly one period's worth of life. So the elimination below is the
/// crossing cycle committing, not a drive that eliminates whatever it reaches; and the survivors'
/// life deltas prove both accepts really drove their full counts.
///
/// REVERT-PROBE: take the per-cycle conformance check over the whole population — the unscoped
/// `conforms` — and the crossing cycle is dropped whole: nobody is eliminated and the crossing
/// seat ends at one period's worth of life, which is the sub-cascade leg's own expectation.
#[test]
fn the_accept_at_a_cascade_crossing_commits_that_cycle_and_eliminates_its_seat() {
    let at_offer = {
        let mut state = restore_dump(&gunzip_dump(include_bytes!(
            "../fixtures/dina_noff_turn5_4p.json.gz"
        )));
        drive_to_bounded_offer(&mut state, 600)
            .expect("the bounded offer must fire on this committed 4p drain");
        state
    };
    let (proposer, certificate, schema) = bounded_offer_parts(&at_offer);
    assert!(
        schema.is_bounded(),
        "REACH-GUARD: this row is about a BOUNDED offer's published ceiling"
    );
    let ceiling = schema.deliverable_capacity;
    let points = schema.points.clone();
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");
    let declaration = match &at_offer.waiting_for {
        WaitingFor::LoopShortcut { declaration, .. } => declaration.clone(),
        other => panic!("bounded_offer_parts already matched the offer, got {other:?}"),
    };

    let opponents: Vec<(PlayerId, i32)> = at_offer
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.id != proposer)
        .map(|p| (p.id, p.life))
        .collect();
    assert!(
        opponents.len() > 2,
        "REACH-GUARD: more than two opponents must be alive, or the crossing takes the last one \
         and CR 104.2a ends the game instead of handing priority back; got {opponents:?}"
    );

    // The crossing the published count contains, re-derived through the engine's own authority.
    let crossings: Vec<(PlayerId, u32)> = opponents
        .iter()
        .filter_map(|&(seat, life)| {
            engine::analysis::resource::PeriodicDelta::first_life_crossing(
                per_cycle.declared_seat_life_charges(
                    seat,
                    declaration.as_ref(),
                    declaration.as_ref(),
                    &points,
                    &at_offer,
                    // The crossing the PUBLISHED ceiling contains, so the ceiling direction.
                    ChargeBound::Ceiling,
                    engine::analysis::resource::AnnouncedLead::None,
                ),
                i64::from(life),
                ceiling,
            )
            .map(|repetition| (seat, repetition))
        })
        .collect();
    let first = crossings
        .iter()
        .map(|&(_, repetition)| repetition)
        .min()
        .expect("REACH-GUARD (live instrument): the published ceiling must contain a crossing");
    let at_first: Vec<(PlayerId, u32)> = crossings
        .iter()
        .copied()
        .filter(|&(_, r)| r == first)
        .collect();
    let [(crossing_seat, repetition)] = at_first[..] else {
        panic!(
            "REACH-GUARD (live instrument): exactly one opponent may cross on the cascade's FIRST \
             repetition — one is this row's claim, and a tie there would make the single-seat \
             elimination below the wrong assertion; got {crossings:?} over {opponents:?}"
        );
    };
    assert!(
        crossings.iter().any(|&(_, r)| r > repetition),
        "REACH-GUARD: the published ceiling {ceiling} must reach PAST this crossing, else driving \
         {repetition} is driving the ceiling itself and the cascade holds a single entry; got \
         {crossings:?}"
    );
    let per_period = -per_cycle
        .delta
        .life
        .get(&crossing_seat)
        .copied()
        .expect("the published period charges the seat it crosses");
    assert!(
        per_period > 0,
        "REACH-GUARD: the published period must DRAIN that seat, or the life arithmetic below \
         states nothing; got {per_period}"
    );

    // ⓐ THE FIRST CROSSING — its cycle commits and the seat leaves the game.
    let mut driven = at_offer.clone();
    r6a_declare_and_accept_all(&mut driven, proposer, repetition);
    let gone: Vec<PlayerId> = driven
        .players
        .iter()
        .filter(|p| p.is_eliminated)
        .map(|p| p.id)
        .collect();
    assert_eq!(
        gone,
        vec![crossing_seat],
        "CR 704.5a: the accept at the first crossing takes the seat that crossing names, and \
         only that seat; lives {:?}",
        seat_lives(&driven)
    );
    assert!(
        matches!(driven.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: two opponents survive, so the ending point is a priority window and not a \
         CR 104.2a game end; got {:?}",
        driven.waiting_for
    );
    for &(seat, life) in &opponents {
        if seat == crossing_seat {
            continue;
        }
        let now = driven
            .players
            .iter()
            .find(|p| p.id == seat)
            .expect("a surviving seat is still on the board")
            .life;
        assert_eq!(
            i64::from(life - now),
            i64::from(repetition) * per_period,
            "CONTROL: every survivor takes the full accepted count of periods, so ⓐ drove the \
             whole sequence rather than stopping early"
        );
    }

    // ⓑ THE PAIRED SUB-CASCADE ACCEPT — one count lower, on this same board.
    let mut sub = at_offer.clone();
    r6a_declare_and_accept_all(&mut sub, proposer, repetition - 1);
    assert!(
        sub.players.iter().all(|p| !p.is_eliminated),
        "SUB-CASCADE: one count below the crossing nobody reaches a threshold; lives {:?}",
        seat_lives(&sub)
    );
    let survived = sub
        .players
        .iter()
        .find(|p| p.id == crossing_seat)
        .expect("the crossing seat is still on the board")
        .life;
    assert_eq!(
        i64::from(survived),
        per_period,
        "SUB-CASCADE: the crossing seat is left exactly one period short of its threshold, which \
         is what makes ⓐ's elimination the crossing CYCLE and not the count"
    );
}

/// **ROW 7b — CR 704.3 + CR 704.5a + CR 800.4a: a TIE ENTRY's WHOLE seat set must depart on that
/// repetition, and a subset is refused.**
///
/// The seam this closes was left undiscriminated on purpose: with a one-entry cascade the drive's
/// only reachable departure was the one that entry named, so no test could fail in the direction
/// the verdict guards. A cascade that can GROUP two seats into one entry is what makes it
/// reachable, and this is the member a one-seat entry cannot express.
///
/// The mismatch is manufactured in the SIGNATURE, not in the board: a published per-seat charge
/// above the period's own net loss raises that seat's `dip` — the deepest point inside one
/// repetition, CR 704.3's own term — so the reduction reads it as crossing on the same repetition
/// as a seat that really is one point from its threshold. The driven cycle then takes ONE of the
/// two. This is the `#[serde(default)]`/hostile-restore shape that field's own doc names, reached
/// through the restore ingress the consumption re-derivation exists for.
///
/// # Reach guards, asserted before the claim
///
/// The cascade the perturbed signature implies is asserted to carry an entry holding TWO seats at
/// the accepted repetition (else this row is the co-departure row again, with a one-seat entry),
/// and both seats are asserted ALIVE going in (else "only one could have left" satisfies the
/// refusal for free).
///
/// # Discrimination
///
/// Collapse the entry to one seat — which is what refusing the grouping does — and ⓐ's departure
/// matches it, so the cycle commits and the unmoved-board assertion FAILS. Compare by LENGTH plus
/// a membership test rather than by set equality and the subset is admitted, failing the same
/// assertion. ⓑ is the paired positive on the same board and the same accepted count with only the
/// inflated charge removed: the identical departure COMMITS against the one-seat entry, so ⓐ's
/// refusal is the set comparison and not the rig.
#[test]
fn a_tie_entrys_seat_set_is_refused_when_only_one_of_them_departs() {
    let (state, proposer, certificate, _honest) = dina_bounded_offer();
    let published = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");
    // P2 is one point from its threshold; P3 is three, with a published charge of three, which
    // raises its `dip` to three and makes the reduction read both as crossing on repetition 1.
    let board = || -> GameState {
        let mut b = state.clone();
        for (seat, life) in [(P1, 36), (P2, 1), (P3, 3)] {
            b.players.iter_mut().find(|p| p.id == seat).unwrap().life = life;
        }
        b
    };
    let with_charge = |charge: Vec<(PlayerId, i64)>| -> PeriodicDelta {
        let mut pc = published.clone();
        pc.seat_life_charge = charge;
        pc
    };

    // ⓐ THE TIE ENTRY AGAINST A SUBSET DEPARTURE.
    let mut grouped = board();
    let before = seat_lives(&grouped);
    let tied = with_charge(vec![(P2, 1), (P3, 3)]);
    let cascade = cascade_from(&grouped, proposer, &tied, None, &[]);
    assert!(
        cascade
            .first()
            .is_some_and(|(repetition, seats)| *repetition == 1 && seats.len() == 2),
        "REACH-GUARD (live instrument): the perturbed signature must imply an entry holding TWO \
         seats at the accepted repetition, or this row is the one-seat co-departure row; got \
         {cascade:?}"
    );
    assert!(
        [P2, P3].iter().all(|seat| !grouped
            .players
            .iter()
            .any(|p| p.id == *seat && p.is_eliminated)),
        "REACH-GUARD: both seats are ALIVE going in, so the refusal is about a departure that \
         really happens rather than one that could not"
    );
    accept_restored_proposal(
        &mut grouped,
        proposer,
        restored_proposal(&certificate, proposer, 1, tied),
    );
    assert_eq!(
        seat_lives(&grouped),
        before,
        "CR 704.3 + CR 800.4a: the entry named two seats and one left, so the sequence performed \
         is not the one the table agreed to and the cycle is dropped whole; cascade {cascade:?}"
    );
    assert_eq!(
        eliminated_seats(&grouped),
        Vec::<PlayerId>::new(),
        "CR 800.4a: a refused cycle removes nobody"
    );
    assert!(
        matches!(grouped.waiting_for, WaitingFor::Priority { player }
            if !grouped.players.iter().any(|p| p.id == player && p.is_eliminated)),
        "CR 732.2a: the dropped cycle ends at a place where a player has priority; got {:?}",
        grouped.waiting_for
    );

    // ⓑ PAIRED POSITIVE — the same board and the same accepted count, with only P3's inflated
    //   charge removed, so the entry names the one seat that actually leaves.
    let mut alone = board();
    let single = with_charge(vec![(P2, 1)]);
    let one_seat = cascade_from(&alone, proposer, &single, None, &[]);
    assert!(
        one_seat
            .first()
            .is_some_and(|(repetition, seats)| *repetition == 1 && seats.len() == 1),
        "REACH-GUARD: the paired positive's entry must hold exactly ONE seat, or it is not the \
         sibling of ⓐ; got {one_seat:?}"
    );
    accept_restored_proposal(
        &mut alone,
        proposer,
        restored_proposal(&certificate, proposer, 1, single),
    );
    assert_eq!(
        eliminated_seats(&alone),
        vec![P2],
        "PAIRED POSITIVE: the identical departure COMMITS against a one-seat entry, so ⓐ's \
         refusal is the set comparison and not the rig"
    );
}

/// CR 732.2b: declares the offer's count and, in the response window, applies `edit` to the
/// departures of the record the take reads (its fields are crate-private, so through serde); then
/// every responder accepts. Returns the departures as the offer recorded them.
pub(crate) fn take_with_edited_departures(
    at_offer: &GameState,
    edit: fn(&mut Vec<serde_json::Value>),
) -> (Vec<(u32, Vec<PlayerId>)>, GameState) {
    fn walk(
        value: &mut serde_json::Value,
        edit: fn(&mut Vec<serde_json::Value>),
        recorded: &mut Vec<serde_json::Value>,
    ) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, field) in fields.iter_mut() {
                    match (key.as_str(), field) {
                        ("departures", serde_json::Value::Array(entries)) => {
                            recorded.clone_from(entries);
                            edit(entries);
                        }
                        (_, field) => walk(field, edit, recorded),
                    }
                }
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().for_each(|item| walk(item, edit, recorded));
            }
            _ => {}
        }
    }
    let (proposer, count, _) = offered(at_offer);
    let mut state = at_offer.clone();
    apply(
        &mut state,
        proposer,
        GameAction::DeclareShortcut {
            count,
            template: None,
        },
    )
    .expect("the offer's own count is declarable");
    let WaitingFor::RespondToShortcut { proposal, .. } = &mut state.waiting_for else {
        panic!(
            "the declaration opens the CR 732.2b window, got {:?}",
            state.waiting_for
        );
    };
    let mut value = serde_json::to_value(&*proposal).expect("a proposal serializes");
    let mut recorded = Vec::new();
    walk(&mut value["period"], edit, &mut recorded);
    *proposal = serde_json::from_value(value).expect("the edited proposal deserializes");
    let recorded: Vec<(u32, Vec<PlayerId>)> = recorded
        .iter()
        .map(|entry| {
            (
                u32::try_from(entry["repetition"].as_u64().expect("a repetition"))
                    .expect("a repetition fits a u32"),
                serde_json::from_value(entry["seats"].clone()).expect("the departing seats"),
            )
        })
        .collect();
    assert!(
        !recorded.is_empty(),
        "reach-guard: the record the take reads predicts departures"
    );
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            &mut state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("accept");
    }
    (recorded, state)
}

/// CR 732.2c: a take refused at the repetition `recorded[refused]`'s seat leaves on commits every
/// repetition before it (that seat's life at the offer frame minus 1) and hands priority back.
fn assert_refused_at(
    label: &str,
    at_offer: &GameState,
    taken: &GameState,
    recorded: &[(u32, Vec<PlayerId>)],
    refused: usize,
) {
    let life = |state: &GameState, seat: PlayerId| {
        state
            .players
            .iter()
            .find(|p| p.id == seat)
            .expect("a seat at the table")
            .life
    };
    let departing = recorded[refused].1[0];
    let committed = life(at_offer, departing) - 1;
    let mut left: Vec<PlayerId> = recorded[..refused]
        .iter()
        .flat_map(|(_, seats)| seats.iter().copied())
        .collect();
    left.sort_unstable();
    let (proposer, ..) = offered(at_offer);
    assert_eq!(
        eliminated_seats(taken),
        left,
        "[{label}] CR 800.4a: only the departures before the refused repetition commit"
    );
    for seat in at_offer
        .players
        .iter()
        .map(|p| p.id)
        .filter(|seat| *seat != proposer && !left.contains(seat))
    {
        assert_eq!(
            life(taken, seat),
            life(at_offer, seat) - committed,
            "[{label}] {seat:?}: CR 732.2c: every repetition before the refused one commits"
        );
    }
    assert_eq!(
        taken.waiting_for,
        WaitingFor::Priority { player: proposer },
        "[{label}]"
    );
}

fn dina_noff_offer() -> GameState {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dina_noff_turn5_4p.json.gz"
    )));
    drive_to_bounded_offer(&mut state, 600).expect("the committed 4p drain is offered");
    state
}

fn shift_departures_one_early(entries: &mut Vec<serde_json::Value>) {
    for entry in entries {
        let repetition = entry["repetition"].as_u64().expect("a repetition");
        entry["repetition"] = serde_json::json!(repetition - 1);
    }
}

fn assert_unedited_take_wins(label: &str, at_offer: &GameState) {
    let (_, taken) = take_with_edited_departures(at_offer, |_| {});
    assert_eq!(
        taken.waiting_for,
        WaitingFor::GameOver {
            winner: Some(offered(at_offer).0)
        },
        "[{label}] locus control: the unedited record's take ends the game"
    );
}

/// CR 732.2a + CR 732.2c: a departure the record did not predict is refused from either arm of the
/// recorded take, every repetition before it committed. On the turn-5 Dina board every entry is
/// moved one repetition early, so P1's departing repetition comes round unpredicted; on the
/// Conqueror board the last entry names P1 for P3, so P3's departing repetition ends with its
/// holder winning, unpredicted.
#[test]
fn an_unpredicted_departure_is_refused_from_either_drive_arm() {
    let noff = dina_noff_offer();
    let (recorded, taken) = take_with_edited_departures(&noff, shift_departures_one_early);
    assert_refused_at("came round", &noff, &taken, &recorded, 0);
    assert_unedited_take_wins("came round", &noff);

    let conqueror = dina_bounded_offer().0;
    let (recorded, taken) = take_with_edited_departures(&conqueror, |entries| {
        entries.last_mut().expect("a last departure")["seats"] = serde_json::json!([1]);
    });
    assert_refused_at(
        "holder won",
        &conqueror,
        &taken,
        &recorded,
        recorded.len() - 1,
    );
    assert_unedited_take_wins("holder won", &conqueror);
}

/// **V3 — CR 704.5a + CR 800.4a: the admitted member the set equality must refuse — the
/// co-departure.**
///
/// A signature that omits one drained seat's life entry — the shape a
/// `#[serde(default)]` charge field really produces on the wire — makes the reduction predict
/// ONE seat while the driven cycle takes TWO. Committing that cycle would apply a removal the
/// table never agreed to.
///
/// Both crossings land on the FIRST driven cycle, which is therefore the terminal one and runs
/// no conformance check at all — so nothing but the set comparison can be producing the
/// refusal.
///
/// # Non-vacuity
///
/// The paired positive is the same board and the same signature with the second crossing
/// removed: it commits and eliminates the predicted seat alone. Both seats are asserted ALIVE
/// going in, so "only one seat could ever have left" cannot satisfy the negative.
///
/// REVERT-PROBE: commit on every departure (drop the set comparison) ⇒ ⓐ eliminates two seats
/// and its unmoved-board assertion FAILS while ⓑ stays green.
#[test]
fn a_co_departure_the_prediction_does_not_name_is_refused() {
    let (state, proposer, certificate, _honest) = dina_bounded_offer();
    let published = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");

    // The signature omits P1's life entry, so the reduction cannot see P1 at all and names P2.
    let blind = |mut pc: PeriodicDelta| -> PeriodicDelta {
        pc.seat_life_charge.clear();
        pc.delta.life.remove(&P1);
        pc
    };

    // ⓐ TWO seats one repetition from their threshold; only one of them is in the signature.
    let mut both = state.clone();
    for (seat, life) in [(P1, 1), (P2, 1)] {
        both.players.iter_mut().find(|p| p.id == seat).unwrap().life = life;
    }
    let before = seat_lives(&both);
    assert!(
        [P1, P2].iter().all(|seat| !both
            .players
            .iter()
            .any(|p| p.id == *seat && p.is_eliminated)),
        "REACH-GUARD: both seats are ALIVE going in, so the refusal below is about a departure \
         that really happens rather than one that could not"
    );
    accept_restored_proposal(
        &mut both,
        proposer,
        restored_proposal(&certificate, proposer, 1, blind(published.clone())),
    );
    assert_eq!(
        seat_lives(&both),
        before,
        "CR 732.2a: the driven cycle removed a seat the sequence never predicted, so it is \
         dropped whole and the board is the one the offer was made on"
    );
    assert_eq!(
        eliminated_seats(&both),
        Vec::<PlayerId>::new(),
        "CR 800.4a: a refused cycle removes nobody"
    );
    assert!(
        matches!(both.waiting_for, WaitingFor::Priority { player }
            if !both.players.iter().any(|p| p.id == player && p.is_eliminated)),
        "CR 732.2a: the dropped cycle ends at a place where a player has priority; got {:?}",
        both.waiting_for
    );

    // ⓑ PAIRED POSITIVE — the same board and signature with the second crossing removed.
    let mut one = state.clone();
    for (seat, life) in [(P1, 36), (P2, 1)] {
        one.players.iter_mut().find(|p| p.id == seat).unwrap().life = life;
    }
    accept_restored_proposal(
        &mut one,
        proposer,
        restored_proposal(&certificate, proposer, 1, blind(published)),
    );
    assert_eq!(
        eliminated_seats(&one),
        vec![P2],
        "PAIRED POSITIVE: with only the predicted seat crossing, the terminal cycle COMMITS and \
         removes exactly that seat — so ⓐ's refusal is the co-departure, not the rig"
    );
}

/// CR 732.2a + CR 732.2c: a record that predicts no departure admits none; the take stops before
/// the first seat leaves.
#[test]
fn an_absent_prediction_admits_no_departure() {
    let at_offer = dina_bounded_offer().0;
    let (recorded, taken) = take_with_edited_departures(&at_offer, Vec::clear);
    assert_refused_at("cleared", &at_offer, &taken, &recorded, 0);
    assert_unedited_take_wins("cleared", &at_offer);
}

/// CR 732.2a + CR 732.2c: a departure on a repetition the record did not name is refused, though
/// the record names its seat.
#[test]
fn a_departure_on_an_iteration_the_prediction_did_not_name_is_refused() {
    let at_offer = dina_bounded_offer().0;
    let (recorded, taken) = take_with_edited_departures(&at_offer, shift_departures_one_early);
    assert_refused_at("one early", &at_offer, &taken, &recorded, 0);
    assert_unedited_take_wins("one early", &at_offer);
}

/// **V5b — CR 732.2a: an unsigned proposal keeps COMMIT-AND-STOP at a mid-drive departure.**
///
/// The row above covers the GUARD's half of the population boundary — no signature, no derived
/// ceiling. This is the DRIVE's half, on the same boundary: with no signature there is no
/// prediction for the terminal arm to discriminate against, so the cycle a seat departs on is
/// committed and the drive stops there, which is what every producer publishing no per-period
/// signature shipped with.
///
/// Four seats, one of them one point from its threshold and the others far from theirs, so the
/// removal is a single seat's and TWO seats survive it — the terminal `SeatLeft` arm rather
/// than the `CrossLethal` crown, which the surviving-seat count and the priority handback
/// separate.
///
/// # Non-vacuity
///
/// Every seat is asserted in the game going in, and the committed board is asserted to have
/// moved: a refused cycle is dropped whole, so BOTH the elimination and the moved life vector
/// are false under a refusal.
///
/// REVERT-PROBE: take this arm's refusal from the EMPTINESS of the prediction instead of from
/// the signature's presence ⇒ the unsigned drive is refused ⇒ the elimination FAILS.
#[test]
fn an_unsigned_proposal_commits_the_cycle_a_seat_departs_on() {
    let (state, proposer, certificate, _honest) = dina_bounded_offer();
    let published = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");

    let mut board = state.clone();
    for (seat, life) in [(P1, 1), (P2, 36), (P3, 36)] {
        board
            .players
            .iter_mut()
            .find(|p| p.id == seat)
            .unwrap()
            .life = life;
    }
    let before = seat_lives(&board);
    assert!(
        board.players.iter().all(|p| !p.is_eliminated),
        "REACH-GUARD: every seat is in the game going in, so the removal below is one that \
         really happens rather than one that already had"
    );

    // The unsigned proposal: the restore ingress's own shape with the signature ABSENT, which
    // is what this seam reads — it consults `proposal.per_cycle` and nothing about the producer.
    accept_restored_proposal(
        &mut board,
        proposer,
        ShortcutProposal {
            per_cycle: None,
            ..restored_proposal(&certificate, proposer, 1, published)
        },
    );

    assert_eq!(
        eliminated_seats(&board),
        vec![P1],
        "CR 732.2a: an unsigned proposal predicts nothing, so there is no divergence to refuse \
         — the drive commits the cycle the removal landed on and that seat has left the game"
    );
    assert_ne!(
        seat_lives(&board),
        before,
        "the committed board is the DRIVEN one; a refused cycle is dropped whole and would \
         leave the life vector the offer was made on"
    );
    assert!(
        board.players.iter().filter(|p| !p.is_eliminated).count() >= 2,
        "REACH-GUARD: two seats survive the removal, so this is the terminal arm a continuing \
         game reaches and not the CR 104.2a crown"
    );
    assert!(
        matches!(board.waiting_for, WaitingFor::Priority { player }
            if !board.players.iter().any(|p| p.id == player && p.is_eliminated)),
        "CR 732.2a: the drive STOPS at an ending point where a living player has priority; got \
         {:?}",
        board.waiting_for
    );
}

/// **V8 — the `CrossLethal` arm, reached at an HONEST count.**
///
/// The consumption ceiling turns every widened count into a refusal, so the arm that used to be
/// reached by declaring past the bound needs a board that reaches it legitimately. This is that
/// board: two seats, so the unique predicted seat's crossing takes the last surviving opponent
/// and CR 104.2a crowns immediately. The drive commits that cycle and the game ends — the
/// disposition the `SeatLeft` discriminator deliberately does not govern, because CR 104.2a
/// leaves no remaining repetition to be unmakeable and no later priority beat for CR 704.3 to
/// sweep.
///
/// Every quantity is derived from the certificate the engine published; no count is a literal.
///
/// REVERT-PROBE: extend the `SeatLeft` discriminator over the `CrossLethal` arm ⇒ the crowning
/// cycle is dropped ⇒ the `GameOver` assertion FAILS.
#[test]
fn the_honest_count_reaches_the_cross_lethal_arm_when_the_crossing_takes_the_last_opponent() {
    let mut state = bloodloop_state(2);
    drive_to_bounded_offer(&mut state, 400)
        .expect("the mandatory-draw cascade raises a bounded offer at two seats");
    let (proposer, certificate, schema) = bounded_offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");
    let honest = schema.deliverable_capacity;
    let opponents: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated && p.id != proposer)
        .map(|p| p.id)
        .collect();
    assert_eq!(
        opponents.len(),
        1,
        "REACH-GUARD: exactly ONE living opponent is what makes the crossing a CR 104.2a win \
         rather than the terminal-removal arm; got {opponents:?}"
    );
    let victim = opponents[0];
    let loss = -per_cycle.delta.life.get(&victim).copied().unwrap_or(0);
    assert!(
        loss > 0,
        "REACH-GUARD: the published period must drain the opponent, else no crossing is derivable"
    );
    let l0 = state.players.iter().find(|p| p.id == victim).unwrap().life as i64;
    let crossing = l0.div_euclid(loss) + i64::from(l0.rem_euclid(loss) != 0);
    assert!(
        crossing <= i64::from(honest),
        "CR 704.5a: the published ceiling spans the cascade, so it CONTAINS the opponent's \
         crossing — which is what makes the honest count reach this arm at all; ceiling {honest} \
         vs crossing {crossing}"
    );

    let result = take_recorded(&mut state, honest);

    // CR 732.1b + CR 104.2a: each repetition is Psychosis Crawler's "Whenever you draw a card,
    // each opponent loses 1 life." answered by a draw, as Kefka, Ruler of Ruin's "Whenever an
    // opponent loses life during your turn, you draw that many cards." does on P0's turn at a
    // loss of 1; the crossing repetition ends the game, and the record keeps the first and it.
    let crossing = u32::try_from(crossing).expect("fits");
    assert_eq!(
        markers(&result.events),
        [(proposer, 1), (proposer, crossing)]
    );

    assert_eq!(
        state.waiting_for,
        WaitingFor::GameOver {
            winner: Some(proposer)
        },
        "CR 104.2a: a player wins once all their opponents have left, and the crossing cycle \
         COMMITS rather than rolling back; lives {:?}",
        state.players.iter().map(|p| p.life).collect::<Vec<_>>()
    );
    assert_eq!(
        eliminated_seats(&state),
        vec![victim],
        "CR 704.5a: exactly the drained seat crossed its threshold"
    );
    assert_eq!(
        per_cycle.delta.life.get(&proposer).copied(),
        None,
        "REACH-GUARD: this period charges the proposer's life not at all, so the crossing above \
         is attributable to the drained seat alone"
    );
}

/// FIX ROUND 1 (MED-4) — the AI's bounded-declare candidate is GENERATED, LEGAL, and DRIVES.
///
/// The `if schema.points.is_empty() && schema.is_bounded()` block in
/// `ai_support::candidates` shipped with zero coverage: deleting it left the engine suite and
/// every `phase-ai` suite green. (Fix round 3, LOW-3: a bare "(4167)" stood here with neither a
/// runner nor a filter recorded beside it, so it named a shape nobody could reproduce; it is
/// deleted rather than re-dressed, exactly as the same count was at
/// `bounded_offer_conjunct_tests`' module doc. The reproducible claim is this row's own
/// REVERT-PROBE line below.) Its sibling one screen away
/// (`the_collapse_candidate_is_clamped_to_a_bound_restored_from_an_older_save`) sets the standard
/// this row mirrors — generate the candidate through the production generator, then `apply()` it.
///
/// Without that candidate an AI proposer at a bounded offer can only decline. The block is the
/// difference between an AI that can take this shortcut and one that structurally cannot.
///
/// ⚠ RECORDED, NOT FIXED (a scope note, measured here): the block is gated on
/// `schema.points.is_empty()`. A TARGETED bounded offer publishes pins, so the AI gets no accept
/// candidate at all — the `Fixed` candidate carries `template: None`, which fail-closes on
/// published pins. A targeted bounded offer is therefore AI-undeclarable today. That is a coverage
/// gap in the candidate generator, not a soundness bug (the AI declines, which is always legal),
/// and it is left for its own round.
///
/// REVERT-PROBE: delete the `schema.points.is_empty() && schema.is_bounded()` block ⇒ assertion (2)
/// FAILS (`Fixed(suggestion)` absent from the generated candidates).
#[test]
fn ai_bounded_declare_candidate_is_generated_legal_and_drives() {
    use engine::analysis::decision_template::IterationCount;

    let mut state = bloodloop_state(3);
    drive_to_bounded_offer(&mut state, 400).expect("the bounded offer must fire at 3 players");
    let (proposer, certificate, schema) = bounded_offer_parts(&state);
    let per_cycle = certificate
        .per_cycle
        .as_ref()
        .and_then(engine::analysis::resource::PeriodSignature::every_repetition)
        .cloned()
        .expect("a bounded offer publishes its per-period signature");
    let suggestion = crate::fantastic_four_bounded_loop::published_suggestion(schema);

    // (1) reach-guards: this row is about the BOUNDED, UNTARGETED shape the block gates on.
    assert!(
        schema.is_bounded(),
        "REACH-GUARD: an unbounded offer takes a different generator arm; suggestion = \
         {suggestion}"
    );
    assert!(
        schema.points.is_empty(),
        "REACH-GUARD: the block is gated on an empty pin set; got {:?}",
        schema.points
    );

    // (2) the production generator offers it — the SUGGESTION the offer publishes, which is the
    //     count a declarer overriding nothing names.
    let expected = GameAction::DeclareShortcut {
        count: IterationCount::Fixed(suggestion),
        template: None,
    };
    let candidates = engine::ai_support::legal_actions(&state);
    assert!(
        candidates.contains(&expected),
        "the AI must be able to declare the bounded offer's own count; got {candidates:?}"
    );

    // The cascade's first crossing, re-derived from the published period and the offer-beat
    // board, where the accepted count ends.
    let lives_before: Vec<(PlayerId, i64)> = state
        .players
        .iter()
        .map(|p| (p.id, p.life as i64))
        .collect();
    let loss = |seat: &PlayerId| -per_cycle.delta.life.get(seat).copied().unwrap_or(0);
    let crossings: Vec<(PlayerId, i64)> = lives_before
        .iter()
        .filter(|(id, _)| loss(id) > 0)
        .map(|(id, l0)| {
            (
                *id,
                l0.div_euclid(loss(id)) + i64::from(l0.rem_euclid(loss(id)) != 0),
            )
        })
        .collect();
    let first = crossings
        .iter()
        .map(|(_, c)| *c)
        .min()
        .expect("REACH-GUARD: the published period must consume a seat on the life axis");
    assert_eq!(
        first,
        i64::from(suggestion),
        "REACH-GUARD: CR 104.2a + CR 704.3: the crossing that takes both opponents ends the game, \
         so the published suggestion is that crossing; crossings {crossings:?} under \
         {suggestion}"
    );
    let first_victims: Vec<PlayerId> = crossings
        .iter()
        .filter(|(_, c)| *c == first)
        .map(|(id, _)| *id)
        .collect();

    // (3) ...and the reducer ACCEPTS it — which is what makes (2) load-bearing rather than a
    //     restatement of the generator. A refused declaration hands straight back to priority.
    apply(&mut state, proposer, expected)
        .expect("the AI's generated candidate must be accepted by the reducer");
    assert!(
        matches!(state.waiting_for, WaitingFor::RespondToShortcut { .. }),
        "CR 732.2b: an accepted declaration opens the APNAP response window; a handback to \
         Priority would mean the generator produced a count the engine refuses. got {:?}",
        state.waiting_for
    );

    // (4) ...and the accepted count DRIVES, to the first crossing the cascade contains. Bound to
    //     the published period, never a literal.
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            &mut state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
    assert_eq!(
        eliminated_seats(&state),
        first_victims,
        "CR 704.5a: the drive commits through the cascade's first entry and exactly its seats \
         leave; lives {:?}",
        seat_lives(&state)
    );
    for (seat, l0) in &lives_before {
        if first_victims.contains(seat) {
            continue;
        }
        let now = state.players.iter().find(|p| p.id == *seat).unwrap().life as i64;
        assert_eq!(
            now - l0,
            first * per_cycle.delta.life.get(seat).copied().unwrap_or(0),
            "{seat:?}: every seat still in the game has taken exactly the first crossing's \
             copies of the published period, so the drive reached that cycle and stopped there"
        );
    }
    assert_eq!(
        state.waiting_for,
        WaitingFor::GameOver {
            winner: Some(proposer)
        },
        "CR 104.2a: this board's first crossing takes EVERY living opponent in one CR 704.3 \
         sweep, so the AI's own declaration ends the game; eliminated {:?}",
        eliminated_seats(&state)
    );
}

// ---------------------------------------------------------------------------
// G1 — THE VALIDATED RANGE MUST COVER THE DRIVEN RANGE (rows R6 / R7 / R9).
//
// INVARIANT: at declare time the firewall must validate the image of the selection function
// over the range the ACCEPTED COUNT will actually drive (`0..n`), against the offer's
// PUBLISHED `legal_targets`.
//
// Every arm drives the PRODUCTION entry `apply_action(GameAction::DeclareShortcut { .. })`
// and asserts on the published `waiting_for`: `RespondToShortcut` = ingested (CR 732.2b's
// response window opened), `Priority` = refused into the manual-play handback.
// ---------------------------------------------------------------------------

/// Two objects on a 3p board, and the declare-time verdict for one (published set, count,
/// schedule) triple. The board is real and the offer is planted, exactly as
/// `declare_illegal_pin_falls_back_legal_ingests` plants it — what is under test is the
/// declare firewall, not the detector that would otherwise mint the offer.
///
/// the published capacity is 1_000 (the un-narrowed global cap), so no arm below is refused by
/// the count cap instead of by the range: the cap and the ceiling are upstream conjuncts that would
/// otherwise dominate every verdict in the table.
fn g1_declare_verdict(
    publish_b: bool,
    count: IterationCount,
    schedule_of: &dyn Fn(&YieldTarget, &YieldTarget) -> TargetSchedule,
    pin_twice: bool,
) -> WaitingFor {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.with_life(P2, 20);
    let obj_a = scenario.add_creature(P0, "Schedule Target A", 1, 1).id();
    let obj_b = scenario.add_creature(P0, "Schedule Target B", 1, 1).id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;

    let source_of = |id: ObjectId| YieldTarget::ThisObject {
        source_id: id,
        incarnation: None,
        trigger_description: None,
    };
    let (a, b) = (source_of(obj_a), source_of(obj_b));
    let slot = DecisionSlot {
        source: a.clone(),
        point: ChoicePoint::AnnouncedTarget,
        index: 0,
    };
    let mut legal_targets = vec![TargetRef::Object(obj_a)];
    if publish_b {
        legal_targets.push(TargetRef::Object(obj_b));
    }
    let schema = ShortcutDecisionSchema {
        suggested_declaration: None,
        leading_aims: Vec::new(),
        iteration_count: count.clone(),
        // This producer measured no CR 704 threshold — `Default` carries the absence and a
        // capacity at the global cap.
        measured_repetition_bound: None,
        deliverable_capacity: ShortcutDecisionSchema::default().deliverable_capacity,
        points: vec![DecisionPoint {
            slot: slot.clone(),
            kind: DecisionPointKind::Targets {
                legal_targets,
                min_targets: 1,
                max_targets: 1,
                ordered: true,
            },
        }],
        convoke_tappable_count: 0,
    };
    let mut targets = vec![TargetPin::Scheduled(schedule_of(&a, &b))];
    if pin_twice {
        // E-neg: two pins against a `min_targets == max_targets == 1` point. The cardinality
        // check sits OUTSIDE the per-index loop, so it must still refuse at count 0.
        targets.push(TargetPin::Scheduled(TargetSchedule::Constant(obj_rank(
            a.clone(),
        ))));
    }
    let template = DecisionTemplate {
        owner: P0,
        decisions: vec![PinnedDecision::Targets { slot, targets }],
        replay: ReplayMode::Scheduled {
            count: count.clone(),
        },
        key: DecisionGroupKey::from_sources(std::slice::from_ref(&a), DecisionKind::LoopChoice),
    };

    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: Some(P0),
        certificate: synthetic_lethal_cert(),
        schema,
        declaration: None,
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };
    runner
        .act(GameAction::DeclareShortcut {
            count,
            template: Some(template),
        })
        .expect("declare dispatch succeeds (a refusal is a manual handback, not an error)");
    runner.state().waiting_for.clone()
}

fn piecewise_a_then_b(a: &YieldTarget, b: &YieldTarget) -> TargetSchedule {
    TargetSchedule::Piecewise(vec![(0, obj_rank(a.clone())), (5, obj_rank(b.clone()))])
}

fn piecewise_b_then_a(a: &YieldTarget, b: &YieldTarget) -> TargetSchedule {
    TargetSchedule::Piecewise(vec![(0, obj_rank(b.clone())), (5, obj_rank(a.clone()))])
}

fn round_robin_a_b(a: &YieldTarget, b: &YieldTarget) -> TargetSchedule {
    TargetSchedule::RoundRobin(vec![obj_rank(a.clone()), obj_rank(b.clone())])
}

/// R6 arms A / B / C — the validated range must COVER the driven range.
///
/// * **A (the fix's positive, ⚠ behaviour change).** Publishes only A; the schedule switches
///   to the UNPUBLISHED B at index 5; the declared count is 8, so the drive reaches index 5.
///   Post-fix this is REFUSED. Pre-fix the validated range was the schedule length (2), so
///   indices 5..8 were never checked and the declaration was INGESTED — the soundness hole.
/// * **B (reach-guard).** The identical schedule at count 5 never reaches the switch, so it
///   is ingested. Without B, arm A would also pass under a firewall that rejected everything.
/// * **C (attribution control).** The identical count-8 declaration with B ALSO published is
///   ingested — so A's refusal is attributable to the PUBLISHED SET and not to the count, the
///   schedule, or the harness.
#[test]
fn declared_count_beyond_the_published_schedule_window_is_refused() {
    // A — the driven range reaches the unpublished arm.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(8), &piecewise_a_then_b, false),
            WaitingFor::Priority { .. }
        ),
        "CR 732.2a: a count that drives into an UNPUBLISHED schedule arm is not a sequence \
         that may be legally taken — refuse to manual play"
    );
    // B — reach-guard: the same schedule inside the published window is ingested.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(5), &piecewise_a_then_b, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "reach-guard: a count that never reaches the switch is a conforming declaration"
    );
    // C — attribution: publish the second arm and the count-8 declaration is fine.
    assert!(
        matches!(
            g1_declare_verdict(true, IterationCount::Fixed(8), &piecewise_a_then_b, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "attribution control: with BOTH arms published, count 8 is conforming — so A's \
         refusal is the published set and not the count"
    );
}

/// CR 732.2a + CR 732.2b: an unbounded declaration names no count, and a shorten may name any
/// place, so its pins are validated over the schedule's extent: a schedule switching to an
/// unpublished target at index 5 is refused into the manual-play handback without end, as at
/// count 7, while a count short of the switch, or both targets published, is
/// ingested.
#[test]
fn an_unbounded_declaration_is_validated_over_its_schedules_extent() {
    {
        let count = IterationCount::WithoutEnd;
        assert!(
            matches!(
                g1_declare_verdict(false, count.clone(), &piecewise_a_then_b, false),
                WaitingFor::Priority { player: P0 }
            ),
            "{count:?}: the schedule reaches the unpublished target"
        );
        assert!(
            matches!(
                g1_declare_verdict(true, count.clone(), &piecewise_a_then_b, false),
                WaitingFor::RespondToShortcut { .. }
            ),
            "{count:?}: attribution: with both targets published the declaration is ingested"
        );
    }
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(5), &piecewise_a_then_b, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "reach-guard: a count short of the switch is ingested"
    );
}

/// CR 732.2b: a shortened proposal without end drives the template, so the declare firewall
/// validates it over the drive period.
#[test]
fn a_without_end_declaration_is_validated_over_the_drive_period() {
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::WithoutEnd, &round_robin_a_b, false),
            WaitingFor::Priority { .. }
        ),
        "a rotation into an unpublished arm is refused without end as it is at count 2"
    );
    assert!(
        matches!(
            g1_declare_verdict(true, IterationCount::WithoutEnd, &round_robin_a_b, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "with both arms published the same declaration is ingested"
    );
}

/// R7 arms D1 / D2 / D3 — the range is EXACTLY the driven range, not a padded one.
///
/// * **D1 (over-refusal fix, ⚠ behaviour change).** A `RoundRobin[A,B]` rotation with only A
///   published, declared at count 1: the drive touches index 0 only, which selects A. Post-fix
///   INGESTED. Pre-fix the schedule-derived period (2) forced index 1 — an index nothing
///   drives — to be validated, and the declaration was refused. This is the over-veto class.
/// * **D2 (must-not-flip).** The same rotation with BOTH arms published is ingested at count 1
///   AND at count 8 — the protected arm, pinned so the fix cannot be mistaken for "accept
///   more".
/// * **D3 (mandatory discriminating negative).** The same rotation with only A published at
///   count 2 DOES reach index 1 ⇒ refused. D3 is what separates D1 from "the validation was
///   deleted": under a deleted firewall D3 would be ingested.
#[test]
fn declared_count_shorter_than_the_rotation_is_not_over_refused() {
    // D1 — the fix's over-refusal half.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(1), &round_robin_a_b, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "CR 732.2a: a count of 1 drives index 0 only, which selects the PUBLISHED arm — \
         refusing it is the over-veto this fix removes"
    );
    // D2 — must-not-flip, both counts.
    for count in [IterationCount::Fixed(1), IterationCount::Fixed(8)] {
        assert!(
            matches!(
                g1_declare_verdict(true, count.clone(), &round_robin_a_b, false),
                WaitingFor::RespondToShortcut { .. }
            ),
            "a fully-published rotation is conforming at {count:?} — this arm must not move"
        );
    }
    // D3 — the discriminating negative at the first index the count DOES reach.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(2), &round_robin_a_b, false),
            WaitingFor::Priority { .. }
        ),
        "count 2 reaches index 1, which selects the UNPUBLISHED arm ⇒ refused. If this is \
         ingested, the firewall is gone rather than correctly ranged"
    );
}

/// R9 arms E / E-neg — `Fixed(0)` validates over an EMPTY range, and the firewall is STILL
/// live there.
///
/// CR 732.2b: a shortened proposal's new ending point is the first deviating choice, and
/// CR 732.2c makes taking the shortcut mandatory once accepted — so a zero-repetition
/// proposal must be representable AND validatable. The `.max(1)` floor validated index 0 of a
/// range nothing drives.
///
/// * **E (⚠ behaviour change).** `Piecewise[(0,B),(5,A)]` with only A published, at count 0.
///   Index 0 selects the UNPUBLISHED B — but nothing drives index 0, so post-fix this is
///   INGESTED. The template shape is load-bearing: under `RoundRobin[A,B]` index 0 selects the
///   PUBLISHED A, so the arm would pass before and after and its revert-probe could not fail.
/// * **E-neg (anti-"validation deleted" control at the SAME count).** Two pins against a
///   one-target point at the same count 0: the cardinality check sits outside the index loop,
///   so it must still refuse. Without E-neg, E would also pass under a firewall that had been
///   deleted outright.
///
/// REVERT-PROBE: restore the `.max(1)` floor ⇒ E FAILS and no other arm moves (every other
/// arm's range is already ≥ 1).
#[test]
fn a_zero_count_declaration_validates_over_an_empty_range_but_still_checks_cardinality() {
    // E — nothing is driven, so nothing is out of the published set.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(0), &piecewise_b_then_a, false),
            WaitingFor::RespondToShortcut { .. }
        ),
        "CR 732.2b/c: a zero-repetition proposal drives no index, so no index can leave the \
         published set — the floor was refusing a conforming declaration"
    );
    // E-neg — the firewall is still live at the same count.
    assert!(
        matches!(
            g1_declare_verdict(false, IterationCount::Fixed(0), &piecewise_b_then_a, true),
            WaitingFor::Priority { .. }
        ),
        "the cardinality check is OUTSIDE the index loop: two pins against a one-target \
         point are refused even at count 0. If this is ingested, E passed because validation \
         was deleted rather than because the range is empty"
    );
}

// ═════════════ PR-7 Phase 5c, ITEM 2 — the kill-declared-target stop-short row ═════════════

/// Player-scope hexproof (CR 702.11c). The refuser is RULED to be
/// hexproof rather than phasing: hexproof makes the pinned seat illegal at the DRIVE's
/// spec-aware CR 608.2b re-validation (the `GameAction::SelectTargets` the injector submits),
/// which is the backstop layer no other 5c row exercises. Phasing would instead fail at
/// `resolve_target`'s EXISTENCE half and double-cover R1's seam.
/// A SYNTHETIC harness prop, deliberately NOT named after any printing: it exists only to be
/// the thing that makes the pinned seat illegal mid-window, and inventing a real card name for
/// non-verbatim text is the fabrication hazard CLAUDE.md's "verify the card, not just the rule"
/// principle warns about. The verbatim-Oracle rule is scoped to the card under test; the
/// card under test here is the SANGUINE_BOND drain, whose text IS verbatim.
const HEXPROOF_GRANT: &str = "You have hexproof.";

const P3: PlayerId = PlayerId(3);

/// The R5 board — 4 seats, P0 running the escalating TARGETED drain
/// (`SANGUINE_BOND` × `BLOODTHIRSTY_CONQUEROR`), P1/P2/P3 at 1000 life so the drive never
/// crosses lethal inside the declared window.
///
/// FOUR SEATS, and that is a reach-guard rather than padding: killing the pinned seat
/// must leave **at least two** other legal seats standing. A one-element surviving set cannot
/// witness "did not re-choose" — a retargeting engine would have exactly one place to go and
/// a stopped engine and a retargeting engine would be indistinguishable at the seat level.
///
/// Returns `(runner, sanguine_bond, hexproof_source, kickoff)`. The hexproof source starts in
/// P1's HAND, where its static does not function, so both arms share a byte-identical board
/// up to the moment the kill arm puts it onto the battlefield.
fn r5_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    for seat in 1..4u8 {
        scenario.with_life(PlayerId(seat), 1000);
    }
    let bond = scenario
        .add_creature_from_oracle(P0, "Sanguine Bond", 2, 2, SANGUINE_BOND)
        .id();
    scenario.add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 3, 4, BLOODTHIRSTY_CONQUEROR);
    let hexproof_src = scenario
        .add_creature_to_hand_from_oracle(P1, "Test Hexproof Source", 0, 4, HEXPROOF_GRANT)
        .id();
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    (runner, bond, hexproof_src, kickoff)
}

/// A `Fixed(count)` template pinning the Sanguine Bond trigger's `target opponent` to one
/// seat for every iteration. The slot's source is the Bond itself, so `slot_source_prompted`
/// matches the mid-drive `TriggerTargetSelection` the injector must answer.
fn r5_pin_template(slot: DecisionSlot, seat: PlayerId, count: u32) -> DecisionTemplate {
    let source = slot.source.clone();
    DecisionTemplate {
        owner: P0,
        decisions: vec![PinnedDecision::Targets {
            slot,
            targets: vec![TargetPin::Player(seat)],
        }],
        replay: ReplayMode::Scheduled {
            count: IterationCount::Fixed(count),
        },
        key: DecisionGroupKey::from_sources(&[source], DecisionKind::LoopChoice),
    }
}

/// [`r5_pin_template`] with its pins removed and `owner` supplied — the declaration a
/// POINTS-EMPTY offer admits on the PIN axis. CR 732.2a: a declaration may pin only choices the
/// offer published, so a SLOT-ADDRESSING pin naming an unexposed slot is refused.
fn r28_pinless_template(slot: DecisionSlot, owner: PlayerId) -> DecisionTemplate {
    let mut template = r5_pin_template(slot, P1, 1);
    template.decisions.clear();
    template.owner = owner;
    template
}

/// Reach the R5 board's own bounded `LoopShortcut` offer and return the runner parked on it
/// plus every seat's life at that instant.
///
/// The offer is the ENGINE's, read off `state.waiting_for` — never an out-of-band call to the
/// offer predicate, which would only prove the predicate agrees with itself.
fn r5_reach_offer() -> (GameRunner, DecisionSlot, ObjectId, ObjectId, Vec<i32>) {
    let (mut runner, bond, hexproof_src, kickoff) = r5_board();
    let _ = runner.cast(kickoff).target_player(P1).resolve();
    let WaitingFor::LoopShortcut {
        proposer, schema, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the 4p targeted drain must OFFER a LoopShortcut, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(proposer, P0, "P0 has priority and proposes the shortcut");
    // SHAPE PIN, re-derived: the Bond's `target opponent` trigger resolves across a
    // `TriggerTargetSelection` window, so the answer-beat sampling site announces its entry
    // and the offer publishes exactly ONE CR 608.2b `Targets` point for the Bond's own slot.
    // A drift here means the announced set changed shape and the callers must be re-derived,
    // not relaxed.
    //
    // LAYER ATTRIBUTION NO LONGER RESTS ON EMPTINESS, and no shared helper replaces it — each
    // caller measures the declare-time outcome ITSELF, on its own board. The kill arm of
    // `a_declared_target_made_illegal_mid_drive_stops_short_and_never_retargets` asserts
    // `WaitingFor::RespondToShortcut` immediately after its `DeclareShortcut` ("LAYER
    // ATTRIBUTION, half two"), which proves the declare firewall INGESTED that declaration and
    // therefore that the refusal it measures later is the DRIVE's; the r28 rows assert the
    // complementary declare-time refusal on their own staged schemas.
    assert_eq!(
        schema.points.len(),
        1,
        "the R5 offer publishes the Bond's re-aimable `Targets` slot and nothing else; got \
         {:?}",
        schema.points
    );
    assert!(
        matches!(
            schema.points[0].kind,
            DecisionPointKind::Targets {
                min_targets: 1,
                max_targets: 1,
                ..
            }
        ),
        "the published point is the Bond trigger's single-player target slot; got {:?}",
        schema.points[0].kind
    );
    // CR 732.2a: the ENGINE-issued slot is the pin authority. Hand-assembling one here
    // silently drifted from it (`incarnation: None` vs the published `Some(0)`), which
    // `validate_pins` then rejected — a test artefact, not an engine defect.
    let pinned_slot = schema.points[0].slot.clone();
    let lives = vec![
        life(&runner, P0),
        life(&runner, P1),
        life(&runner, P2),
        life(&runner, P3),
    ];
    (runner, pinned_slot, bond, hexproof_src, lives)
}

/// The per-cycle life the pinned seat loses, probed by an independent `Fixed(1)`
/// materialization of this same board (one recurrence = one full cycle). Mirrors
/// [`probe_drain_delta`]; nothing below is bound to a literal drain rate.
fn r5_probe_delta() -> i32 {
    let (mut runner, slot, _bond, _hexproof_src, l0) = r5_reach_offer();
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: Some(r5_pin_template(slot.clone(), P1, 1)),
        })
        .expect("declare Fixed(1) with a Player pin");
    accept_all_opponents(&mut runner);
    let delta = l0[1] - life(&runner, P1);
    assert!(
        delta > 0,
        "Fixed(1) must materialize a nonzero drain cycle on the PINNED seat, got {delta}"
    );
    delta
}

/// ITEM 2 / R5 ⭐ — **a declared target made illegal mid-drive stops the drive short and is
/// NEVER re-chosen.** Governing ruling, ledgered verbatim: *stop-short/abort, never silently
/// re-choose or skip.*
///
/// # The seam, and why hexproof is the ruled refuser
///
/// A `Fixed(n)` drive re-resolves its template per cycle and answers each mid-cycle
/// `TriggerTargetSelection`, submitting the pinned value as a real `GameAction::SelectTargets`.
/// That submission is the **drive-time CR 608.2b re-validation** — the backstop layer, one below
/// the declare-time firewall. Hexproof (CR 702.11c, "can't be the target of spells or abilities
/// **your opponents control**": the source is P0's permanent and the pinned seat is P0's opponent)
/// makes the pinned seat illegal exactly there and nowhere earlier. Phasing was rejected as the
/// refuser precisely because it fails one layer up, at `resolve_target`'s existence half,
/// double-covering R1.
///
/// # Constructed-board deviation, DISCLOSED (constructibility-first)
///
/// This row is built on a constructed 4-seat board rather than on a dump fixture. Two
/// measurements forced it, and both are INLINED here rather than
/// cited, because the probe archive that holds them is untracked and never ships: (i) the
/// real `dellian_emblem_conqueror_4p` dump — the only tracked fixture whose loop targets a
/// player — runs **309 beats to `GameOver` and raises no `LoopShortcut` at all** under the
/// generic dump driver, so it cannot host a declared drive; (ii) the refuser has to be
/// *introduced* on a named seat's battlefield mid-window, which is a board construction
/// whichever fixture carries it. The construction itself is the tracked one —
/// `declare_illegal_pin_falls_back_legal_ingests` builds its declare-seam board the same way.
///
/// # The four-assertion anti-retarget set, each named at its assertion below
///
/// 1. the drive **stops short** — zero of `N` cycles commit, and `N * delta` is what the same
///    board commits with the refuser absent;
/// 2. **no retarget** — neither surviving legal seat is drained;
/// 3. **no silent skip** — the aborting cycle is not skipped-and-continued: no seat moves at
///    all, and the drain's mirror gain on P0 does not move either;
/// 4. **state coherent post-abort** — ring cleared, priority handed to a living seat, nobody
///    eliminated, the board still carries the refuser.
///
/// # Reach-guards (without these every assertion above is vacuous)
///
/// * the CLEAN arm is the positive control: the identical board with the hexproof source left in
///   hand drives all `N` cycles onto the pinned seat, so a `0` in the kill arm is the refuser
///   firing and not a dead harness;
/// * `player_has_hexproof(P1)` flips `false → true` across the move, so the setup cannot
///   silently no-op;
/// * **the surviving legal set has `len() >= 2`** — this row's own reach-guard. A one-element set
///   cannot witness "did not re-choose";
/// * the declare firewall **passed** (`RespondToShortcut` opened) and the offer publishes
///   **no points**, so the refusal is attributable to the drive and not to `validate_pins`.
#[test]
fn a_declared_target_made_illegal_mid_drive_stops_short_and_never_retargets() {
    use engine::types::zones::Zone;

    const N: u32 = 3;
    let delta = r5_probe_delta();

    // ───────────────────────── CLEAN arm — the positive control ─────────────────────────
    // Identical board, hexproof source left in hand (its static does not function there), so the
    // ONLY difference from the kill arm is whether the refuser is on the battlefield.
    let (mut clean, clean_slot, _clean_bond, _clean_hexproof_src, clean_l0) = r5_reach_offer();
    clean
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(N),
            template: Some(r5_pin_template(clean_slot.clone(), P1, N)),
        })
        .expect("declare Fixed(N) with a Player pin");
    accept_all_opponents(&mut clean);
    assert_eq!(
        life(&clean, P1),
        clean_l0[1] - (N as i32) * delta,
        "control: with no refuser the drive commits EXACTLY N cycles onto the PINNED seat"
    );
    assert_eq!(
        (life(&clean, P2), life(&clean, P3)),
        (clean_l0[2], clean_l0[3]),
        "control: the pin, not the seat order, is what selects the drained seat"
    );

    // ───────────────────────────────── KILL arm ─────────────────────────────────────────
    let (mut runner, slot, bond, hexproof_src, l0) = r5_reach_offer();
    assert!(
        !engine::game::static_abilities::player_has_hexproof(runner.state(), P1),
        "setup anti-vacuity: the pinned seat must START without hexproof, or the kill below \
         changes nothing"
    );

    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(N),
            template: Some(r5_pin_template(slot.clone(), P1, N)),
        })
        .expect("declare Fixed(N) with a Player pin");
    // LAYER ATTRIBUTION, half two: the declare-time firewall INGESTED this declaration. The
    // refusal measured below therefore happened at the drive, which is the whole point of
    // choosing hexproof over phasing as the refuser.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "the declare firewall must PASS — otherwise this row measures `validate_pins`, not \
         the drive's CR 608.2b backstop; got {:?}",
        runner.state().waiting_for
    );

    // THE KILL: the refuser arrives on the pinned seat's battlefield through the production
    // zone pipeline, after the declaration has been ingested and before the table's Accept.
    {
        let mut events = Vec::new();
        engine::game::zones::move_to_zone(
            runner.state_mut(),
            hexproof_src,
            Zone::Battlefield,
            &mut events,
        );
        // CR 613.1: the grant is a continuous effect — re-derive the board so the legality
        // reads below are taken against the post-kill layers rather than a stale cache.
        engine::game::layers::mark_layers_full(runner.state_mut());
        engine::game::layers::evaluate_layers(runner.state_mut());
    }
    assert!(
        engine::game::static_abilities::player_has_hexproof(runner.state(), P1),
        "setup anti-vacuity: the kill must actually land — a silently inert hexproof source would \
         make every assertion below pass for the wrong reason"
    );
    assert!(
        !engine::game::targeting::player_is_legal_target(runner.state(), P1, bond, P0),
        "CR 702.11c: the pinned seat must now be an ILLEGAL target of the Bond's ability"
    );
    // REACH-GUARD, asserted as a count so a shrinking board fails loudly: after the kill
    // at least TWO other seats are still legal, so a retargeting engine has somewhere to go.
    let surviving_legal: Vec<PlayerId> = [P2, P3]
        .into_iter()
        .filter(|&seat| {
            engine::game::targeting::player_is_legal_target(runner.state(), seat, bond, P0)
        })
        .collect();
    assert!(
        surviving_legal.len() >= 2,
        "a 1-element surviving legal set cannot witness `did not re-choose`; got \
         {surviving_legal:?}"
    );

    accept_all_opponents(&mut runner);

    // (1) STOPS SHORT — zero of N cycles committed. Bound to the measured `delta` and to the
    //     control arm above, never to a literal: `N * delta > 0` is what a completing drive
    //     would have taken off the pinned seat.
    assert!(
        N >= 2 && delta > 0,
        "the stop-short claim needs a window longer than one cycle and a nonzero drain rate"
    );
    assert_eq!(
        life(&runner, P1),
        l0[1],
        "the pinned seat must lose NOTHING: the drive stopped at the first cycle whose \
         re-validation refused, and that cycle rolled back whole"
    );
    // (2) NO RETARGET — both seats that were measured LEGAL above are untouched.
    assert_eq!(
        (life(&runner, P2), life(&runner, P3)),
        (l0[2], l0[3]),
        "stop-short, never silently re-choose: neither surviving legal seat may be drained. \
         A `first legal target` fallback in the injector drains P2 here"
    );
    // (3) NO SILENT SKIP — the drain's mirror gain on the controller did not move either, so
    //     the drive did not skip the refused cycle and press on with the remaining ones.
    assert_eq!(
        life(&runner, P0),
        l0[0],
        "no silent skip: a drive that skipped the refused cycle and continued would still \
         have run the remaining cycles and moved the controller's mirror gain"
    );
    // (4) STATE COHERENT POST-ABORT.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the abort hands priority back to a living seat (manual fallback), not a wrong-crown \
         and not a stuck response window"
    );
    assert!(
        runner.state().loop_detect_ring.is_empty(),
        "the ring is cleared on handback so the same apply() does not instantly re-offer"
    );
    assert!(
        [P0, P1, P2, P3]
            .into_iter()
            .all(|seat| !is_eliminated(&runner, seat)),
        "the table stays live — this row is about a refused target, not about anyone dying"
    );
    assert!(
        runner.state().battlefield.contains(&hexproof_src),
        "the roll-back is scoped to the DRIVE: the board change that made the pin illegal is \
         not undone by the abort"
    );
}

/// **Row R27 conjunct (a1) — THE SPLIT IS REAL AND THE TWO HALVES DIFFER.**
///
/// CR 104.4b + CR 732.2a: `LoopDetectSample` separates the equality *comparand* from the
/// shortcut *evaluable*. Every later conjunct of R27 (a2/a3/b/c, U3) asserts that a
/// period-touch consumer reads the un-normalized half; **all of them are false PASSes if
/// the two halves happen to hold the same thing.** This row is the BASE/POST discipline
/// applied to the split itself: it pins that a real production sample's halves DIFFER on
/// the axis that made the split necessary.
///
/// The axis is the object allocator. `normalize_for_loop` zeroes `next_object_id`
/// (`types/game_state.rs`, `clone.next_object_id = 0;`) while the live sample keeps it,
/// and `zones::create_object` allocates `ObjectId(state.next_object_id)` then
/// `state.objects.insert(id, obj)` — so evaluating a token creation against a normalized
/// frame allocates `ObjectId(0)` and REPLACES whatever object id 0 is, corrupting the map
/// the resolution runs on. That is the concrete defect the split exists to prevent.
///
/// **Revert-probe:** make `GameState::loop_detect_live_sample` return
/// `self.normalize_for_loop()` (i.e. collapse the split) ⇒ `live.next_object_id` becomes
/// `0` ⇒ the `assert_ne!` and the `live == pre_sample` assertion both FAIL. The row is
/// deliberately NOT sensitive to which half any consumer reads — that is (a2)/(a3)'s job —
/// so it stays the honest positive control while those arms are the subject.
///
/// Fixture: the tracked `dellian_emblem_conqueror_4p` dump, driven through the production
/// `apply()` path so the ring is populated by `record_loop_detect_sample` itself and not by
/// a hand-built fixture.
#[test]
fn a_recorded_loop_detect_sample_keeps_a_live_half_normalization_would_have_erased() {
    let json = gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    ));
    let mut state = restore_dump(&json);

    // ── REACH-GUARDS. Without these the assertions below are vacuous.
    assert!(
        state.loop_detection.samples(),
        "reach-guard: the dump must load with a SAMPLING loop-detection mode, else no sample \
         is ever recorded; got {:?}",
        state.loop_detection
    );
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring — every frame below was accumulated by \
         THIS drive through the production producer, not restored from the dump"
    );

    let pin = engine_live_opponents(&state, P0).first().copied();

    // Drive until the production sampler has recorded at least one sample, capturing the
    // live `next_object_id` observed at the beat immediately BEFORE the ring grew. That
    // pre-sample value is what the `live` half must have preserved.
    let mut witness: Option<(u64, u64, u64, u64)> = None;
    for _ in 0..400 {
        if matches!(state.waiting_for, WaitingFor::LoopShortcut { .. }) {
            break;
        }
        let before = state.loop_detect_ring.len();
        let next_object_id_before = state.next_object_id;
        if dump_drive_one_beat(&mut state, pin).is_err() {
            break;
        }
        if state.loop_detect_ring.len() > before {
            let sample = state
                .loop_detect_ring
                .back()
                .expect("the ring just grew, so it has a back element");
            witness = Some((
                next_object_id_before,
                sample.live.next_object_id,
                sample.normalized.next_object_id,
                state.next_object_id,
            ));
            break;
        }
    }

    let (before_beat, live, normalized, after_beat) = witness.expect(
        "reach-guard: the drive must record at least one loop-detect sample, else this row \
         asserts about a ring that was never populated and passes vacuously",
    );

    // The whole point of the axis: it must be non-degenerate on this board, so that
    // `live != normalized` is a real inequality and not `0 != 0` dressed up, and so that
    // the lower bound below actually bites.
    assert!(
        before_beat > 0,
        "reach-guard: the allocator axis must be non-degenerate — a board that had allocated \
         ZERO objects makes the split unobservable on this axis and every assertion below \
         trivially true; got next_object_id = {before_beat}"
    );

    // ── THE CLAIM, both directions.
    assert_eq!(
        normalized, 0,
        "CR 104.4b: the comparand half is `normalize_for_loop()`d, which zeroes the volatile \
         monotonic allocator so two positions reached at different times can compare equal"
    );
    // The sampler runs at the POST-pipeline frame, so objects allocated earlier in the same
    // beat are already counted: the live half is bracketed by the beat's own endpoints
    // rather than equal to either. Collapsing the split drives it to 0, which is below
    // `before_beat` (> 0 by the reach-guard above) and so fails this bound.
    assert!(
        (before_beat..=after_beat).contains(&live),
        "CR 732.2a: the evaluable half is the beat un-normalized — it must carry the live \
         allocator cursor as of the post-pipeline frame the sampler runs at, i.e. inside \
         [{before_beat}, {after_beat}], because a shortcut's 'predictable results' are \
         evaluated by really resolving against it and `zones::create_object` allocates \
         `ObjectId(state.next_object_id)`; got {live}"
    );
    assert_ne!(
        live, normalized,
        "THE SPLIT IS REAL: if the two halves agreed on this axis, R27's later conjuncts \
         (a2)/(a3)/(b)/(c) — every one of which asserts that a period-touch consumer reads the \
         un-normalized half — would be satisfiable by a build in which the split does not exist"
    );
}

// ─────────── 5d U2 / R28 — the declared template's `owner` is ENGINE-BOUND ───────────

/// CR 732.2a: stage the live offer with an EMPTY point set, so arm (a″) runs the declare gates
/// on an offer that exposed no slot for them to decide. Counterpart to
/// [`r28_nonempty_schema_offer`]; `offer.proposer` still comes from the live
/// `WaitingFor::LoopShortcut`, which is the firewall's engine-issued comparand.
fn r28_empty_schema_offer(runner: &mut GameRunner) {
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema,
        declaration: _,
        road,
        counted_take: _,
        period: _,
    } = runner.state().waiting_for.clone()
    else {
        panic!("staged from the live offer, never from thin air");
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema: ShortcutDecisionSchema {
            points: vec![],
            suggested_declaration: None,
            leading_aims: Vec::new(),
            ..schema
        },
        // `None` is a RULING, not a default. Passing the live field through would stage the R5
        // board's measured `Some` declaration into a control whose whole purpose is to be
        // declaration-free — and it would contradict the invariant that an empty schema
        // publishes no declaration, at the very fixture that stages an empty schema.
        declaration: None,
        road,
        counted_take: Default::default(),
        period: Default::default(),
    };
}

/// The engine-issued offer's own point set, hand-assembled to match `r5_pin_template`'s slot.
///
/// The R5 board's live offer publishes ONE `Targets` point whose `legal_targets` are minted
/// from the live board; this stages the same shape with a FIXED target list so the row is
/// insensitive to seat-population drift. `offer.proposer` — the firewall's engine-issued
/// comparand — still comes from `WaitingFor::LoopShortcut`, which is what the row is about.
fn r28_nonempty_schema_offer(runner: &mut GameRunner, slot: DecisionSlot) {
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema,
        declaration: _,
        road,
        counted_take: _,
        period: _,
    } = runner.state().waiting_for.clone()
    else {
        panic!("staged from the live offer, never from thin air");
    };
    runner.state_mut().waiting_for = WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        certificate,
        schema: ShortcutDecisionSchema {
            points: vec![DecisionPoint {
                slot,
                kind: DecisionPointKind::Targets {
                    legal_targets: vec![
                        TargetRef::Player(P1),
                        TargetRef::Player(P2),
                        TargetRef::Player(P3),
                    ],
                    min_targets: 1,
                    max_targets: 1,
                    ordered: false,
                },
            }],
            ..schema
        },
        // Same ruling as [`r28_empty_schema_offer`]: both helpers exist to stage `schema.points`
        // and NOTHING else, so the two must keep differing in exactly one field. Staging a live
        // `Some` here would add a second axis to a pair whose whole value is being one apart.
        declaration: None,
        road,
        counted_take: Default::default(),
        period: Default::default(),
    };
}

/// R28 arms (a)/(a′) — **CR 732.2a + CR 603.5: a declaration whose `template.owner` names
/// another seat is refused AT DECLARE.**
///
/// `template.owner` arrives VERBATIM from the client (`GameAction::DeclareShortcut { template }`
/// is forwarded whole); the declare-time binding to the engine-issued
/// `LoopShortcutOffer.proposer` refuses a foreign owner before any proposal is built.
///
/// **(a′) is the reach-guard**: the byte-identical declaration with `owner = P0` builds the
/// proposal and opens APNAP, proving the fixture reaches the firewall and that (a) is keyed to
/// the `owner` axis rather than to `predictability_gate` / `validate_pins` / the count cap.
///
/// REVERT-PROBE: delete `if template.as_ref().is_some_and(|t| t.owner != offer.proposer) { .. }`
/// from `handle_declare_shortcut` ⇒ the wrong-owner declaration is accepted, a proposal is
/// built, APNAP opens ⇒ **(a) FLIPS TO FAIL** while (a′) stays green.
#[test]
fn r28_a_declared_template_owning_another_seat_is_refused_at_declare() {
    for hostile in [false, true] {
        let (mut runner, slot, _bond, _hexproof, _lives) = r5_reach_offer();
        r28_nonempty_schema_offer(&mut runner, slot.clone());
        let WaitingFor::LoopShortcut { schema, .. } = runner.state().waiting_for.clone() else {
            panic!("staged offer");
        };
        assert_eq!(
            schema.points.len(),
            1,
            "reach-guard: this arm runs on a NON-empty schema, so `predictability_gate` and \
             `validate_pins` really run and (a′) proves they PASS"
        );

        let mut template = r5_pin_template(slot.clone(), P1, 1);
        if hostile {
            template.owner = P1;
        }
        assert_eq!(
            template.owner,
            if hostile { P1 } else { P0 },
            "the two arms differ in exactly one field"
        );
        let before = runner.state().clone();
        let result = runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: Some(template),
            })
            .expect("the declaration is dispatched either way — refusal is a HANDBACK");

        if hostile {
            // (a) refused into the manual handback.
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 },
                "(a) a wrong-`owner` declaration hands priority back to P0 (CR 732.2a)"
            );
            assert!(
                !matches!(
                    runner.state().waiting_for,
                    WaitingFor::RespondToShortcut { .. }
                ),
                "(a) no `ShortcutProposal` may be built"
            );
            assert!(
                result.events.is_empty(),
                "(a) `handle_declare_shortcut` pushes NO events at all, so this is an exact \
                 assertion rather than a wildcard: {:?}",
                result.events
            );
            assert_eq!(
                before.players.iter().map(|p| p.life).collect::<Vec<_>>(),
                runner
                    .state()
                    .players
                    .iter()
                    .map(|p| p.life)
                    .collect::<Vec<_>>(),
                "(a) nothing was driven"
            );
        } else {
            // (a′) the matched positive: the proposal IS built and APNAP opens.
            let WaitingFor::RespondToShortcut { proposal, .. } = &runner.state().waiting_for else {
                panic!(
                    "(a′) the honest declaration must open APNAP, got {:?}",
                    runner.state().waiting_for
                );
            };
            assert_eq!(
                proposal.template.as_ref().map(|t| t.owner),
                Some(P0),
                "(a′) the proposal carries the engine-bound owner"
            );
        }
    }
}

/// R28 arm (a″) — **an empty-schema offer is NOT short-circuited past the declare gates.**
///
/// The gates run on every declaration the handler resolves, whatever the schema published, so
/// an offer exposing no decision point is still gated. Arms (a)/(a′) run on a non-empty schema
/// and so cannot see a short-circuit keyed to an empty one. The firewall's PLACEMENT is NOT
/// this row's subject and no arm here measures it: `declaration_conforms` reads no `owner`, so
/// the firewall refuses the same declarations anywhere between the `or_else` that resolves the
/// template and the end of the match.
///
/// Both declarations here are PIN-FREE. CR 732.2a lets a declaration pin only choices the offer
/// published, so a SLOT-ADDRESSING pin naming an unexposed slot is refused on the PIN axis and
/// the owner axis this row varies would not be the operative one.
///
/// ⚠ **DISCLOSED REACHABILITY DOWNGRADE.** This arm used to run on the R5 offer's OWN empty
/// schema — the empty-schema path was reached NATURALLY. It no longer is: the answer-beat
/// sampling site announces the Bond's trigger entry, so the LIVE schema now publishes one
/// `Targets` point (`r5_reach_offer` pins that shape). BOTH arms below therefore STAGE the
/// empty schema through `r28_empty_schema_offer`, the same idiom `r28_nonempty_schema_offer`
/// uses in the other direction. What survives the downgrade: `offer.proposer` — the firewall's
/// engine-issued comparand — is still the live one, and the matched positive accepts an honest
/// declaration on the SAME staged path, so the row still discriminates the firewall from "the
/// staged path refuses everything". What does NOT survive: the claim that a real board reaches
/// this path on its own. Treat that as unproven here until a fixture whose live offer publishes
/// nothing is added.
///
/// REVERT-PROBE: short-circuit the handler on an exposed-nothing offer — build the proposal
/// directly when `offer.schema.points.is_empty()`, skipping both the owner firewall and the
/// `match &template` ⇒ the wrong-owner declaration opens APNAP ⇒ **(a″) FLIPS TO FAIL** while
/// (a)/(a′), which run on a non-empty schema, stay green.
#[test]
fn r28_a_an_empty_schema_offer_is_not_short_circuited_past_the_declare_gates() {
    // matched positive first: the empty-schema path DOES accept an honest declaration.
    let (mut runner, slot, _bond, _h, _l) = r5_reach_offer();
    r28_empty_schema_offer(&mut runner);
    runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: Some(r28_pinless_template(slot, P0)),
        })
        .expect("declare");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "(a″) reach-guard: an EMPTY-schema offer accepts an owner-correct declaration, so the \
         refusal below is the firewall and not the empty-schema path refusing everything"
    );

    let (mut runner, slot, _bond, _h, _l) = r5_reach_offer();
    r28_empty_schema_offer(&mut runner);
    let result = runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(1),
            template: Some(r28_pinless_template(slot, P1)),
        })
        .expect("dispatched");
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "(a″) an offer publishing no point still runs the declare gates, so the foreign owner \
         is refused here too"
    );
    assert!(result.events.is_empty(), "(a″) no events on the handback");
}

/// R28 arms (c)/(c′)/(c″) — **the RESTORE ingress, the only one the declare firewall cannot
/// see, closed at the single consumption chokepoint.**
///
/// `ShortcutProposal` is plainly serialized inside `GameState.waiting_for`, and the
/// untrusted-restore scrubber rewrites only the two PRE-CAST waits — so a persisted
/// `WaitingFor::RespondToShortcut` decodes with its template intact, having never run
/// `handle_declare_shortcut`. `apply_confirmed_shortcut` is the sole route into both drives,
/// which is why the re-validation conjunct joins ITS existing fail-closed guard.
///
/// Driven over BOTH trust branches, because `Deserialize for PersistedGameState` dispatches on
/// the presence of a top-level `"state"` key and only the RAW arm runs the scrubber. Asserting
/// one branch would leave the other untested — and (c″) below shows they really are different
/// code paths rather than one path asserted twice.
///
/// * **(c)** `t.owner = P1` on a `proposal.proposer = P0` proposal ⇒ Accept is refused into the
///   manual handback: priority to a living seat, ZERO cycles committed, no `GameOver`.
/// * **(c′)** MATCHED POSITIVE, byte-identical except that one field ⇒ the drive runs and
///   commits. This is the reach-guard proving the fixture reaches `apply_confirmed_shortcut` at
///   all, and that (c) is keyed to the `owner` axis rather than to `is_alive`, the count cap or
///   the conformance check.
/// * **(c″-Raw)** the scrubber RUNS on this branch and leaves the wait untouched — which is the
///   mechanism that makes (c) necessary. **(c″-Trusted)** is a DIFFERENT claim: the scrubber is
///   not on that path at all, so the arm asserts survival across the trusted envelope and
///   claims nothing about the scrubber.
///
/// REVERT-PROBE: delete the `|| proposal.template.as_ref().is_some_and(|t| t.owner !=
/// proposal.proposer)` conjunct from `apply_confirmed_shortcut`'s guard ⇒ the round-tripped
/// wrong-owner proposal drives and commits ⇒ **(c) FLIPS TO FAIL** on both branches, while
/// (c′) stays green (it never depended on the conjunct).
#[test]
fn r28_c_a_restored_proposal_with_a_foreign_template_owner_is_refused_at_consumption() {
    for hostile in [false, true] {
        for trusted in [false, true] {
            let label = format!("hostile={hostile} trusted={trusted}");
            let (mut runner, slot, _bond, _h, lives) = r5_reach_offer();
            runner
                .act(GameAction::DeclareShortcut {
                    count: IterationCount::Fixed(1),
                    template: Some(r5_pin_template(slot.clone(), P1, 1)),
                })
                .expect("declare opens APNAP");

            // Tamper the persisted wait exactly as a hand-edited dump would, THEN round-trip.
            // The declare firewall has already run and passed on the honest value, so nothing
            // below can be attributed to it.
            let WaitingFor::RespondToShortcut { proposal, .. } =
                &mut runner.state_mut().waiting_for
            else {
                panic!("{label}: APNAP must be open");
            };
            let expected_owner = if hostile { P1 } else { P0 };
            proposal
                .template
                .as_mut()
                .expect("the declared template rode into the proposal")
                .owner = expected_owner;
            // MEASURED CONSTRAINT ON THIS INGRESS, applied to BOTH arms so they stay
            // byte-identical except `owner`: `ShortcutProposal.per_cycle` carries a
            // `PlayerId`-keyed resource map, and `PlayerId` cannot deserialize from a JSON
            // object KEY — so a persisted `RespondToShortcut` whose proposal carries a
            // per-cycle signature fails to decode with `invalid type: string "0", expected
            // u8`. That is a pre-existing serde asymmetry this change does not touch; its
            // consequence here is that ingress I3 is reachable only for `per_cycle: None`
            // proposals, which is exactly the shipped `Some(template)` population. The guard
            // under test does not read `per_cycle`, so nulling it costs the row nothing.

            // The TRUSTED arm must carry a real resolution-wire envelope, not a bare
            // `GameState` under a `"state"` key. Upstream #6933 made
            // `resolution_state_version` a required discriminator and gave only the
            // PersistedRaw ingress permission to stamp v1 onto a legacy payload; the
            // TrustedEnvelope ingress deliberately stamps nothing, because a trusted
            // snapshot is WRITTEN as a versioned envelope and must retain its declared
            // compatibility mode. `GameState`'s derived `Serialize` emits no such field,
            // so hand-wrapping it produced a payload the trusted path is right to refuse.
            // Building through `ResolutionStateWire` is what `TrustedGameStateEnvelope`'s
            // own `Serialize` does, so this arm now round-trips the shape production
            // writes instead of one only this test ever constructed.
            let payload = if trusted {
                let wire = engine::types::resolution::ResolutionStateWire::from_game_state(
                    runner.state().clone(),
                );
                serde_json::json!({ "state": serde_json::to_value(wire).expect("wire serializes") })
            } else {
                serde_json::to_value(runner.state()).expect("state serializes")
            };
            let restored: GameState =
                serde_json::from_value::<engine::types::game_state::PersistedGameState>(payload)
                    .unwrap_or_else(|error| {
                        panic!("{label}: decodes through the production boundary: {error}")
                    })
                    .into_game_state()
                    .expect("persisted test snapshot satisfies the checked restore contract");

            // (c″) — the wait and its tampered owner SURVIVE the decode. On the Raw branch the
            // scrubber ran and left it alone (its `semantic_owner` match names only the two
            // pre-cast waits); on the Trusted branch the scrubber is not on the path at all.
            let WaitingFor::RespondToShortcut { proposal, .. } = &restored.waiting_for else {
                panic!(
                    "{label}: (c\u{2033}) the restore must NOT drop the wait — otherwise (c) \
                     would pass by a different mechanism entirely; got {:?}",
                    restored.waiting_for
                );
            };
            assert_eq!(
                proposal.template.as_ref().map(|t| t.owner),
                Some(expected_owner),
                "{label}: (c\u{2033}) the tampered owner reaches `apply_confirmed_shortcut` \
                 unchanged"
            );
            assert_eq!(
                proposal.proposer, P0,
                "{label}: the proposer is engine state"
            );

            let mut restored_runner = GameRunner::from_state(restored);
            accept_all_opponents(&mut restored_runner);

            let after: Vec<i32> = restored_runner
                .state()
                .players
                .iter()
                .map(|p| p.life)
                .collect();
            if hostile {
                assert!(
                    matches!(
                        restored_runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ),
                    "{label}: (c) manual handback, got {:?}",
                    restored_runner.state().waiting_for
                );
                assert_eq!(
                    after, lives,
                    "{label}: (c) ZERO cycles committed — the board is byte-equal on life"
                );
            } else {
                assert_ne!(
                    after, lives,
                    "{label}: (c\u{2032}) the honest proposal DRIVES — without this the \
                     hostile arm's `no delta` assertion is vacuous"
                );
            }
        }
    }
}

/// **The GLOBAL count cap, refused at CONSUMPTION on a restored proposal nobody answered.**
///
/// `handle_declare_shortcut` refuses an over-cap `Fixed` before the proposal is built and its
/// own note records that the drive helpers do NOT re-check — so the cap was defended at declare
/// and only at declare. A RESTORED `WaitingFor::RespondToShortcut` never
/// passes the declare at all, for exactly the reason the sibling `owner` row above exists: the
/// untrusted-restore scrubber rewrites only the two pre-cast waits. A hand-edited count therefore
/// reached `materialize_fixed_shortcut` through one Accept — a `GameState` clone plus a drive per
/// cycle, which is the vector the declare site calls the catastrophic remote one.
///
/// What the guard costs is named rather than hidden: CR 732.2c says the shortcut IS taken, and a
/// handback does not take it. The cap is ours and not the game's, and failing closed onto manual
/// play runs in the more-responder-agency direction.
///
/// The cap is read as `ShortcutDecisionSchema::default().deliverable_capacity`, which IS
/// `MAX_SHORTCUT_CYCLES` (`default_deliverable_capacity`) — the const itself is `pub(crate)` and
/// invisible across this boundary. Reading it rather than hard-coding `u32::MAX` puts the
/// refused arm on the BOUNDARY member `cap + 1`, where an off-by-one in the comparison shows.
///
/// * **(a)** `Fixed(cap + 1)` ⇒ refused: manual handback, ZERO cycles committed.
/// * **(b)** `Fixed(1)`, the same construction differing only in the count ⇒ DRIVES. Without
///   (b), (a)'s no-delta observation is satisfied by a fixture that never reached the guard.
///
/// VACUITY TRAP, inherited from `over_cap_fixed_count_hands_back_with_no_drive`: a handback
/// lands on `WaitingFor::Priority` and so does a stop-short drive, so `waiting_for` is an
/// invariant here, not the discriminator. The DRIVE is — hence the life-delta assertions.
///
/// REVERT-PROBE: delete the count arm from `apply_confirmed_shortcut`'s guard — stop asking
/// `shortcut_count_is_drivable` there — ⇒ (a) drives and its no-delta assertion FAILS, while
/// (b) stays green.
#[test]
fn an_over_cap_count_is_refused_at_consumption() {
    let cap = ShortcutDecisionSchema::default().deliverable_capacity;
    for count in [cap + 1, 1] {
        let over_cap = count > cap;
        let (mut runner, slot, _bond, _h, lives) = r5_reach_offer();
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1),
                template: Some(r5_pin_template(slot.clone(), P1, 1)),
            })
            .expect("declare opens APNAP");

        // Tamper the persisted wait exactly as a hand-edited dump would, AFTER the declare
        // firewall has run and passed on the honest value — so nothing below is attributable
        // to it.
        let WaitingFor::RespondToShortcut { proposal, .. } = &mut runner.state_mut().waiting_for
        else {
            panic!("count={count}: APNAP must be open");
        };
        proposal.count = IterationCount::Fixed(count);

        // `PersistedGameState::Raw` is the untrusted branch — the one the scrubber runs on.
        let payload = serde_json::to_value(runner.state()).expect("state serializes");
        let restored: GameState =
            serde_json::from_value::<engine::types::game_state::PersistedGameState>(payload)
                .unwrap_or_else(|error| {
                    panic!("count={count}: decodes through the production boundary: {error}")
                })
                .into_game_state()
                .expect("persisted test snapshot satisfies the checked restore contract");

        // The tampered count SURVIVES the decode. Without this the scrubber could be doing the
        // refusing and (a) would measure something else entirely.
        let WaitingFor::RespondToShortcut { proposal, .. } = &restored.waiting_for else {
            panic!(
                "count={count}: the restore must NOT drop the wait; got {:?}",
                restored.waiting_for
            );
        };
        assert_eq!(
            proposal.count,
            IterationCount::Fixed(count),
            "count={count}: the tampered count reaches `apply_confirmed_shortcut` unchanged"
        );

        let mut restored_runner = GameRunner::from_state(restored);
        accept_all_opponents(&mut restored_runner);

        let after: Vec<i32> = restored_runner
            .state()
            .players
            .iter()
            .map(|p| p.life)
            .collect();
        if over_cap {
            assert_eq!(
                after, lives,
                "count={count}: (a) ZERO cycles committed — the cap fired before the first \
                 clone, which is the discriminator a handback alone cannot be"
            );
            assert!(
                matches!(
                    restored_runner.state().waiting_for,
                    WaitingFor::Priority { .. }
                ),
                "count={count}: (a) control comes back at a priority wait rather than \
                 mid-drive, got {:?}",
                restored_runner.state().waiting_for
            );
        } else {
            assert_ne!(
                after, lives,
                "count={count}: (b) a legal count still DRIVES — without this (a)'s no-delta \
                 assertion is vacuous"
            );
        }
    }
}

// ─────── AI1 — the AI's bounded-declare candidate withdraws on a 0→1 schema ───────

/// **AI1 — the generator's `Fixed(suggestion)` candidate is keyed to the PUBLISHED PIN SET, measured
/// in BOTH directions on ONE board.**
///
/// CR 732.2a. `ai_support::candidates` emits `DeclareShortcut { count: Fixed(suggestion),
/// template: None }` only `if schema.points.is_empty() && schema.is_bounded()`, because a
/// `template: None` declaration fail-closes against a published pin set — the engine would
/// ACCEPT it and then discard it, handing the search layer an action that looks legal and is
/// not.
///
/// The R5 offer is exactly the board that MOVED: it used to publish nothing (so the `Fixed`
/// candidate was emitted), and the answer-beat sampling site now announces the Bond's trigger
/// entry, so it publishes one `Targets` point. This row is the pin for that transition.
///
/// * **arm (a), the live board:** one published point, and the offer carries the engine's own
///   declaration for it ⇒ the `Fixed` candidate is emitted CARRYING THAT DECLARATION.
/// * **arm (b), the POSITIVE CONTROL, same board one field apart:** stage the schema's `points`
///   empty ([`r28_empty_schema_offer`]) ⇒ the `Fixed` candidate is emitted with `template: None`.
///   Without this arm, arm (a) would be satisfied by a generator that emitted `Fixed`
///   unconditionally.
///
/// ⚠ **ARM (a)'S PREVIOUS CLAIM WAS THE OPPOSITE, AND IT IS SUPERSEDED, NOT BROKEN.** As
/// `ai1_the_bounded_declare_candidate_withdraws_when_the_offer_publishes_a_pin` it asserted
/// `assert_eq!(live, vec![GameAction::DeclineShortcut])` — that a published pin set WITHDREW the
/// declare candidate, because the only declaration the generator could emit carried
/// `template: None` and would be accepted-then-discarded. item-4 C2b gives the generator the
/// offer's own declaration to carry, so the withdrawal is exactly the behaviour this commit
/// replaces, and the name had to stop saying "withdraws".
///
/// **ARM (b) IS BYTE-IDENTICAL AND THAT IS EARNED, NOT LUCK.** [`r28_empty_schema_offer`] is a
/// rest-less destructure plus a rebuild literal, so C2b had to CHOOSE a value for `declaration`
/// there; it passes `None`. Threading the live field through would stage this board's measured
/// `Some` declaration into a control that exists to be declaration-free, and arm (b)'s
/// `template: None` match would fail. See that helper's own comment.
///
/// Both arms read the ENGINE's candidate set through `legal_actions`, the same seam
/// `phase-ai`'s search calls, so this is not a re-implementation of the gate agreeing with
/// itself. The row is deliberately NOT `#[ignore]`d: the two pre-existing `phase-ai` bounded
/// rows are, and an ignored row reports `ok` while executing nothing.
#[test]
fn ai1_the_bounded_declare_candidate_carries_the_offers_own_pin_when_one_is_published() {
    // ── arm (a): the LIVE offer, which now publishes one point ──
    let (mut runner, slot, _bond, _hexproof, _lives) = r5_reach_offer();
    let WaitingFor::LoopShortcut {
        schema,
        declaration,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!("r5_reach_offer returns at the offer");
    };
    assert!(
        schema.is_bounded(),
        "REACH-GUARD: the `Fixed` candidate is gated on `is_bounded()` as well, so an unbounded \
         offer would withhold it for the wrong reason"
    );
    assert_eq!(
        schema.points.len(),
        1,
        "REACH-GUARD: the published pin set is the conjunct this row is about; got {:?}",
        schema.points
    );
    let declaration = declaration.expect(
        "REACH-GUARD: this board's proposer answered its one published point, so the offer \
         publishes a declaration — without one arm (a) would measure the fail-closed path \
         `d6n_a_points_carrying_offer_without_a_declaration_enumerates_only_decline` covers",
    );
    assert_eq!(
        declaration.replay,
        ReplayMode::Scheduled {
            count: IterationCount::Fixed(999)
        },
        "REACH-GUARD: the observation drives P1's 999 life"
    );
    // CR 104.2a: a win takes 999 + 1000 + 1000 repetitions, past the 1000 capacity, and the
    // proposer only gains, so the suggestion is the witness at the capacity, cut to the segments
    // 1000 repetitions reach.
    assert_eq!(schema.deliverable_capacity, 1000);
    assert_eq!(schema.iteration_count, IterationCount::Fixed(1000));
    let suggestion = schema
        .suggested_declaration
        .clone()
        .expect("the offer publishes its witness as the suggestion");
    let seat = |s: PlayerId| Ranking::one(AnnouncementSubject::Seat(s));
    assert_eq!(
        suggestion.decisions,
        vec![PinnedDecision::Targets {
            slot: slot.clone(),
            targets: vec![TargetPin::Scheduled(TargetSchedule::Piecewise(vec![
                (0, seat(P1)),
                (998, seat(P2)),
            ]))],
        }]
    );
    let live = engine::ai_support::legal_actions(runner.state());
    assert_eq!(
        live,
        vec![
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(1000),
                template: Some(suggestion),
            },
            GameAction::DeclineShortcut,
        ],
        "AI1(a): against a points-carrying bounded offer that HAS a declaration, the generator \
         emits the offer's suggestion — carrying the ENGINE's own pin set, never one the AI built"
    );
    // CR 732.2b: the responder is stated the declaration the AI took, at its count.
    let mut declared = runner.state().clone();
    apply(&mut declared, P0, live[0].clone()).expect("the AI's declaration is admitted");
    let WaitingFor::RespondToShortcut { player, .. } = declared.waiting_for.clone() else {
        panic!("the declaration opens the respond window");
    };
    engine::game::interaction::bind_interaction_authority(
        &mut declared,
        engine::types::interaction::InteractionSessionId("ai1".into()),
    )
    .expect("valid interaction authority binding");
    let filtered = engine::game::visibility::filter_state_for_viewer(&declared, player);
    let view = engine::game::interaction::derive_viewer_interaction(&declared, &filtered, player);
    let [opportunity] = view.opportunities.as_slice() else {
        panic!("the responder reads one opportunity");
    };
    let engine::types::interaction::InteractionOpportunityResponse::Schema {
        spec:
            engine::types::interaction::InteractionResponseSpec::ShortcutReply {
                declared: Some(preview),
                ..
            },
        ..
    } = &opportunity.response
    else {
        panic!(
            "the responder reads the declared preview, got {:?}",
            opportunity.response
        );
    };
    let life: Vec<(Option<u8>, i32)> = preview
        .entries
        .iter()
        .map(|entry| (entry.player, entry.amount))
        .collect();
    assert_eq!(life, vec![(Some(0), 1000), (Some(1), -999), (Some(2), -1)]);

    // ── arm (b): the POSITIVE CONTROL — the same board with an EMPTY point set ──
    r28_empty_schema_offer(&mut runner);
    let staged = engine::ai_support::legal_actions(runner.state());
    assert!(
        staged.iter().any(|a| matches!(
            a,
            GameAction::DeclareShortcut {
                count: IterationCount::Fixed(_),
                template: None,
            }
        )),
        "AI1(b) POSITIVE CONTROL: with `points` empty the generator MUST emit the \
         suggestion-valued `Fixed` candidate again. Its absence here would mean arm (a) measured \
         a generator that emits nothing rather than one keyed to the pin set. got {staged:?}"
    );
    assert!(
        staged.contains(&GameAction::DeclineShortcut),
        "AI1(b): the decline stays legal on both arms — only the `Fixed` candidate moves, which \
         is what makes the pair one axis apart"
    );
}

/// **Row D7 — a PRE-DECLARATION save decodes with `declaration: None`, i.e. today's refusal.**
///
/// CR 732.2a. `WaitingFor::LoopShortcut.declaration` carries `#[serde(default)]`, following
/// `schema`'s precedent on the same variant. The consequence is CHOSEN, not discovered: a
/// snapshot written before this field existed decodes with `None`, the AI's declare candidate
/// stays withheld (`declaration.is_some()` is false) and the human path is unchanged — the same
/// behaviour that shipped before the field. Fail-closed by construction.
///
/// # Non-vacuity
///
/// The positive control is the round-trip WITH the key present: a decoder that always produced
/// `None` — or a `declaration` that never serialized at all — fails it. And the key's removal is
/// asserted to have actually removed something, so a typo in the field name cannot make the
/// "old save" arm pass by decoding an unmodified payload.
///
/// # ⚠ REVERT-PROBE, MEASURED — and the OBVIOUS probe is INERT, which is why it is named here
///
/// Deleting `#[serde(default)]` from the field does **NOT** red this row: measured, the stripped
/// payload still decodes and this test still passes. `serde_derive` routes a missing field
/// through `serde::__private::de::missing_field`, whose deserializer answers `deserialize_option`
/// with `visit_none` — so an `Option<T>` field is already missing-tolerant, and the attribute is
/// belt-and-braces here (it follows `schema`'s precedent on the same variant and states the
/// intent explicitly; it becomes load-bearing the moment the field stops being an `Option`).
///
/// The two probes that DO red this row, one per arm, both RUN:
///
/// * `#[serde(skip)]` in place of `#[serde(default)]` ⇒ the declaration never reaches the wire
///   ⇒ the POSITIVE CONTROL round-trip fails (a `Some(..)` decodes back as `None`);
/// * `#[serde(default = "…")]` pointing at a function returning `Some(..)` ⇒ the stripped
///   payload decodes with a fabricated declaration ⇒ the old-save arm's `matches!` fails.
#[test]
fn d7_a_pre_declaration_save_decodes_with_no_declaration() {
    let slot = DecisionSlot::first(
        YieldTarget::ThisObject {
            source_id: ObjectId(881),
            incarnation: Some(1),
            trigger_description: None,
        },
        ChoicePoint::AnnouncedTarget,
    );
    let offer = WaitingFor::LoopShortcut {
        proposer: P0,
        predicted_winner: None,
        certificate: synthetic_lethal_cert(),
        schema: ShortcutDecisionSchema {
            suggested_declaration: None,
            leading_aims: Vec::new(),
            iteration_count: IterationCount::Fixed(3),
            measured_repetition_bound: Some(3),
            deliverable_capacity: 3,
            points: vec![DecisionPoint {
                slot: slot.clone(),
                kind: DecisionPointKind::Targets {
                    legal_targets: vec![TargetRef::Player(P1)],
                    min_targets: 1,
                    max_targets: 1,
                    ordered: false,
                },
            }],
            convoke_tappable_count: 0,
        },
        declaration: Some(DecisionTemplate {
            owner: P0,
            decisions: vec![PinnedDecision::Targets {
                slot: slot.clone(),
                targets: vec![TargetPin::Player(P1)],
            }],
            replay: ReplayMode::Scheduled {
                count: IterationCount::Fixed(3),
            },
            key: DecisionGroupKey::from_sources(&[slot.source], DecisionKind::LoopChoice),
        }),
        road: engine::analysis::loop_check::OfferRoad::Ring,
        counted_take: Default::default(),
        period: Default::default(),
    };

    let mut json = serde_json::to_value(&offer).expect("the offer serializes");
    // POSITIVE CONTROL: with the key present the declaration survives the wire intact.
    assert_eq!(
        serde_json::from_value::<WaitingFor>(json.clone()).expect("round-trips"),
        offer,
        "a live offer's declaration must survive serialization — otherwise the `None` below \
         would prove nothing about the DEFAULT"
    );

    // The pre-C2b payload: the same offer with no `declaration` key at all.
    let removed = json["data"]
        .as_object_mut()
        .expect("the adjacently-tagged payload is an object")
        .remove("declaration");
    assert!(
        removed.is_some(),
        "reach-guard: the key must have been present to remove, else the 'old save' arm below \
         decodes an unmodified payload and asserts nothing"
    );
    let decoded: WaitingFor = serde_json::from_value(json).expect(
        "an OLD save must still decode (CR 732.2a offers \
             predate this field)",
    );
    assert!(
        matches!(
            decoded,
            WaitingFor::LoopShortcut {
                declaration: None,
                ..
            }
        ),
        "the forward-compatible default is `None`, which is today's refusal — fail-closed. got \
         {decoded:?}"
    );
}

// ───── PR #7005 maintainer item: the answer-beat sampler records the SYNCHRONIZED window ─────

// ───── #7023 maintainer item: a beat's minted frames are a MEMBERSHIP set, not a length delta ─────

/// **Row T1b — STRUCTURAL, and the tier is FORCED.** CR 608.2b + CR 601.2c (reached via
/// CR 603.3d): BOTH `WaitingFor::TriggerTargetSelection` reducer arms route their
/// announcement through the single write authority `record_trigger_target_answer`.
///
/// # ⚠ WHY THIS IS A SOURCE CENSUS AND NOT A WIRE ROW — measured, not conceded
///
/// The `ChooseTarget` arm is covered end-to-end at the wire tier by
/// `fantastic_four_bounded_loop.rs`'s
/// `c2a_row_t1_the_announced_target_is_journalled_at_the_f4_offers_published_slot` and its
/// P2 provenance sibling. **The `SelectTargets` arm has NO tracked fixture that reaches it.**
/// Driving all five tracked 4p dumps in this file for 60 beats each through production
/// `apply()`: only `dellian_emblem_conqueror_4p` reaches a `TriggerTargetSelection` window,
/// it reaches seven of them, and every one enumerates `ChooseTarget` ×3 / `SelectTargets` ×0;
/// the other four reach none. See [`dump_drive_one_beat`]'s doc for the per-dump numbers.
/// A wire row for that arm is therefore not writable from this repo's fixtures today —
/// recorded as a BACKLOG item (needs a dump whose trigger declares a multi-slot or
/// object-target announcement), never as a silently-absent row.
///
/// This census covers exactly what it can: that the arm is WIRED. The writer's BEHAVIOUR is
/// proven separately and at a tier that can carry it — `game::engine`'s
/// `c2a_row_t5_an_unresolvable_target_abandons_the_whole_journal_write` drives the helper
/// itself, and both arms call that one helper, which is the point of it being one helper.
/// It is the same instrument class, and the same reasoning, as
/// `fantastic_four_bounded_loop.rs`'s ring-clear census.
///
/// # Discrimination
///
/// Delete the `record_trigger_target_answer(..)` call from EITHER arm ⇒ that arm lands in
/// `unwired` and this row reds NAMING the arm and its line. The mutation compiles (the other
/// caller survives), so it reds on the assert, not on a compile error. Without this row, r2's
/// own finding stands: deleting the `SelectTargets` call reds nothing in the suite.
///
/// # Reach-guard
///
/// The arm COUNT is asserted first and is independent of the call: a pattern reflow that hid
/// an arm from this scanner would read 1 or 0 and fail here rather than passing on a census
/// that found nothing to check.
#[test]
fn c2a_row_t1b_both_trigger_target_selection_arms_route_through_the_single_writer() {
    use std::path::Path;

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("game/engine.rs");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let lines: Vec<&str> = text.lines().collect();

    let (mut wired, mut unwired) = (Vec::new(), Vec::new());
    for (i, line) in lines.iter().enumerate() {
        let action = if line.contains("GameAction::SelectTargets {") {
            "SelectTargets"
        } else if line.contains("GameAction::ChooseTarget {") {
            "ChooseTarget"
        } else {
            continue;
        };
        // A reducer arm's `WaitingFor` half sits a few lines above its `GameAction` half in
        // the tuple pattern; anything further away is a different construct.
        if !lines[i.saturating_sub(10)..i]
            .join("\n")
            .contains("WaitingFor::TriggerTargetSelection {")
        {
            continue;
        }
        if lines[i..(i + 14).min(lines.len())]
            .join("\n")
            .contains("record_trigger_target_answer(")
        {
            wired.push(action);
        } else {
            unwired.push(format!("game/engine.rs:{} ({action})", i + 1));
        }
    }

    assert_eq!(
        wired.len() + unwired.len(),
        2,
        "reach-guard: `apply_action` has exactly TWO `WaitingFor::TriggerTargetSelection` \
         reducer arms (`SelectTargets` and `ChooseTarget`). A different count means an arm \
         was added, removed, or reflowed out of this scanner's reach — re-derive this census, \
         do not re-number it. wired={wired:?} unwired={unwired:?}"
    );
    assert!(
        unwired.is_empty(),
        "CR 608.2b: every `TriggerTargetSelection` reducer arm must journal its announcement \
         through `record_trigger_target_answer`, the single write authority. Unwired: \
         {unwired:?}"
    );
    assert_eq!(
        {
            let mut w = wired.clone();
            w.sort_unstable();
            w
        },
        vec!["ChooseTarget", "SelectTargets"],
        "both arms by NAME, not just by count: a census that found the same arm twice would \
         satisfy a bare count while leaving the other one unmeasured"
    );
}

/// One leg of the detector-cost drive: the same fixture, the same drive helper and the same
/// beat count at one [`LoopDetectionMode`], with this thread's meter zeroed at its start.
struct CostLeg {
    cost: LoopDetectCost,
    wall_ns: u128,
    beats: usize,
    lives: Vec<i32>,
    stack_len: usize,
    waiting: String,
}

/// The `WaitingFor` variant name alone — the discriminant the leg-equality check compares,
/// without a `LoopShortcut`'s whole certificate in the failure message.
fn waiting_kind(waiting: &WaitingFor) -> String {
    let debug = format!("{waiting:?}");
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn drive_cost_leg(mode: LoopDetectionMode, beats: usize) -> CostLeg {
    let mut state = restore_dump(&gunzip_dump(include_bytes!(
        "../fixtures/dellian_emblem_conqueror_4p.json.gz"
    )));
    assert_eq!(
        state.loop_detect_ring.len(),
        0,
        "reach-guard: the dump ships with an EMPTY ring, so the ring the reduction walks \
         below is THIS drive's accumulation through the production sampler"
    );
    state.loop_detection = mode;
    let pin = engine_live_opponents(&state, P0).first().copied();

    // Fixture identity: the drive this row sizes is the bridge's, whose seam wants a stack.
    assert!(
        !state.stack.is_empty(),
        "{mode:?} entry: the fixture must ship with a NON-EMPTY stack"
    );

    reset_loop_detect_cost();
    let started = std::time::Instant::now();
    let mut ran = 0usize;
    for beat in 0..beats {
        if dump_drive_one_beat(&mut state, pin).is_err() {
            break;
        }
        ran = beat + 1;
        // The ground for the PRINTED empty-stack sites, read off the METER rather than
        // inferred from the stack: `finish_action_boundary_with_lifecycle` reconciles twice
        // per `apply()`, either side of `run_auto_pass_loop`, so a stack observed after
        // `apply()` returns never dominates the first reconcile's guard.
        let so_far = loop_detect_cost();
        assert_eq!(
            (
                so_far.object_growth_calls,
                so_far.compares_cover_modulo_fodder_growth
            ),
            (0, 0),
            "{mode:?} beat {beat}: `object_growth` read {} calls and \
             `compares_cover_modulo_fodder_growth` {}; both must stay zero on this drive",
            so_far.object_growth_calls,
            so_far.compares_cover_modulo_fodder_growth
        );
    }
    let wall_ns = started.elapsed().as_nanos();

    CostLeg {
        cost: loop_detect_cost(),
        wall_ns,
        beats: ran,
        lives: state.players.iter().map(|p| p.life).collect(),
        stack_len: state.stack.len(),
        waiting: waiting_kind(&state.waiting_for),
    }
}

/// The `Interactive` and detector-`Off` legs of one cost drive on the same fixture, with the
/// hostile-fixture leg equality both cost rows rest on already asserted.
///
/// THE LEG ORDER IS LOAD-BEARING: `Interactive` runs BEFORE the timed `Off` leg, because with
/// `Off` first that leg contributes zero anyway, so an inoperative `reset_loop_detect_cost()`
/// would be invisible and the `Off`-reads-zero assertion would pass for the wrong reason.
fn detector_cost_legs(beats: usize) -> (CostLeg, CostLeg) {
    let interactive = drive_cost_leg(LoopDetectionMode::Interactive, beats);
    let off = drive_cost_leg(LoopDetectionMode::Off, beats);
    assert_eq!(
        (
            interactive.beats,
            &interactive.lives,
            interactive.stack_len,
            &interactive.waiting
        ),
        (off.beats, &off.lives, off.stack_len, &off.waiting),
        "the two legs must run the same beat count and end on the same life vector, stack \
         length and `waiting_for` discriminant"
    );
    assert_eq!(
        interactive.beats, beats,
        "reach-guard: both legs must have driven the full {beats} beats; a short drive is a \
         drive helper failure, not a measurement"
    );
    (interactive, off)
}

/// CR 732.2a: the same drive, ATTRIBUTED — every part's share of `wall(Interactive) -
/// wall(Off)`, with the residual reported rather than absorbed into a neighbouring share.
///
/// An instrument, not a CI row: share stability is what needs the long drive, and a wall clock
/// only means something under the build profile it is read in. It PRINTS its attribution, so
/// `--no-capture` is part of the command and not a taste — without it the run is a green PASS
/// and no attribution at all. Run it as
/// `cargo nextest run -p phase-engine --cargo-profile server-release --run-ignored all
/// --no-capture -E 'test(=loop_shortcut::the_detector_cost_attributes_to_its_named_parts)'`.
#[test]
#[ignore = "measurement instrument, not an assertion; see the doc comment"]
fn the_detector_cost_attributes_to_its_named_parts() {
    const BEATS: usize = 90;
    let (interactive, off) = detector_cost_legs(BEATS);

    // The divisor of every share printed below.
    let detector_ns = interactive.wall_ns.saturating_sub(off.wall_ns);
    assert!(
        detector_ns > 0,
        "wall(Interactive) {} ns must EXCEED wall(Off) {} ns — a vanished difference divides \
         every share below by zero",
        interactive.wall_ns,
        off.wall_ns
    );

    let ms = |nanos: u128| nanos as f64 / 1.0e6;
    let share = |nanos: u64| 100.0 * nanos as f64 / detector_ns as f64;
    // Leaf parts only: `reconcile` is the CONTAINER of the reduction sites, so summing it
    // beside them would double-count the whole bridge.
    let leaves: Vec<(&str, u64, u32)> = interactive
        .cost
        .parts()
        .into_iter()
        .filter(|(name, _, _)| *name != "reconcile")
        .collect();
    let accounted: u128 = leaves.iter().map(|(_, nanos, _)| u128::from(*nanos)).sum();
    let residual = detector_ns as i128 - accounted as i128;

    println!(
        "BEATS {BEATS}  wall(Interactive) {:.1} ms  wall(Off) {:.1} ms  \
         DETECTOR-BY-DIFFERENCE {:.1} ms ({:.2} ms/beat)  lives {:?}  stack {}  waiting {}",
        ms(interactive.wall_ns),
        ms(off.wall_ns),
        ms(detector_ns),
        ms(detector_ns) / BEATS as f64,
        interactive.lives,
        interactive.stack_len,
        interactive.waiting
    );
    println!(
        "  CONTAINER reconcile {:.2} ms  calls={}  share={:.2}%  (holds every reduction site \
         below it; excluded from the leaf sum)",
        ms(u128::from(interactive.cost.reconcile_ns)),
        interactive.cost.reconcile_calls,
        share(interactive.cost.reconcile_ns)
    );
    let mut ordered = leaves.clone();
    ordered.sort_by_key(|(_, nanos, _)| std::cmp::Reverse(*nanos));
    println!("  LEAF PARTS, ordered by share of the detector:");
    for (name, nanos, calls) in &ordered {
        println!(
            "    {name:<28} {:>10.2} ms  calls={calls:>6}  share={:>7.2}%",
            ms(u128::from(*nanos)),
            share(*nanos)
        );
    }
    println!(
        "  DERIVED resource_compares {}  vs projected_clones {}",
        interactive.cost.resource_compares(),
        interactive.cost.projected_clones
    );
    println!(
        "  ACCOUNTED {:.1} ms = {:.2}% of the detector;  RESIDUAL {:.1} ms = {:.2}%  \
         (reported, never absorbed into a neighbouring share)",
        ms(accounted),
        100.0 * accounted as f64 / detector_ns as f64,
        residual as f64 / 1.0e6,
        100.0 * residual as f64 / detector_ns as f64
    );
}

/// The window's trace offers no span.
fn no_offered_span(state: &GameState) -> bool {
    engine::game::play_trace_view(state).is_none_or(|view| view.offered.is_none())
}

// ───────────── loop endings and the match (CR 104.1 + CR 100.4) ─────────────

fn install_match(runner: &mut GameRunner, match_type: MatchType, mode: LoopDetectionMode) {
    runner.state_mut().set_match_config(MatchConfig {
        match_type,
        loop_detection: mode,
    });
}

/// What a game left for its match: the record, the match phase, the game number, the wait, and
/// the seat a sideboard prompt asks.
type MatchReading = (
    Option<GameEnd>,
    MatchPhase,
    u8,
    &'static str,
    Option<PlayerId>,
);

fn match_reading(state: &GameState) -> MatchReading {
    let asked = match state.waiting_for {
        WaitingFor::BetweenGamesSideboard { player, .. } => Some(player),
        _ => None,
    };
    (
        state.game_end,
        state.match_phase,
        state.game_number,
        state.waiting_for.variant_name(),
        asked,
    )
}

/// CR 104.1 + CR 100.4: game 1 is over on the record for `winner`; a best-of-one is complete,
/// and a best-of-three asks P0 to sideboard for game 2.
fn ended(match_type: MatchType, winner: Option<PlayerId>) -> MatchReading {
    let record = Some(GameEnd { winner });
    match match_type {
        MatchType::Bo1 => (record, MatchPhase::Completed, 1, "GameOver", None),
        MatchType::Bo3 => (
            record,
            MatchPhase::BetweenGames,
            2,
            "BetweenGamesSideboard",
            Some(P0),
        ),
    }
}

/// Declares `count` on the standing offer and has every opponent accept; the events emitted.
pub(crate) fn declare_and_accept(runner: &mut GameRunner, count: IterationCount) -> Vec<GameEvent> {
    let mut events = runner
        .act(GameAction::DeclareShortcut {
            count,
            template: None,
        })
        .expect("the proposer declares")
        .events;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        events.extend(
            runner
                .act(GameAction::RespondToShortcut {
                    response: ShortcutResponse::Accept,
                })
                .expect("an opponent accepts")
                .events,
        );
    }
    events
}

/// CR 732.2a + CR 104.2a: a mandatory drain the detector crowns, from the `On` arm and from the
/// interactive bridge, ends the game on the record and reaches its match.
#[test]
fn a_crowned_mandatory_drain_is_recorded_and_reaches_its_match() {
    let mut got = Vec::new();
    let mut want = Vec::new();
    for mode in [LoopDetectionMode::On, LoopDetectionMode::Interactive] {
        for match_type in [MatchType::Bo1, MatchType::Bo3] {
            let (mut runner, kickoff) = setup_2p_drain(mode);
            install_match(&mut runner, match_type, mode);
            let _ = runner.cast(kickoff).resolve();
            let _ = drive_collect(&mut runner, 500);
            assert!(
                life(&runner, P1) > 0 && !is_eliminated(&runner, P1),
                "{mode:?} {match_type:?}: reach: a crown ended the game, not a CR 704.5a death"
            );
            got.push((mode, match_type, match_reading(runner.state())));
            want.push((mode, match_type, ended(match_type, Some(P0))));
        }
    }
    assert_eq!(got, want);
}

/// CR 704.5a + CR 100.4: with the detector off the same drain kills P1 and the best-of-three
/// goes on to sideboarding — the ending every crowned row is held to.
#[test]
fn a_natural_drain_death_in_a_best_of_three_reaches_sideboarding() {
    let (mut runner, kickoff) = setup_2p_drain(LoopDetectionMode::Off);
    install_match(&mut runner, MatchType::Bo3, LoopDetectionMode::Off);
    let _ = runner.cast(kickoff).resolve();
    let _ = drive_collect(&mut runner, 500);
    assert!(is_eliminated(&runner, P1) && life(&runner, P1) == 0);
    assert_eq!(
        match_reading(runner.state()),
        ended(MatchType::Bo3, Some(P0))
    );
}

/// CR 104.4b + CR 732.4: "Whenever you gain life, you gain 1 life." after a life gain is a
/// mandatory loop that moves nobody toward losing — a drawn game, which a best-of-three scores
/// and follows with sideboarding.
#[test]
fn a_mandatory_no_loss_loop_draw_in_a_best_of_three_reaches_sideboarding() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Test Life Engine", 2, 2, SELF_LIFE_ENGINE);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    install_match(&mut runner, MatchType::Bo3, LoopDetectionMode::Interactive);
    let _ = runner.cast(kickoff).resolve();
    let _ = drive_collect(&mut runner, 500);

    let state = runner.state();
    assert_eq!(state.match_score.draws, 1, "reach: the game was drawn");
    assert!(state.players.iter().all(|p| !p.is_eliminated));
    assert_eq!(match_reading(state), ended(MatchType::Bo3, None));
}

/// CR 704.5a + CR 104.2a: a take whose own driven cycle takes the opponent to 0 ends the game
/// with that cycle, once — Marauding Blight-Priest's drain offered at P1's 19 and taken 19 — while
/// 20, a count past the game's end, is refused.
#[test]
fn a_take_whose_drive_ends_the_game_commits_that_ending() {
    let mut got = Vec::new();
    let mut want = Vec::new();
    for match_type in [MatchType::Bo1, MatchType::Bo3] {
        let (mut runner, wine, _bolt, _priest) = setup_2p_blight_priest_drain();
        install_match(&mut runner, match_type, LoopDetectionMode::Interactive);
        let _ = runner.cast(wine).resolve();
        let (_events, offer) = drive_collect(&mut runner, 500);
        assert!(
            matches!(offer, WaitingFor::LoopShortcut { .. }),
            "reach: the drain is offered, got {offer:?}"
        );
        assert_eq!(life(&runner, P1), 19, "reach: the offer frame");
        let lives = seat_lives(runner.state());
        let mut past = runner.state().clone();
        r6a_declare_and_accept_all(&mut past, P0, 20);
        let events = declare_and_accept(&mut runner, IterationCount::Fixed(19));
        let game_overs: Vec<GameEvent> = events
            .into_iter()
            .filter(|event| matches!(event, GameEvent::GameOver { .. }))
            .collect();
        got.push((
            match_type,
            match_reading(runner.state()),
            life(&runner, P1),
            is_eliminated(&runner, P1),
            game_overs,
            past.waiting_for.clone(),
            seat_lives(&past),
        ));
        want.push((
            match_type,
            ended(match_type, Some(P0)),
            0,
            true,
            vec![GameEvent::GameOver { winner: Some(P0) }],
            WaitingFor::Priority { player: P0 },
            lives,
        ));
    }
    assert_eq!(got, want);
}

/// CR 732.2a + CR 104.2a: a mandatory drain the detector crowns only after auto-pass has advanced
/// the game ends on the record and reaches its match.
#[test]
fn a_drain_crowned_after_an_auto_pass_advance_reaches_its_match() {
    let arm = || GameAction::SetAutoPass {
        mode: AutoPassRequest::UntilTurnBoundary {
            until: Default::default(),
        },
    };
    let mut got = Vec::new();
    let mut want = Vec::new();
    for match_type in [MatchType::Bo1, MatchType::Bo3] {
        let (mut runner, kickoff) = setup_2p_drain(LoopDetectionMode::On);
        install_match(&mut runner, match_type, LoopDetectionMode::On);
        runner.state_mut().players[1].life = 100_000;
        let p0_cards: Vec<_> = runner
            .state()
            .objects
            .values()
            .filter(|object| object.controller == P0)
            .map(|object| object.card_id)
            .collect();
        for card_id in p0_cards {
            runner.state_mut().add_priority_yield(
                P1,
                YieldTarget::AllCopies {
                    card_id,
                    trigger_description: None,
                },
            );
        }
        runner.cast(kickoff).commit();
        // P0 arms with the kickoff on the stack, P1 passes it and arms on the first trigger.
        let mut events = Vec::new();
        for action in [arm(), GameAction::PassPriority, arm()] {
            events.extend(runner.act(action).expect("a legal beat").events);
        }
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::ResolutionHalted { .. })),
            "{match_type:?}: reach: auto-pass advanced the game before the crown"
        );
        assert!(
            life(&runner, P1) > 0 && !is_eliminated(&runner, P1),
            "{match_type:?}: reach: a crown ended the game, not a CR 704.5a death"
        );
        got.push((match_type, match_reading(runner.state())));
        want.push((match_type, ended(match_type, Some(P0))));
    }
    assert_eq!(got, want);
}

/// Drain Cleric, Blood Sipper and Bloodthirsty Conqueror: each loss of P1's raises two
/// simultaneous triggers for P0 to order. P1's castable Bolt keeps the loop optional.
fn setup_2p_two_gainer_drain(mode: LoopDetectionMode) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(P0, "Test Drain Cleric", 2, 2, DRAIN_CLERIC);
    scenario.add_creature_from_oracle(P0, "Test Blood Sipper", 2, 2, BLOOD_SIPPER);
    scenario.add_creature_from_oracle(P0, "Bloodthirsty Conqueror", 5, 5, BLOODTHIRSTY_CONQUEROR);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let kickoff = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Lifegain Kickoff", false, KICKOFF)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = mode;
    (runner, kickoff)
}

/// The two-gainer drain played out by hand with the detector off: P0's final life and the size
/// of the stack the game ended on.
fn two_gainer_drain_by_hand(match_type: MatchType) -> (i32, usize) {
    let (mut runner, kickoff) = setup_2p_two_gainer_drain(LoopDetectionMode::Off);
    install_match(&mut runner, match_type, LoopDetectionMode::Off);
    let _ = runner.cast(kickoff).resolve();
    let _ = drive_collect(&mut runner, 5000);
    assert!(
        runner.state().game_end.is_some(),
        "reach: play ends the game"
    );
    (life(&runner, P0), runner.state().stack.len())
}

/// CR 704.5a + CR 104.1: a take whose lethal beat is the answer to a mid-cycle prompt ends the
/// game there, once, where playing the same board by hand ends it.
#[test]
fn a_take_ending_at_an_answer_beat_ends_where_play_by_hand_does() {
    let mut got = Vec::new();
    let mut want = Vec::new();
    let mut stacks = Vec::new();
    for match_type in [MatchType::Bo1, MatchType::Bo3] {
        let (by_hand_life, by_hand_stack) = two_gainer_drain_by_hand(match_type);
        let (mut runner, kickoff) = setup_2p_two_gainer_drain(LoopDetectionMode::Interactive);
        install_match(&mut runner, match_type, LoopDetectionMode::Interactive);
        let _ = runner.cast(kickoff).resolve();
        let (_events, offer) = drive_collect(&mut runner, 2000);
        assert!(
            matches!(offer, WaitingFor::LoopShortcut { .. }),
            "reach: the drain is offered, got {offer:?}"
        );
        let count = offered(runner.state()).1;
        let events = declare_and_accept(&mut runner, count.clone());
        let game_overs: Vec<GameEvent> = events
            .into_iter()
            .filter(|event| matches!(event, GameEvent::GameOver { .. }))
            .collect();
        stacks.push((runner.state().stack.len(), by_hand_stack));
        got.push((
            count.clone(),
            match_type,
            match_reading(runner.state()),
            life(&runner, P0),
            game_overs,
        ));
        want.push((
            count,
            match_type,
            ended(match_type, Some(P0)),
            by_hand_life,
            vec![GameEvent::GameOver { winner: Some(P0) }],
        ));
    }
    assert_eq!(got, want);
    assert!(
        stacks.iter().all(|(taken, by_hand)| taken == by_hand),
        "reach: play by hand and a take ending on the answer both end with the lethal loss's \
         two triggers on the stack: {stacks:?}"
    );
}
/// What each Sprout Swarm recast offers on the board of [`sprout_swarm_scenario_with_drain`]: the
/// first offer taken whole when `take_first`, declined otherwise, then every later offer declined
/// and Sprout Swarm recast twelve times, convoked by an untapped Saproling. `Some((bounded,
/// capacity))` for an offer, `None` for none.
fn self_damage_recast_offers(
    mode: LoopDetectionMode,
    take_first: bool,
) -> Vec<Option<(bool, u32)>> {
    let drain = "Whenever you cast a spell, Test Drain Engine deals 1 damage to Test Drain Engine.";
    let (mut runner, sprout, fodder) = sprout_swarm_scenario_with_drain(4, Some(drain));
    runner.state_mut().loop_detection = mode;
    runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    let offered = |state: &GameState| match &state.waiting_for {
        WaitingFor::LoopShortcut { schema, .. } => {
            Some((schema.is_bounded(), schema.deliverable_capacity))
        }
        _ => None,
    };
    let mut offers = vec![offered(runner.state())];
    if take_first {
        let count = runner_capacity(&runner);
        runner
            .act(GameAction::DeclareShortcut {
                count: IterationCount::Fixed(count),
                template: None,
            })
            .expect("declare");
        while matches!(
            runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ) {
            runner
                .act(GameAction::RespondToShortcut {
                    response: ShortcutResponse::Accept,
                })
                .expect("accept");
        }
    }
    for _ in 0..12 {
        if matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }) {
            runner.act(GameAction::DeclineShortcut).expect("decline");
        }
        let state = runner.state();
        let card = state
            .objects
            .values()
            .find(|o| o.name == "Sprout Swarm" && o.zone == engine::types::zones::Zone::Hand)
            .map(|o| o.id)
            .expect("Sprout Swarm returns by buyback");
        let tapper = state
            .battlefield
            .iter()
            .copied()
            .find(|id| state.objects[id].name == "Saproling" && !state.objects[id].tapped)
            .expect("an untapped Saproling");
        runner
            .cast(card)
            .accept_optional()
            .convoke_with(&[tapper])
            .commit()
            .resolve();
        offers.push(offered(runner.state()));
    }
    offers
}

fn runner_capacity(runner: &GameRunner) -> u32 {
    match &runner.state().waiting_for {
        WaitingFor::LoopShortcut { schema, .. } => schema.deliverable_capacity,
        other => panic!("not offered: {other:?}"),
    }
}

/// CR 704.5g + CR 732.2a: each recast fires "Whenever you cast a spell, Test Drain Engine deals 1
/// damage to Test Drain Engine." on the controller's 9/9, so damage grows on the engine and the
/// period is counted by replay, not left unbounded. Nothing stops the recasts when the engine
/// dies, so the count stands at the cap; a take of the whole count makes a Saproling a cycle and
/// the engine is gone. Once it is gone the period repeats without bound: after the take at once,
/// and after declining once its death is behind the count. Where the replay would cross its death
/// the reproduced-class arm refuses the pair, so the seventh and eighth recasts are not offered.
/// The same loop without the drain is unbounded from the first recast.
#[test]
fn object_growth_self_damage_recast_is_counted() {
    let counted = Some((true, 25));
    let unbounded = Some((false, 1000));
    for mode in [LoopDetectionMode::On, LoopDetectionMode::Interactive] {
        let mut declined = vec![counted; 6];
        declined.extend([None, None]);
        declined.extend([unbounded; 5]);
        assert_eq!(self_damage_recast_offers(mode, false), declined, "{mode:?}");
        let mut took = vec![counted];
        took.extend([unbounded; 12]);
        assert_eq!(self_damage_recast_offers(mode, true), took, "{mode:?}");
    }

    let (mut taker, sprout, fodder) = sprout_swarm_scenario_with_drain(
        4,
        Some("Whenever you cast a spell, Test Drain Engine deals 1 damage to Test Drain Engine."),
    );
    taker
        .cast(sprout)
        .accept_optional()
        .convoke_with(&[fodder[0]])
        .commit()
        .resolve();
    let before = saproling_count(taker.state());
    taker
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(25),
            template: None,
        })
        .expect("declare");
    while matches!(
        taker.state().waiting_for,
        WaitingFor::RespondToShortcut { .. }
    ) {
        taker
            .act(GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            })
            .expect("accept");
    }
    assert_eq!(saproling_count(taker.state()), before + 25);
    assert!(!taker
        .state()
        .battlefield
        .iter()
        .any(|id| taker.state().objects[id].name == "Test Drain Engine"));

    let (mut free, free_sprout, free_fodder) = sprout_swarm_scenario(4);
    free.cast(free_sprout)
        .accept_optional()
        .convoke_with(&[free_fodder[0]])
        .commit()
        .resolve();
    let WaitingFor::LoopShortcut { schema, .. } = &free.state().waiting_for else {
        panic!("the drain-free loop is not offered");
    };
    assert!(!schema.is_bounded());
}

/// Verbatim Oracle texts (Scryfall) for the departure boards below.
const BLESSED_WINE: &str =
    "You gain 1 life.\nDraw a card at the beginning of the next turn's upkeep.";
const MARAUDING_BLIGHT_PRIEST: &str = "Whenever you gain life, each opponent loses 1 life.";
const SOOTHING_BALM: &str = "Target player gains 5 life.";
const PLATINUM_EMPERION: &str = "Your life total can't change. (You can't gain or lose life. You can't pay any amount of life except 0.)";
const EXQUISITE_BLOOD: &str = "Whenever an opponent loses life, you gain that much life.";
const KEFKA_RULER_OF_RUIN: &str =
    "Flying\nWhenever an opponent loses life during your turn, you draw that many cards.";

#[derive(Clone, Copy)]
enum Departing {
    BlightPriest,
    Conqueror,
    Kefka,
    ExquisiteBlood,
    SanguineBond,
    Vito,
}

/// Three players in P0's main phase, P0 at 20 with `permanents` entered in order and `library`
/// cards on top of its library, P1 holding Lightning Bolt and a Mountain; P0 has cast Blessed
/// Wine, whose delayed draw falls in the next turn.
fn departure_board(p1: i32, p2: i32, permanents: &[Departing], library: usize) -> GameState {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, p1);
    scenario.with_life(P2, p2);
    for permanent in permanents {
        match permanent {
            Departing::BlightPriest => {
                scenario.add_creature_from_oracle(
                    P0,
                    "Marauding Blight-Priest",
                    3,
                    2,
                    MARAUDING_BLIGHT_PRIEST,
                );
            }
            Departing::Conqueror => {
                scenario.add_creature_from_oracle(
                    P0,
                    "Bloodthirsty Conqueror",
                    5,
                    5,
                    BLOODTHIRSTY_CONQUEROR,
                );
            }
            Departing::Kefka => {
                scenario.add_creature_from_oracle(
                    P0,
                    "Kefka, Ruler of Ruin",
                    5,
                    7,
                    KEFKA_RULER_OF_RUIN,
                );
            }
            Departing::ExquisiteBlood => {
                scenario.add_enchantment_from_oracle(P0, "Exquisite Blood", EXQUISITE_BLOOD);
            }
            Departing::SanguineBond => {
                scenario.add_enchantment_from_oracle(P0, "Sanguine Bond", SANGUINE_BOND);
            }
            Departing::Vito => {
                scenario.add_creature_from_oracle(P0, "Vito, Thorn of the Dusk Rose", 1, 3, VITO);
            }
        }
    }
    let names: Vec<String> = (0..library).map(|i| format!("Library card {i}")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    scenario.with_library_top(P0, &names);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_bolt_to_hand(P1);
    let wine = scenario
        .add_spell_to_hand_from_oracle(P0, "Blessed Wine", true, BLESSED_WINE)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    drop(runner.cast(wine).commit());
    runner.state().clone()
}

/// Plays `state` on to its next loop-shortcut offer or game end, P0 ordering its Kefka triggers
/// below the others (CR 603.3b); calls `at_priority` at every priority frame on the way.
fn play_to_next_offer(state: &mut GameState, mut at_priority: impl FnMut(&GameState)) {
    for _ in 0..400 {
        match state.waiting_for.clone() {
            WaitingFor::LoopShortcut { .. } | WaitingFor::GameOver { .. } => return,
            WaitingFor::OrderTriggers { player, triggers } => {
                let (kefka, rest): (Vec<usize>, Vec<usize>) = (0..triggers.len())
                    .partition(|&i| triggers[i].source_name.starts_with("Kefka"));
                let actor =
                    engine::game::turn_control::authorized_submitter_for_player(state, player);
                apply(
                    state,
                    actor,
                    GameAction::OrderTriggers {
                        order: kefka.into_iter().chain(rest).collect(),
                    },
                )
                .expect("P0 orders its triggers");
                continue;
            }
            WaitingFor::Priority { .. } => at_priority(state),
            _ => {}
        }
        dump_drive_one_beat(state, Some(P1)).expect("the board plays on");
    }
    panic!("no offer or game end within the beat budget");
}

fn offered(state: &GameState) -> (PlayerId, IterationCount, Option<PlayerId>) {
    let WaitingFor::LoopShortcut {
        proposer,
        predicted_winner,
        schema,
        ..
    } = &state.waiting_for
    else {
        panic!(
            "expected a loop-shortcut offer, got {:?}",
            state.waiting_for
        );
    };
    (*proposer, schema.iteration_count.clone(), *predicted_winner)
}

fn library_len(state: &GameState, seat: PlayerId) -> usize {
    state.players[seat.0 as usize].library.len()
}

/// CR 603.3b + CR 732.2a + CR 800.4a: Marauding Blight-Priest's loss triggers Bloodthirsty
/// Conqueror and Kefka; P0 orders the Kefka triggers below, so they stay buried and P0 draws
/// nothing. From P1 2 / P2 8, P1 leaves at repetition 2, and every later repetition orders the
/// remaining Conqueror and Kefka triggers as the record did; P2 leaves at repetition 8 and P0 wins
/// (CR 104.2a), whichever of Conqueror and Kefka entered first.
#[test]
fn a_departure_kefka_entry_order_does_not_move_its_count() {
    for order in [
        [Departing::Conqueror, Departing::Kefka],
        [Departing::Kefka, Departing::Conqueror],
    ] {
        let mut state = departure_board(3, 9, &[Departing::BlightPriest, order[0], order[1]], 4);
        play_to_next_offer(&mut state, |_| {});
        assert_eq!(
            (offered(&state), seat_lives(&state)[1..].to_vec()),
            (
                (P0, IterationCount::Fixed(8), Some(P0)),
                vec![(P1, 2), (P2, 8)]
            )
        );
        let library = library_len(&state, P0);
        r6a_declare_and_accept_all(&mut state, P0, 8);
        assert_eq!(
            (state.waiting_for.clone(), library_len(&state, P0)),
            (WaitingFor::GameOver { winner: Some(P0) }, library)
        );
    }
}

/// CR 800.4a: after the first offer is declined, the frame where P1 is at 1 loses P1 in its first
/// repetition, so it names no period the compared cycles repeat; the drive goes on to P1 gone and
/// P2 at 4, where each repetition costs P2 1 and P0 wins at the fourth (CR 104.2a).
#[test]
fn a_departure_kefka_declined_offer_is_offered_again_after_the_departure() {
    let mut state = departure_board(
        3,
        9,
        &[
            Departing::BlightPriest,
            Departing::Conqueror,
            Departing::Kefka,
        ],
        4,
    );
    play_to_next_offer(&mut state, |_| {});
    assert_eq!(offered(&state), (P0, IterationCount::Fixed(8), Some(P0)));
    apply(&mut state, P0, GameAction::DeclineShortcut).expect("P0 declines");
    let mut lead_ins = Vec::new();
    play_to_next_offer(&mut state, |frame| {
        if frame.players[1].life == 1 && !frame.players[1].is_eliminated {
            lead_ins.extend(
                engine::game::period_confirm::confirm_for_tests(frame)
                    .into_iter()
                    .filter(|(span, _)| {
                        span.cause == engine::game::NamingCause::MandatoryRecurrence
                    })
                    .map(|(_, verdict)| verdict.map(drop)),
            );
        }
    });
    assert!(
        !lead_ins.is_empty(),
        "reach: a frame with P1 at 1 asks a span"
    );
    assert!(
        lead_ins.iter().all(
            |verdict| *verdict == Err(engine::game::period_confirm::OfferRefusal::NoRecurrence)
        ),
        "{lead_ins:?}"
    );
    assert_eq!(
        (
            offered(&state),
            seat_lives(&state)[2],
            eliminated_seats(&state)
        ),
        ((P0, IterationCount::Fixed(4), Some(P0)), (P2, 4), vec![P1])
    );
    r6a_declare_and_accept_all(&mut state, P0, 4);
    assert_eq!(state.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
}

/// CR 704.3 + CR 704.5a + CR 104.2a: Marauding Blight-Priest with Exquisite Blood costs each
/// opponent 1 per repetition, so from a frame with both at L both reach 0 at repetition L and
/// leave in one state-based action, and P0 wins.
#[test]
fn a_drain_whose_last_opponents_fall_together_is_offered_and_won() {
    for (life, frame, count) in [(4, 3, 3), (3, 2, 2)] {
        let mut state = departure_board(
            life,
            life,
            &[Departing::BlightPriest, Departing::ExquisiteBlood],
            0,
        );
        play_to_next_offer(&mut state, |_| {});
        assert_eq!(
            (offered(&state), seat_lives(&state)[1..].to_vec()),
            (
                (P0, IterationCount::Fixed(count), Some(P0)),
                vec![(P1, frame), (P2, frame)]
            ),
            "from {life}/{life}"
        );
        r6a_declare_and_accept_all(&mut state, P0, count);
        assert_eq!(state.waiting_for, WaitingFor::GameOver { winner: Some(P0) });
    }
}

fn offered_bound(state: &GameState) -> Option<u32> {
    match &state.waiting_for {
        WaitingFor::LoopShortcut { schema, .. } => schema.measured_repetition_bound,
        other => panic!("expected a loop-shortcut offer, got {other:?}"),
    }
}

/// Takes `n` on a copy of the offer, which must leave P0 the only player (CR 104.2a), and `n + 1`
/// on another, which must hand priority back to P0 with every life where the offer found it.
fn take_wins_and_one_more_is_refused(offer: &GameState, n: u32) {
    let mut taken = offer.clone();
    r6a_declare_and_accept_all(&mut taken, P0, n);
    let mut past = offer.clone();
    r6a_declare_and_accept_all(&mut past, P0, n + 1);
    assert_eq!(
        (
            taken.waiting_for.clone(),
            eliminated_seats(&taken),
            past.waiting_for.clone(),
            seat_lives(&past),
        ),
        (
            WaitingFor::GameOver { winner: Some(P0) },
            vec![P1, P2],
            WaitingFor::Priority { player: P0 },
            seat_lives(offer),
        )
    );
}

/// CR 603.3d + CR 704.5a + CR 800.4a: Sanguine Bond's trigger aimed at P1 costs one opponent 1
/// per repetition; P1 leaves at its 2nd, after which P2 is every trigger's one legal target, so
/// from P1 2 / P2 5 the 7th repetition leaves P0 alone (CR 104.2a).
#[test]
fn a_departure_sanguine_bond_retargets_to_the_last_opponent() {
    let mut state = departure_board(3, 5, &[Departing::SanguineBond, Departing::Conqueror], 0);
    play_to_next_offer(&mut state, |_| {});
    assert_eq!(
        (offered(&state), offered_bound(&state), seat_lives(&state)),
        (
            (P0, IterationCount::Fixed(7), Some(P0)),
            Some(7),
            vec![(P0, 22), (P1, 2), (P2, 5)]
        )
    );
    take_wins_and_one_more_is_refused(&state, 7);
}

/// CR 405.5 + CR 608.1: with Vito beside Sanguine Bond each gain puts two loss triggers on the
/// stack, every aim on P1, and the top one resolves before the next pair lands on it, so a
/// repetition still costs one loss: from P1 2 / P2 5 the count is 7.
#[test]
fn a_departure_vito_aimed_at_one_opponent_counts_one_loss_per_repetition() {
    let mut state = departure_board(
        3,
        5,
        &[
            Departing::Vito,
            Departing::SanguineBond,
            Departing::Conqueror,
        ],
        0,
    );
    play_to_next_offer(&mut state, |_| {});
    assert_eq!(
        (offered(&state), offered_bound(&state), seat_lives(&state)),
        (
            (P0, IterationCount::Fixed(7), Some(P0)),
            Some(7),
            vec![(P0, 22), (P1, 2), (P2, 5)]
        )
    );
    take_wins_and_one_more_is_refused(&state, 7);
}

/// CR 405.5 + CR 608.1: Sanguine Bond's trigger resolves first, its loss gains P0 1 through
/// Exquisite Blood, and the next pair lands on Marauding Blight-Priest's trigger, which stays
/// buried: one loss per repetition, so from P1 2 / P2 9 the count is 11.
#[test]
fn a_departure_sanguine_bond_over_a_buried_blight_priest_counts_one_loss_per_repetition() {
    let mut state = departure_board(
        3,
        9,
        &[
            Departing::SanguineBond,
            Departing::BlightPriest,
            Departing::ExquisiteBlood,
        ],
        0,
    );
    play_to_next_offer(&mut state, |_| {});
    assert_eq!(
        (offered(&state), offered_bound(&state), seat_lives(&state)),
        (
            (P0, IterationCount::Fixed(11), Some(P0)),
            Some(11),
            vec![(P0, 22), (P1, 2), (P2, 9)]
        )
    );
    take_wins_and_one_more_is_refused(&state, 11);
}

/// CR 405.5 + CR 608.1: the Vito board with the "target opponent" aims alternating between the
/// living opponents charges only the trigger that resolves each repetition, so from P1 3 / P2 4
/// P1 takes repetitions 1, 3 and 5, P2 the rest, and the count is 7.
#[test]
fn a_departure_vito_with_alternating_aims_charges_only_resolved_announcements() {
    let mut state = departure_board(
        3,
        5,
        &[
            Departing::Vito,
            Departing::SanguineBond,
            Departing::Conqueror,
        ],
        0,
    );
    let mut aims = 0;
    for _ in 0..400 {
        match &state.waiting_for {
            WaitingFor::LoopShortcut { .. } | WaitingFor::GameOver { .. } => break,
            WaitingFor::TriggerTargetSelection { player, .. } => {
                let player = *player;
                let living: Vec<PlayerId> = state
                    .players
                    .iter()
                    .filter(|p| p.id != P0 && !p.is_eliminated)
                    .map(|p| p.id)
                    .collect();
                let aim = living[aims % living.len()];
                aims += 1;
                apply(
                    &mut state,
                    player,
                    GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(aim)),
                    },
                )
                .expect("P0 aims at a living opponent");
            }
            _ => {
                dump_drive_one_beat(&mut state, Some(P1)).expect("the board plays on");
            }
        }
    }
    assert!(aims > 0, "reach: P0 chose the trigger aims");
    assert_eq!(
        (offered(&state), offered_bound(&state), seat_lives(&state)),
        (
            (P0, IterationCount::Fixed(7), Some(P0)),
            Some(7),
            vec![(P0, 22), (P1, 3), (P2, 4)]
        )
    );
    take_wins_and_one_more_is_refused(&state, 7);
}
