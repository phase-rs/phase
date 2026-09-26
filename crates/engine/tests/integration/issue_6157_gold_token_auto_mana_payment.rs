//! Issue #6157: Gold tokens are not used by automatic mana payment.
//!
//! Gold's mana ability ("Sacrifice this token: Add one mana of any color.",
//! CR 111.10c) has a bare `Sacrifice` cost with no `{T}` component. Auto-tap
//! source discovery (`mana_sources::is_active_tap_mana_ability`) required a
//! `{T}` cost component on every scanned ability, so Gold was invisible to
//! `CastPaymentMode::Auto` even though its cost sacrifices only the token
//! itself and needs no player choice — exactly as deterministic as a `{T}`
//! cost. Treasure worked only because its cost happens to also include `{T}`
//! (`{T}, Sacrifice this artifact: ...`).
//!
//! Fix: auto-tap source discovery now also accepts an unambiguous
//! self-sacrifice cost (`Sacrifice` targeting only the source, count 1).

use engine::game::casting::{can_cast_object_now_with_probe, PriorityCastProbe};
use engine::game::effects::token::predefined_token_abilities;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, CardSelectionMode,
    DiscardSelfScope, Effect, ManaContribution, ManaProduction, QuantityExpr, SacrificeCost,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, ManaChoice, PayCostKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn make_token(
    state: &mut engine::types::game_state::GameState,
    card_id: u64,
    subtype: &str,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        subtype.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push(subtype.to_string());
    obj.base_card_types = obj.card_types.clone();
    let abilities = predefined_token_abilities(subtype);
    *std::sync::Arc::make_mut(&mut obj.abilities) = abilities.clone();
    *std::sync::Arc::make_mut(&mut obj.base_abilities) = abilities;
    id
}

fn draw_spell(scenario: &mut GameScenario) -> ObjectId {
    scenario.with_library_top(P0, &["Filler Card"]);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Auto-Pay Draw", true, "Draw a card.")
        .with_mana_cost(ManaCost::generic(1))
        .id()
}

fn run_with_mana_test_stack(test: fn()) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(test)
        .expect("spawn mana test thread")
        .join()
        .expect("mana test thread panicked");
}

#[test]
fn gold_token_is_auto_tapped_for_mana_like_treasure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let mut runner = scenario.build();
    let gold = make_token(runner.state_mut(), 900, "Gold");

    let outcome = runner.cast(spell).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "auto payment should fully resolve using the Gold token without pausing for manual input, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[gold], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
}

#[test]
fn treasure_token_is_auto_tapped_for_mana_control_case() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let mut runner = scenario.build();
    let treasure = make_token(runner.state_mut(), 901, "Treasure");

    let outcome = runner.cast(spell).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "auto payment should fully resolve using the Treasure token without pausing for manual input, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[treasure], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
}

/// Regression for the maintainer review on PR #6230: a Gold token that is
/// already **tapped** (and summoning-sick) must still be selected by the
/// auto-tap payment path. Gold's cost is "Sacrifice this token: Add one mana
/// of any color." — an unambiguous self-sacrifice with **no** `{T}` component
/// (CR 111.10c). CR 106.12 / CR 302.6 gate only `{T}`/`{Q}` costs on an
/// untapped, non-summoning-sick source, so a tapped Gold can legally pay a
/// sacrifice cost. The earlier object-level tapped/summoning-sickness prefilter
/// short-circuited before the self-sacrifice predicate could run, leaving the
/// spell unpayable; this test fails on that pre-fix code and passes once the
/// prefilter is made conditional on the ability requiring `{T}`.
#[test]
fn tapped_gold_token_still_auto_taps_for_self_sacrifice_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let mut runner = scenario.build();
    let gold = make_token(runner.state_mut(), 902, "Gold");
    // Tap the Gold token and mark it summoning-sick: neither state may block a
    // sacrifice-cost mana ability, since the cost carries no `{T}` symbol.
    {
        let obj = runner.state_mut().objects.get_mut(&gold).unwrap();
        obj.tapped = true;
        obj.summoning_sick = true;
    }

    let outcome = runner.cast(spell).resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "auto payment should fully resolve using a tapped Gold token via its \
         self-sacrifice mana ability without pausing for manual input, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[gold], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
}

