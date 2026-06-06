//! Integration coverage for Frostcliff Siege (Tarkir: Dragonstorm, {1}{U}{R}).
//!
//! Oracle text:
//!   "As this enchantment enters, choose Jeskai or Temur.
//!    • Jeskai — Whenever one or more creatures you control deal combat damage
//!      to a player, draw a card.
//!    • Temur — Creatures you control get +1/+0 and have trample and haste."
//!
//! Exercises the CR 614.12c + CR 607.2d anchor-word linked-ability machinery
//! end-to-end. The parser lowers the card to:
//!   - one `Moved` `ReplacementDefinition` whose execute is
//!     `Effect::Choose { ChoiceType::Labeled["Jeskai","Temur"], persist: true }`
//!     so the controller's answer is persisted on the Siege as
//!     `ChosenAttribute::Label`;
//!   - one `TriggerDefinition` (Jeskai mode) gated by
//!     `TriggerCondition::ChosenLabelIs { label: "Jeskai" }`;
//!   - one `StaticDefinition` (Temur mode) gated by
//!     `StaticCondition::ChosenLabelIs { label: "Temur" }`.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 614.12c: "Some replacement effects cause a permanent to enter the
//!     battlefield with its controller's choice of one of two abilities, each
//!     marked with an anchor word and preceded by a bullet point." — the
//!     authorising rule for the as-enters anchor-word choice and the linked
//!     abilities it controls.
//!   - CR 607.2d: Linked abilities — when an object has an ability that
//!     causes a player to "choose a [value]" and an ability that refers to
//!     "the chosen [value]," those abilities are linked. The anchor-word
//!     bulleted ability is the chooser-side; the named-label-marked ability
//!     is the chosen-side.
//!   - CR 113.6 + CR 113.6b: Static abilities function only on the battlefield
//!     (default for both the chosen-label gate and the granted continuous
//!     effects).
//!   - CR 603.2 + CR 603.4: A triggered ability triggers when the event
//!     occurs; its intervening-if is checked at fire-time AND resolution-time
//!     (CR 603.4) — `ChosenLabelIs` returns false on the Temur reading so the
//!     Jeskai trigger never fires when Temur was chosen.
//!   - CR 510.1b: A creature deals combat damage equal to its power.
//!   - CR 121.1: A draw moves the top card from the library to the player's
//!     hand.
//!   - CR 613.4c (P/T modification) + CR 613.1f (keyword grant): The Temur
//!     static contributes `AddPower(+1)` in layer 7c and `AddKeyword(Trample)`
//!     / `AddKeyword(Haste)` in layer 6.
//!
//! The tests below load Frostcliff Siege from the real `card-data.json`
//! database (so any parser regression in the as-enters anchor-word lowering
//! breaks integration coverage) and then drive each mode through the real
//! `apply()` pipeline.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChoiceType, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use super::rules::run_combat;

use crate::support::shared_card_db as load_db;

/// Drive the as-enters labeled choice for an already-placed Frostcliff Siege
/// through the real `apply()` pipeline so `ChosenAttribute::Label(<chosen>)`
/// is persisted via `ChooseOption` → `ChosenAttribute::from_choice` →
/// `ChoiceValue::Label` → `ChosenAttribute::Label` (the path we extended).
/// Poking `chosen_attributes` directly would bypass the very mapping under
/// test.
///
/// `add_real_card` to Battlefield (`scenario_db.rs`) bypasses the
/// zone-change pipeline, so the as-enters replacement doesn't fire on its
/// own — we drive the prompt manually here while still routing the answer
/// through the production resolution choice handler.
fn drive_siege_choice(
    runner: &mut engine::game::scenario::GameRunner,
    siege: ObjectId,
    chosen_label: &str,
) {
    runner.state_mut().waiting_for = WaitingFor::NamedChoice {
        player: P0,
        choice_type: ChoiceType::Labeled {
            options: vec!["Jeskai".to_string(), "Temur".to_string()],
        },
        options: vec!["Jeskai".to_string(), "Temur".to_string()],
        source_id: Some(siege),
    };
    runner
        .act(GameAction::ChooseOption {
            choice: chosen_label.to_string(),
        })
        .unwrap_or_else(|_| panic!("ChooseOption({chosen_label}) must resolve"));

    // CR 614.12c persistence invariant — the chosen label must land on the
    // Siege's `chosen_attributes` as `ChosenAttribute::Label`.
    assert_eq!(
        runner.state().objects[&siege]
            .chosen_label()
            .map(str::to_string),
        Some(chosen_label.to_string()),
        "ChooseOption({chosen_label}) must persist as ChosenAttribute::Label"
    );
}

