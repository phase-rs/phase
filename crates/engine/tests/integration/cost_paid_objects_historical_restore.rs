//! Restoring a game saved BEFORE `ResolvedAbility::cost_paid_object_ids`
//! (`Vec<ObjectId>`) became `cost_paid_objects` (`Vec<CostPaidObjectRecord>`).
//!
//! The historical wire form of that array was a list of BARE ids — pure
//! storage-identity membership, with no last-known characteristics and no
//! incarnation epoch. The collection now has two consumers that need different
//! amounts of authority from it, so a historical record can neither be dropped
//! nor promoted:
//!
//!   * `exclude_cost_paid_object_that_left_battlefield`
//!     (`game/ability_utils.rs`) needs STORAGE identity only — "this object left
//!     the battlefield to pay this spell's own cost, so it was never a legal
//!     target under CR 601.2c/CR 602.2b's real target-before-cost order". A bare
//!     id answers that question completely. Dropping historical records instead
//!     would silently un-exclude every object a multi-object cost paid but one,
//!     because the singular `cost_paid_object` fallback names at most one.
//!   * `ZoneChoiceCandidateSource::CostPaidObjects`
//!     (`game/effects/choose_from_zone.rs`) resolves the record to a LIVE card
//!     it offers to a player. CR 400.7 makes a cost-exiled card that left exile
//!     and came back a NEW object the reference must no longer name, and the
//!     engine reuses `ObjectId` across zone changes, so a bare id cannot answer
//!     that question at all. A historical record must fail CLOSED there rather
//!     than invent an incarnation it never carried.
//!
//! `CostPaidObjectRecord::MembershipOnly` is exactly that distinction. Both
//! tests below drive it through the production `PersistedGameState` boundary
//! with a hand-authored historical payload and then through the production
//! continuation that consumes it — a current-schema round trip cannot
//! discriminate either loss, because it would restore full snapshots on both
//! sides of the save.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, PayCostKind, PersistedGameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// CR 702.153a: Casualty's optional additional cost is decided and PAID before
/// targets are declared (CR 601.2b + CR 601.2f-h), so the sacrificed creature is
/// already in the graveyard this spell searches when its target candidates are
/// built. Two target instructions, so answering the first slot forces the engine
/// to rebuild the second one from live state — the production continuation this
/// test drives after restore.
const CASUALTY_RECURSION: &str = "Casualty 1\nReturn target creature card from your graveyard to your hand. Return target creature card from your graveyard to your hand.";

/// Verbatim Oracle text (engine card-data export) — the
/// `ZoneChoiceCandidateSource::CostPaidObjects` card.
const COIN_OF_FATE: &str = "When this artifact enters, surveil 1.\n{3}{W}, {T}, Exile two creature cards from your graveyard, Sacrifice this artifact: An opponent chooses one of the exiled cards. You put that card on the bottom of your library and return the other to the battlefield tapped. You become the monarch.";

