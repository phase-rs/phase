//! Runtime pipeline tests for granting foretell/miracle/blitz to cards IN HAND
//! with an MV-derived cost (Dream Devourer, Aminatou Veil Piercer, Henzie
//! "Toolbox" Torre).
//!
//! CR 702.143a (Foretell) / CR 702.94a (Miracle) / CR 702.152a (Blitz) /
//! CR 601.2f (generic reduction floors at {0}) / CR 113.6b (keyword functions
//! from its stated zone) / CR 118.9 (alternative costs).
//!
//! These drive the real engine (`apply()` via `GameRunner`), not helper-only
//! parse assertions: each test would fail if the reduction/zone/resolution fix
//! were reverted.

use engine::game::casting::{can_foretell_card, current_casting_variant_choice_options};
use engine::game::effects::draw::resolve as resolve_draw;
use engine::game::keywords::effective_foretell_cost;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    CastingPermission, ContinuousModification, Effect, QuantityExpr, ResolvedAbility,
    StaticDefinition, TargetFilter,
};
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::game_state::{CastPaymentMode, CastingVariant, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const DREAM_DEVOURER: &str = "Each nonland card in your hand without foretell has foretell. Its foretell cost is equal to its mana cost reduced by {2}.";
const AMINATOU: &str = "Each enchantment card in your hand has miracle. Its miracle cost is equal to its mana cost reduced by {4}.";
// CR 702.152a: Henzie "Toolbox" Torre's blitz-granting line. The dynamic
// "costs you pay {1} less" second line (a commander-cast-count reduction) is
// explicitly out of scope for this fix — see issue #5435 — so only the
// self-referential-cost grant line is modeled here.
const HENZIE_BLITZ_GRANT: &str = "Each creature spell you cast with mana value 4 or greater has blitz. The blitz cost is equal to its mana cost.";

/// `n` colorless mana units, for pre-funding a player's mana pool in a scenario.
fn colorless_pool(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

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

/// Negatives: a LAND in hand is never granted foretell; an MV<2 card floors
/// its foretell cost at {0}. The PRINTED-foretell exclusion is its own
/// dedicated test below (`dream_devourer_declines_grant_for_printed_foretell_card`)
/// — it needs the full parsed-static `WithoutKeywordKind{Foretell}` affected
/// filter plus the off-zone recursion guard, not just this scenario's fixtures.
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

/// CR 613.1f + CR 702.143a/d: A hand card that already has a PRINTED Foretell
/// keyword must be excluded from Dream Devourer's grant by the PARSED
/// `WithoutKeywordKind(Foretell)` affected filter (not a hand-built
/// `SpecificObject` remover), and must keep its OWN printed cost — never
/// Dream Devourer's MV−2 grant.
///
/// This is also the load-bearing regression for the CR 613.1f off-zone
/// recursion guard (`off_zone_characteristics::OffZoneRecursionGuard`):
/// deciding whether the "without foretell" filter matches this card requires
/// asking "does this card already have foretell", which re-enters off-zone
/// keyword computation for the SAME object the grant is being evaluated for.
/// Without the guard that query recurses forever; with it, the nested query
/// resolves against `base_keywords` only, correctly seeing the printed
/// Foretell and declining the grant. Every other test in this file removes a
/// keyword via a synthetic `SpecificObject` continuous effect or never
/// exercises a card with a pre-existing printed keyword at all, so none of
/// them touch this path.
///
/// Revert-failing: if the affected filter's exclusion or the recursion guard
/// regresses, this either hangs (unguarded infinite recursion) or silently
/// returns Dream Devourer's MV−2 cost instead of the printed cost.
#[test]
fn dream_devourer_declines_grant_for_printed_foretell_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Dream Devourer", 2, 3)
        .from_oracle_text(DREAM_DEVOURER);
    // MV(6) so Dream Devourer's grant, if wrongly applied, would compute
    // {4} — distinct from the printed {5} below, so the two can't be
    // confused by coincidence.
    let printed_cost = generic(5);
    let card = scenario
        .add_spell_to_hand(P0, "AlreadyForetoldSorcery", false)
        .with_mana_cost(generic(6))
        .with_keyword(Keyword::Foretell(printed_cost.clone()))
        .id();

    let runner = scenario.build();

    let cost = effective_foretell_cost(runner.state(), card)
        .expect("a card with printed foretell must still be foretellable");
    assert_eq!(
        cost, printed_cost,
        "must keep its own printed foretell cost ({{5}}), not Dream Devourer's \
         MV-2 grant ({{4}}) — the WithoutKeywordKind(Foretell) filter must \
         have excluded this card from the grant entirely"
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

// --------------------------------------------------------------------------
// Blitz (Henzie "Toolbox" Torre) — self-referential granted alt cost from
// hand. Issue #5435: an unresolved `ManaCost::SelfManaCost` has mana value 0
// but is not "without paying mana", so it silently acted as a real {0}
// alternative cost — letting the granted Blitz be offered (and auto-routed
// to) for free regardless of the recipient's actual mana value.
// --------------------------------------------------------------------------

/// CR 702.152a + CR 604.1 + CR 118.9: a creature spell with mana value 4 or
/// greater in hand under Henzie's grant must be offered Blitz at the spell's
/// own concrete mana cost — never the raw `SelfManaCost` placeholder and
/// never a free {0} cost.
///
/// Revert-failing: pre-fix, `blitz.mana_cost` is `ManaCost::SelfManaCost`
/// (mana value 0), not `generic(6)`.
#[test]
fn henzie_granted_blitz_surfaces_concrete_spell_mana_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Henzie \"Toolbox\" Torre", 3, 3)
        .from_oracle_text(HENZIE_BLITZ_GRANT);
    let spell = scenario
        .add_spell_to_hand(P0, "SixMVCreature", false)
        .as_creature()
        .with_mana_cost(generic(6))
        .id();
    // Fund both the printed {6} and a concretized {6} blitz cost so the option
    // is offered (affordability is a separate gate from the cost it displays).
    scenario.with_mana_pool(P0, colorless_pool(6));

    let runner = scenario.build();
    let options = current_casting_variant_choice_options(runner.state(), P0, spell);
    let blitz = options
        .iter()
        .find(|o| o.variant == CastingVariant::Blitz)
        .expect("granted Blitz must be offered for an MV>=4 creature spell in hand");
    assert_eq!(
        blitz.mana_cost,
        generic(6),
        "granted Blitz must surface the spell's own concrete mana cost ({{6}}), not \
         the unresolved SelfManaCost placeholder and not a free {{0}} cost, got {:?}",
        blitz.mana_cost
    );
    assert_ne!(
        blitz.mana_cost,
        ManaCost::SelfManaCost,
        "granted Blitz must not surface the raw self-referential placeholder"
    );
}

/// CR 702.152a + CR 118.9 + CR 601.2f: with only {5} available and the
/// concretized blitz cost at {6} (equal to the spell's mana value), Blitz
/// must NOT be offered as an affordable option — this is the exact Discord
/// report (AI blitz-casting MV6/7 creatures with only 6 mana available).
///
/// Revert-failing: pre-fix, the raw `SelfManaCost` placeholder has mana value
/// 0, so it is affordable with ANY amount of mana (including zero), and the
/// Blitz option is always offered/auto-routable regardless of the printed
/// cost's actual affordability.
#[test]
fn henzie_granted_blitz_not_offered_when_unaffordable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Henzie \"Toolbox\" Torre", 3, 3)
        .from_oracle_text(HENZIE_BLITZ_GRANT);
    let spell = scenario
        .add_spell_to_hand(P0, "SixMVCreature", false)
        .as_creature()
        .with_mana_cost(generic(6))
        .id();
    // Only {5} available — insufficient for either the printed {6} or the
    // concretized {6} blitz cost.
    scenario.with_mana_pool(P0, colorless_pool(5));

    let runner = scenario.build();
    let options = current_casting_variant_choice_options(runner.state(), P0, spell);
    assert!(
        !options.iter().any(|o| o.variant == CastingVariant::Blitz),
        "granted Blitz must not be offered when its concretized {{6}} cost is \
         unaffordable at {{5}} available mana, got {:?}",
        options
    );
}

