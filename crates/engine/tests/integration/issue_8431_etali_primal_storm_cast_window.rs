//! Issue #8431 — Etali, Primal Storm: "Whenever Etali attacks, exile the
//! top card of each player's library, then you may cast any number of spells
//! from among those cards without paying their mana costs."
//!
//! https://github.com/phase-rs/phase/issues/8431
//!
//! Reported bug: the attack trigger exiles the cards but never offers the
//! free casts — the game just moves on to the next combat step. Built from
//! the FULL verbatim Oracle text via `add_creature_from_oracle` so the parsed
//! `ExileTop` + `CastFromZone ResolutionWindow` chain is exercised exactly as
//! production sees it.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Etali, Primal Storm — full verbatim Oracle text (card-data.json key
/// "etali, primal storm").
const ETALI_ORACLE: &str = "Whenever Etali attacks, exile the top card of each player's library, then you may cast any number of spells from among those cards without paying their mana costs.";

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// CR 508.2 + CR 608.2g: Etali attacks, the trigger exiles the top card of
/// EACH player's library, and the free-cast window opens offering both exiled
/// spells. On the bug the trigger resolves to exile with no `CastOffer` —
/// combat just continues — so the window match below panics.
#[test]
fn etali_attack_trigger_opens_free_cast_window_for_exiled_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Storm", 6, 6, ETALI_ORACLE)
        .id();
    // Known castable spells on top of each library. Both carry a trivial
    // resolvable effect so the free cast leaves the stack on resolution.
    // The P0 filler sits beneath P0's top so the P0-controlled "Draw a card"
    // resolution draws a real card instead of decking P0 mid-test.
    scenario.add_card_to_library_top(P0, "P0 Filler");
    let p0_spell = scenario
        .add_spell_to_library_top(P0, "P0 Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Draw a card.")
        .id();
    let p1_spell = scenario
        .add_spell_to_library_top(P1, "P1 Divination", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etali, AttackTarget::Player(P1))])
        .expect("P0 attacks with Etali");

    // CR 508.2: after attackers are declared the active player gets priority;
    // both sides pass so the attack trigger resolves.
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    // Reach-guards: the trigger resolved and exiled the top card of EACH
    // library — the effect ran (not a vacuous no-op).
    assert_eq!(
        zone_of(&runner, p0_spell),
        Zone::Exile,
        "reach-guard: P0's top card must be exiled by the trigger"
    );
    assert_eq!(
        zone_of(&runner, p1_spell),
        Zone::Exile,
        "reach-guard: P1's top card must be exiled by the trigger"
    );

    // PRIMARY: the free-cast window must open offering both exiled spells,
    // with no cast cap ("any number" is unbounded — CR 608.2g contemplates
    // casting other spells during resolution with no stated cap).
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            player,
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    ..
                },
        } => {
            assert_eq!(player, P0, "Etali's controller is offered the casts");
            assert_eq!(
                remaining_casts, None,
                "any number of spells: the window must be unbounded"
            );
            assert!(
                candidates.contains(&p0_spell),
                "P0's exiled spell must be a candidate; got {candidates:?}"
            );
            assert!(
                candidates.contains(&p1_spell),
                "P1's exiled spell must be a candidate; got {candidates:?}"
            );
            assert_eq!(
                candidates.len(),
                2,
                "exactly the two exiled spells are offered; got {candidates:?}"
            );
        }
        other => panic!("expected the Etali free-cast window to open, got {other:?}"),
    }

    // Free-cast P1's spell: it goes onto the stack during resolution at no
    // cost (CR 118.9 + CR 608.2g).
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(p1_spell),
        })
        .expect("free-casting the exiled spell must succeed");
    assert_eq!(
        zone_of(&runner, p1_spell),
        Zone::Stack,
        "the free-cast spell must be on the stack"
    );

    // Decline the rest — the window closes and the trigger finishes.
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the remaining free casts must succeed");

    // The free-cast spell resolves (draws a card) and goes to its owner's
    // graveyard — no exile rider on Etali.
    runner.advance_until_stack_empty();
    assert_eq!(
        zone_of(&runner, p1_spell),
        Zone::Graveyard,
        "the resolved free-cast spell goes to the graveyard"
    );
}