/// Builds a permanent whose only mana ability is Lion's Eye Diamond-shaped:
/// `Composite[Discard{ selection: Chosen }, Sacrifice(SelfRef, 1)]` — "Discard
/// your hand, Sacrifice this artifact: Add one mana of any color." (matches the
/// LED build in `mana_abilities.rs`).
///
/// The `Sacrifice(SelfRef, 1)` sibling looks like Gold's unambiguous
/// self-sacrifice, but the `Discard{Chosen}` sibling forces a player choice, so
/// the *whole* cost is NOT choice-free. This unrestricted synthetic source stays on the
/// manual-payment path (its discard prompt must never be silently bypassed) and
/// differs from real LED, whose priority-only restriction forbids payment use. It
/// must NOT be selected by auto-tap.
fn make_led_source(state: &mut engine::types::game_state::GameState, card_id: u64) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Lion Eye Diamond".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.base_card_types = obj.card_types.clone();
    let led_ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: vec![
                    ManaColor::White,
                    ManaColor::Blue,
                    ManaColor::Black,
                    ManaColor::Red,
                    ManaColor::Green,
                ],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                selection: CardSelectionMode::Chosen,
                self_scope: DiscardSelfScope::FromHand,
            },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    });
    let abilities = vec![led_ability];
    *std::sync::Arc::make_mut(&mut obj.abilities) = abilities.clone();
    *std::sync::Arc::make_mut(&mut obj.base_abilities) = abilities;
    id
}

/// End-to-end regression for the maintainer review on PR #6230: prove the
/// Lion's Eye Diamond-shaped composite cost
/// (`Composite[Discard{Chosen}, Sacrifice(SelfRef, 1)]`) **remains on the
/// manual-payment path** — auto-tap must never select it — while a bare Gold
/// token (unambiguous self-sacrifice) stays eligible.
///
/// This drives the same `CastPaymentMode::Auto` entry point as the three tests
/// above (`runner.cast(spell).resolve()`), not just the
/// `has_unambiguous_self_sacrifice_component` predicate. The discriminating
/// board is **LED + Gold together**: auto-tap must skip LED (its `Discard`
/// sibling needs a player choice) and pay the generic-1 spell with Gold, so the
/// spell resolves, Gold is sacrificed, and the LED source is left untouched on
/// the battlefield.
///
/// On the pre-fix `any`-match predicate LED is misclassified as an unambiguous
/// self-sacrifice source, so auto-tap discovery admits it into the payment plan;
/// its `Discard` cost then cannot be satisfied by the auto path and the whole
/// payment is rejected — the otherwise-payable spell (Gold is right there)
/// becomes unpayable. This test fails on that pre-fix code (`try_resolve`
/// returns `Err`) and passes once the whole-tree choice-free check keeps LED off
/// the auto-tap path.
#[test]
fn led_shaped_discard_sacrifice_not_auto_tapped_stays_manual() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    // Cards in hand so LED's Discard cost is *satisfiable* — this rules out
    // "LED failed only because the hand was empty" and isolates the auto-tap
    // classification as the cause.
    scenario.add_card_to_hand(P0, "Spare Card A");
    scenario.add_card_to_hand(P0, "Spare Card B");
    let mut runner = scenario.build();
    let led = make_led_source(runner.state_mut(), 950);
    let gold = make_token(runner.state_mut(), 951, "Gold");

    let outcome = runner
        .cast(spell)
        .try_resolve()
        .expect("auto-tap must pay the generic-1 spell with Gold and skip the LED-shaped source");

    // Auto-tap resolved the spell using Gold; it must not have paused for input.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "auto payment should resolve via the Gold token without pausing, got {:?}",
        outcome.final_waiting_for()
    );
    // Gold paid (sacrificed); the spell resolved and drew.
    outcome.assert_zone(&[gold], Zone::Graveyard);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
    // The LED-shaped source was NOT auto-tapped: it stays on the battlefield and
    // no discard was forced through it.
    let led_zone = outcome.state().objects.get(&led).map(|o| o.zone);
    assert_eq!(
        led_zone,
        Some(Zone::Battlefield),
        "the LED-shaped composite source must remain on the battlefield \
         (manual-payment path), never auto-tapped for the spell"
    );

    // Sanity floor: with **only** the LED-shaped source available (no Gold), the
    // spell is not auto-payable at all — auto-tap has nothing choice-free to use,
    // confirming LED itself never covers the cost on the auto path.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    scenario.add_card_to_hand(P0, "Spare Card A");
    let mut runner = scenario.build();
    let led = make_led_source(runner.state_mut(), 952);
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting with only the LED-shaped source must reach manual mana payment");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { player, .. } if player == P0
        ),
        "with only the LED-shaped source, auto payment must pause at manual mana payment, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cast.as_deref(),
            Some(pending) if pending.object_id == spell
        ),
        "the manual mana-payment pause must preserve the pending spell cast"
    );
    assert_eq!(
        runner.state().objects.get(&led).map(|object| object.zone),
        Some(Zone::Battlefield),
        "the LED-shaped source must remain available for its manual discard-and-sacrifice payment"
    );
}

