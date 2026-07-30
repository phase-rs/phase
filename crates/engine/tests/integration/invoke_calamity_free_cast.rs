//! Integration test for issue #2385 — Invoke Calamity resolves with no effect;
//! the free-cast window from graveyard/hand never opens.
//!
//! Oracle:
//!   "You may cast up to two instant and/or sorcery spells with total mana value
//!    6 or less from your graveyard and/or hand without paying their mana costs.
//!    If those spells would be put into your graveyard, exile them instead.
//!    Exile Invoke Calamity."
//!
//! Root cause: the whole resolution text was swallowed into a
//! `GraveyardCastPermission` static (which only functions for permanents on the
//! battlefield), leaving the spell's `abilities` empty — so casting Invoke
//! Calamity did nothing. The fix routes the line to a real interactive
//! `Effect::FreeCastFromZones` that opens a budgeted free-cast window
//! (`WaitingFor::CastOffer { FreeCastWindow }`).
//!
//! CR 608.2g: an effect may instruct a player to cast spells during resolution.
//! CR 601.2: "up to two" — the controller may cast 0, 1, or 2 spells.
//! CR 202.3: the running total mana value of the chosen spells must stay ≤ 6.
//! CR 614.1a: spells cast this way are exiled instead of going to the graveyard.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const INVOKE_CALAMITY_TEXT: &str = "You may cast up to two instant and/or sorcery spells with \
     total mana value 6 or less from your graveyard and/or hand without paying their mana costs. \
     If those spells would be put into your graveyard, exile them instead. Exile Invoke Calamity.";

