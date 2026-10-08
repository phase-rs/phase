//! Captain America, First Avenger (MAR) — the "Throw"/"Catch" pair.
//!
//! Throw — {3}, Unattach an Equipment from Captain America, First Avenger:
//!   Captain America, First Avenger deals damage equal to that Equipment's mana
//!   value divided as you choose among one, two, or three targets.
//! Catch — At the beginning of combat on your turn, attach up to one target
//!   Equipment you control to Captain America, First Avenger.
//!
//! These drive the REAL pipeline (parser → `ActivateAbility` → target selection
//! → interactive `PayCost { UnattachFrom }` → deferred `DistributeAmong` →
//! `push_activated_ability_to_stack`) so every seam of the `UnattachFrom`
//! cost, the cost-paid-object mana-value provenance, and the Change B-1
//! activated-vs-spell resume split is covered. The tests parse the real Oracle
//! text directly so they are hermetic — independent of the generated card
//! corpus (the MAR set is released and ungated; see `set_gating.rs`).

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{
    AbilityCost, Effect, ObjectScope, QuantityExpr, QuantityRef, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CAP: &str = "Throw — {3}, Unattach an Equipment from Captain America, First Avenger: Captain America, First Avenger deals damage equal to that Equipment's mana value divided as you choose among one, two, or three targets.\nCatch — At the beginning of combat on your turn, attach up to one target Equipment you control to Captain America, First Avenger.";

fn cap_parse() -> ParsedAbilities {
    parse_oracle_text(
        CAP,
        "Captain America, First Avenger",
        &["Throw".to_string(), "Catch".to_string()],
        &["Creature".to_string()],
        &[
            "Human".to_string(),
            "Soldier".to_string(),
            "Hero".to_string(),
        ],
    )
}

/// Create an Equipment on the battlefield with mana value `mv`, attached to
/// `host` and controlled by `controller`.
fn attach_equipment(
    runner: &mut GameRunner,
    controller: engine::types::player::PlayerId,
    host: ObjectId,
    name: &str,
    mv: u32,
) -> ObjectId {
    let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
    let id = engine::game::zones::create_object(
        runner.state_mut(),
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Artifact];
    obj.card_types.subtypes = vec!["Equipment".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.mana_cost = ManaCost::Cost {
        shards: vec![],
        generic: mv,
    };
    obj.attached_to = Some(AttachTarget::Object(host));
    if let Some(h) = runner.state_mut().objects.get_mut(&host) {
        h.attachments.push(id);
    }
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());
    id
}

/// Index of Captain America's Throw activated ability.
fn throw_index(runner: &GameRunner, cap: ObjectId) -> usize {
    runner.state().objects[&cap]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::DealDamage { .. }))
        .expect("Captain America must carry a DealDamage (Throw) activated ability")
}

fn give_generic_mana(runner: &mut GameRunner, player: engine::types::player::PlayerId, n: usize) {
    for _ in 0..n {
        let _ = runner.state_mut().add_mana_to_pool(
            player,
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
        );
    }
}