/// CR 304.5 + CR 605.3a: printed timing limits still apply during mana payment.
#[test]
fn instant_only_mana_ability_cannot_pay_for_cast() {
    run_with_mana_test_stack(check_instant_only_mana_ability_cannot_pay_for_cast);
}

fn check_instant_only_mana_ability_cannot_pay_for_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let led = scenario
        .add_artifact_from_oracle(
            P0,
            "Lion's Eye Diamond",
            "Discard your hand, Sacrifice this artifact: Add three mana of any one color. Activate only as an instant.",
        )
        .id();
    let mut runner = scenario.build();
    let gold = make_token(runner.state_mut(), 953, "Gold");
    let ability_index = runner.state().objects[&led]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .activation_restrictions
                .contains(&ActivationRestriction::AsInstant)
        })
        .expect("Oracle-built Diamond must retain its instant-only restriction");
    assert!(matches!(
        *runner.state().objects[&led].abilities[ability_index].effect,
        Effect::Mana { .. }
    ));

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the spell must reach a manual mana-payment window");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert!(
        matches!(runner.state().pending_cast.as_deref(), Some(pending) if pending.object_id == spell)
    );
    let hand_size = runner.state().players[P0.0 as usize].hand.len();
    let mana_before = runner.state().players[P0.0 as usize].mana_pool.total();
    let error = runner
        .act(GameAction::ActivateAbility {
            source_id: led,
            ability_index,
        })
        .expect_err("Diamond cannot activate during mana payment");
    assert!(
        matches!(error, engine::game::engine::EngineError::ActionNotAllowed(ref message)
            if message == "Activation restriction not satisfied: AsInstant"),
        "expected Diamond's timing restriction, got {error:?}"
    );
    assert_eq!(runner.state().objects[&led].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[P0.0 as usize].hand.len(), hand_size);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        mana_before
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { player: P0, .. }
    ));
    assert!(
        matches!(runner.state().pending_cast.as_deref(), Some(pending) if pending.object_id == spell)
    );

    let gold_ability_index = runner.state().objects[&gold]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::Mana { .. }))
        .expect("Gold has a mana ability");
    let prompted = runner
        .act(GameAction::ActivateAbility {
            source_id: gold,
            ability_index: gold_ability_index,
        })
        .expect("Gold may activate during the same mana-payment window");
    assert!(matches!(
        prompted.waiting_for,
        WaitingFor::ChooseManaColor { player: P0, .. }
    ));
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("choose Gold's produced mana");
    runner
        .act(GameAction::PassPriority)
        .expect("Gold's mana must finish the original cast");
    assert_eq!(runner.state().objects[&gold].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    assert!(runner.state().pending_cast.is_none());
}