/// Jeskai mode end-to-end: chose Jeskai, a creature P0 controls deals combat
/// damage to P1, the trigger fires, and P0 draws a card.
///
/// Discriminator: P0's hand and library sizes both bracket the draw. The
/// hand grows by exactly one and the library shrinks by exactly one, which
/// is impossible without the Jeskai trigger actually firing AND its
/// intervening-if (`ChosenLabelIs("Jeskai")`) returning true. If the Temur
/// gate were active instead, neither would change.
#[test]
fn jeskai_mode_draws_when_creature_deals_combat_damage() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege = scenario.add_real_card(P0, "Frostcliff Siege", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    // Library cards so the draw has something to fetch — also gives us
    // a discriminator on library size.
    let _fodder1 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let _fodder2 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let mut runner = scenario.build();

    drive_siege_choice(&mut runner, siege, "Jeskai");

    // Snapshot before-combat hand/library sizes so the after-combat
    // assertion is the unambiguous +1/-1 expected from `Draw 1`.
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let library_before = runner.state().players[P0.0 as usize].library.len();

    // CR 510.1b — Grizzly Bears (2/2) attacks P1 unblocked → 2 combat
    // damage to P1 → `GameEvent::CombatDamageDealtToPlayer` → the Jeskai
    // trigger's `DamageDoneOnceByController` matcher fires → intervening-if
    // `ChosenLabelIs("Jeskai")` returns true → `Draw 1` resolves.
    run_combat(&mut runner, vec![bear], vec![]);
    runner.advance_until_stack_empty();

    let hand_after = runner.state().players[P0.0 as usize].hand.len();
    let library_after = runner.state().players[P0.0 as usize].library.len();

    assert_eq!(
        hand_after,
        hand_before + 1,
        "Jeskai mode: combat damage to a player must trigger Draw 1 — \
         hand size must grow by exactly one (before={hand_before}, after={hand_after})"
    );
    assert_eq!(
        library_after,
        library_before - 1,
        "Jeskai mode: Draw 1 must remove one card from the library \
         (before={library_before}, after={library_after})"
    );
    assert_eq!(
        runner.life(P1),
        20 - 2,
        "Grizzly Bears (2/2) deals 2 combat damage to P1; life total drops by 2"
    );
}

/// Negative companion to the Jeskai test: choosing Temur must NOT fire the
/// Jeskai draw trigger. Same combat shape, same precondition snapshots — the
/// only difference is the choice, and the only allowed outcome is that hand
/// and library sizes stay unchanged.
///
/// This is the load-bearing assertion for `TriggerCondition::ChosenLabelIs`:
/// without the intervening-if check, the parser shape alone wouldn't
/// distinguish the two modes — both linked abilities live on the same Siege
/// object, and the matcher would fire for both.
#[test]
fn temur_mode_does_not_fire_jeskai_draw_trigger() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege = scenario.add_real_card(P0, "Frostcliff Siege", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let _f1 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let _f2 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let mut runner = scenario.build();

    drive_siege_choice(&mut runner, siege, "Temur");

    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let library_before = runner.state().players[P0.0 as usize].library.len();

    run_combat(&mut runner, vec![bear], vec![]);
    runner.advance_until_stack_empty();

    let hand_after = runner.state().players[P0.0 as usize].hand.len();
    let library_after = runner.state().players[P0.0 as usize].library.len();

    assert_eq!(
        hand_after, hand_before,
        "Temur mode: the Jeskai draw trigger MUST NOT fire — hand size unchanged"
    );
    assert_eq!(
        library_after, library_before,
        "Temur mode: no draw means library unchanged"
    );
}

