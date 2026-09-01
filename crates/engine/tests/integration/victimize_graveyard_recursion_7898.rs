//! Coverage for issue #7898: Victimize is a silent no-op — it neither
//! sacrifices a creature nor returns the chosen graveyard cards.
//!
//! CARD TEXT (verified verbatim against the Scryfall API, `cards/named?exact=Victimize`):
//!   Victimize — {2}{B} Sorcery — "Choose two target creature cards in your
//!   graveyard. Sacrifice a creature. If you do, return the chosen cards to the
//!   battlefield tapped."
//!
//! The card parses correctly (zero `Effect::Unimplemented`); the RUNTIME defect
//! that made it vacuous is in the sacrifice resolver:
//!
//! The `Sacrifice a creature.` instruction carries a non-anaphoric filter with
//! no controller clause, so it INHERITED the parent instruction's two object
//! targets — creature cards in a GRAVEYARD. That made the targeted set
//! non-empty (suppressing the untargeted battlefield pool) while the targeted
//! loop then skipped every inherited card on its `zone != Battlefield` guard.
//! Net result: zero sacrifices, and therefore no `If you do` rider either.
//!
//! CR 701.21a: sacrifice moves a permanent from the BATTLEFIELD to its owner's
//! graveyard — a graveyard card can never be sacrificed.
//! CR 115.1: an effect's targets are only the ones its own filter declares.
//!
//! Once the graveyard cards are dropped from the sacrifice's targeted set, the
//! sacrifice reaches the untargeted battlefield pool, actually happens, and the
//! `If you do` consequence (CR 608.2c) fires on both the mandatory fast path
//! and the interactive `EffectZoneChoice` path — V1/V2 cover one each.
//!
//! CR 400.7 (V9): an object that changes zones becomes a NEW object with no
//! relation to its previous existence, so a chosen card that leaves the
//! graveyard before resolution is not returned — and no substitute is returned
//! in its place.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const VICTIMIZE_ORACLE: &str = "Choose two target creature cards in your graveyard. \
Sacrifice a creature. If you do, return the chosen cards to the battlefield tapped.";

/// Staged fixture: Victimize in P0's hand, `graveyard_names` creature cards in
/// P0's graveyard, and `battlefield_names` creatures P0 controls.
///
/// Both players get a stocked library: a draw on an empty library decks a player
/// into `GameOver` (CR 104.3c) and would contaminate every assertion below.
struct Fixture {
    runner: GameRunner,
    victimize: ObjectId,
    graveyard: Vec<ObjectId>,
    battlefield: Vec<ObjectId>,
}

fn setup(graveyard_names: &[&str], battlefield_names: &[&str]) -> Fixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // {2}{B}: three black units cover the generic shards too.
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );

    // Neither player may deck out mid-test.
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    scenario.with_library_top(P1, &["Filler D", "Filler E", "Filler F"]);

    let victimize = {
        let b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Victimize",
            /* is_instant */ false,
            VICTIMIZE_ORACLE,
        );
        b.id()
    };

    let graveyard: Vec<ObjectId> = graveyard_names
        .iter()
        .map(|name| scenario.add_creature_to_graveyard(P0, name, 2, 2).id())
        .collect();

    let battlefield: Vec<ObjectId> = battlefield_names
        .iter()
        .map(|name| scenario.add_creature(P0, name, 1, 1).id())
        .collect();

    Fixture {
        runner: scenario.build(),
        victimize,
        graveyard,
        battlefield,
    }
}