/// Rewrite every persisted cost-payment record array into the PRE-MIGRATION
/// wire shape: the historical `cost_paid_object_ids` key holding bare
/// `ObjectId`s (`ObjectId` is `#[serde(transparent)]` over `u64`, so a
/// historical element was a plain JSON number).
///
/// `also_paid` is appended to each rewritten record as additional bare ids. A
/// historical save could record any number of objects for one cost — that is
/// precisely the shape whose loss this file is about — and no live payment seam
/// in the current build writes such a payload at this window, so the multi-id
/// record is authored here rather than round-tripped out of a current value.
/// Re-serializing a current-schema value and calling it historical would
/// restore snapshots on both sides and discriminate nothing.
///
/// Keyed on BOTH field names so the payload stays historical no matter which
/// key the current schema serializes under — that keeps a revert experiment on
/// the migration honest.
///
/// Returns the LARGEST historical record it wrote, in ids.
fn downgrade_cost_payment_records_to_historical_ids(
    value: &mut serde_json::Value,
    also_paid: &[ObjectId],
) -> usize {
    let mut widest = 0;
    match value {
        serde_json::Value::Object(map) => {
            let recorded = ["cost_paid_objects", "cost_paid_object_ids"]
                .iter()
                .filter_map(|key| map.remove(*key))
                .find_map(|entry| match entry {
                    serde_json::Value::Array(elements) => Some(elements),
                    _ => None,
                });
            if let Some(elements) = recorded {
                let mut ids: Vec<serde_json::Value> = elements
                    .into_iter()
                    .map(|element| match element {
                        serde_json::Value::Object(record) => record
                            .get("object_id")
                            .cloned()
                            .expect("a current-schema payment record carries its object_id"),
                        bare => bare,
                    })
                    .collect();
                for extra in also_paid.iter().map(|id| serde_json::Value::from(id.0)) {
                    if !ids.contains(&extra) {
                        ids.push(extra);
                    }
                }
                widest = widest.max(ids.len());
                map.insert(
                    "cost_paid_object_ids".to_string(),
                    serde_json::Value::Array(ids),
                );
            }
            for nested in map.values_mut() {
                widest = widest.max(downgrade_cost_payment_records_to_historical_ids(
                    nested, also_paid,
                ));
            }
        }
        serde_json::Value::Array(elements) => {
            for element in elements.iter_mut() {
                widest = widest.max(downgrade_cost_payment_records_to_historical_ids(
                    element, also_paid,
                ));
            }
        }
        _ => {}
    }
    widest
}

/// Save the live game, downgrade its cost-payment records to the historical
/// bare-id shape, and restore it through the production `PersistedGameState`
/// boundary — not a bare `serde_json::from_str::<ResolvedAbility>`, which would
/// skip the hand-written persisted codec the real load path uses.
///
/// `min_ids` is a reach guard: the payload must really carry a NONEMPTY
/// MULTI-id historical record, or everything downstream is vacuous.
fn save_as_historical_and_restore(
    runner: GameRunner,
    also_paid: &[ObjectId],
    min_ids: usize,
) -> GameRunner {
    let mut saved = serde_json::to_value(PersistedGameState::capture(runner.state().clone()))
        .expect("the paused state serializes through the authoritative persistence envelope");
    let current = saved.clone();

    let widest = downgrade_cost_payment_records_to_historical_ids(&mut saved, also_paid);
    assert!(
        widest >= min_ids,
        "reach guard: the payload must carry a historical record of at least {min_ids} cost \
         payments — a single-id or empty record cannot discriminate this loss, widest={widest}"
    );
    assert_ne!(
        saved, current,
        "reach guard: the persisted payload must actually have been rewritten into the \
         historical shape, or this test asserts nothing"
    );

    GameRunner::from_state(
        serde_json::from_value::<PersistedGameState>(saved)
            .expect("a historical save must still deserialize at the persisted boundary")
            .into_game_state()
            .expect("a historical save must satisfy the checked restore contract"),
    )
}

/// Answer whatever the engine raises until the stack is empty, preferring the
/// supplied targets. Deliberately tolerant: Casualty's copy trigger raises its
/// own windows, and none of them are what this test pins.
fn finish_resolution(runner: &mut GameRunner, prefer: &[ObjectId]) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                let pick = prefer
                    .iter()
                    .map(|id| TargetRef::Object(*id))
                    .find(|target| selection.current_legal_targets.contains(target))
                    .or_else(|| selection.current_legal_targets.first().cloned());
                if runner
                    .act(GameAction::ChooseTarget { target: pick })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                if runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// CR 601.2h + CR 602.2b (issue #4948 / issue #1301): a game saved with a
