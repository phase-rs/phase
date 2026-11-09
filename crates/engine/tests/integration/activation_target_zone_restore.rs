//! CR 601.2c + CR 109.2 + CR 109.2a: a committed target's announcement ZONE is
//! part of what the save has to state, and a save that states none is refused
//! rather than read as a battlefield permanent.
//!
//! CR 109.2 scopes a bare type description to a permanent on the battlefield;
//! CR 109.2a scopes a "card … in [zone]" description to that zone. Professor
//! Hojo's "The first activated ability you activate during your turn that
//! targets a creature you control costs {2} less to activate" is a bare-type
//! qualifier, so it reads a BATTLEFIELD permanent. Tortured Existence's "{B},
//! Discard a creature card: Return target creature card from your graveyard to
//! your hand" announces a CARD in a graveyard (CR 109.2a) and must not satisfy
//! it.
//!
//! Live play already answers that from the announced target's zone. A restored
//! save answers it from the zone the record carries, so the record's zone is
//! load-bearing evidence — and a payload that predates the field carries none.
//! Defaulting that silence to the battlefield manufactures exactly the claim
//! that makes the widest class of target-gated rules fire: the restored
//! Tortured Existence row consumes Hojo's once-per-turn discount, and the Icy
//! Manipulator activation that really does target a creature you control pays
//! full price.
//!
//! Both carriers of an activation's facts are covered, because both reach the
//! same reader one action apart: a COMPLETED activation's record is already in
//! the turn journal, and a PENDING activation's record is published to that
//! journal by `casting::take_activation_record` when payment finishes. Each case
//! is paired with a current-format control — the save this build writes states
//! the zone, so it restores and still answers CR 109.2 correctly.

use engine::game::casting::can_activate_ability_now;
use engine::game::mana_sources::activatable_mana_actions_for_player;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    ActivationTargetFact, AnnouncedTargetZone, CastPaymentMode, PayCostKind, PersistedGameState,
    PersistedRestoreError, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

/// Professor Hojo — the first sentence is the once-per-turn reducer under test.
const HOJO: &str = "The first activated ability you activate during your turn that targets a creature you control costs {2} less to activate.";

/// Tortured Existence — "{B}, Discard a creature card: Return target creature
/// card from your graveyard to your hand." The target is a CARD in a graveyard
/// (CR 109.2a), which is what a bare-type qualifier must not read.
const TORTURED_EXISTENCE: &str =
    "{B}, Discard a creature card: Return target creature card from your graveyard to your hand.";

/// Icy Manipulator — "{1}, {T}: Tap target creature." Hojo's reduction applies
/// when it targets a creature you control, so with an empty pool the activation
/// is offered at `{0}` while the discount is available and at `{1}` when it is
/// not.
const ICY_MANIPULATOR: &str = "{1}, {T}: Tap target creature.";

struct DiscountBoard {
    runner: GameRunner,
    tortured: ObjectId,
    icy: ObjectId,
    graveyard_creature: ObjectId,
    discard_creature: ObjectId,
    swamp: ObjectId,
}

/// Hojo + Tortured Existence + Icy Manipulator, one creature card in the
/// graveyard to return, one in hand to discard, and one untapped Swamp to pay
/// for it — the board where the discount is observable through Icy alone.
fn discount_board() -> DiscountBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Professor Hojo", 2, 2, HOJO);
    let icy = scenario
        .add_artifact_from_oracle(P0, "Icy Manipulator", ICY_MANIPULATOR)
        .id();
    let graveyard_creature = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();
    let discard_creature = scenario.add_creature_to_hand(P0, "Hand Bear", 2, 2).id();
    let tortured = scenario
        .add_creature(P0, "Tortured Existence", 0, 0)
        .as_enchantment()
        .from_oracle_text(TORTURED_EXISTENCE)
        .with_mana_cost(engine::types::mana::ManaCost::generic(0))
        .id();
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    DiscountBoard {
        runner: scenario.build(),
        tortured,
        icy,
        graveyard_creature,
        discard_creature,
        swamp,
    }
}

/// CR 602.2b: how far to drive the activation before handing the state back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DriveTo {
    /// Pay for the activation, let it resolve, and hand back the settled board.
    Resolved,
    /// Leave the activation parked at its own `ManaPayment` window, with its
    /// recorded facts still riding the ability — the carrier the pending half of
    /// this file is about.
    ManaPayment,
}