/// Drive the full cast pipeline: Invoke Calamity resolves, opens the free-cast
/// window with the eligible instant (graveyard) and sorcery (hand), the
/// controller free-casts one within the MV budget, that spell resolves and is
/// exiled (not put into the graveyard), then the window re-offers and on
/// decline Invoke Calamity itself is exiled.
///
/// On the bug (no effect / no prompt) the spell would resolve straight to the
/// graveyard, no `FreeCastWindow` would open, and neither eligible spell would
/// be castable for free — this test fails on that behavior.
#[test]
fn invoke_calamity_opens_free_cast_window_and_exiles_cast_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Invoke Calamity in hand. Cost {3}{U}{R}{B} is irrelevant to the window;
    // give it a cheap castable cost and matching pool.
    let invoke_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", true, INVOKE_CALAMITY_TEXT)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    // Eligible candidates: an instant in P0's graveyard (MV 2) and a sorcery in
    // P0's hand (MV 3) — total 5 ≤ 6. Both have a trivial resolvable effect so
    // they leave the stack on resolution.
    let gy_instant = scenario
        .add_spell_to_graveyard(P0, "Graveyard Bolt", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.")
        .id();
    let hand_sorcery = scenario
        .add_spell_to_hand(P0, "Hand Divination", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("Draw a card.")
        .id();

    // Ineligible: a creature card in P0's graveyard (wrong type) and an
    // opponent's instant in their graveyard (not the controller's).
    let _gy_creature = scenario
        .add_creature_to_graveyard(P0, "Dead Bear", 2, 2)
        .id();
    let _opp_instant = scenario
        .add_spell_to_graveyard(P1, "Opponent Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    // {1} for Invoke Calamity itself.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    let invoke_card_id = runner.state().objects[&invoke_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: invoke_id,
            card_id: invoke_card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Invoke Calamity must succeed");

    // Pass priority so Invoke Calamity resolves and opens the window.
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    // PRIMARY: the free-cast window must open, offering exactly the eligible
    // instant + sorcery. On the bug there is no window at all.
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            player,
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    remaining_mv_budget,
                    exile_instead_of_graveyard,
                    ..
                },
        } => {
            assert_eq!(player, P0);
            assert_eq!(remaining_casts, 2, "up to two casts");
            assert_eq!(remaining_mv_budget, Some(6));
            assert!(exile_instead_of_graveyard);
            assert!(
                candidates.contains(&gy_instant),
                "the graveyard instant must be a free-cast candidate"
            );
            assert!(
                candidates.contains(&hand_sorcery),
                "the hand sorcery must be a free-cast candidate"
            );
            assert_eq!(
                candidates.len(),
                2,
                "only the controller's eligible instant/sorcery cards are candidates; got {candidates:?}"
            );
        }
        other => panic!("expected FreeCastWindow to open, got {other:?}"),
    }

    // Free-cast the graveyard instant. It has no targets, so it goes straight
    // onto the stack during resolution.
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(gy_instant),
        })
        .expect("free-casting the graveyard instant must succeed");

    // The free-cast spell is on the stack, cast at no cost (CR 118.9).
    assert_eq!(
        runner.state().objects[&gy_instant].zone,
        Zone::Stack,
        "the free-cast instant must be on the stack",
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "the free cast must not consume mana beyond Invoke Calamity's own cost",
    );

    // CR 608.2g: the just-cast spell goes on the stack ABOVE Invoke Calamity and
    // the window re-offers immediately (budget reduced by MV 2 → 4, one cast
    // remaining) — Invoke Calamity continues resolving, so the free-cast spell
    // has NOT resolved yet.
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    remaining_mv_budget,
                    ..
                },
            ..
        } => {
            assert_eq!(remaining_casts, 1, "one free cast must remain");
            assert_eq!(
                remaining_mv_budget,
                Some(4),
                "the MV budget must shrink by the cast spell's mana value (6 - 2)",
            );
            assert!(
                candidates.contains(&hand_sorcery),
                "the MV-3 hand sorcery still fits the remaining budget of 4",
            );
            assert!(
                !candidates.contains(&gy_instant),
                "the spell already cast this way must not be offered a second time",
            );
        }
        other => panic!("the window must re-offer after the first free cast, got {other:?}"),
    }

    // Decline the remaining cast — the window closes, the continuation runs
    // (Exile Invoke Calamity), and priority returns.
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the remaining free cast must succeed");

    // CR 601.2a + CR 608.2g: "Exile Invoke Calamity" — the resolving spell exiles
    // itself when it finishes resolving, before the spell it cast this way
    // resolves above it on the stack.
    assert_eq!(
        runner.state().objects[&invoke_id].zone,
        Zone::Exile,
        "Invoke Calamity must exile itself when it finishes resolving",
    );

    // Resolve the rest of the stack — the free-cast instant resolves and, per the
    // CR 614.1a rider, is exiled instead of being put into the graveyard.
    for _ in 0..12 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert_eq!(
        runner.state().objects[&gy_instant].zone,
        Zone::Exile,
        "the free-cast instant must be exiled instead of going to the graveyard \
         when it resolves (CR 614.1a)",
    );
}

/// Regression for issue #2385 BLOCKER — free-casting the HAND candidate must be
/// genuinely free (CR 118.9 / CR 608.2g). Before the fix the free-cast handler
/// drove the cast through the normal pipeline, where a hand-origin card got
/// `CastingVariant::Normal` and was charged its printed mana cost — the
/// cost-zeroing alt-cost path only fired for exile/graveyard origins. With an
/// empty mana pool (Invoke Calamity's own {1} already spent), the pre-fix code
/// could not put the printed-{3} hand sorcery on the stack at all; post-fix the
/// runtime `ExileWithAltCost { resolution_cleanup }` zeroes the cost regardless
/// of origin zone, so the spell lands on the stack with zero mana spent.
#[test]
fn invoke_calamity_free_casts_hand_spell_for_zero_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let invoke_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", true, INVOKE_CALAMITY_TEXT)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    // The only free-cast candidate is a sorcery in P0's HAND (MV 3). Its printed
    // mana cost is {3}, which P0 cannot afford after spending its only mana on
    // Invoke Calamity — so if the free cast is not actually free, the cast either
    // fails or charges mana, and the spell never lands on the stack at zero cost.
    let hand_sorcery = scenario
        .add_spell_to_hand(P0, "Hand Divination", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("Draw a card.")
        .id();

    // Exactly {1} — consumed casting Invoke Calamity, leaving an empty pool for
    // the free cast.
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    let invoke_card_id = runner.state().objects[&invoke_id].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: invoke_id,
            card_id: invoke_card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Invoke Calamity must succeed");

    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    // The window opens with the hand sorcery as the sole candidate. Invoke
    // Calamity's {1} is already spent, so the pool is empty here.
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            player,
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
        } => {
            assert_eq!(player, P0);
            assert_eq!(
                candidates,
                vec![hand_sorcery],
                "the hand sorcery must be the sole free-cast candidate"
            );
        }
        other => panic!("expected FreeCastWindow to open, got {other:?}"),
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "Invoke Calamity's own {{1}} must already be spent before the free cast",
    );

    // Free-cast the HAND sorcery. It has no targets, so it goes straight onto the
    // stack during resolution — at ZERO cost.
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(hand_sorcery),
        })
        .expect("free-casting the hand sorcery must succeed");

    // CR 118.9 / CR 608.2g: the hand spell is on the stack and NO mana was spent.
    assert_eq!(
        runner.state().objects[&hand_sorcery].zone,
        Zone::Stack,
        "the free-cast hand sorcery must be on the stack",
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "free-casting from HAND must not consume any mana (the pool was already \
         empty; a non-free cast would have failed or charged mana)",
    );
}