/// Temur mode end-to-end: choosing Temur must grant +1/+0, trample, and haste
/// to each creature P0 controls. Verified by recomputing the layer system
/// (Step 6 keyword grants and Step 7c P/T modification) against a vanilla
/// 2/2 Grizzly Bears.
#[test]
fn temur_mode_grants_plus_one_zero_trample_haste_to_creatures_you_control() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege = scenario.add_real_card(P0, "Frostcliff Siege", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();

    // Sanity baseline — Grizzly Bears is a 2/2 with no keywords printed.
    {
        let s = runner.state_mut();
        evaluate_layers(s);
    }
    let bear_obj = &runner.state().objects[&bear];
    assert_eq!(bear_obj.power, Some(2), "baseline Grizzly Bears power is 2");
    assert_eq!(
        bear_obj.toughness,
        Some(2),
        "baseline Grizzly Bears toughness is 2"
    );
    assert!(
        !bear_obj
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Trample)),
        "baseline Grizzly Bears has no Trample"
    );
    assert!(
        !bear_obj
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Haste)),
        "baseline Grizzly Bears has no Haste"
    );

    // CR 614.12c — choose Temur via the real `apply()` pipeline so the
    // chosen label persists onto the Siege.
    drive_siege_choice(&mut runner, siege, "Temur");

    // Re-evaluate layers — `ChooseOption`'s post-state may not have triggered
    // a layers-dirty bump on its own; the test re-runs the layer pipeline
    // explicitly so we read post-choice characteristics regardless.
    {
        let s = runner.state_mut();
        s.layers_dirty.mark_full();
        evaluate_layers(s);
    }

    let bear_obj = &runner.state().objects[&bear];
    // CR 613.4c (layer 7c): `AddPower(+1)` from the Temur static.
    assert_eq!(
        bear_obj.power,
        Some(3),
        "Temur mode: Grizzly Bears must gain +1/+0 → power 3"
    );
    // `AddToughness(+0)` is a no-op contribution (the static lists +1/+0;
    // toughness contribution is zero by design — verify toughness is unchanged).
    assert_eq!(
        bear_obj.toughness,
        Some(2),
        "Temur mode: +1/+0 leaves toughness at 2 (the +0 contribution is a no-op)"
    );
    // CR 613.1f (layer 6): `AddKeyword(Trample)` + `AddKeyword(Haste)` from
    // the Temur static.
    assert!(
        bear_obj
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Trample)),
        "Temur mode: Grizzly Bears must gain Trample"
    );
    assert!(
        bear_obj
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Haste)),
        "Temur mode: Grizzly Bears must gain Haste"
    );
}

/// CR 614.12c + per the printed Frostcliff Siege ruling
/// ("If you somehow control Frostcliff Siege and no choice was made for it
/// (perhaps because another permanent on the battlefield became a copy of
/// it), it has neither of the two abilities."): a Siege with no persisted
/// `ChosenAttribute::Label` must have neither linked ability functioning.
/// Verified by the absence of both effects on the same combat shape used by
/// the Jeskai positive test.
///
/// This is the load-bearing assertion for `is_some_and` on the
/// `chosen_label()` lookup in `evaluate_condition_with_context` /
/// `check_trigger_condition` — without that gate, a copy of a Siege would
/// fire both abilities (or neither, depending on default semantics) instead
/// of the rules-correct "neither".
#[test]
fn no_choice_persisted_means_neither_linked_ability_functions() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege = scenario.add_real_card(P0, "Frostcliff Siege", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let _f1 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let _f2 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let mut runner = scenario.build();

    // CR 614.12c precondition: NO `ChosenAttribute::Label` on the Siege
    // (simulating a copied/cloned permanent that never made the as-enters
    // choice).
    assert!(
        runner.state().objects[&siege].chosen_label().is_none(),
        "precondition: a freshly-placed Siege with no driven choice has no chosen label"
    );

    // Re-evaluate layers so the Temur static would apply if its gate were
    // permissive when the label is absent.
    {
        let s = runner.state_mut();
        s.layers_dirty.mark_full();
        evaluate_layers(s);
    }

    let bear_obj = &runner.state().objects[&bear];
    assert_eq!(
        bear_obj.power,
        Some(2),
        "no-choice: Temur anthem must NOT apply — power stays at 2"
    );
    assert!(
        !bear_obj
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Trample)),
        "no-choice: Temur anthem must NOT grant Trample"
    );

    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let library_before = runner.state().players[P0.0 as usize].library.len();

    // Drive combat — the Jeskai trigger's matcher fires on combat damage,
    // but its intervening-if `ChosenLabelIs("Jeskai")` returns false
    // because no label is persisted, so the ability triggers nothing.
    run_combat(&mut runner, vec![bear], vec![]);
    runner.advance_until_stack_empty();

    let hand_after = runner.state().players[P0.0 as usize].hand.len();
    let library_after = runner.state().players[P0.0 as usize].library.len();

    assert_eq!(
        hand_after, hand_before,
        "no-choice: Jeskai draw trigger must NOT fire — hand unchanged"
    );
    assert_eq!(
        library_after, library_before,
        "no-choice: no draw means library unchanged"
    );
}