/// CR 702.152a + CR 601.2f: actually casting via the granted Blitz variant
/// must drain the spell's own concrete mana cost from the pool — proving real
/// payment, not just a displayed cost. A {6} creature under Henzie's grant
/// pays exactly {6} (both the printed and blitz costs happen to coincide
/// here; the discriminating fact is that {6}, not {0}, leaves the pool).
///
/// Revert-failing: pre-fix, the unresolved `SelfManaCost` placeholder pays
/// nothing, so the pool would still hold {6} after a blitz cast.
#[test]
fn henzie_granted_blitz_cast_pays_concrete_mana_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Henzie \"Toolbox\" Torre", 3, 3)
        .from_oracle_text(HENZIE_BLITZ_GRANT);
    let spell = scenario
        .add_spell_to_hand(P0, "SixMVCreature", false)
        .as_creature()
        .with_mana_cost(generic(6))
        .id();
    scenario.with_mana_pool(P0, colorless_pool(6));

    let mut runner = scenario.build();
    // CR 601.2b + CR 118.9: a single granted alternative cost is offered through
    // the two-slot `AlternativeCastChoice` modal (normal vs alternative), not the
    // N-way `CastingVariantChoice` prompt — so the blitz election is declared
    // with `.alternative_cast(..)`. Reaching that modal at all is itself
    // meaningful: it proves the concretized {6} blitz cost was affordable
    // alongside the printed {6}, so the engine had a real choice to offer.
    let outcome = runner
        .cast(spell)
        .alternative_cast(AlternativeCastDecision::Alternative)
        .resolve();

    assert_eq!(
        outcome.zone_of(spell),
        engine::types::zones::Zone::Battlefield,
        "the blitz-cast creature must resolve onto the battlefield"
    );
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "casting via the granted Blitz option must drain the full concrete {{6}} \
         cost from the pool, not leave it untouched (unresolved placeholder = \
         free), got {} floating",
        outcome.mana_pool_total(P0)
    );
}