/// Drive the Tortured Existence activation: choose the graveyard card, discard
/// the hand card, tap the Swamp for `{B}` at whichever payment window the engine
/// opens, and then either let it resolve or stop at that window.
fn drive_tortured_existence(
    board: &mut DiscountBoard,
    payment_mode: CastPaymentMode,
    goal: DriveTo,
) {
    let DiscountBoard {
        runner,
        tortured,
        graveyard_creature,
        discard_creature,
        swamp,
        ..
    } = board;
    runner
        .act(GameAction::ActivateAbility {
            source_id: *tortured,
            ability_index: 0,
            payment_mode,
        })
        .expect("Tortured Existence must be activatable");
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(
                            *graveyard_creature,
                        )),
                    })
                    .expect("choose the graveyard card to return");
            }
            WaitingFor::PayCost {
                kind: PayCostKind::Discard,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![*discard_creature],
                    })
                    .expect("discard a creature card to pay the activation cost");
            }
            WaitingFor::ManaPayment { .. } => {
                if goal == DriveTo::ManaPayment {
                    return;
                }
                let action = activatable_mana_actions_for_player(
                    runner.state(),
                    runner
                        .state()
                        .waiting_for
                        .acting_player()
                        .expect("an acting player owns the payment"),
                )
                .into_iter()
                .find(|action| {
                    matches!(action, GameAction::TapLandForMana { selection }
                        if selection.source.object_id == *swamp)
                })
                .expect("the Swamp must expose a semantic mana action");
                runner.act(action).expect("tap the Swamp for {B}");
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay {B} from the tapped land");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other if runner.state().stack.is_empty() => {
                panic!("unexpected waiting state: {other:?}")
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
    panic!("the activation never settled");
}

/// Remove the announcement zone from every committed target fact in the
/// payload, reproducing a save written before the fact carried one. Only fact
/// payloads are touched — an object-fact `data` object is the one carrying both
/// `id` and `lki` — so `source_zone`, `origin_zone` and the zone-change
/// journal's `from_zone`/`to_zone` are left alone.
fn strip_target_fact_zones(value: &mut serde_json::Value) -> usize {
    fn walk(value: &mut serde_json::Value) -> usize {
        let mut stripped = 0;
        match value {
            serde_json::Value::Array(items) => {
                for item in items {
                    stripped += walk(item);
                }
            }
            serde_json::Value::Object(map) => {
                for (key, child) in map.iter_mut() {
                    if key == "targets" {
                        if let serde_json::Value::Array(facts) = child {
                            for fact in facts {
                                if let Some(data) = fact
                                    .get_mut("data")
                                    .and_then(serde_json::Value::as_object_mut)
                                {
                                    if data.contains_key("lki") && data.remove("zone").is_some() {
                                        stripped += 1;
                                    }
                                }
                            }
                        }
                    }
                    stripped += walk(child);
                }
            }
            _ => {}
        }
        stripped
    }
    walk(value)
}

/// Remove the announcement zone from every committed target fact of the COMPLETED
/// SPELL history — `spells_cast_this_turn_by_player` / `spells_cast_this_game_by_player`
/// — reproducing a save written before `SpellCastRecord`'s facts carried one.
///
/// Deliberately narrower than [`strip_target_fact_zones`]: it leaves the pending
/// carriers and the resolved-rules journal's replay operand byte-identical, so a
/// refusal can only be about the completed history row.
fn strip_completed_cast_history_zones(json: &mut serde_json::Value) -> usize {
    fn strip_records(records: &mut serde_json::Value) -> usize {
        let Some(records) = records.as_array_mut() else {
            return 0;
        };
        strip_facts_of(records)
    }
    fn strip_facts_of(records: &mut [serde_json::Value]) -> usize {
        let mut stripped = 0;
        for record in records.iter_mut() {
            let Some(facts) = record
                .get_mut("targets")
                .and_then(serde_json::Value::as_array_mut)
            else {
                continue;
            };
            for fact in facts.iter_mut() {
                if let Some(data) = fact
                    .get_mut("data")
                    .and_then(serde_json::Value::as_object_mut)
                {
                    if data.contains_key("lki") && data.remove("zone").is_some() {
                        stripped += 1;
                    }
                }
            }
        }
        stripped
    }
    let mut stripped = 0;
    for history in [
        "spells_cast_this_turn_by_player",
        "spells_cast_this_game_by_player",
    ] {
        let Some(by_player) = json
            .get_mut("state")
            .and_then(|state| state.get_mut(history))
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };
        for records in by_player.values_mut() {
            stripped += strip_records(records);
        }
    }
    stripped
}