/// V1 — the headline regression. Two chosen graveyard creatures return to the
/// battlefield tapped, and the fodder creature is sacrificed.
///
/// REVERT-FAILING ASSERTION: `assert_zone(&[fodder], Zone::Graveyard)` — before
/// the `sacrifice.rs` retain, the inherited graveyard targets suppressed the
/// untargeted pool and NOTHING was sacrificed (fodder stayed on the battlefield).
#[test]
fn victimize_sacrifices_fodder_and_returns_both_chosen_cards_tapped() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder"]);
    let fodder = battlefield[0];

    let outcome = runner
        .cast(victimize)
        .target_objects(&[graveyard[0], graveyard[1]])
        .effect_zone(&[fodder])
        .resolve();

    // CR 701.21a: the sacrificed permanent moves from the battlefield to its
    // owner's graveyard.
    outcome.assert_zone(&[fodder], Zone::Graveyard);

    // CR 608.2c: "If you do" fired, so both chosen cards returned...
    outcome.assert_zone(&[graveyard[0], graveyard[1]], Zone::Battlefield);
    // ...tapped.
    assert!(
        outcome.is_tapped(graveyard[0]) && outcome.is_tapped(graveyard[1]),
        "CR 608.2c: both returned creatures must enter tapped; got {:?}/{:?}",
        outcome.is_tapped(graveyard[0]),
        outcome.is_tapped(graveyard[1])
    );
}

/// V2 — the sacrifice half in isolation: exactly ONE creature is sacrificed,
/// not two (one per chosen card) and not zero.
#[test]
fn victimize_sacrifices_exactly_one_creature() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder", "Spare"]);
    let (fodder, spare) = (battlefield[0], battlefield[1]);

    let outcome = runner
        .cast(victimize)
        .target_objects(&[graveyard[0], graveyard[1]])
        .effect_zone(&[fodder])
        .resolve();

    // CR 701.21a: `count: Fixed(1)` — the chosen fodder and ONLY the fodder.
    outcome.assert_zone(&[fodder], Zone::Graveyard);
    outcome.assert_zone(&[spare], Zone::Battlefield);
}

/// V3 — the returned cards are the CHOSEN ones and they enter under their
/// owner's control on the battlefield, not merely "somewhere else".
#[test]
fn victimize_returned_cards_are_controlled_by_caster() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder"]);

    let outcome = runner
        .cast(victimize)
        .target_objects(&[graveyard[0], graveyard[1]])
        .effect_zone(&[battlefield[0]])
        .resolve();

    outcome.assert_zone(&[graveyard[0], graveyard[1]], Zone::Battlefield);
    for &id in &graveyard {
        assert_eq!(
            outcome.controller(id),
            P0,
            "returned card {id:?} must be controlled by Victimize's controller"
        );
    }
}

/// V4 — the spell finishes cleanly: no dangling prompt is left behind once the
/// sacrifice selection has been answered and the rider has resolved.
#[test]
fn victimize_resolution_completes_without_dangling_prompt() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder"]);

    let outcome = runner
        .cast(victimize)
        .target_objects(&[graveyard[0], graveyard[1]])
        .effect_zone(&[battlefield[0]])
        .resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Victimize must resolve to a clean priority window, halted at {:?}",
        outcome.final_waiting_for()
    );
    // POSITIVE REACH-GUARD: the spell actually did its work before halting.
    outcome.assert_zone(&[graveyard[0], graveyard[1]], Zone::Battlefield);
}

/// V5 — the sacrifice draws from the BATTLEFIELD pool, never from the
/// graveyard cards the spell targeted. CR 701.21a.
///
/// REVERT-FAILING ASSERTION: the graveyard cards were the inherited "targets"
/// under the old behavior; this proves the fodder on the battlefield is what
/// actually got sacrificed.
#[test]
fn victimize_sacrifice_pool_is_battlefield_not_graveyard() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder"]);
    let fodder = battlefield[0];

    let outcome = runner
        .cast(victimize)
        .target_objects(&[graveyard[0], graveyard[1]])
        .effect_zone(&[fodder])
        .resolve();

    // The battlefield creature is the one that left.
    outcome.assert_zone(&[fodder], Zone::Graveyard);
    // And the graveyard cards moved the OTHER way (they were returned, not
    // consumed as sacrifice fodder).
    outcome.assert_zone(&[graveyard[0], graveyard[1]], Zone::Battlefield);
}