/// Negative control: a creature spell BELOW Henzie's mana-value-4 filter gets
/// no Blitz option at all — the `Cmc GE 4` `affected` filter on the grant must
/// still gate the grant correctly after the cost-concretization fix.
#[test]
fn henzie_grant_excludes_creature_below_mana_value_filter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Henzie \"Toolbox\" Torre", 3, 3)
        .from_oracle_text(HENZIE_BLITZ_GRANT);
    let spell = scenario
        .add_spell_to_hand(P0, "ThreeMVCreature", false)
        .as_creature()
        .with_mana_cost(generic(3))
        .id();
    scenario.with_mana_pool(P0, colorless_pool(3));

    let mut runner = scenario.build();
    let options = current_casting_variant_choice_options(runner.state(), P0, spell);
    assert!(
        !options.iter().any(|o| o.variant == CastingVariant::Blitz),
        "a creature spell below Henzie's mana value 4 filter must not be \
         offered Blitz, got {:?}",
        options
    );

    // The spell must still cast normally at its printed {3}. Declaring NO
    // `.alternative_cast(..)` is the load-bearing part: the harness panics if an
    // `AlternativeCastChoice` modal is ever reached, so a completed cast proves
    // no blitz alternative was offered for this below-threshold creature — and
    // draining exactly {3} proves the grant didn't silently zero the cost.
    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        outcome.zone_of(spell),
        engine::types::zones::Zone::Battlefield,
        "the below-threshold creature must still cast normally"
    );
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the normal cast must pay the printed {{3}}, got {} floating",
        outcome.mana_pool_total(P0)
    );
}
