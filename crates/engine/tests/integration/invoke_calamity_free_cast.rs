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

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, SpellStackToGraveyardReplacement, TargetRef,
};
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
                    graveyard_replacement,
                    ..
                },
        } => {
            assert_eq!(player, P0);
            assert_eq!(remaining_casts, Some(2), "up to two casts");
            assert_eq!(remaining_mv_budget, Some(6));
            assert_eq!(
                graveyard_replacement,
                Some(SpellStackToGraveyardReplacement::Exile)
            );
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
            assert_eq!(remaining_casts, Some(1), "one free cast must remain");
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

/// The exact production outcome of the direct graveyard/hand free-cast route.
///
/// The graveyard card is the discriminator on every row: it is castable ONLY
/// through a free-cast grant, whereas `spell_objects_available_to_cast` lists
/// every card in hand unconditionally, so a hand card cannot tell a granted
/// permission apart from an ordinary hand cast.
#[derive(Debug, PartialEq, Eq)]
enum FreeCastOutcome {
    /// A free-cast window opened, carrying this bound and offering (or not) the
    /// graveyard card.
    Window {
        remaining_casts: Option<u8>,
        offers_the_graveyard_card: bool,
    },
    /// No window opened, but the graveyard card is still castable at a later
    /// priority — the uncapped `CastFromZone` permission the refused clause used
    /// to fall through into.
    UncappedPermission,
    /// No window, and no free-cast permission once priority comes back: the
    /// clause was refused end to end.
    Refused,
}

/// CR 608.2c + CR 608.2g: a printed free-cast bound the representation cannot
/// express must be REFUSED, never fabricated and never widened.
///
/// Production-path companion to the parser regressions
/// `free_cast_from_zones_cap_the_window_cannot_represent_is_refused` and
/// `from_among_cast_cap_the_window_cannot_represent_is_refused`. This drives the
/// real cast pipeline (`GameAction::CastSpell` → resolution →
/// `WaitingFor::CastOffer`) rather than asserting on parsed AST shape, so it
/// fails on the *runtime* consequence of the defect.
///
/// Three successive ways the code got this wrong, all silent:
///   * `count as u8` wrapped `up to 300` to a 44-cast window, and `up to 256` to
///     a **zero**-cast window;
///   * `u8::try_from(300).ok()` decayed to `None`, which this PR made the
///     UNBOUNDED sentinel — a stated cap silently widened to unlimited;
///   * the strict rejection was then treated as "not my shape", so the clause
///     fell out of `try_parse_free_cast_from_zones` into `try_parse_cast_effect`
///     and became an uncapped `CastFromZone` permission over the same
///     graveyard/hand pool. `FreeCastOutcome::UncappedPermission` is that third
///     wrong answer, named so this test distinguishes it instead of merely
///     excluding a window.
///
/// CR 608.2c makes the printed "up to N" part of the instruction the controller
/// follows, so the only honest outcomes are to carry the bound or to refuse.
/// `up to 256` is the boundary case and `up to 300` the wrap case, per the
/// maintainer's explicit ask; `0` and `X` are the two other readings the same
/// authority refuses. None is a real card — the largest printed "cast up to N"
/// in the corpus is THREE — so all four are representation-boundary hostile
/// fixtures by construction.
#[test]
fn a_free_cast_bound_the_window_cannot_represent_is_refused_not_fabricated() {
    /// The printed surface under test, parameterized by its cast bound.
    fn oracle_for(bound: &str) -> String {
        format!(
            "You may cast up to {bound} instant and/or sorcery spells with total mana value 6 \
             or less from your graveyard and/or hand without paying their mana costs. If those \
             spells would be put into your graveyard, exile them instead. Exile Invoke Calamity."
        )
    }

    /// Every `Effect::Unimplemented` gap name the parser produced, walking the
    /// ability / sub-ability spine.
    fn gap_names(oracle: &str) -> Vec<String> {
        fn walk(definition: &AbilityDefinition, out: &mut Vec<String>) {
            if let Effect::Unimplemented { name, .. } = definition.effect.as_ref() {
                out.push(name.clone());
            }
            if let Some(sub) = definition.sub_ability.as_deref() {
                walk(sub, out);
            }
        }
        let parsed = parse_oracle_text(
            oracle,
            "Invoke Calamity",
            &[],
            &["Instant".to_string()],
            &[],
        );
        let mut names = Vec::new();
        for definition in &parsed.abilities {
            walk(definition, &mut names);
        }
        for execute in parsed.triggers.iter().filter_map(|t| t.execute.as_deref()) {
            walk(execute, &mut names);
        }
        names
    }

    /// Build and resolve the same scenario with a parameterized printed bound,
    /// reporting what the production pipeline actually offered.
    fn free_cast_outcome_for(bound: &str) -> FreeCastOutcome {
        let text = oracle_for(bound);

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let invoke_id = scenario
            .add_spell_to_hand_from_oracle(P0, "Invoke Calamity", true, &text)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let graveyard_bolt = scenario
            .add_spell_to_graveyard(P0, "Graveyard Bolt", true)
            .with_mana_cost(ManaCost::generic(2))
            .from_oracle_text("Draw a card.")
            .id();
        scenario
            .add_spell_to_hand(P0, "Hand Divination", false)
            .with_mana_cost(ManaCost::generic(3))
            .from_oracle_text("Draw a card.");
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

        if let WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    remaining_casts,
                    candidates,
                    ..
                },
            ..
        } = &runner.state().waiting_for
        {
            return FreeCastOutcome::Window {
                remaining_casts: *remaining_casts,
                offers_the_graveyard_card: candidates.contains(&graveyard_bolt),
            };
        }

        // REACH GUARD: "refused" and "uncapped permission" are only
        // distinguishable once the resolution chain has finished and handed
        // priority back with an empty stack. A run parked on some other
        // `WaitingFor` would report an empty permission scan vacuously.
        let mut reached_empty_stack_priority = false;
        for _ in 0..24 {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                && runner.state().stack.is_empty()
            {
                reached_empty_stack_priority = true;
                break;
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }
        assert!(
            reached_empty_stack_priority,
            "\"up to {bound}\": the chain must finish and hand priority back with an empty \
             stack before the permission scan is meaningful; parked at {:?} with stack {}",
            runner.state().waiting_for,
            runner.state().stack.len(),
        );

        // `spell_objects_available_to_cast` reports what P0 may cast right now.
        // Only the GRAVEYARD card discriminates: that list contains every card
        // in hand unconditionally (`casting.rs` seeds it from `player.hand`), so
        // the hand card cannot tell a granted permission apart from an ordinary
        // hand cast. A graveyard card is castable only through a grant.
        let available = spell_objects_available_to_cast(runner.state(), P0);
        if available.contains(&graveyard_bolt) {
            return FreeCastOutcome::UncappedPermission;
        }
        FreeCastOutcome::Refused
    }

    // REACH GUARD (mandatory paired positive): the identical scenario with an
    // in-range printed bound DOES open a window carrying exactly that bound —
    // and that window offers the graveyard card. The second half is what makes
    // the refusals below non-vacuous: it proves this fixture can surface the
    // graveyard card at all, so its ABSENCE afterwards is a real signal rather
    // than an unreachable probe.
    assert_eq!(
        free_cast_outcome_for("two"),
        FreeCastOutcome::Window {
            remaining_casts: Some(2),
            offers_the_graveyard_card: true,
        },
        "reach guard: the in-range control must open a window bounded by its printed cap \
         and offer the graveyard card"
    );
    assert!(
        gap_names(&oracle_for("two")).is_empty(),
        "reach guard: the in-range surface must parse cleanly, with no gap node"
    );

    for bound in ["256", "300", "0", "X"] {
        // The EXACT parser result: the shared strict-refusal gap node. Asserting
        // it here is what keeps the runtime row below non-vacuous — an unrelated
        // upstream parse loss would also produce "no permission", but it would
        // not produce this name.
        assert_eq!(
            gap_names(&oracle_for(bound)),
            vec!["unrepresentable_cast_cap".to_string()],
            "\"up to {bound}\" must lower to exactly the shared cast-cap refusal gap \
             (see UNREPRESENTABLE_CAST_CAP_GAP in parser::oracle_effect)"
        );
        assert_eq!(
            free_cast_outcome_for(bound),
            FreeCastOutcome::Refused,
            "\"up to {bound}\" must be refused outright. A window would carry a bound the \
             engine invented (wrapped, zeroed, or unbounded); an uncapped permission would \
             let the whole graveyard/hand pool be cast for free at a later priority. Both \
             are strictly more permissive than the printed instruction (CR 608.2c)."
        );
    }
}