/// CR 608.2g ("any number" is unbounded): after the first free cast the
/// window re-offers the remaining exiled spell — the report's "no chance to
/// cast one of those cards" could be a broken re-offer rather than a missing
/// window. Both spells are cast free and both resolve.
#[test]
fn etali_window_reoffers_so_any_number_of_spells_can_be_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Storm", 6, 6, ETALI_ORACLE)
        .id();
    // Fillers sit BENEATH the tops (added first: `insert(0, …)` pushes them
    // down). BOTH free casts are controlled by P0, so both "Draw a card"
    // resolutions draw from P0's library — it needs two fillers or P0 decks
    // himself mid-test and the loss-removal confounds the zone assertions.
    scenario.add_card_to_library_top(P0, "P0 Filler");
    scenario.add_card_to_library_top(P0, "P0 Filler Two");
    scenario.add_card_to_library_top(P1, "P1 Filler");
    let p0_spell = scenario
        .add_spell_to_library_top(P0, "P0 Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Draw a card.")
        .id();
    let p1_spell = scenario
        .add_spell_to_library_top(P1, "P1 Divination", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etali, AttackTarget::Player(P1))])
        .expect("P0 attacks with Etali");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    assert_eq!(zone_of(&runner, p0_spell), Zone::Exile);
    assert_eq!(zone_of(&runner, p1_spell), Zone::Exile);

    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(p0_spell),
        })
        .expect("first free cast must succeed");
    assert_eq!(
        zone_of(&runner, p0_spell),
        Zone::Stack,
        "the first free-cast spell must be on the stack"
    );

    // The window must re-offer with only the second spell remaining.
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            assert!(
                candidates.contains(&p1_spell),
                "the second spell must be re-offered; got {candidates:?}"
            );
            assert!(
                !candidates.contains(&p0_spell),
                "the already-cast spell must not be re-offered; got {candidates:?}"
            );
        }
        other => panic!("the window must re-offer after the first cast, got {other:?}"),
    }

    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(p1_spell),
        })
        .expect("second free cast must succeed");
    assert_eq!(
        zone_of(&runner, p1_spell),
        Zone::Stack,
        "the second free-cast spell must be on the stack"
    );
    // Both casts coexist on the stack: announcing the second must not disturb
    // the first (no yank-back-to-exile on re-offer).
    assert_eq!(
        (zone_of(&runner, p0_spell), zone_of(&runner, p1_spell)),
        (Zone::Stack, Zone::Stack),
        "both free-cast spells must be on the stack together; waiting_for={:?}",
        runner.state().waiting_for
    );
    // Exhausted window auto-closes: no candidates remain, so the trigger
    // finishes and priority returns to the active player — no lingering
    // empty offer.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the exhausted window must close back to priority"
    );

    runner.advance_until_stack_empty();
    assert_eq!(
        zone_of(&runner, p0_spell),
        Zone::Graveyard,
        "the first free-cast spell resolves to the graveyard"
    );
    assert_eq!(
        zone_of(&runner, p1_spell),
        Zone::Graveyard,
        "the second free-cast spell resolves to the graveyard"
    );
}