/// CR 304.5: automatic payment must select the unrestricted source.
#[test]
fn instant_only_tap_mana_cannot_auto_pay_for_cast() {
    run_with_mana_test_stack(check_instant_only_tap_mana_cannot_auto_pay_for_cast);
}

fn check_instant_only_tap_mana_cannot_auto_pay_for_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Instant-only mana source", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap)
            .activation_restrictions(vec![ActivationRestriction::AsInstant]),
        )
        .id();
    let mut runner = scenario.build();
    let gold = make_token(runner.state_mut(), 954, "Gold");
    assert!(runner.state().objects[&source].abilities[0]
        .activation_restrictions
        .contains(&ActivationRestriction::AsInstant));

    let outcome = runner.cast(spell).resolve();
    outcome.assert_zone(&[gold, spell], Zone::Graveyard);
    outcome.assert_hand_drawn(P0, 1);
    assert!(!outcome.state().objects[&source].tapped);
}

/// CR 305.6: a basic land subtype grants its intrinsic mana ability.
#[test]
fn subtype_only_land_still_pays_for_cast() {
    run_with_mana_test_stack(check_subtype_only_land_still_pays_for_cast);
}

fn check_subtype_only_land_still_pays_for_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = draw_spell(&mut scenario);
    let mut runner = scenario.build();
    let land = create_object(
        runner.state_mut(),
        CardId(955),
        P0,
        "Intrinsic Forest".to_string(),
        Zone::Battlefield,
    );
    let object = runner.state_mut().objects.get_mut(&land).unwrap();
    object.card_types.core_types.push(CoreType::Land);
    object.card_types.subtypes.push("Forest".to_string());
    object.base_card_types = object.card_types.clone();
    assert!(object.abilities.is_empty());

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the intrinsic Forest mana ability must pay for the spell");
    assert!(runner.state().objects[&land].tapped);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

#[test]
fn instant_only_mana_ability_activates_with_priority() {
    run_with_mana_test_stack(check_instant_only_mana_ability_activates_with_priority);
}

fn check_instant_only_mana_ability_activates_with_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_artifact_from_oracle(
            P0,
            "Instant-only mana artifact",
            "{T}: Add {G}. Activate only as an instant.",
        )
        .id();
    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .activation_restrictions
                .contains(&ActivationRestriction::AsInstant)
        })
        .expect("the mana ability must retain its instant-only restriction");

    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index,
        })
        .expect("instant-only mana must be activatable with priority");
    assert!(runner.state().objects[&source].tapped);
}

#[test]
fn lions_eye_diamond_activates_with_priority() {
    run_with_mana_test_stack(check_lions_eye_diamond_activates_with_priority);
}