fn persisted_json(state: &engine::types::game_state::GameState) -> serde_json::Value {
    serde_json::to_value(PersistedGameState::capture(state.clone())).expect("the save serializes")
}

fn admitted(
    json: serde_json::Value,
) -> Result<engine::types::game_state::GameState, PersistedRestoreError> {
    serde_json::from_value::<PersistedGameState>(json)
        .expect("the save decodes")
        .into_game_state()
}

/// CR 109.2 + CR 109.2a + CR 601.2c: the completed activation's recorded target
/// zone is what keeps the graveyard card from satisfying Hojo's battlefield
/// qualifier after a restore — and the save that states no zone is refused
/// instead of defaulted to the battlefield.
#[test]
fn a_completed_activation_without_a_target_zone_is_refused_at_restore() {
    let mut board = discount_board();
    drive_tortured_existence(&mut board, CastPaymentMode::Auto, DriveTo::Resolved);
    let DiscountBoard {
        runner,
        tortured,
        icy,
        graveyard_creature,
        swamp,
        ..
    } = &board;

    // --- Reach guards: the journal row carries the graveyard announcement, and
    // the discount is still Hojo's to give (the live activation never qualified
    // for it).
    {
        let state = runner.state();
        let rows = state
            .abilities_activated_this_turn_by_player
            .get(&P0)
            .expect("the activation must be journaled");
        assert_eq!(rows.len(), 1, "one activation this turn: {rows:?}");
        let row = &rows[0];
        assert_eq!(row.source, *tortured);
        let facts = &row.targets;
        assert_eq!(facts.len(), 1, "one committed target: {facts:?}");
        let ActivationTargetFact::Object { id, zone, .. } = &facts[0] else {
            panic!("the committed target is an object fact: {facts:?}");
        };
        assert_eq!(*id, *graveyard_creature);
        assert_eq!(
            *zone,
            AnnouncedTargetZone::Known(engine::types::zones::Zone::Graveyard),
            "CR 109.2a: Tortured Existence announces a card in a graveyard"
        );
        assert_eq!(
            state.objects[graveyard_creature].zone,
            engine::types::zones::Zone::Hand,
            "reach: the ability resolved and returned the card to hand"
        );
        assert!(
            state.players[P0.0 as usize].mana_pool.total() == 0,
            "reach: the {{B}} was spent, so the pool is empty"
        );
        assert!(
            state.objects[swamp].tapped,
            "reach: the Swamp paid, so it is not an available source"
        );
        assert!(
            can_activate_ability_now(state, P0, *icy, 0),
            "reach: the live graveyard activation did not consume Hojo's discount, so \
             Icy Manipulator targeting a creature you control is offered at {{0}}. \
             DEBUG hojo={:?} icy={:?}",
            state
                .objects
                .values()
                .filter(|object| object.name == "Professor Hojo")
                .flat_map(|object| object.static_definitions.iter_all().collect::<Vec<_>>())
                .map(|def| (&def.mode, def.affected.is_some()))
                .collect::<Vec<_>>(),
            state
                .objects
                .get(icy)
                .map(|object| (object.tapped, object.zone))
        );
    }

    // --- The current-format control: the save this build writes is admitted and
    // still answers CR 109.2 correctly.
    {
        let restored = admitted(persisted_json(runner.state()))
            .expect("a current-format save must be admitted");
        assert!(
            can_activate_ability_now(&restored, P0, *icy, 0),
            "CR 109.2: a stated graveyard zone keeps the restored row from consuming \
             Hojo's battlefield-scoped discount"
        );
    }

    // --- The legacy shape: the same save, with the announcement zone absent.
    let mut legacy = persisted_json(runner.state());
    // Two carriers hold the SAME record: the turn journal the reader scans, and
    // the resolved-rules command operand that appended it. Both lose the zone in
    // a payload that predates the field, so both are stripped here.
    assert_eq!(
        strip_target_fact_zones(&mut legacy),
        2,
        "every committed target fact must lose its zone"
    );
    match admitted(legacy) {
        Err(PersistedRestoreError::UnknownAnnouncedTargetZone { owner, target }) => {
            assert_eq!(owner, Some(*tortured));
            assert_eq!(
                target, *graveyard_creature,
                "the refusal names the target whose zone the save cannot prove"
            );
        }
        Err(other) => panic!("expected an unknown-target-zone refusal, got {other:?}"),
        Ok(state) => {
            // Demonstrated rather than asserted, so the failure names what the
            // admitted save actually does with the unstated zone. A battlefield
            // default makes the restored row Hojo's qualifying first activation
            // and Icy — the ability that really does target a creature you
            // control — stops being offered at {0}; leaving the zone
            // unanswerable instead under-grants the discount on a save that was
            // never proved. Either way the save must not have been admitted.
            let offered_at_zero = can_activate_ability_now(&state, P0, *icy, 0);
            let restored_zone = state
                .abilities_activated_this_turn_by_player
                .get(&P0)
                .and_then(|rows| rows.iter().find(|row| row.source == *tortured))
                .and_then(|row| row.targets.first())
                .and_then(|fact| match fact {
                    ActivationTargetFact::Object { zone, .. } => Some(*zone),
                    ActivationTargetFact::Player(_) => None,
                });
            panic!(
                "CR 109.2 + CR 109.2a + CR 601.2c: an activation record that states no \
                 announcement zone must be refused. This save was admitted with the restored \
                 Tortured Existence row's zone decoded as {restored_zone:?}, and Icy \
                 Manipulator's {{1}} activation targeting a creature you control was offered \
                 at {{0}}: {offered_at_zero} (true = the graveyard card was not read as a \
                 battlefield permanent; false = the unstated zone was read as one, consuming \
                 Hojo's once-per-turn discount)"
            );
        }
    }
}