/// MULTI-object cost payment already recorded, in the historical bare-id shape,
/// must still exclude EVERY object that cost consumed from this same spell's
/// own target candidates after restore.
///
/// The seam is Casualty (CR 702.153a): CR 601.2b makes its optional additional
/// cost a pre-target decision, so the sacrifice is paid BEFORE targets are
/// declared and the sacrificed creature lands in the very graveyard this spell
/// searches. The save is taken at the first target slot — cost recorded, later
/// slots not yet built — and the restored game then walks the production
/// continuation (`GameAction::ChooseTarget` rebuilds the next slot's
/// candidates), which is the path
/// `exclude_cost_paid_object_that_left_battlefield` feeds.
///
/// The historical record carries TWO ids: the creature this build's Casualty
/// really sacrificed, plus the second object the same historical cost consumed.
/// One id would not discriminate the loss — the singular `cost_paid_object`
/// referent already names one object by itself, and it is exactly the objects
/// BEYOND the first that a dropped record silently un-excludes.
#[test]
fn historical_multi_id_cost_record_still_excludes_every_paid_object_after_restore() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Three pre-existing creature cards in the graveyard: two are returned by
    // the spell's two slots, and the third stands in for the second object the
    // historical cost consumed.
    let bear_one = scenario
        .add_creature_to_graveyard(P0, "First Graveyard Bear", 2, 2)
        .id();
    let bear_two = scenario
        .add_creature_to_graveyard(P0, "Second Graveyard Bear", 2, 2)
        .id();
    let historically_paid = scenario
        .add_creature_to_graveyard(P0, "Historically Paid Bear", 2, 2)
        .id();
    let fodder = scenario.add_creature(P0, "Casualty Fodder", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Recursion", false, CASUALTY_RECURSION)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce the Casualty spell");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "CR 601.2b: Casualty is decided before targets, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("accept the optional additional cost");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ),
        "the accepted Casualty cost must raise its sacrifice window, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("sacrifice the creature to pay the additional cost");

    let WaitingFor::TargetSelection { selection, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "targets are declared after this cost, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        selection.current_slot, 0,
        "the save is taken at the first slot, before the later slots are built"
    );
    // Positive reach guards FIRST: the card the historical record will name is
    // really a candidate in the graveyard this spell searches, and the
    // sacrificed creature really left the battlefield — so the exclusions after
    // restore are identity decisions, not empty-zone accidents.
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(historically_paid)),
        "reach guard: {historically_paid:?} must be an ordinary candidate before the historical \
         payment record names it, got {:?}",
        selection.current_legal_targets
    );
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "reach guard: the sacrificed creature must really have left the battlefield"
    );

    // The save/restore under test: a historical TWO-id record through the
    // production persisted boundary.
    let mut runner = save_as_historical_and_restore(runner, &[historically_paid], 2);

    let WaitingFor::TargetSelection { selection, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the restored game resumes at its target declaration, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        selection.current_slot, 0,
        "the restored game resumes on the same unanswered slot"
    );

    // Drive the production continuation: answering slot 0 rebuilds slot 1's
    // candidates from live state and the restored payment record.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear_one)),
        })
        .expect("answer the first target slot on the restored game");

    let WaitingFor::TargetSelection { selection, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "the second target slot must open after the first is answered, got {:?}",
            runner.state().waiting_for
        );
    };
    let rebuilt = selection.current_legal_targets.clone();
    assert!(
        rebuilt.contains(&TargetRef::Object(bear_two)),
        "reach guard: an untouched graveyard creature must still be offered, or the exclusion \
         below is a vacuous empty pool; rebuilt={rebuilt:?}"
    );
    assert!(
        !rebuilt.contains(&TargetRef::Object(historically_paid)),
        "CR 601.2h + CR 602.2b: every object the historical record names as paid for THIS \
         spell's own cost must stay excluded from its own target candidates across a restore — \
         a bare id answers that question completely, so a historical record must not be \
         dropped; rebuilt={rebuilt:?}"
    );
    assert!(
        !rebuilt.contains(&TargetRef::Object(fodder)),
        "the creature actually sacrificed to the same cost stays excluded too; rebuilt={rebuilt:?}"
    );
    assert!(
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(historically_paid)),
            })
            .is_err(),
        "the production action must refuse a historically-paid object as a target"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear_two)),
        })
        .expect("answer the rebuilt slot with an untouched graveyard creature");
    finish_resolution(&mut runner, &[bear_one, bear_two]);

    // Observable outcome: the two untouched cards came back and the
    // historically-paid one never could.
    for returned in [bear_one, bear_two] {
        assert_eq!(
            runner.state().objects[&returned].zone,
            Zone::Hand,
            "the spell must actually have returned {returned:?} to hand"
        );
    }
    assert_eq!(
        runner.state().objects[&historically_paid].zone,
        Zone::Graveyard,
        "an object the historical record names as paid for this spell's own cost must never be \
         returned by that same spell"
    );
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the sacrificed creature stays in the graveyard too"
    );
}

