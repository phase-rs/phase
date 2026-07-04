//! Runtime pipeline tests for granting foretell/miracle to cards IN HAND with an
//! MV-derived cost (Dream Devourer, Aminatou Veil Piercer).
//!
//! CR 702.143a (Foretell) / CR 702.94a (Miracle) / CR 601.2f (generic reduction
//! floors at {0}) / CR 113.6b (keyword functions from its stated zone).
//!
//! These drive the real engine (`apply()` via `GameRunner`), not helper-only
//! parse assertions: each test would fail if the reduction/zone/resolution fix
//! were reverted.

use engine::game::casting::can_foretell_card;
use engine::game::effects::draw::resolve as resolve_draw;
use engine::game::keywords::effective_foretell_cost;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    CastingPermission, ContinuousModification, Effect, QuantityExpr, ResolvedAbility,
    StaticDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const DREAM_DEVOURER: &str = "Each nonland card in your hand without foretell has foretell. Its foretell cost is equal to its mana cost reduced by {2}.";
const AMINATOU: &str = "Each enchantment card in your hand has miracle. Its miracle cost is equal to its mana cost reduced by {4}.";

fn generic(n: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: n,
    }
}

fn draw_one_for_controller(runner: &mut GameRunner) {
    let draw = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(0),
        P0,
    );
    let mut events = Vec::new();
    resolve_draw(runner.state_mut(), &draw, &mut events).expect("draw resolves");
}

// --------------------------------------------------------------------------
// Miracle (Aminatou) — draw-first-card offer + MV−4 cast cost.
// --------------------------------------------------------------------------

/// CR 702.94a + CR 601.2f: Aminatou grants miracle to enchantment cards in hand.
/// Drawing a {6} enchantment as the FIRST draw queues a miracle offer whose cost
/// is the concrete MV−4 = {2} (proving stamp-point resolution — the stored offer
/// cost is a concrete `Cost`, not a `SelfManaCostReduced` placeholder).
#[test]
fn aminatou_miracle_offer_cost_is_printed_mv_minus_4() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Aminatou, Veil Piercer", 3, 4)
        .from_oracle_text(AMINATOU);
    let drawn = scenario
        .add_spell_to_library_top(P0, "SixEnchant", false)
        .as_enchantment()
        .with_mana_cost(generic(6))
        .id();

    let mut runner = scenario.build();
    draw_one_for_controller(&mut runner);

    assert_eq!(
        runner.state().pending_miracle_offers.len(),
        1,
        "first-draw enchantment must queue a miracle offer under Aminatou"
    );
    let offer = &runner.state().pending_miracle_offers[0];
    assert_eq!(offer.object_id, drawn);
    // Revert-failing assertion: MV(6) − 4 = 2, concrete Cost.
    assert_eq!(
        offer.cost,
        generic(2),
        "granted miracle cost must be concrete MV-4 ({{2}}), got {:?}",
        offer.cost
    );
}

/// CR 601.2f floor: a {2}-MV enchantment reduced by {4} floors at {0}.
#[test]
fn aminatou_miracle_cost_floors_at_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Aminatou, Veil Piercer", 3, 4)
        .from_oracle_text(AMINATOU);
    scenario
        .add_spell_to_library_top(P0, "TwoEnchant", false)
        .as_enchantment()
        .with_mana_cost(generic(2));

    let mut runner = scenario.build();
    draw_one_for_controller(&mut runner);

    let offer = &runner.state().pending_miracle_offers[0];
    assert!(
        offer.cost.is_without_paying_mana(),
        "MV(2) reduced by {{4}} must floor at {{0}}, got {:?}",
        offer.cost
    );
}