/// CR 109.2 + CR 109.2a + CR 601.2c: the PENDING activation's record needs the
/// same protection. It is not in the journal yet, but
/// `casting::take_activation_record` publishes exactly this record when payment
/// finishes, so an unstated zone there becomes an unstated zone in the journal
/// one action later.
#[test]
fn a_pending_activation_without_a_target_zone_is_refused_at_restore() {
    let mut board = discount_board();
    // Manual payment parks the activation at `ManaPayment` with its record still
    // on the ability, which is the carrier under test.
    drive_tortured_existence(&mut board, CastPaymentMode::Manual, DriveTo::ManaPayment);
    let DiscountBoard {
        runner,
        tortured,
        graveyard_creature,
        ..
    } = &board;

    // Reach guards: the activation is parked, its record rides the ability, and
    // it is the record with the graveyard announcement.
    {
        let state = runner.state();
        assert!(
            matches!(state.waiting_for, WaitingFor::ManaPayment { .. }),
            "reach: a manual activation stops for its own payment, got {:?}",
            state.waiting_for
        );
        let pending = state
            .pending_cast
            .as_ref()
            .expect("the parked activation owns a pending cast");
        assert_eq!(
            pending.activation_ability_index,
            Some(0),
            "reach: the pending cast is an ACTIVATION"
        );
        let record = pending
            .ability
            .activation_record
            .as_deref()
            .expect("the announced record rides the parked ability");
        assert_eq!(record.source, *tortured);
        assert_eq!(record.targets.len(), 1, "one committed target");
        let ActivationTargetFact::Object { id, zone, .. } = &record.targets[0] else {
            panic!(
                "the committed target is an object fact: {:?}",
                record.targets
            );
        };
        assert_eq!(*id, *graveyard_creature);
        assert_eq!(
            *zone,
            AnnouncedTargetZone::Known(engine::types::zones::Zone::Graveyard),
            "reach: the parked record carries the graveyard announcement"
        );
        assert!(
            state.abilities_activated_this_turn_by_player.is_empty(),
            "reach: nothing is journaled until the activation is placed"
        );
    }

    // The current-format control: the parked save is admitted.
    assert!(
        admitted(persisted_json(runner.state())).is_ok(),
        "a current-format pending activation must stay admitted"
    );

    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_target_fact_zones(&mut legacy),
        1,
        "exactly one committed target fact must lose its zone"
    );
    match admitted(legacy) {
        Err(PersistedRestoreError::UnknownAnnouncedTargetZone { owner, target }) => {
            assert_eq!(owner, Some(*tortured));
            assert_eq!(target, *graveyard_creature);
        }
        Err(other) => panic!("expected an unknown-target-zone refusal, got {other:?}"),
        Ok(_) => panic!(
            "CR 601.2c + CR 109.2: a PENDING activation whose record states no \
             announcement zone must be refused — `take_activation_record` publishes \
             that record to the turn journal as soon as payment finishes"
        ),
    }
}