/// Negative companion to the Temur layer assertion: choosing Jeskai must
/// leave creatures you control with their unmodified base characteristics —
/// no +1/+0, no Trample, no Haste. Without `StaticCondition::ChosenLabelIs`
/// gating the Temur static, this assertion would fail symmetrically with
/// the Jeskai-side "no draw on Temur" test.
#[test]
fn jeskai_mode_does_not_grant_temur_anthem() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege = scenario.add_real_card(P0, "Frostcliff Siege", Zone::Battlefield, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();

    drive_siege_choice(&mut runner, siege, "Jeskai");

    {
        let s = runner.state_mut();
        s.layers_dirty.mark_full();
        evaluate_layers(s);
    }

    let bear_obj = &runner.state().objects[&bear];
    assert_eq!(
        bear_obj.power,
        Some(2),
        "Jeskai mode: Temur anthem must NOT apply — power stays at 2"
    );
    assert!(
        !bear_obj
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Trample)),
        "Jeskai mode: Temur anthem must NOT apply — no Trample"
    );
    assert!(
        !bear_obj
            .keywords
            .iter()
            .any(|k| matches!(k, Keyword::Haste)),
        "Jeskai mode: Temur anthem must NOT apply — no Haste"
    );
}

/// Fund `player`'s mana pool with the given `ManaType`s. Mirrors the local
/// `add_mana` helper used by other integration tests; reproduced here to keep
/// this test module self-contained.
fn add_mana_to(runner: &mut GameRunner, player: PlayerId, mana: &[ManaType]) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