/// V6 — NEGATIVE: with no creature on the battlefield to sacrifice, the
/// `If you do` rider is suppressed and the chosen cards STAY in the graveyard.
/// CR 609.3: the effect does only as much as possible.
///
/// PAIRED POSITIVE REACH-GUARD: `victimize_rider_fires_when_sacrifice_happens`
/// below runs the IDENTICAL fixture shape plus one fodder creature and asserts
/// the cards DO return — proving this negative is not passing merely because
/// the spell fizzled upstream (e.g. failed to be cast or target).
#[test]
fn victimize_rider_suppressed_when_no_creature_to_sacrifice() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &[]);
    assert!(
        battlefield.is_empty(),
        "fixture intent: no sacrifice fodder exists"
    );

    let outcome = runner
        .cast(victimize)
        .target_objects(&[graveyard[0], graveyard[1]])
        .resolve();

    // POSITIVE REACH-GUARD (in-test): the spell was cast and left the hand, so
    // resolution genuinely ran and the negative below is not vacuous.
    assert_ne!(
        outcome.zone_of(victimize),
        Zone::Hand,
        "reach-guard: Victimize must have been cast and resolved"
    );

    // CR 608.2c: no sacrifice occurred, so the dependent clause does nothing.
    outcome.assert_zone(&[graveyard[0], graveyard[1]], Zone::Graveyard);
}

/// V6-PAIR — the positive twin of V6 on the identical fixture shape, differing
/// only by the presence of one sacrificeable creature. This is what makes V6's
/// negative assertion non-vacuous.
#[test]
fn victimize_rider_fires_when_sacrifice_happens() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder"]);

    let outcome = runner
        .cast(victimize)
        .target_objects(&[graveyard[0], graveyard[1]])
        .effect_zone(&[battlefield[0]])
        .resolve();

    assert_ne!(
        outcome.zone_of(victimize),
        Zone::Hand,
        "reach-guard: Victimize must have been cast and resolved"
    );
    // Same fixture shape as V6, opposite outcome — the ONLY difference is that
    // a sacrifice was possible.
    outcome.assert_zone(&[graveyard[0], graveyard[1]], Zone::Battlefield);
}

/// V7 — IDENTITY DISCRIMINATION. Four distinct creature cards are staged in the
/// graveyard; exactly TWO are chosen. Asserts BIDIRECTIONALLY BY `ObjectId`:
/// both chosen ids are on the battlefield AND tapped, and EACH non-chosen id is
/// still in the graveyard.
///
/// This is the test that proves the fix honours the player's actual choice.
/// Counting battlefield creatures, or asserting on card names, cannot detect a
/// wrong-cards bug; only per-id assertions can. CR 115.1: the chosen targets are
/// fixed on announcement and cannot be swapped during resolution.
#[test]
fn victimize_returns_exactly_the_two_chosen_cards_by_identity() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(
        &["Grave One", "Grave Two", "Grave Three", "Grave Four"],
        &["Fodder"],
    );
    assert_eq!(graveyard.len(), 4, "fixture intent: four distinct choices");

    // Deliberately choose a non-adjacent, non-leading pair so a naive
    // "first two" or "all of them" implementation cannot coincide with correct.
    let chosen = [graveyard[1], graveyard[3]];
    let not_chosen = [graveyard[0], graveyard[2]];

    let outcome = runner
        .cast(victimize)
        .target_objects(&chosen)
        .effect_zone(&[battlefield[0]])
        .resolve();

    // Direction 1: each CHOSEN id is on the battlefield, tapped.
    for &id in &chosen {
        assert_eq!(
            outcome.zone_of(id),
            Zone::Battlefield,
            "chosen card {id:?} must be returned to the battlefield"
        );
        assert!(
            outcome.is_tapped(id),
            "chosen card {id:?} must enter tapped"
        );
    }

    // Direction 2: each NON-CHOSEN id is still in the graveyard, untouched.
    for &id in &not_chosen {
        assert_eq!(
            outcome.zone_of(id),
            Zone::Graveyard,
            "unchosen card {id:?} must remain in the graveyard"
        );
    }
}