// ---------------------------------------------------------------------------
// CR 115.1 + CR 601.2c + CR 109.2: the same question for a COMPLETED SPELL
// ---------------------------------------------------------------------------

/// Head of the Class, complete printed Oracle text, both abilities — the card whose
/// first sentence is the once-per-turn gate this witness measures: "The first spell
/// you cast during each of your turns that targets a creature costs {W}{B} less to
/// cast."
const HEAD_OF_THE_CLASS: &str = "The first spell you cast during each of your turns that targets a creature costs {W}{B} less to cast. This effect reduces only the amount of colored mana you pay.\nRepartee — Whenever you cast an instant or sorcery spell that targets a creature, this creature perpetually gets +1/+1.";

/// Doom Blade — a printed `{1}{B}` instant that targets a creature, so the gate is
/// measurable as a pool delta: full `{1}{B}` spends 2, the once-per-turn reduction
/// makes it spend 1.
const DOOM_BLADE: &str = "Destroy target nonblack creature.";

fn doom_blade_cost() -> engine::types::mana::ManaCost {
    engine::types::mana::ManaCost::Cost {
        shards: vec![engine::types::mana::ManaCostShard::Black],
        generic: 1,
    }
}

/// The gate board: Head of the Class, two creature victims (the first is destroyed
/// by the earlier targeting cast, so the later one still has a legal target), two
/// Doom Blades in hand, and an empty pool.
struct GateBoard {
    runner: GameRunner,
    blade_a: ObjectId,
    blade_b: ObjectId,
    victim_a: ObjectId,
    victim_b: ObjectId,
}

fn gate_board() -> GateBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Head of the Class", 2, 2, HEAD_OF_THE_CLASS);
    let victim_a = scenario.add_vanilla(P0, 1, 1);
    let victim_b = scenario.add_vanilla(P0, 1, 1);
    let blade_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", false, DOOM_BLADE)
        .with_mana_cost(doom_blade_cost())
        .id();
    let blade_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", false, DOOM_BLADE)
        .with_mana_cost(doom_blade_cost())
        .id();
    GateBoard {
        runner: scenario.build(),
        blade_a,
        blade_b,
        victim_a,
        victim_b,
    }
}

/// Fill the pool the way the sibling gate suite does: two black and four colorless.
fn fill_gate_pool(runner: &mut GameRunner) {
    let state = runner.state_mut();
    for mana in [
        engine::types::mana::ManaType::Black,
        engine::types::mana::ManaType::Black,
        engine::types::mana::ManaType::Colorless,
        engine::types::mana::ManaType::Colorless,
        engine::types::mana::ManaType::Colorless,
        engine::types::mana::ManaType::Colorless,
    ] {
        state.players[P0.0 as usize]
            .mana_pool
            .add(engine::types::mana::ManaUnit::new(
                mana,
                ObjectId(0),
                false,
                vec![],
            ));
    }
}

/// Cast `spell` at `victim` and report the mana actually spent, which is how
/// Head of the Class's reduction is observable.
fn cast_and_measure(runner: &mut GameRunner, spell: ObjectId, victim: ObjectId) -> usize {
    let before = runner.state().players[P0.0 as usize].mana_pool.total();
    runner.cast(spell).target_objects(&[victim]).resolve();
    before - runner.state().players[P0.0 as usize].mana_pool.total()
}