fn check_lions_eye_diamond_activates_with_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spare_a = scenario.add_card_to_hand(P0, "Spare Card A");
    let spare_b = scenario.add_card_to_hand(P0, "Spare Card B");
    let diamond = scenario
        .add_artifact_from_oracle(
            P0,
            "Lion's Eye Diamond",
            "Discard your hand, Sacrifice this artifact: Add three mana of any one color. Activate only as an instant.",
        )
        .id();
    let mut runner = scenario.build();
    let ability_index = runner.state().objects[&diamond]
        .abilities
        .iter()
        .position(|ability| {
            ability
                .activation_restrictions
                .contains(&ActivationRestriction::AsInstant)
        })
        .expect("Oracle-built Diamond must retain its instant-only restriction");
    assert!(matches!(
        *runner.state().objects[&diamond].abilities[ability_index].effect,
        Effect::Mana { .. }
    ));

    runner
        .act(GameAction::ActivateAbility {
            source_id: diamond,
            ability_index,
        })
        .expect("Diamond must activate while its controller has priority");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            player: P0,
            kind: PayCostKind::Discard,
            count: 2,
            ..
        }
    ));
    runner
        .act(GameAction::SelectCards {
            cards: vec![spare_a, spare_b],
        })
        .expect("discarding the hand must complete Diamond's activation cost");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { player: P0, .. }
        ),
        "Diamond activation waits for {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().players[P0.0 as usize].hand.is_empty());
    assert_eq!(runner.state().objects[&diamond].zone, Zone::Graveyard);

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Green),
            count: 1,
        })
        .expect("Diamond adds three mana of the chosen color");
    // CR 605.3b: mana resolves immediately without a stack entry.
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 3);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// CR 304.5 + CR 605.3b: priority suffices even with an empty hand, on the
/// opponent's turn, with a spell on the stack, and when controller differs from owner.
#[test]
fn diamond_priority_uses_controller_not_owner_or_active_player() {
    run_with_mana_test_stack(|| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Stack marker", true, "Draw a card.")
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let diamond = scenario.add_artifact_from_oracle(
            P0, "Lion's Eye Diamond",
            "Discard your hand, Sacrifice this artifact: Add three mana of any one color. Activate only as an instant.",
        ).id();
        let mut runner = scenario.build();
        runner.state_mut().objects.get_mut(&diamond).unwrap().owner = P1;
        runner.state_mut().active_player = P1;
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("put an instant on the opponent's turn stack");
        assert_eq!(runner.state().stack.len(), 1);
        assert!(runner.state().players[P0.0 as usize].hand.is_empty());

        // CR 602.2: the source's controller, not its owner, may activate it.
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
        runner.state_mut().priority_player = P1;
        runner
            .act(GameAction::ActivateAbility {
                source_id: diamond,
                ability_index: 0,
            })
            .expect_err("the owner cannot activate another player's permanent");
        assert_eq!(runner.state().objects[&diamond].zone, Zone::Battlefield);
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
        runner.state_mut().priority_player = P0;
        runner
            .act(GameAction::ActivateAbility {
                source_id: diamond,
                ability_index: 0,
            })
            .expect("the controller can activate Diamond with an empty hand");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { player: P0, .. }
        ));
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            })
            .expect("resolve Diamond immediately");
        assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 3);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(runner.state().objects[&diamond].zone, Zone::Graveyard);
    });
}

/// CR 304.5: cached affordability and actual payment must filter the selected
/// ability, while a legal sibling on the same permanent remains usable.
#[test]
fn instant_only_cached_payment_preserves_legal_sibling() {
    run_with_mana_test_stack(|| {
        for oracle in [
            None,
            Some("{T}: Add {G}. Activate only as an instant."),
            Some("{T}: Add {G}."),
            Some("{T}: Add {G}. Activate only as an instant.\n{T}: Add {G}."),
        ] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let spell = draw_spell(&mut scenario);
            let source = oracle.map(|text| {
                scenario
                    .add_artifact_from_oracle(P0, "Mana source", text)
                    .id()
            });
            let mut runner = scenario.build();
            let expected = oracle.is_some_and(|text| text.ends_with("Add {G}."));
            let probe = PriorityCastProbe::new(runner.state(), P0);
            let before = engine::game::perf_counters::snapshot().cached_auto_tap_source_reuses;
            assert_eq!(
                can_cast_object_now_with_probe(probe.state(), P0, spell, Some(&probe)),
                expected,
                "{oracle:?}"
            );
            assert!(
                engine::game::perf_counters::snapshot().cached_auto_tap_source_reuses > before,
                "the affordability check must consume the cached source list"
            );
            if expected {
                let outcome = runner.cast(spell).resolve();
                outcome.assert_zone(&[spell], Zone::Graveyard);
                outcome.assert_hand_drawn(P0, 1);
                assert!(outcome.state().objects[&source.unwrap()].tapped);
            }
        }
    });
}