/// Total floating mana in a player's pool (CR 106.4). `GameRunner` exposes no
/// pool accessor, so read it off the state directly.
fn pool_total(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Parser — the load-bearing AST shape the runtime consumes.
// ---------------------------------------------------------------------------

/// Both abilities must parse to non-`Unimplemented` AST: the Throw activated
/// ability with a `Composite[Mana, UnattachFrom]` cost and a
/// `DealDamage { ObjectManaValue { CostPaidObject } }` effect, and the Catch
/// combat trigger with an `Attach` effect.
///
/// Revert probe: without the parser branch, the cost falls to the unit
/// `Unattach` (or `Unimplemented`) and the effect's amount is not a
/// `CostPaidObject` mana-value reference — every assertion below flips.
#[test]
fn captain_america_both_abilities_parse() {
    let parsed = cap_parse();

    // Throw — activated DealDamage.
    let throw = parsed
        .abilities
        .iter()
        .find(|a| matches!(a.effect.as_ref(), Effect::DealDamage { .. }))
        .expect("Throw activated ability");

    let mut has_unattach_from = false;
    match throw.cost.as_ref().expect("Throw has a cost") {
        AbilityCost::Composite { costs } => {
            for c in costs {
                if let AbilityCost::UnattachFrom { count, .. } = c {
                    assert_eq!(*count, 1);
                    has_unattach_from = true;
                }
            }
        }
        AbilityCost::UnattachFrom { count, .. } => {
            assert_eq!(*count, 1);
            has_unattach_from = true;
        }
        other => panic!("expected Composite/UnattachFrom cost, got {other:?}"),
    }
    assert!(
        has_unattach_from,
        "Throw cost must contain an UnattachFrom leg, got {:?}",
        throw.cost
    );

    match throw.effect.as_ref() {
        Effect::DealDamage { amount, .. } => {
            assert!(
                quantity_reads_cost_paid_mana_value(amount),
                "damage amount must read the cost-paid Equipment's mana value, got {amount:?}"
            );
        }
        other => panic!("expected DealDamage, got {other:?}"),
    }

    // CR 601.2d: the divided-damage flag must be set on the activated ability so
    // the activation pipeline surfaces the DistributeAmong step.
    assert_eq!(
        throw.distribute,
        Some(engine::types::game_state::DistributionUnit::Damage),
        "Throw must carry a Damage distribute flag"
    );
    assert!(
        throw.multi_target.is_some(),
        "Throw must divide among 1-3 targets"
    );

    // Catch — combat trigger that attaches.
    assert!(
        parsed.triggers.iter().any(|t| matches!(
            t.execute.as_ref().map(|e| e.effect.as_ref()),
            Some(Effect::Attach { .. })
        )),
        "Catch must parse to a trigger whose effect is Attach, got {:?}",
        parsed.triggers
    );

    // No parse warning should be emitted for these two abilities.
    assert!(
        parsed.parse_warnings.is_empty(),
        "expected zero parse warnings, got {:?}",
        parsed.parse_warnings
    );
}

fn quantity_reads_cost_paid_mana_value(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref {
            qty:
                QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
        } => true,
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_reads_cost_paid_mana_value(inner),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Runtime — full activation pipeline.
// ---------------------------------------------------------------------------

/// Build Captain America on the battlefield with `mv`-valued Equipment attached,
/// a P1 wall to take the damage, {3} in P0's pool, and P0 holding priority.
fn setup(equip_mvs: &[u32]) -> (GameRunner, ObjectId, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let cap = scenario
        .add_creature_from_oracle(P0, "Captain America, First Avenger", 4, 4, CAP)
        .id();
    // A big P1 wall so 1-3 damage never kills it (keeps the target legal and the
    // damage observable).
    let wall = scenario.add_creature(P1, "Wall", 0, 20).id();
    let mut runner = scenario.build();

    let equips: Vec<ObjectId> = equip_mvs
        .iter()
        .enumerate()
        .map(|(i, &mv)| attach_equipment(&mut runner, P0, cap, &format!("Equip{i}"), mv))
        .collect();

    give_generic_mana(&mut runner, P0, 3);
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    (runner, cap, equips, wall)
}

/// Throw resolves onto the stack as an ActivatedAbility (not a Spell) after the
/// division is announced, and the chosen Equipment stays on the battlefield,
/// detached, while the target takes damage equal to that Equipment's mana value.
///
/// Revert probe (Change B-1): reverting the DistributeAmong split makes the
/// resume call `finalize_cast`, which pushes a `StackEntryKind::Spell` for an
/// activation — the `ActivatedAbility` assertion flips (and the cast desyncs).
#[test]
fn throw_resolves_as_activated_ability_not_spell() {
    let (mut runner, cap, equips, wall) = setup(&[3]);
    let equip = equips[0];
    let idx = throw_index(&runner, cap);

    runner
        .act(GameAction::ActivateAbility {
            source_id: cap,
            ability_index: idx,
        })
        .expect("activate Throw");

    // Target selection (1..=3 targets); choose exactly the wall.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "expected target selection, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(wall)],
        })
        .expect("select one target");

    // The Unattach cost surfaces AFTER targets are chosen.
    match runner.state().waiting_for.clone() {
        WaitingFor::PayCost {
            kind: engine::types::game_state::PayCostKind::UnattachFrom { .. },
            choices,
            count,
            min_count,
            ..
        } => {
            assert_eq!(count, 1);
            assert_eq!(min_count, 1, "unattach-from is a required cost");
            assert!(choices.contains(&equip));
        }
        other => panic!("expected PayCost UnattachFrom, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![equip] })
        .expect("unattach the equipment");

    // The deferred division re-surfaces now that the MV (=3) is known.
    match runner.state().waiting_for.clone() {
        WaitingFor::DistributeAmong { total, targets, .. } => {
            assert_eq!(
                total, 3,
                "divided total must equal the Equipment's mana value"
            );
            assert_eq!(targets, vec![TargetRef::Object(wall)]);
        }
        other => panic!("expected DistributeAmong after unattach, got {other:?}"),
    }
    runner
        .act(GameAction::DistributeAmong {
            distribution: vec![(TargetRef::Object(wall), 3)],
        })
        .expect("distribute all 3 to the wall");

    // CR 602.2b + CR 601.2d (Change B-1): the ability is on the stack as an
    // ActivatedAbility, never a Spell.
    assert_eq!(runner.state().stack.len(), 1, "one stack object");
    assert!(
        matches!(
            runner.state().stack[0].kind,
            StackEntryKind::ActivatedAbility { .. }
        ),
        "Throw must resolve as an ActivatedAbility, got {:?}",
        runner.state().stack[0].kind
    );

    // CR 601.2g: the `{3}` mana leg was actually charged. The shared `setup`
    // grants exactly {3} and P0 has no mana sources, so a fully-paid activation
    // leaves the pool empty. Revert probe: if the `{3}` leg were silently
    // skipped (Throw free), the three floating mana would remain and this flips.
    assert_eq!(
        pool_total(&runner, P0),
        0,
        "the {{3}} activation cost must consume P0's whole pool"
    );

    // CR 701.3d: the Equipment stayed on the battlefield, detached.
    let eq = &runner.state().objects[&equip];
    assert_eq!(eq.zone, Zone::Battlefield);
    assert!(eq.attached_to.is_none(), "equipment must be unattached");

    runner.advance_until_stack_empty();

    // CR 120.1: the wall took damage equal to the Equipment's mana value (3).
    assert_eq!(
        runner.state().objects[&wall].damage_marked,
        3,
        "wall must take 3 damage (the unattached Equipment's mana value)"
    );
}

/// Provenance: with two attached Equipment of differing mana value, the CHOSEN
/// one's mana value drives the divided damage — not the sibling, not the sum.
///
/// Revert probe: if the snapshot read the wrong object (or summed both), the
/// MV-1 branch would deal 3 or 4 instead of 1.
#[test]
fn chosen_equipment_mana_value_drives_damage() {
    for (pick_index, expected) in [(0usize, 3u32), (1usize, 1u32)] {
        let (mut runner, cap, equips, wall) = setup(&[3, 1]);
        let chosen = equips[pick_index];
        let idx = throw_index(&runner, cap);

        runner
            .act(GameAction::ActivateAbility {
                source_id: cap,
                ability_index: idx,
            })
            .expect("activate Throw");
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(wall)],
            })
            .expect("select one target");
        runner
            .act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .expect("unattach chosen equipment");

        match runner.state().waiting_for.clone() {
            WaitingFor::DistributeAmong { total, .. } => {
                assert_eq!(
                    total, expected,
                    "divided total must equal the CHOSEN Equipment's mana value"
                );
            }
            other => panic!("expected DistributeAmong, got {other:?}"),
        }
        runner
            .act(GameAction::DistributeAmong {
                distribution: vec![(TargetRef::Object(wall), expected)],
            })
            .expect("distribute");
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&wall].damage_marked,
            expected,
            "wall must take exactly the chosen Equipment's mana value"
        );
        // The sibling Equipment is untouched (still attached).
        let sibling = equips[1 - pick_index];
        assert!(
            runner.state().objects[&sibling].attached_to.is_some(),
            "the non-chosen Equipment must stay attached"
        );
    }
}