/// Cast Frostcliff Siege from `P0`'s hand through the real `CastSpell` → stack
/// → resolution → as-enters replacement pipeline, then drive the resulting
/// `WaitingFor::NamedChoice` with `chosen_label`. Returns the (still-stable)
/// `ObjectId` of the Siege so callers can read its `chosen_attributes` /
/// continuous-modification contribution after ETB.
///
/// Unlike `drive_siege_choice`, which mutates `waiting_for` directly for tests
/// that need a pre-placed battlefield permanent, this helper exercises the
/// full chain:
///   - `GameAction::CastSpell` (CR 601 cast steps),
///   - the Siege resolving off the stack onto the battlefield
///     (CR 608.3 + CR 614.12),
///   - the `Moved` `ReplacementDefinition` parsed from "As ~ enters, choose
///     Jeskai or Temur" firing on the real ETB event and pushing the
///     `Effect::Choose { persist: true }` resolution-choice prompt
///     (CR 614.12c),
///   - `ChooseOption` answering the prompt and persisting the label on
///     `chosen_attributes` (CR 607.2d — the chosen-side ability now reads
///     `ChosenAttribute::Label`).
///
/// If this helper ever fails to surface a `NamedChoice` after the cast resolves
/// it means the as-enters replacement pipeline is NOT actually firing on a
/// real ETB — that is a hard engine bug, not a test gap.
fn cast_siege_from_hand(runner: &mut GameRunner, siege: ObjectId, chosen_label: &str) -> ObjectId {
    // CR 202.1 — Frostcliff Siege costs {1}{U}{R}. Fund P0's pool exactly.
    add_mana_to(
        runner,
        P0,
        &[ManaType::Blue, ManaType::Red, ManaType::Colorless],
    );

    let card_id = runner.state().objects[&siege].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: siege,
            card_id,
            targets: vec![],
        })
        .expect("P0 must be able to cast Frostcliff Siege from hand");

    // CR 608 — pass priority through the stack so the Siege resolves and the
    // as-enters replacement fires. Stop the moment a NamedChoice surfaces.
    for _ in 0..16 {
        if matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }) {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner.pass_both_players();
            continue;
        }
        break;
    }

    // CR 614.12c invariant: the Siege resolving must hand control of the
    // labeled choice to P0 via `WaitingFor::NamedChoice` carrying the
    // anchor-word options. If this fails, the as-enters replacement pipeline
    // is silently dropped on real ETB.
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            source_id,
        } => {
            assert_eq!(
                *player, P0,
                "CR 614.12c: the Siege's controller (P0) must be asked to choose"
            );
            assert!(
                matches!(choice_type, ChoiceType::Labeled { .. }),
                "the anchor-word choice must surface as ChoiceType::Labeled — \
                 got {choice_type:?}"
            );
            assert!(
                options.iter().any(|o| o == "Jeskai") && options.iter().any(|o| o == "Temur"),
                "Jeskai and Temur must both be offered — got {options:?}"
            );
            assert_eq!(
                *source_id,
                Some(siege),
                "the NamedChoice source_id must be the Siege whose replacement \
                 just fired (so chosen_attributes lands on the right object)"
            );
        }
        other => panic!(
            "expected WaitingFor::NamedChoice after casting Frostcliff Siege \
             (the Moved replacement should have fired on ETB); got {other:?} — \
             this would be a real engine bug in the as-enters replacement pipeline, \
             not a test gap"
        ),
    }

    runner
        .act(GameAction::ChooseOption {
            choice: chosen_label.to_string(),
        })
        .unwrap_or_else(|_| panic!("ChooseOption({chosen_label}) must resolve"));

    // CR 614.12c + CR 607.2d: the chosen label must land on the Siege as
    // `ChosenAttribute::Label`, which is the chooser-side artifact the linked
    // chosen-side ability reads via `ChosenLabelIs`.
    assert_eq!(
        runner.state().objects[&siege]
            .chosen_label()
            .map(str::to_string),
        Some(chosen_label.to_string()),
        "After the real ETB choose, ChosenAttribute::Label({chosen_label}) must \
         be persisted on the Siege"
    );
    assert_eq!(
        runner.state().objects[&siege].zone,
        Zone::Battlefield,
        "After the choose resolves, the Siege must be on the battlefield (the \
         Moved replacement is post-zone-change per CR 614.1a)"
    );

    siege
}

/// Real-ETB integration test for CR 614.12c — drives Frostcliff Siege from
/// hand through the actual `CastSpell` → stack → resolution → Moved
/// replacement → choose pipeline (no `waiting_for` mutation). Then verifies
/// the Jeskai trigger fires when a creature P0 controls deals combat damage,
/// matching the in-place battlefield test's draw assertion.
///
/// This is the load-bearing test for the `OracleBlockAst::AsEntersAnchorWordModal`
/// lowering's `ReplacementDefinition` actually firing as part of the ETB
/// pipeline. The other tests in this file place the Siege directly on the
/// battlefield via `add_real_card`, which bypasses the zone-change pipeline
/// and the replacement that drives the choose prompt — so without this test
/// the replacement could silently never fire on real cast resolution.
#[test]
fn cast_siege_from_hand_fires_as_enters_replacement_and_jeskai_draws() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege = scenario.add_real_card(P0, "Frostcliff Siege", Zone::Hand, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let _f1 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let _f2 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Drive the real CastSpell → ETB → as-enters choose pipeline and answer
    // Jeskai. If the as-enters replacement does not fire on real ETB, the
    // helper's NamedChoice assertion panics with a hard-blocker message.
    cast_siege_from_hand(&mut runner, siege, "Jeskai");

    // Snapshot the hand/library so the combat-damage assertion is the
    // unambiguous +1/-1 expected from Draw 1.
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let library_before = runner.state().players[P0.0 as usize].library.len();

    // CR 510.1b — Grizzly Bears (2/2) attacks P1 unblocked, deals 2 combat
    // damage, and the Jeskai trigger (gated by ChosenLabelIs("Jeskai"))
    // resolves to Draw 1.
    run_combat(&mut runner, vec![bear], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before + 1,
        "Jeskai mode (real ETB): combat damage to a player must trigger Draw 1"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before - 1,
        "Jeskai mode (real ETB): Draw 1 must remove one card from the library"
    );
}