/// CR 115.1 + CR 601.2c + CR 109.2: a COMPLETED cast's recorded target facts are
/// read by the same "targets a creature" gate an activation's are, so a save whose
/// completed record states a target but no announcement ZONE must be refused — not
/// admitted as an unreadable fact.
///
/// The consequence is what makes it a blocker rather than a tidiness complaint. The
/// stage this replaces asked only whether the LIST existed
/// (`reject_unknown_cast_history_provenance`). A list with an object target whose
/// zone is `Unknown` reaches `filter::declared_target_fact_matches`, where a bare
/// "targets a creature" carries `InZone { Battlefield }` (CR 109.2) and an
/// unanswerable zone axis answers `false`. The record then reads as "did not
/// qualify", the gate's zero-count comparison is satisfied a second time, and the
/// turn's NEXT creature-targeting spell is handed the same once-per-turn reduction
/// again.
///
/// So the witness measures it: if this save is admitted, the second Doom Blade pays
/// 1 instead of 2 — the failure message reports the number instead of merely
/// complaining that admission succeeded.
#[test]
fn a_completed_spell_without_a_target_zone_is_refused_at_restore() {
    let mut board = gate_board();
    fill_gate_pool(&mut board.runner);
    let first = cast_and_measure(&mut board.runner, board.blade_a, board.victim_a);
    assert_eq!(
        first, 1,
        "reach-guard: the turn's first creature-targeting spell receives the once-per-turn \
         reduction and pays the reduced {{1}}"
    );
    // The completed cast is now in the turn history, with its committed target fact.
    {
        let state = board.runner.state();
        let record = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the completed cast is recorded");
        let targets = record
            .targets
            .as_deref()
            .expect("reach: the record states its declared targets");
        let ActivationTargetFact::Object { id, zone, .. } = &targets[0] else {
            panic!("the committed target is an object fact: {targets:?}");
        };
        assert_eq!(*id, board.victim_a);
        assert_eq!(
            *zone,
            AnnouncedTargetZone::Known(engine::types::zones::Zone::Battlefield),
            "reach: the completed record carries the battlefield announcement"
        );
    }

    // The control: the save this build wrote is admitted, and the restored ordinal
    // counts the earlier cast, so the second targeting spell pays FULL price.
    let control = admitted(persisted_json(board.runner.state()))
        .expect("a current-format completed-cast history must stay admitted");
    let mut control_board = GateBoard {
        runner: GameRunner::from_state(control),
        blade_a: board.blade_a,
        blade_b: board.blade_b,
        victim_a: board.victim_a,
        victim_b: board.victim_b,
    };
    fill_gate_pool(&mut control_board.runner);
    let second = cast_and_measure(
        &mut control_board.runner,
        control_board.blade_b,
        control_board.victim_b,
    );
    assert_eq!(
        second, 2,
        "CR 115.9: the restored history still counts the earlier creature-targeting cast, so the \
         turn's reduction is NOT granted twice — the second one pays full {{1}}{{B}}"
    );

    // The witness: the same save, with the COMPLETED history row's committed target
    // facts stripped of their zone (nothing else in the payload is touched).
    let mut legacy = persisted_json(board.runner.state());
    assert_eq!(
        strip_completed_cast_history_zones(&mut legacy),
        2,
        "reach-guard: one completed cast, recorded once in the turn history and once in the game \
         history — both committed target facts lose their zone"
    );
    match admitted(legacy) {
        Err(PersistedRestoreError::UnknownAnnouncedTargetZone { owner, target }) => {
            assert_eq!(
                owner,
                Some(board.blade_a),
                "the refusal names the spell whose record cannot be read"
            );
            assert_eq!(target, board.victim_a);
        }
        Err(other) => panic!("expected an unknown-target-zone refusal, got {other:?}"),
        Ok(state) => {
            // Only reachable with the guard missing: measure the consequence so the
            // failure names it rather than only reporting the admission.
            let mut probe = GateBoard {
                runner: GameRunner::from_state(state),
                blade_a: board.blade_a,
                blade_b: board.blade_b,
                victim_a: board.victim_a,
                victim_b: board.victim_b,
            };
            fill_gate_pool(&mut probe.runner);
            let spent = cast_and_measure(&mut probe.runner, probe.blade_b, probe.victim_b);
            panic!(
                "CR 115.1 + CR 601.2c + CR 109.2: a COMPLETED cast whose target list exists but \
                 states no announcement zone must be refused. Admitted, the unreadable fact fails \
                 the creature qualifier, the gate counts zero, and the turn's reduction is granted \
                 a second time: the next targeting spell paid {spent}, expected 2"
            );
        }
    }
}