/// MV < announced target count: announce two targets, but the only attached
/// Equipment has mana value 1 — the activation is unpayable and nothing detaches.
///
/// Revert probe: dropping the `mana_value >= n` floor in
/// `find_eligible_unattach_for_cost_targets` makes the cost payable and the
/// activation succeed — the `Err` assertion flips and the equipment detaches.
#[test]
fn unpayable_when_mana_value_below_target_count() {
    let (mut runner, cap, equips, wall) = setup(&[1]);
    let equip = equips[0];
    let wall2 = {
        let w_card = engine::types::identifiers::CardId(runner.state().next_object_id);
        let w = engine::game::zones::create_object(
            runner.state_mut(),
            w_card,
            P1,
            "Wall2".to_string(),
            Zone::Battlefield,
        );
        let obj = runner.state_mut().objects.get_mut(&w).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.toughness = Some(20);
        w
    };
    let idx = throw_index(&runner, cap);

    runner
        .act(GameAction::ActivateAbility {
            source_id: cap,
            ability_index: idx,
        })
        .expect("activate Throw");
    // Announce TWO targets: division requires the MV to be at least 2.
    let result = runner.act(GameAction::SelectTargets {
        targets: vec![TargetRef::Object(wall), TargetRef::Object(wall2)],
    });
    assert!(
        result.is_err(),
        "announcing 2 targets with only a MV-1 Equipment must be an unpayable cost"
    );
    // CR 733: pre-commit revert — the Equipment is still attached, nothing detached.
    assert!(
        runner.state().objects[&equip].attached_to.is_some(),
        "no Equipment may be detached when the cost was unpayable"
    );
}