/// Multi-attacker Jeskai semantics for `TriggerMode::DamageDoneOnceByController`
/// (CR 603.1 + the printed Frostcliff Siege ruling):
///
/// > "If you chose Jeskai and creatures you control deal combat damage to
/// > multiple players at the same time, Frostcliff Siege's ability will
/// > trigger once for each player dealt combat damage this way."
///
/// The single-opponent corollary is the one this test exercises: when N > 1
/// creatures P0 controls all deal combat damage to the SAME opponent in the
/// same combat damage step, the trigger fires EXACTLY ONCE (one player was
/// dealt combat damage, regardless of how many of P0's creatures contributed)
/// — not once per attacker. With `DamageDoneOnceByController` semantics
/// broken (e.g. fired per damage event instead of per damaged player), this
/// test would assert two draws and fail loudly.
///
/// Discriminator: P0's hand grows by EXACTLY one and the library shrinks by
/// EXACTLY one. Two attackers, one defender, one combat step.
#[test]
fn jeskai_two_attackers_one_defender_draws_exactly_once_per_combat() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let siege = scenario.add_real_card(P0, "Frostcliff Siege", Zone::Battlefield, db);
    // Two attackers, both 2/2, both deal combat damage to P1 in the same
    // damage step. With Jeskai chosen, the Siege trigger's matcher
    // (DamageDoneOnceByController) should fire ONCE per (controller, damaged
    // player) pair per combat damage step, not once per attacker.
    let bear1 = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let bear2 = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    // Enough library to support up to two draws — so the assertion catches
    // either over-firing (hand grows by 2) or under-firing (hand unchanged).
    let _f1 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let _f2 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let _f3 = scenario.add_real_card(P0, "Plains", Zone::Library, db);
    let mut runner = scenario.build();

    drive_siege_choice(&mut runner, siege, "Jeskai");

    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let library_before = runner.state().players[P0.0 as usize].library.len();

    // Both bears attack P1 unblocked → both deal 2 combat damage to P1 in the
    // same combat-damage step. DamageDoneOnceByController must collapse the
    // two source events into a single trigger fire.
    run_combat(&mut runner, vec![bear1, bear2], vec![]);
    runner.advance_until_stack_empty();

    let hand_after = runner.state().players[P0.0 as usize].hand.len();
    let library_after = runner.state().players[P0.0 as usize].library.len();

    assert_eq!(
        hand_after,
        hand_before + 1,
        "DamageDoneOnceByController must fire EXACTLY ONCE when multiple \
         creatures P0 controls deal combat damage to the SAME opponent — \
         hand grew from {hand_before} to {hand_after} (expected +1, not +2)"
    );
    assert_eq!(
        library_after,
        library_before - 1,
        "Exactly one Draw 1 must resolve — library shrank from {library_before} \
         to {library_after} (expected -1, not -2)"
    );
    assert_eq!(
        runner.life(P1),
        20 - 4,
        "Both Grizzly Bears (2/2) deal 2 combat damage to P1; life drops by 4"
    );
}