/// CR 601.2c + CR 608.2g: a during-resolution cast cannot select a spell whose
/// required target does not exist. The unrelated draw spell remains castable.
#[test]
fn invoke_calamity_does_not_offer_spell_without_required_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let invoke_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", true, INVOKE_CALAMITY_TEXT)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let draw_spell = scenario
        .add_spell_to_graveyard(P0, "Graveyard Draw", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Draw a card.")
        .id();
    let removal = scenario
        .add_spell_to_graveyard(P0, "Graveyard Removal", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Destroy target creature.")
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    let invoke_card_id = runner.state().objects[&invoke_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: invoke_id,
            card_id: invoke_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Invoke Calamity must succeed");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            assert!(candidates.contains(&draw_spell));
            assert!(
                !candidates.contains(&removal),
                "a spell requiring a creature target must not be offered on an empty battlefield"
            );
        }
        other => panic!("expected FreeCastWindow to open, got {other:?}"),
    }
}

/// CR 601.2c + CR 608.2g: the resolving Invoke remains a legal target for a
/// counterspell cast by its own window, then exiles before that spell resolves.
#[test]
fn invoke_calamity_can_target_resolving_source_with_first_counterspell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let invoke_id = scenario
        .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", true, INVOKE_CALAMITY_TEXT)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let draw_spell = scenario
        .add_spell_to_graveyard(P0, "Graveyard Draw", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Draw a card.")
        .id();
    let counterspell = scenario
        .add_spell_to_graveyard(P0, "Graveyard Counter", true)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Counter target spell.")
        .id();
    scenario.add_card_to_library_top(P0, "Draw Filler");
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    let invoke_card_id = runner.state().objects[&invoke_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: invoke_id,
            card_id: invoke_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Invoke Calamity must succeed");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            assert!(candidates.contains(&draw_spell));
            assert!(
                candidates.contains(&counterspell),
                "the resolving Invoke Calamity is a legal target for the first counterspell"
            );
        }
        other => panic!("expected FreeCastWindow to open, got {other:?}"),
    }
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(counterspell),
        })
        .expect("free-casting the counterspell must succeed");
    let counter_entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.id == counterspell)
        .expect("the free-cast counterspell must be on the stack");
    let StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &counter_entry.kind
    else {
        panic!("the free-cast counterspell must have its resolved spell ability");
    };
    assert!(
        ability.targets.contains(&TargetRef::Object(invoke_id)),
        "the first counterspell must target the currently resolving Invoke Calamity"
    );
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => assert!(candidates.contains(&draw_spell)),
        other => panic!("expected the second FreeCastWindow, got {other:?}"),
    }
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(draw_spell),
        })
        .expect("free-casting the draw spell must succeed");
    assert_eq!(runner.state().objects[&invoke_id].zone, Zone::Exile);
    for _ in 0..12 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority to resolve the free-cast stack must succeed");
    }
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&counterspell].zone,
        Zone::Exile,
        "the counterspell loses its only target and is exiled by Invoke's rider"
    );
}