/// The `{3}` mana leg is a REAL cost: with only {2} floating and no mana
/// sources, a Throw activation whose non-mana legs (target, unattach) are all
/// otherwise legal cannot complete — the `{3}` leg is unpayable, so the ability
/// never reaches the stack and no damage is dealt.
///
/// Revert probe: this is the discriminating negative for the `{3}` leg. If the
/// `{3}` were dropped from the cost, this activation would finish for free —
/// the `is_err()` assertion flips and the wall takes 3 damage. Paired with
/// `throw_resolves_as_activated_ability_not_spell`'s empty-pool assertion (the
/// positive reach-guard proving the same input completes with {3} available),
/// this makes the negative non-vacuous.
#[test]
fn throw_unpayable_with_insufficient_mana() {
    let (mut runner, cap, equips, wall) = setup(&[3]);
    let equip = equips[0];
    // Reduce P0's pool from the shared {3} down to only {2} — one short of
    // Throw's {3} mana leg. P0 has no lands, so the leg cannot be funded.
    if let Some(p) = runner.state_mut().players.iter_mut().find(|p| p.id == P0) {
        p.mana_pool.clear();
    }
    give_generic_mana(&mut runner, P0, 2);
    assert_eq!(pool_total(&runner, P0), 2, "start with only {{2}} floating");

    let idx = throw_index(&runner, cap);
    runner
        .act(GameAction::ActivateAbility {
            source_id: cap,
            ability_index: idx,
        })
        .expect("activate Throw");
    // One target — MV-3 Equipment covers the division, so the unattach leg is
    // legal; only the mana leg can fail.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(wall)],
        })
        .expect("select one target");
    runner
        .act(GameAction::SelectCards { cards: vec![equip] })
        .expect("unattach the equipment");

    // The division re-surfaces (total 3). The `{3}` mana leg is paid when this
    // is announced (CR 601.2g), and there is not enough mana to pay it.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DistributeAmong { .. }
        ),
        "expected DistributeAmong before the mana leg is charged, got {:?}",
        runner.state().waiting_for
    );
    let result = runner.act(GameAction::DistributeAmong {
        distribution: vec![(TargetRef::Object(wall), 3)],
    });
    // CR 601.2h: an unpayable cost can't be paid — the activation is illegal.
    assert!(
        result.is_err(),
        "the {{3}} leg must be unpayable with only {{2}} floating"
    );
    // The ability never reached the stack and dealt no damage.
    assert_eq!(
        runner.state().stack.len(),
        0,
        "no ability on the stack when the mana leg is unpayable"
    );
    assert_eq!(
        runner.state().objects[&wall].damage_marked,
        0,
        "the wall takes no damage when Throw's mana leg is unpayable"
    );
}