/// Negative: a NON-enchantment first draw gets no miracle offer (filter excludes
/// it). Negative: a 2nd draw of an enchantment gets no offer (CR 702.94a first
/// card only). Negative: no Aminatou on the battlefield → no offer at all.
#[test]
fn aminatou_miracle_negatives() {
    // Non-enchantment first draw: no offer.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Aminatou, Veil Piercer", 3, 4)
            .from_oracle_text(AMINATOU);
        scenario
            .add_spell_to_library_top(P0, "PlainInstant", true)
            .with_mana_cost(generic(6));
        let mut runner = scenario.build();
        draw_one_for_controller(&mut runner);
        assert!(
            runner.state().pending_miracle_offers.is_empty(),
            "non-enchantment first draw must not queue a miracle offer"
        );
    }
    // Second-draw enchantment: no offer.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Aminatou, Veil Piercer", 3, 4)
            .from_oracle_text(AMINATOU);
        // First (top) draw is a plain card; second draw is the enchantment.
        scenario
            .add_spell_to_library_top(P0, "SecondEnchant", false)
            .as_enchantment()
            .with_mana_cost(generic(6));
        scenario.add_card_to_library_top(P0, "FirstPlain");
        let mut runner = scenario.build();
        draw_one_for_controller(&mut runner); // FirstPlain
        draw_one_for_controller(&mut runner); // SecondEnchant
        assert!(
            runner.state().pending_miracle_offers.is_empty(),
            "an enchantment drawn as the SECOND card must not queue an offer"
        );
    }
    // No Aminatou: no offer.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_spell_to_library_top(P0, "LoneEnchant", false)
            .as_enchantment()
            .with_mana_cost(generic(6));
        let mut runner = scenario.build();
        draw_one_for_controller(&mut runner);
        assert!(
            runner.state().pending_miracle_offers.is_empty(),
            "without Aminatou there is no miracle grant"
        );
    }
}

/// CR 702.94a + CR 601.2f end-to-end: accept the miracle reveal and cast via
/// `CastingVariant::Miracle`, paying the concrete MV−4. A {6} enchantment pays
/// {2}; after payment the pool is empty (no printed {6} paid).
#[test]
fn aminatou_accepted_miracle_cast_pays_mv_minus_4() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Aminatou, Veil Piercer", 3, 4)
        .from_oracle_text(AMINATOU);
    let drawn = scenario
        .add_spell_to_library_top(P0, "SixEnchant", false)
        .as_enchantment()
        .with_mana_cost(generic(6))
        .id();

    let mut runner = scenario.build();
    draw_one_for_controller(&mut runner);
    let offer = runner.state().pending_miracle_offers[0].clone();
    let card_id = runner.state().objects[&drawn].card_id;

    runner.state_mut().waiting_for = WaitingFor::MiracleReveal {
        player: P0,
        object_id: drawn,
        cost: offer.cost.clone(),
    };
    runner.state_mut().pending_miracle_offers.clear();

    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: drawn,
            card_id,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("miracle reveal accept should succeed");
    runner.act(GameAction::PassPriority).expect("P0 pass");
    runner.act(GameAction::PassPriority).expect("P1 pass");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }),
        "miracle trigger should surface a cast offer, got {:?}",
        runner.state().waiting_for
    );

    // Supply the concrete {2} the granted miracle cost requires.
    {
        use engine::types::mana::{ManaType, ManaUnit};
        let pool = &mut runner.state_mut().players[0].mana_pool;
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: drawn,
            card_id,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("miracle cast should succeed paying MV-4");

    let entry = runner.state().stack.last().expect("spell on stack");
    match &entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => assert_eq!(*casting_variant, CastingVariant::Miracle),
        other => panic!("expected Spell on stack, got {other:?}"),
    }
    // Revert-failing: paid exactly {2}, pool now empty. If the reduction were
    // dropped the payment path would demand {6} and this cast would fail (or a
    // stale pool would remain).
    assert!(
        runner.state().players[0].mana_pool.mana.is_empty(),
        "granted miracle {{2}} must consume the whole {{2}} pool, got {:?}",
        runner.state().players[0].mana_pool.mana
    );
}

// --------------------------------------------------------------------------
// Foretell (Dream Devourer) — special action stamps concrete MV−2 permission.
// --------------------------------------------------------------------------