/// CR 400.7: the paired fail-closed half. The SAME historical record that is
/// still good enough for target exclusion above must NOT be offered as a live
/// card by `ZoneChoiceCandidateSource::CostPaidObjects` — a bare id cannot prove
/// which incarnation the cost exiled, and the engine reuses `ObjectId` across
/// zone changes, so offering it would risk naming a new object at a reused id.
///
/// CR 609.3 + CR 608.2c: an empty eligible pool is not a stall. The ability must
/// still resolve as far as it can, which is what the monarch assertion pins.
#[test]
fn historical_cost_paid_ids_are_never_offered_as_live_choice_candidates() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // {3}{W}: four white mana covers the white pip and the three generic.
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );
    let coin = scenario
        .add_artifact_from_oracle(P0, "Coin of Fate", COIN_OF_FATE)
        .id();
    let grave_a = scenario
        .add_creature_to_graveyard(P0, "Graveyard Creature A", 2, 2)
        .id();
    let grave_b = scenario
        .add_creature_to_graveyard(P0, "Graveyard Creature B", 3, 3)
        .id();
    scenario.add_card_to_library_top(P0, "Library Filler");

    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&coin]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .expect("Coin of Fate must carry an activated ability with a cost");
    runner
        .act(GameAction::ActivateAbility {
            source_id: coin,
            ability_index,
        })
        .expect("Coin's ability must be activatable with the cost available");

    // Answer every cost window the engine raises, then save and restore at the
    // priority seam after payment and before resolution.
    let mut restored = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost { choices, count, .. } => {
                let mut selection: Vec<ObjectId> = [grave_a, grave_b]
                    .into_iter()
                    .filter(|id| choices.contains(id))
                    .collect();
                if selection.len() != count {
                    selection = choices.iter().copied().take(count).collect();
                }
                runner
                    .act(GameAction::SelectCards { cards: selection })
                    .expect("cost payment must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("the mana cost must finalize from the floating pool");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() && !restored => {
                for exiled in [grave_a, grave_b] {
                    assert_eq!(
                        runner.state().objects[&exiled].zone,
                        Zone::Exile,
                        "reach guard: the cost must have exiled {exiled:?} before the save, or \
                         the empty pool below is an empty-zone accident"
                    );
                }
                runner = save_as_historical_and_restore(runner, &[], 2);
                restored = true;
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => panic!(
                "CR 400.7: a historical bare-id payment record carries no incarnation authority, \
                 so it must never be offered as a live card — {player:?} was offered {cards:?}"
            ),
            _ => break,
        }
    }

    assert!(
        restored,
        "reach guard: the historical save/restore must have run, or this test asserts nothing"
    );

    let state = runner.state();
    // Positive reach guard FIRST: the ability really did resolve.
    assert_eq!(
        state.monarch,
        Some(P0),
        "CR 608.2c + CR 609.3 + CR 725.1: an empty candidate pool skips the halves it cannot \
         name, but 'You become the monarch' still happens"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
        "a fail-closed pool must not strand an unanswerable choice window, got {:?}",
        state.waiting_for
    );
    for exiled in [grave_a, grave_b] {
        assert_eq!(
            state.objects[&exiled].zone,
            Zone::Exile,
            "CR 400.7: {exiled:?} is named only by a membership-only historical record, so \
             neither 'that card' nor 'the other' may move it"
        );
    }
    assert!(
        !state
            .battlefield
            .iter()
            .any(|id| *id == grave_a || *id == grave_b),
        "CR 609.3: with no eligible candidate, nothing is returned to the battlefield"
    );
}