// ---------------------------------------------------------------------------
// Change B-1 regression — an existing X-divide damage spell still resolves as a
// Spell. Shatterskull Smashing is an X-divide sorcery (no activation index), so
// the DistributeAmong resume must still land on `finalize_cast`.
// ---------------------------------------------------------------------------

const SHATTERSKULL: &str = "Shatterskull Smashing deals X damage divided as you choose among up to two target creatures and/or planeswalkers.";

/// Revert probe: if the Change B-1 split mis-routed spells through
/// `push_activated_ability_to_stack`, this X-divide spell would panic or push an
/// ActivatedAbility — the `Spell` assertion flips.
#[test]
fn x_divide_spell_still_finalizes_as_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let wall = scenario.add_creature(P1, "Wall", 0, 20).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Shatterskull Smashing", false, SHATTERSKULL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let card_id = engine::types::identifiers::CardId(spell.0);
    let mut runner = scenario.build();
    give_generic_mana(&mut runner, P0, 6); // {X}{R} with X=2 → pay from pool
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Shatterskull");

    // Drive through X-choice, targets, and division to the stack.
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 30, "cast pipeline did not converge");
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: 2 })
                    .expect("choose X=2");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(wall)],
                    })
                    .expect("select target");
            }
            WaitingFor::DistributeAmong { total, .. } => {
                runner
                    .act(GameAction::DistributeAmong {
                        distribution: vec![(TargetRef::Object(wall), total)],
                    })
                    .expect("distribute");
                break;
            }
            other => panic!("unexpected wait during Shatterskull cast: {other:?}"),
        }
    }

    assert_eq!(runner.state().stack.len(), 1, "one stack object");
    assert!(
        matches!(runner.state().stack[0].kind, StackEntryKind::Spell { .. }),
        "an X-divide spell must remain a Spell on the stack, got {:?}",
        runner.state().stack[0].kind
    );
}

// ---------------------------------------------------------------------------
// Catch — begin-combat trigger, your turn only, attaches to self.
// ---------------------------------------------------------------------------

/// Build Captain America (P0) with a free-floating Equipment P0 controls, with
/// `active` the active player, at the pre-combat main phase.
fn catch_setup(active: engine::types::player::PlayerId) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cap = scenario
        .add_creature_from_oracle(P0, "Captain America, First Avenger", 4, 4, CAP)
        .id();
    let mut runner = scenario.build();
    // A loose Equipment P0 controls, not attached to anything.
    let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
    let equip = engine::game::zones::create_object(
        runner.state_mut(),
        card_id,
        P0,
        "Loose Equip".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&equip).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.card_types.subtypes = vec!["Equipment".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
    }
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());
    runner.state_mut().active_player = active;
    runner.state_mut().priority_player = active;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: active };
    (runner, cap, equip)
}