/// CR 304.5 + CR 601.2g: priority-only mana contributes no capacity to an
/// announced X, but the same manual ability without that restriction does.
#[test]
fn instant_only_sources_do_not_inflate_x_limit_or_manual_capacity() {
    run_with_mana_test_stack(|| {
        for (restricted, legal_sibling, land_source, expected_max) in [
            (true, false, true, 0),
            (true, false, false, 0),
            (false, false, false, 3),
            (true, true, false, 1),
        ] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_basic_land(P0, ManaColor::Green);
            // The legal manual sibling can discard a card other than the pending spell.
            scenario.add_card_to_hand(P0, "Discard fodder");
            let spell = scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Stream of Life",
                    false,
                    "Target player gains X life.",
                )
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::X, ManaCostShard::Green],
                    generic: 0,
                })
                .id();
            let source = if land_source {
                scenario
                    .add_land_from_oracle(
                        P0,
                        "Restricted land",
                        "{T}: Add {G}. Activate only as an instant.",
                    )
                    .id()
            } else {
                scenario.add_artifact_from_oracle(
                P0, "Lion's Eye Diamond",
                "Discard your hand, Sacrifice this artifact: Add three mana of any one color. Activate only as an instant.",
            ).id()
            };
            let mut runner = scenario.build();
            if !restricted {
                let object = runner.state_mut().objects.get_mut(&source).unwrap();
                std::sync::Arc::make_mut(&mut object.abilities)[0]
                    .activation_restrictions
                    .clear();
                std::sync::Arc::make_mut(&mut object.base_abilities)[0]
                    .activation_restrictions
                    .clear();
            }
            if legal_sibling {
                let manual = make_led_source(runner.state_mut(), 957);
                let ability = runner.state().objects[&manual].abilities[0].clone();
                runner.state_mut().battlefield.retain(|id| *id != manual);
                runner.state_mut().objects.remove(&manual);
                let object = runner.state_mut().objects.get_mut(&source).unwrap();
                std::sync::Arc::make_mut(&mut object.abilities).push(ability.clone());
                std::sync::Arc::make_mut(&mut object.base_abilities).push(ability);
            }
            if land_source {
                assert!(!engine::game::mana_sources::activatable_mana_options(
                    runner.state(),
                    source,
                    P0
                )
                .is_empty());
            } else {
                assert!(engine::game::mana_sources::activatable_mana_actions_for_player(runner.state(), P0)
                    .iter().any(|action| matches!(action, GameAction::ActivateAbility { source_id, ability_index: 0 } if *source_id == source)));
            }
            let card_id = runner.state().objects[&spell].card_id;
            runner
                .act(GameAction::CastSpell {
                    object_id: spell,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("begin Stream of Life announcement");
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P0)),
                })
                .expect("select Stream of Life's target before announcing X");
            match runner.state().waiting_for {
                WaitingFor::ChooseXValue { max, .. } => assert_eq!(max, expected_max),
                ref other => panic!("expected X announcement, got {other:?}"),
            }
        }

        // During priority, the restricted land has a live mana option. A legal
        // manual source passes the non-tap guard, exposing the capacity fallback.
        for restricted in [true, false] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let spell = scenario
                .add_spell_to_hand_from_oracle(P0, "Manual draw", true, "Draw a card.")
                .with_mana_cost(ManaCost::generic(2))
                .id();
            scenario.add_card_to_hand(P0, "Discard fodder");
            let land = scenario
                .add_land_from_oracle(
                    P0,
                    "Manual payment land",
                    if restricted {
                        "{T}: Add {G}. Activate only as an instant."
                    } else {
                        "{T}: Add {G}."
                    },
                )
                .id();
            let mut runner = scenario.build();
            make_led_source(runner.state_mut(), 960);
            assert!(!engine::game::mana_sources::activatable_mana_options(
                runner.state(),
                land,
                P0
            )
            .is_empty());
            let probe = PriorityCastProbe::new(runner.state(), P0);
            assert_eq!(
                can_cast_object_now_with_probe(probe.state(), P0, spell, Some(&probe)),
                !restricted,
                "the restricted land cannot augment legal manual capacity"
            );
            if !restricted {
                let card_id = runner.state().objects[&spell].card_id;
                runner
                    .act(GameAction::CastSpell {
                        object_id: spell,
                        card_id,
                        targets: vec![],
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .expect("unrestricted land plus manual source can fund two mana");
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::ManaPayment { player: P0, .. }
                ));
            }
        }
    });
}