/// Parser-level smoke test for the full CR 614.12c anchor-word Siege class.
/// Loads each card from the real `card-data.json` database and asserts the
/// minimum lowering shape required for the class:
///
///   1. Exactly one `ReplacementDefinition` whose `event` is `Moved` and whose
///      `execute` is an `Effect::Choose { ChoiceType::Labeled, persist: true }`
///      (the chooser-side per CR 614.12c).
///   2. At least one chosen-side ability — `TriggerDefinition` with
///      `TriggerCondition::ChosenLabelIs(_)` OR `StaticDefinition` with
///      `StaticCondition::ChosenLabelIs(_)` (CR 607.2d linked ability).
///   3. No `Effect::Unimplemented` or `TriggerMode::Unknown` anywhere in the
///      card's ability tree (the class must be parsed end-to-end, not left as
///      a parser fallback).
///
/// Frostcliff Siege is exercised by the rest of this file; the smoke test
/// covers the remaining nine Sieges from FRF, BFZ-era Mirrodin Besieged, and
/// the TDM cycle. A regression in the as-enters anchor-word lowering would
/// break this list before it surfaces as a runtime gameplay bug.
#[test]
fn anchor_word_sieges_load_with_no_parse_gaps() {
    use engine::types::ability::{StaticCondition, TriggerCondition};

    let Some(db) = load_db() else {
        return;
    };

    // CR 614.12c cycle (TDM + FRF + Mirrodin Besieged). Frostcliff itself is
    // covered by the rest of this file's runtime tests.
    let sieges = &[
        // FRF cycle ("Khans" / "Dragons")
        "Citadel Siege",
        "Outpost Siege",
        "Monastery Siege",
        "Palace Siege",
        "Frontier Siege",
        // Original CR 614.12c source card (SOM / Mirrodin Besieged era)
        "Mirrodin Besieged",
        // TDM cycle (anchor-word colors)
        "Barrensteppe Siege",
        "Glacierwood Siege",
        "Hollowmurk Siege",
        "Windcrag Siege",
    ];

    for name in sieges {
        let face = db
            .get_face_by_name(name)
            .unwrap_or_else(|| panic!("`{name}` must be loadable from card-data.json"));

        // (1) Exactly one Moved-replacement whose execute is the labeled-choose.
        assert_eq!(
            face.replacements.len(),
            1,
            "{name}: anchor-word Sieges parse to exactly one Moved replacement \
             (the as-enters choose), got {}",
            face.replacements.len()
        );
        let rep = &face.replacements[0];
        assert_eq!(
            rep.event,
            ReplacementEvent::Moved,
            "{name}: anchor-word as-enters choose must lower to a Moved \
             replacement (CR 614.1a + CR 614.12c)"
        );
        assert_eq!(
            rep.destination_zone,
            Some(Zone::Battlefield),
            "{name}: anchor-word replacement must target the battlefield \
             zone-change (CR 614.12c is an enters-the-battlefield replacement)"
        );
        let execute = rep
            .execute
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: Moved replacement must carry an execute body"));
        match execute.effect.as_ref() {
            Effect::Choose {
                choice_type: ChoiceType::Labeled { options },
                persist,
            } => {
                assert!(
                    *persist,
                    "{name}: the as-enters choose must persist its answer onto \
                     chosen_attributes (CR 607.2d linked-ability machinery)"
                );
                assert_eq!(
                    options.len(),
                    2,
                    "{name}: anchor-word choose offers exactly two labeled \
                     options per CR 614.12c, got {options:?}"
                );
            }
            other => panic!(
                "{name}: Moved replacement's execute must be Effect::Choose {{ \
                 ChoiceType::Labeled, persist: true }} (CR 614.12c lowering), \
                 got {other:?}"
            ),
        }

        // (2) At least one chosen-side ability gated by ChosenLabelIs.
        let trigger_gates = face
            .triggers
            .iter()
            .filter(|t| matches!(t.condition, Some(TriggerCondition::ChosenLabelIs { .. })))
            .count();
        let static_gates = face
            .static_abilities
            .iter()
            .filter(|s| matches!(s.condition, Some(StaticCondition::ChosenLabelIs { .. })))
            .count();
        assert!(
            trigger_gates + static_gates >= 1,
            "{name}: must have at least one ChosenLabelIs-gated trigger or \
             static (CR 607.2d chosen-side linked ability) — got \
             {trigger_gates} gated triggers + {static_gates} gated statics"
        );

        // (3) No Unimplemented/Unknown anywhere in the card's ability tree.
        let serialized = serde_json::to_string(face)
            .unwrap_or_else(|_| panic!("{name}: serialize for parse-gap inspection"));
        assert!(
            !serialized.contains("\"Unimplemented\""),
            "{name}: anchor-word Siege must parse end-to-end — found \
             `Unimplemented` in the serialized ability tree"
        );
        assert!(
            !serialized.contains("\"Unknown\""),
            "{name}: anchor-word Siege must parse end-to-end — found \
             `Unknown` (trigger/replacement mode fallback) in the serialized \
             ability tree"
        );
    }
}