/// On your turn, Catch's begin-combat trigger attaches the single legal
/// Equipment you control to Captain America itself (its `SelfRef` recipient).
///
/// Parse-path regression (this issue): parsing Captain America's Throw line —
/// "…divided as you choose among one, two, or three targets" — used to route
/// the count-list " or " through the generic `try_parse_choose_one_of_inline`
/// binary splitter, whose trial parse of the orphan "three targets" tail
/// re-entered the clause parser deeply and overflowed the test thread's stack
/// (nondeterministically, depending on stack headroom). This test drives the
/// real `add_creature_from_oracle` parse path, so a revert of the
/// `try_parse_choose_one_of_inline` distribution-clause guard re-introduces the
/// overflow at setup here.
#[test]
fn catch_attaches_equipment_to_self_on_your_turn() {
    let (mut runner, cap, equip) = catch_setup(P0);
    runner.advance_to_combat();

    // CR 603.3d + CR 601.2c: Catch targets "up to one target Equipment you
    // control" (min 0, max 1). Because "target the Equipment" is a legal
    // completion, the engine either auto-selects the single legal Equipment
    // during advancement or surfaces the optional target choice — both are
    // rules-legal, and which one happens can vary with the auto-target policy.
    // Answer the prompt if it is offered so the controller attaches the
    // Equipment either way; there is no auto-decline outcome here because
    // targeting the Equipment is always a valid completion.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
    ) {
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(equip)),
            })
            .expect("choose the Equipment as Catch's optional target");
    }
    runner.advance_until_stack_empty();

    // CR 701.3a + CR 301.5: the Equipment is attached to Captain America itself.
    assert_eq!(
        runner.state().objects[&equip].attached_to,
        Some(AttachTarget::Object(cap)),
        "Catch must attach the loose Equipment to Captain America on your turn"
    );
    assert!(
        runner.state().objects[&cap].attachments.contains(&equip),
        "Captain America must carry the attached Equipment"
    );
}

/// Catch is "up to one target Equipment" (min 0, max 1). With ZERO Equipment
/// the controller controls, the begin-combat trigger must resolve as a no-op —
/// it may target nothing — and must NOT soft-lock on a stuck target-selection
/// `WaitingFor`. The game must reach a stable, stack-empty state.
///
/// Revert probe: if the min-0 slot were mis-modeled as required-1, the trigger
/// could not resolve with 0 legal targets and `advance_until_stack_empty` would
/// leave a stuck `TriggerTargetSelection` / `TargetSelection` — the wait-state
/// and stack-empty assertions flip.
#[test]
fn catch_resolves_with_no_equipment() {
    // Captain America on P0's battlefield, but P0 controls NO Equipment.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cap = scenario
        .add_creature_from_oracle(P0, "Captain America, First Avenger", 4, 4, CAP)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner.advance_to_combat();

    // CR 115.6: an "up to one target" trigger may choose zero targets, so with
    // no legal Equipment it resolves choosing none — it must not surface a stuck
    // target-selection wait.
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
        ),
        "Catch with 0 legal Equipment must not soft-lock on target selection, got {:?}",
        runner.state().waiting_for
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().stack.len(),
        0,
        "the Catch trigger must resolve to a stable, stack-empty state"
    );
    // CR 301.5: nothing was attached — Captain America has no attachments.
    assert!(
        runner.state().objects[&cap].attachments.is_empty(),
        "Catch must attach nothing when the controller controls no Equipment"
    );
}

/// Catch's target must parse as an optional "up to one" (min 0, max 1), so a
/// player may decline. The runtime decline path itself is governed by the
/// shared trigger target-slot infrastructure (unchanged by this card).
#[test]
fn catch_target_is_optional_up_to_one() {
    let parsed = cap_parse();
    let catch = parsed
        .triggers
        .iter()
        .find_map(|t| t.execute.as_ref())
        .filter(|e| matches!(e.effect.as_ref(), Effect::Attach { .. }))
        .expect("Catch attach trigger");
    let spec = catch
        .multi_target
        .as_ref()
        .expect("Catch must carry an up-to-one multi-target spec");
    assert_eq!(
        spec.min,
        QuantityExpr::Fixed { value: 0 },
        "\"up to one\" must allow declining (min 0)"
    );
    assert_eq!(
        spec.max,
        Some(QuantityExpr::Fixed { value: 1 }),
        "\"up to one\" caps at one target"
    );
}

/// On an opponent's turn, Catch's "on your turn" trigger must not fire.
///
/// Revert probe: dropping the OnlyDuringYourTurn scope would fire the trigger on
/// P1's turn and the Equipment could attach — the unattached assertion flips.
#[test]
fn catch_does_not_trigger_on_opponents_turn() {
    let (mut runner, _cap, equip) = catch_setup(P1);
    runner.advance_to_combat();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Catch must not surface a target choice on the opponent's turn, got {:?}",
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&equip].attached_to.is_none(),
        "Catch must not attach on the opponent's turn"
    );
}