/// CR 304.5 + CR 601.2g: a legal colorless manual source passes the manual
/// payment guard but cannot make Diamond's forbidden colored profile payable.
#[test]
fn instant_only_colored_profile_cannot_supply_manual_payment() {
    run_with_mana_test_stack(|| {
        for restricted in [true, false] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let generic = draw_spell(&mut scenario);
            let colored = scenario
                .add_spell_to_hand_from_oracle(P0, "Colored draw", true, "Draw a card.")
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                })
                .id();
            scenario.add_card_to_hand(P0, "Discard fodder");
            let diamond = scenario.add_artifact_from_oracle(
                P0, "Lion's Eye Diamond",
                "Discard your hand, Sacrifice this artifact: Add three mana of any one color. Activate only as an instant.",
            ).id();
            let mut runner = scenario.build();
            let manual = make_led_source(runner.state_mut(), 956);
            let object = runner.state_mut().objects.get_mut(&manual).unwrap();
            let mut ability = object.abilities[0].clone();
            if let Effect::Mana { produced, .. } = &mut *ability.effect {
                *produced = ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 1 },
                };
            }
            *std::sync::Arc::make_mut(&mut object.abilities) = vec![ability.clone()];
            *std::sync::Arc::make_mut(&mut object.base_abilities) = vec![ability];
            if !restricted {
                let object = runner.state_mut().objects.get_mut(&diamond).unwrap();
                std::sync::Arc::make_mut(&mut object.abilities)[0]
                    .activation_restrictions
                    .clear();
                std::sync::Arc::make_mut(&mut object.base_abilities)[0]
                    .activation_restrictions
                    .clear();
            }
            let probe = PriorityCastProbe::new(runner.state(), P0);
            assert!(
                can_cast_object_now_with_probe(probe.state(), P0, generic, Some(&probe)),
                "legal manual source must cover generic mana"
            );
            assert_eq!(
                can_cast_object_now_with_probe(probe.state(), P0, colored, Some(&probe)),
                !restricted,
                "only the unrestricted Diamond may cover the colored shard"
            );
            let card_id = runner.state().objects[&generic].card_id;
            runner
                .act(GameAction::CastSpell {
                    object_id: generic,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("the legal manual source makes the generic spell castable");
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ManaPayment { player: P0, .. }
            ));
        }
    });
}

/// CR 305.6: layer-supplied intrinsic mana remains available for X payment.
#[test]
fn subtype_only_forests_pay_announced_x() {
    run_with_mana_test_stack(|| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Stream of Life",
                false,
                "Target player gains X life.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Green],
                generic: 0,
            })
            .id();
        let mut runner = scenario.build();
        for card_id in [958, 959] {
            let land = create_object(
                runner.state_mut(),
                CardId(card_id),
                P0,
                "Forest".into(),
                Zone::Battlefield,
            );
            let object = runner.state_mut().objects.get_mut(&land).unwrap();
            object.card_types.core_types.push(CoreType::Land);
            object.card_types.subtypes.push("Forest".into());
            object.base_card_types = object.card_types.clone();
            assert!(object.abilities.is_empty());
        }
        let mut announcement =
            engine::game::scenario::GameRunner::from_state(runner.state().clone());
        let card_id = announcement.state().objects[&spell].card_id;
        announcement
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("intrinsic Forests permit Stream of Life announcement");
        announcement
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P0)),
            })
            .expect("select Stream of Life's target before announcing X");
        assert!(
            matches!(announcement.state().waiting_for, WaitingFor::ChooseXValue { max, .. } if max >= 1)
        );
        let outcome = runner.cast(spell).x(1).target_player(P0).resolve();
        outcome.assert_life_delta(P0, 1);
        outcome.assert_zone(&[spell], Zone::Graveyard);
    });
}