/// CR 702.143a + CR 601.2f: foretell a hand nonland under Dream Devourer. The
/// stamped `CastingPermission::Foretold { cost }` must be the concrete MV−2 (a
/// `ManaCost::Cost`, NOT a `SelfManaCostReduced` placeholder) — this proves
/// stamp-point resolution at the foretell special action.
#[test]
fn dream_devourer_foretell_stamps_concrete_mv_minus_2() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Dream Devourer", 2, 3)
        .from_oracle_text(DREAM_DEVOURER);
    // A {4} nonland (sorcery) in hand → foretell cost MV−2 = {2}.
    let spell = scenario
        .add_spell_to_hand(P0, "FourSorcery", false)
        .with_mana_cost(generic(4))
        .id();

    let mut runner = scenario.build();
    // Pay the {2} foretell special-action cost.
    {
        use engine::types::mana::{ManaType, ManaUnit};
        let pool = &mut runner.state_mut().players[0].mana_pool;
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::Foretell {
            object_id: spell,
            card_id,
        })
        .expect("foretell special action should succeed under Dream Devourer's grant");

    let obj = &runner.state().objects[&spell];
    let foretold = obj
        .casting_permissions
        .iter()
        .find_map(|p| match p {
            CastingPermission::Foretold { cost, .. } => Some(cost.clone()),
            _ => None,
        })
        .expect("foretold permission must be stamped");
    // Revert-failing: MV(4) − 2 = 2, and it must be a CONCRETE Cost (not the
    // SelfManaCostReduced placeholder).
    assert_eq!(
        foretold,
        generic(2),
        "foretell cost must be concrete MV-2 ({{2}}), got {:?}",
        foretold
    );
}

/// Latch: removing Dream Devourer AFTER foretell must not disturb the already-
/// stamped MV−2 permission (the granted keyword only mattered at stamp time).
#[test]
fn dream_devourer_removed_after_foretell_latches_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let devourer = scenario
        .add_creature(P0, "Dream Devourer", 2, 3)
        .from_oracle_text(DREAM_DEVOURER)
        .id();
    let spell = scenario
        .add_spell_to_hand(P0, "FourSorcery", false)
        .with_mana_cost(generic(4))
        .id();

    let mut runner = scenario.build();
    {
        use engine::types::mana::{ManaType, ManaUnit};
        let pool = &mut runner.state_mut().players[0].mana_pool;
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::Foretell {
            object_id: spell,
            card_id,
        })
        .expect("foretell succeeds");

    // Remove Dream Devourer from the battlefield.
    runner.state_mut().battlefield.retain(|&id| id != devourer);
    runner.state_mut().objects.remove(&devourer);

    let obj = &runner.state().objects[&spell];
    let foretold = obj
        .casting_permissions
        .iter()
        .find_map(|p| match p {
            CastingPermission::Foretold { cost, .. } => Some(cost.clone()),
            _ => None,
        })
        .expect("foretold permission survives the source leaving");
    assert_eq!(
        foretold,
        generic(2),
        "the MV-2 foretell cost latches at stamp time, got {:?}",
        foretold
    );
}

/// Negatives: a card with PRINTED foretell is NOT re-granted (the
/// `WithoutKeywordKind{Foretell}` guard excludes it, so no second foretell
/// keyword is applied); a LAND in hand is never granted foretell; an MV<2 card
/// floors its foretell cost at {0}.
#[test]
fn dream_devourer_foretell_negatives() {
    use engine::game::keywords::effective_foretell_cost;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Dream Devourer", 2, 3)
        .from_oracle_text(DREAM_DEVOURER);
    // A land in hand — no foretell grant (nonland-only filter). `as_land` adds
    // the Land core type, so the `Non(Land)` subject filter excludes it.
    let land = scenario
        .add_spell_to_hand(P0, "PlainsCard", false)
        .as_land()
        .id();
    // A cheap nonland ({1}) → foretell cost floors at {0}.
    let cheap = scenario
        .add_spell_to_hand(P0, "OneSorcery", false)
        .with_mana_cost(generic(1))
        .id();

    let runner = scenario.build();

    assert!(
        effective_foretell_cost(runner.state(), land).is_none(),
        "a land must never receive a foretell grant"
    );
    let cheap_cost =
        effective_foretell_cost(runner.state(), cheap).expect("cheap nonland is granted foretell");
    assert!(
        cheap_cost.is_without_paying_mana(),
        "MV(1) reduced by {{2}} must floor at {{0}}, got {cheap_cost:?}"
    );
}