/// CR 601.2c + CR 608.2g: an exiled spell WITH a target (Lightning Bolt) can
/// be free-cast through the window — target selection happens as part of the
/// resolution-time cast and the spell resolves for full effect.
#[test]
fn etali_window_casts_targeted_spell_with_target_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Storm", 6, 6, ETALI_ORACLE)
        .id();
    let bolt = scenario
        .add_spell_to_library_top(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();
    let other = scenario
        .add_spell_to_library_top(P1, "P1 Divination", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etali, AttackTarget::Player(P1))])
        .expect("P0 attacks with Etali");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    assert_eq!(zone_of(&runner, bolt), Zone::Exile);
    assert_eq!(zone_of(&runner, other), Zone::Exile);

    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(bolt),
        })
        .expect("choosing the Bolt must succeed");

    // The Bolt needs a target: answer its target selection, aiming at P1.
    match runner.state().waiting_for.clone() {
        WaitingFor::TargetSelection { .. } => {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                })
                .expect("targeting P1 with the free-cast Bolt must succeed");
        }
        other => panic!("expected target selection for the Bolt, got {other:?}"),
    }
    assert_eq!(
        zone_of(&runner, bolt),
        Zone::Stack,
        "the targeted free-cast spell must reach the stack"
    );

    // One candidate remains, so the window must re-offer (not auto-close);
    // decline it unconditionally.
    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            assert_eq!(
                candidates,
                vec![other],
                "only the uncast spell is re-offered; got {candidates:?}"
            );
        }
        other => panic!("expected the window to re-offer the rest, got {other:?}"),
    }
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the rest must succeed");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "CR 120.3a: the free-cast Bolt must deal its 3 damage to P1"
    );
}

/// CR 601.2 vs CR 305.1 (a land is never cast — it is played): Etali
/// exiles a land and a spell — the window opens offering ONLY the spell. The
/// land stays exiled, uncastable but correctly excluded rather than breaking
/// the offer.
#[test]
fn etali_window_offers_spell_but_excludes_exiled_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Storm", 6, 6, ETALI_ORACLE)
        .id();
    // The P0 filler sits beneath P0's top so the P0-controlled "Draw a card"
    // resolution draws a real card instead of decking P0 mid-test.
    scenario.add_card_to_library_top(P0, "P0 Filler");
    let land = scenario.add_land_to_library_top(P0, "Plains").id();
    let spell = scenario
        .add_spell_to_library_top(P1, "P1 Divination", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etali, AttackTarget::Player(P1))])
        .expect("P0 attacks with Etali");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    // Reach-guards: both cards were exiled — the trigger ran.
    assert_eq!(
        zone_of(&runner, land),
        Zone::Exile,
        "reach-guard: the land must be exiled"
    );
    assert_eq!(
        zone_of(&runner, spell),
        Zone::Exile,
        "reach-guard: the spell must be exiled"
    );

    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            assert_eq!(
                candidates,
                vec![spell],
                "only the exiled spell is offered, never the land; got {candidates:?}"
            );
        }
        other => panic!("expected the window with the lone spell, got {other:?}"),
    }

    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(spell),
        })
        .expect("free-casting the spell must succeed");
    // Exhausted window auto-closes: the land was never a candidate, so no
    // offer remains — the trigger finishes back to priority.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 },
        "the exhausted window must close back to priority"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        zone_of(&runner, spell),
        Zone::Graveyard,
        "the free-cast spell resolves to the graveyard"
    );
    assert_eq!(
        zone_of(&runner, land),
        Zone::Exile,
        "the land stays exiled — it was never castable"
    );
}