/// V8 — the returned cards enter TAPPED specifically (not merely "on the
/// battlefield"), isolated from V1's combined assertion, and the sacrificed
/// fodder is confirmed as the untapped-pool source.
#[test]
fn victimize_returned_cards_enter_tapped() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder"]);

    let outcome = runner
        .cast(victimize)
        .target_objects(&[graveyard[0], graveyard[1]])
        .effect_zone(&[battlefield[0]])
        .resolve();

    for &id in &graveyard {
        assert_eq!(outcome.zone_of(id), Zone::Battlefield);
        assert!(
            outcome.is_tapped(id),
            "CR 608.2c: `return the chosen cards to the battlefield tapped` — \
             {id:?} entered untapped"
        );
    }
}

/// V9 — CR 400.7 STALE-PIN DROP. One chosen card leaves the graveyard before
/// resolution, so its incarnation changes and it is no longer the object that
/// was targeted.
///
/// Asserts three things: the stale card is NOT returned; NO substitute is
/// returned in its place (every non-chosen graveyard id is still in the
/// graveyard); and the SURVIVING chosen card IS returned tapped — the last of
/// which is the positive reach-guard making the two negatives non-vacuous.
///
/// This is the case that would catch a regression to a battlefield-only
/// tracked-set rescan, which would silently substitute an arbitrary card.
#[test]
fn victimize_stale_chosen_card_is_dropped_without_substitution() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(
        &["Grave One", "Grave Two", "Grave Three", "Grave Four"],
        &["Fodder"],
    );

    let chosen = [graveyard[0], graveyard[1]];
    let not_chosen = [graveyard[2], graveyard[3]];
    let (stale, survivor) = (chosen[0], chosen[1]);

    // `.commit()` stops at the stack-commit boundary (CR 601.2a): targets are
    // locked in, but resolution has not begun.
    let mut commit = runner
        .cast(victimize)
        .target_objects(&chosen)
        .effect_zone(&[battlefield[0]])
        .commit();

    // CR 400.7: move the first chosen card out of the graveyard AFTER targets
    // are locked in but BEFORE the spell resolves. It becomes a new object with
    // no relation to the targeted one. Routed through the real zone primitive
    // rather than hand-patching state, so every zone ledger stays consistent.
    let mut drift_events = Vec::new();
    engine::game::zones::move_to_zone(commit.state_mut(), stale, Zone::Exile, &mut drift_events);
    assert_eq!(
        commit.state().objects[&stale].zone,
        Zone::Exile,
        "fixture intent: the chosen card left the graveyard before resolution"
    );

    let outcome = commit.resolve();

    // NEGATIVE 1: the stale card is not returned to the battlefield.
    assert_ne!(
        outcome.zone_of(stale),
        Zone::Battlefield,
        "CR 400.7: a card that changed zones is a new object and must not be returned"
    );

    // NEGATIVE 2: nothing was substituted for it — every unchosen card is still
    // in the graveyard.
    for &id in &not_chosen {
        assert_eq!(
            outcome.zone_of(id),
            Zone::Graveyard,
            "CR 400.7: no substitute may be returned in the stale card's place; \
             {id:?} was silently swapped in"
        );
    }

    // POSITIVE REACH-GUARD: the surviving chosen card DID return, tapped —
    // proving resolution ran the return effect at all.
    assert_eq!(
        outcome.zone_of(survivor),
        Zone::Battlefield,
        "the still-legal chosen card must be returned"
    );
    assert!(
        outcome.is_tapped(survivor),
        "the still-legal chosen card must enter tapped"
    );
}