// --------------------------------------------------------------------------
// DEFECT 1 (miracle latched-cost) / DEFECT 2 (foretell single off-zone authority)
// regression tests: a GRANTED alt-cost keyword's source can leave the battlefield
// (or an off-zone effect can strip a PRINTED keyword), which a printed keyword
// never does.
// --------------------------------------------------------------------------

/// DEFECT 1 — CR 702.94a + CR 603.11 + CR 608.2g + CR 608.2b: Aminatou grants
/// miracle at MV−4 and enqueues a concrete offer cost. The player accepts the
/// reveal (pushing the miracle trigger); THEN Aminatou leaves the battlefield
/// before the trigger's cast-offer resolves. The spell must STILL cast for the
/// LATCHED {2}, because the miracle triggered ability granted the cast during its
/// resolution at the offer cost — live keywords (which no longer see miracle once
/// Aminatou is gone) are NOT authoritative.
///
/// Revert-failing: on unpatched code the :8218 live-miracle guard rejects the cast
/// ("Card no longer has miracle"), or the cost re-reads live keywords and finds
/// none — either way `expect("miracle cast should succeed ...")` panics.
#[test]
fn aminatou_miracle_casts_at_latched_cost_after_source_removed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aminatou = scenario
        .add_creature(P0, "Aminatou, Veil Piercer", 3, 4)
        .from_oracle_text(AMINATOU)
        .id();
    let drawn = scenario
        .add_spell_to_library_top(P0, "SixEnchant", false)
        .as_enchantment()
        .with_mana_cost(generic(6))
        .id();

    let mut runner = scenario.build();
    draw_one_for_controller(&mut runner);
    let offer = runner.state().pending_miracle_offers[0].clone();
    let card_id = runner.state().objects[&drawn].card_id;

    // Accept the miracle reveal → pushes the miracle triggered ability.
    runner.state_mut().waiting_for = WaitingFor::MiracleReveal {
        player: P0,
        object_id: drawn,
        cost: offer.cost.clone(),
    };
    runner.state_mut().pending_miracle_offers.clear();
    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: drawn,
            card_id,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("miracle reveal accept should succeed");

    // Remove Aminatou BEFORE the trigger's cast-offer resolves. The granted
    // miracle keyword is now gone from live characteristics — only the latched
    // offer cost keeps the cast alive.
    runner.state_mut().battlefield.retain(|&id| id != aminatou);
    runner.state_mut().objects.remove(&aminatou);

    // Advance to the CastOffer (resolve the miracle trigger).
    runner.act(GameAction::PassPriority).expect("P0 pass");
    runner.act(GameAction::PassPriority).expect("P1 pass");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }),
        "miracle trigger should surface a cast offer even after the source left, got {:?}",
        runner.state().waiting_for
    );

    // Supply the latched {2}.
    {
        use engine::types::mana::{ManaType, ManaUnit};
        let pool = &mut runner.state_mut().players[0].mana_pool;
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    runner
        .act(GameAction::CastSpellAsMiracle {
            object_id: drawn,
            card_id,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("miracle cast must still succeed at the latched cost after the source left");

    let entry = runner.state().stack.last().expect("spell on stack");
    match &entry.kind {
        StackEntryKind::Spell {
            casting_variant, ..
        } => assert_eq!(*casting_variant, CastingVariant::Miracle),
        other => panic!("expected miracle Spell on stack, got {other:?}"),
    }
    // Revert-failing: paid exactly the LATCHED {2}, pool now empty. Unpatched code
    // never reaches here (guard/live-cost failure above).
    assert!(
        runner.state().players[0].mana_pool.mana.is_empty(),
        "latched miracle {{2}} must consume the whole pool, got {:?}",
        runner.state().players[0].mana_pool.mana
    );
}

/// DEFECT 2 — CR 702.143a + CR 113.6b: `effective_foretell_cost` uses the off-zone
/// keyword layer as its SINGLE authority. A card with a PRINTED foretell keyword
/// under an off-zone continuous `RemoveKeyword(Foretell)` effect must NOT be
/// foretellable, because the off-zone layer (`base_keywords` minus off-zone
/// removals) is the sole source of truth — the old `obj.keywords`-first
/// short-circuit wrongly returned the printed cost despite the removal.
///
/// Revert-failing: the unpatched short-circuit reads `obj.keywords` first and
/// returns `Some(cost)`, so the `is_none()` / `!can_foretell_card` asserts fail.
#[test]
fn printed_foretell_removed_off_zone_is_not_foretellable() {
    let foretell_cost = generic(3);

    // Positive sibling: printed foretell, no removal → the concrete printed cost.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let card = scenario
            .add_spell_to_hand(P0, "PrintedForetellSorcery", false)
            .with_mana_cost(generic(5))
            .with_keyword(Keyword::Foretell(foretell_cost.clone()))
            .id();
        let mut runner = scenario.build();
        // Fund the {2} foretell special-action cost so `can_foretell_card`'s
        // affordability check (orthogonal to the off-zone authority under test)
        // does not veto the positive case.
        {
            use engine::types::mana::{ManaType, ManaUnit};
            let pool = &mut runner.state_mut().players[0].mana_pool;
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        assert_eq!(
            effective_foretell_cost(runner.state(), card),
            Some(foretell_cost.clone()),
            "a printed foretell (in base_keywords) must be foretellable at its concrete cost"
        );
        assert!(
            can_foretell_card(runner.state(), P0, card),
            "printed foretell with no removal must be foretellable"
        );
    }

    // Negative: printed foretell + off-zone RemoveKeyword(Foretell) over the hand
    // card → not foretellable.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let card = scenario
            .add_spell_to_hand(P0, "PrintedForetellSorcery", false)
            .with_mana_cost(generic(5))
            .with_keyword(Keyword::Foretell(foretell_cost.clone()))
            .id();
        // A battlefield source whose continuous static strips Foretell from the
        // hand card (models e.g. an off-zone RemoveAllAbilities / RemoveKeyword).
        scenario.add_creature(P0, "AbilityStripper", 1, 1);

        let mut runner = scenario.build();
        // Fund {2} so `can_foretell_card` returning false is driven by the removed
        // keyword, not by inability to pay the special-action cost.
        {
            use engine::types::mana::{ManaType, ManaUnit};
            let pool = &mut runner.state_mut().players[0].mana_pool;
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
            pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        let stripper = *runner
            .state()
            .battlefield
            .iter()
            .find(|&&id| runner.state().objects[&id].name == "AbilityStripper")
            .expect("stripper on battlefield");
        runner
            .state_mut()
            .objects
            .get_mut(&stripper)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SpecificObject { id: card })
                    .modifications(vec![ContinuousModification::RemoveKeyword {
                        keyword: Keyword::Foretell(foretell_cost.clone()),
                    }]),
            );

        // Revert-failing: unpatched short-circuit returns Some(foretell_cost).
        assert!(
            effective_foretell_cost(runner.state(), card).is_none(),
            "an off-zone RemoveKeyword(Foretell) must strip a PRINTED foretell (single \
             off-zone authority), got {:?}",
            effective_foretell_cost(runner.state(), card)
        );
        assert!(
            !can_foretell_card(runner.state(), P0, card),
            "a card whose printed foretell was removed off-zone must not be foretellable"
        );
    }
}