/// CR 608.2d (a player can't choose an impossible option): Etali exiles two
/// lands — there is nothing any spell could be cast from, so NO window opens
/// and combat continues normally. This pins the correct no-window outcome:
/// "exiled cards but no prompt" with all-land (or otherwise uncastable)
/// exiles is rules-correct, not the reported defect.
#[test]
fn etali_exiling_only_lands_opens_no_window_and_combat_continues() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Storm", 6, 6, ETALI_ORACLE)
        .id();
    let p0_land = scenario.add_land_to_library_top(P0, "Plains").id();
    let p1_land = scenario.add_land_to_library_top(P1, "Mountain").id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etali, AttackTarget::Player(P1))])
        .expect("P0 attacks with Etali");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    // Reach-guards: the trigger ran — both lands are exiled.
    assert_eq!(
        zone_of(&runner, p0_land),
        Zone::Exile,
        "reach-guard: P0's land must be exiled"
    );
    assert_eq!(
        zone_of(&runner, p1_land),
        Zone::Exile,
        "reach-guard: P1's land must be exiled"
    );

    // No castable exile: no window. Combat must be sitting at a normal
    // post-trigger point (priority), never a CastOffer.
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }),
        "no castable exile means no cast window; got {:?}",
        runner.state().waiting_for
    );

    // Combat continues normally: P1 controls no creatures, so there is no
    // block to declare — drive priority until the unblocked 6/6 Etali deals
    // its combat damage to P1.
    let p1_life_before = runner.life(P1);
    for _ in 0..32 {
        if runner.life(P1) < p1_life_before {
            break;
        }
        // The window must never appear while combat runs its course.
        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::CastOffer { .. }),
            "no cast window may open on the all-land exile; got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("priority must pass while combat continues");
    }
    assert_eq!(
        runner.life(P1),
        p1_life_before - 6,
        "combat must continue: the unblocked Etali deals 6 to P1"
    );
    assert_eq!(
        zone_of(&runner, etali),
        Zone::Battlefield,
        "Etali survives the unblocked attack"
    );
}

/// Three players — Etali exiles the top card of EACH library and the window
/// offers all three. Pins the `player_scope: All` fan-out batch binding
/// beyond the two-player case.
#[test]
fn etali_window_offers_exile_from_every_library_in_multiplayer() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 7_431);
    scenario.at_phase(Phase::PreCombatMain);

    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Storm", 6, 6, ETALI_ORACLE)
        .id();
    let p0_spell = scenario
        .add_spell_to_library_top(P0, "P0 Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Draw a card.")
        .id();
    let p1_spell = scenario
        .add_spell_to_library_top(P1, "P1 Divination", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("Draw a card.")
        .id();
    let p2_spell = scenario
        .add_spell_to_library_top(p2, "P2 Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etali, AttackTarget::Player(P1))])
        .expect("P0 attacks with Etali");
    // Three seats pass priority so the attack trigger resolves.
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");
    runner.act(GameAction::PassPriority).expect("p2 pass");

    assert_eq!(zone_of(&runner, p0_spell), Zone::Exile);
    assert_eq!(zone_of(&runner, p1_spell), Zone::Exile);
    assert_eq!(zone_of(&runner, p2_spell), Zone::Exile);

    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            player,
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
        } => {
            assert_eq!(player, P0);
            assert_eq!(
                candidates.len(),
                3,
                "all three exiled spells must be offered; got {candidates:?}"
            );
            assert!(candidates.contains(&p0_spell));
            assert!(candidates.contains(&p1_spell));
            assert!(candidates.contains(&p2_spell));
        }
        other => panic!("expected the three-candidate window, got {other:?}"),
    }
}

/// CR 609.3 (do as much as possible): P1's library is empty, so only P0's
/// top card is exiled — the window still opens offering that one spell
/// rather than collapsing to nothing.
#[test]
fn etali_window_opens_for_lone_exile_when_a_library_is_empty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Storm", 6, 6, ETALI_ORACLE)
        .id();
    // Only P0 has a library top; P1's library stays empty.
    let spell = scenario
        .add_spell_to_library_top(P0, "P0 Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Draw a card.")
        .id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(etali, AttackTarget::Player(P1))])
        .expect("P0 attacks with Etali");
    runner.act(GameAction::PassPriority).expect("p0 pass");
    runner.act(GameAction::PassPriority).expect("p1 pass");

    assert_eq!(
        zone_of(&runner, spell),
        Zone::Exile,
        "reach-guard: P0's top card must be exiled"
    );

    match runner.state().waiting_for.clone() {
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { candidates, .. },
            ..
        } => {
            assert_eq!(
                candidates,
                vec![spell],
                "the lone exiled spell must be offered; got {candidates:?}"
            );
        }
        other => panic!("expected the window with the lone exile, got {other:?}"),
    }
}
