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
//! sacrifice reaches the untargeted battlefield pool and actually happens.
//!
//! CR 118.12: in `[Do something]. If you do, [effect].` the `[do something]`
//! action is a COST paid on resolution, and the `If you do` clause checks
//! whether the controller started to pay it. (Its own worked example —
//! Standstill's "sacrifice this enchantment. If you do, each of that player's
//! opponents draws three cards" — is structurally identical to Victimize.) So
//! the rider fires exactly when the sacrifice cost was paid, and CR 608.2c
//! supplies only the ORDER: the return happens after the sacrifice, not before.
//!
//! That `If you do` consequence then has to fire on BOTH sacrifice completion
//! paths, which reach it through different seams:
//!   * the mandatory auto path (`sacrifice.rs`, `!up_to && eligible.len() <=
//!     count`) sacrifices inline, so the event-slice seed in `effects/mod.rs`
//!     sets the performed-flag. V1/V3-V9 drive this path.
//!   * the interactive `EffectZoneChoice` path (2+ eligible creatures) parks and
//!     returns before sacrificing, so the flag is instead seeded at the
//!     sacrifice-completion seam once the choice is answered. V10-V13 drive this
//!     path and are the only tests that assert the rider on it.
//!
//! V2 also happens to drive the interactive path, but asserts ONLY the sacrifice
//! half (fodder to the graveyard, spare untouched) — it does not cover the rider.
//!
//! MEASURED PATH-COVERAGE TABLE (battlefield creature count decides the path:
//! 0 = no eligible pool, 1 = mandatory AUTO, 2+ = interactive EffectZoneChoice):
//!
//! | # | test | bf | path | what it pins |
//! |---|------|----|------|--------------|
//! | V1 | `sacrifices_fodder_and_returns_both_chosen_cards_tapped` | 1 | auto | headline: fodder sacrificed, both chosen return tapped |
//! | V2 | `sacrifices_exactly_one_creature` | 2 | interactive | sacrifice half only (count discipline); NOT the rider |
//! | V3 | `returned_cards_are_controlled_by_caster` | 1 | auto | returned cards are controlled by the caster |
//! | V4 | `resolution_completes_without_dangling_prompt` | 1 | auto | terminal `WaitingFor::Priority` |
//! | V5 | `sacrifice_pool_is_battlefield_not_graveyard` | 1 | auto | CR 701.21a pool origin (the headline defect) |
//! | V6 | `rider_suppressed_when_no_creature_to_sacrifice` | 0 | none | CR 118.12 + CR 609.3 rider suppression (unpayable cost) — the ONLY honest suppression fixture |
//! | V6-pair | `rider_fires_when_sacrifice_happens` | 1 | auto | positive twin making V6 non-vacuous |
//! | V7 | `returns_exactly_the_two_chosen_cards_by_identity` | 1 | auto | bidirectional ObjectId identity |
//! | V8 | `returned_cards_enter_tapped` | 1 | auto | CR 118.12 tapped rider |
//! | V9 | `stale_chosen_card_is_dropped_without_substitution` | 1 | auto | CR 400.7 stale target, no substitution |
//! | V10 | `interactive_path_sacrifices_fodder_and_returns_both_chosen_cards_tapped` | 2 | interactive | headline repeated on the interactive seam |
//! | V11 | `interactive_path_returns_exactly_the_two_chosen_cards_by_identity` | 2 | interactive | identity discrimination on the interactive seam |
//! | V12 | `interactive_path_rejects_empty_mandatory_sacrifice_selection` | 2 | interactive | submits an EMPTY `SelectCards` DIRECTLY via `act()`; asserts the reducer REJECTS it (CR 608.2d + CR 701.21a), the prompt survives intact, then answers legally and drives to `Priority` with the rider fired |
//! | V13 | `interactive_path_real_selection_fires_rider` | 2 | interactive | positive counterweight to V6's suppression negative |
//!
//! V12 deliberately does NOT use the `.effect_zone(&[])` builder: the shared
//! driver short-circuits its `EffectZoneChoice` arm on
//! `if effect_zone_cards.is_empty() { break; }`, so an empty declared policy
//! never emits a `SelectCards` action and merely parks on a stalled,
//! half-resolved state. Only `runner.act(..)` reaches the reducer, which is what
//! makes the rejection assertion real rather than vacuous.
//!
//! CR 400.7 (V9): an object that changes zones becomes a NEW object with no
//! relation to its previous existence, so a chosen card that leaves the
//! graveyard before resolution is not returned — and no substitute is returned
//! in its place.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::EngineError;
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
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

    // CR 118.12: the sacrifice cost was paid, so the "If you do" clause is
    // satisfied and both chosen cards returned. CR 608.2c: the return follows
    // the sacrifice in the order written.
    outcome.assert_zone(&[graveyard[0], graveyard[1]], Zone::Battlefield);
    // ...tapped.
    assert!(
        outcome.is_tapped(graveyard[0]) && outcome.is_tapped(graveyard[1]),
        "CR 118.12: both returned creatures must enter tapped; got {:?}/{:?}",
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
///
/// CR 118.12: the sacrifice is a COST paid on resolution, and the `If you do`
/// clause checks whether the controller started to pay it. With no creature to
/// sacrifice the cost cannot be paid, so the clause is false and the rider does
/// nothing — precisely the rule's own Standstill example ("you're unable to pay
/// the 'sacrifice Standstill' cost. No player will draw cards.").
/// CR 609.3: the sacrifice instruction itself attempts something impossible, so
/// it does only as much as possible — here, nothing.
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

    // CR 118.12: the sacrifice cost was never paid, so the `If you do` clause
    // is false and the dependent effect does nothing.
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
            "CR 118.12: `return the chosen cards to the battlefield tapped` — \
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

// ---------------------------------------------------------------------------
// INTERACTIVE-PATH COVERAGE (V10-V13).
//
// `sacrifice.rs` has TWO completion paths, and they complete the "If you do"
// rider through DIFFERENT seams:
//
//   * AUTO path (`sacrifice.rs`, `!up_to && eligible.len() <= count`): when the
//     controller has exactly as many eligible creatures as the sacrifice needs,
//     the resolver sacrifices them inline. `PermanentSacrificed` lands in the
//     local event slice, so the CR 118.12 seed in `effects/mod.rs` sees it and
//     sets `optional_effect_performed` on the parent context.
//   * INTERACTIVE path (`WaitingFor::EffectZoneChoice`): with 2+ eligible
//     creatures the player must pick. The resolver returns before any sacrifice
//     happens, so the local slice holds NO `PermanentSacrificed` when that seed
//     evaluates; the flag must instead be seeded at the sacrifice-completion
//     seam once the choice is answered.
//
// Every V1-V9 test above stages exactly ONE battlefield creature (V6 stages
// none) and therefore only ever exercised the AUTO path. V2 is the sole
// exception (it stages two) and asserts ONLY the sacrifice half, never the
// rider — so the rider on the interactive path was entirely uncovered. These
// tests close that gap; each one stages 2+ battlefield creatures to force
// `EffectZoneChoice`.
//
// V10/V11/V13 answer that prompt through the `.effect_zone(&[..])` builder.
// V12 instead drives the prompt RAW (`commit()` + `advance_until_stack_empty()`
// + `runner.act(..)`) because the builder cannot express an empty submission:
// the driver breaks out of its `EffectZoneChoice` arm before acting when the
// declared pick list is empty. See the module-level path-coverage table.

/// V10 — INTERACTIVE-PATH HEADLINE. Assertion-identical to V1, but stages TWO
/// battlefield creatures so the sacrifice routes through `EffectZoneChoice`
/// instead of the mandatory auto path.
///
/// REVERT-FAILING ASSERTION: `assert_zone(&chosen, Zone::Battlefield)` — without
/// the completion-seam seed the fodder IS sacrificed but the rider never fires,
/// so both chosen cards stay in the graveyard (`object N expected in
/// Battlefield, found in Graveyard`).
#[test]
fn victimize_interactive_path_sacrifices_fodder_and_returns_both_chosen_cards_tapped() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder", "Spare"]);
    let (fodder, spare) = (battlefield[0], battlefield[1]);
    let chosen = [graveyard[0], graveyard[1]];

    let outcome = runner
        .cast(victimize)
        .target_objects(&chosen)
        .effect_zone(&[fodder])
        .resolve();

    // CR 701.21a: only the chosen fodder is sacrificed; the spare survives.
    outcome.assert_zone(&[fodder], Zone::Graveyard);
    outcome.assert_zone(&[spare], Zone::Battlefield);

    // CR 118.12: the cost was paid on this path too, so the rider fires and
    // both chosen cards return...
    outcome.assert_zone(&chosen, Zone::Battlefield);
    // ...and enter tapped.
    for &id in &chosen {
        assert!(
            outcome.is_tapped(id),
            "CR 118.12: chosen card {id:?} must return tapped on the interactive path"
        );
    }
}

/// V11 — INTERACTIVE-PATH IDENTITY DISCRIMINATION. The V7 identity assertion
/// carried onto the interactive path: four distinct graveyard creatures, two
/// battlefield creatures (forcing `EffectZoneChoice`), and a NON-ADJACENT,
/// NON-LEADING chosen pair asserted BIDIRECTIONALLY by `ObjectId`.
///
/// This is the binding requirement — the fix must accept the targets the player
/// actually chose and return exactly those — verified on the path that a real
/// game takes whenever the caster controls more than one creature.
#[test]
fn victimize_interactive_path_returns_exactly_the_two_chosen_cards_by_identity() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(
        &["Grave One", "Grave Two", "Grave Three", "Grave Four"],
        &["Fodder", "Spare"],
    );
    assert_eq!(graveyard.len(), 4, "fixture intent: four distinct choices");
    assert_eq!(
        battlefield.len(),
        2,
        "fixture intent: 2 eligible creatures force the EffectZoneChoice path"
    );

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
            "chosen card {id:?} must be returned on the interactive path"
        );
        assert!(
            outcome.is_tapped(id),
            "chosen card {id:?} must enter tapped on the interactive path"
        );
    }

    // Direction 2: each NON-CHOSEN id is untouched in the graveyard.
    for &id in &not_chosen {
        assert_eq!(
            outcome.zone_of(id),
            Zone::Graveyard,
            "unchosen card {id:?} must remain in the graveyard"
        );
    }
}

/// V12 — INTERACTIVE-PATH MANDATORY-SELECTION REJECTION. Drives the real
/// `EffectZoneChoice` prompt, submits an EMPTY `GameAction::SelectCards`
/// DIRECTLY through `apply()`, and proves the engine REJECTS it — then answers
/// the same prompt legally and drives the spell to a clean priority window.
///
/// WHY THE DIRECT SUBMISSION IS LOAD-BEARING: the `.effect_zone(&[])` builder
/// CANNOT express this. `drive_resolution` short-circuits its
/// `WaitingFor::EffectZoneChoice` arm on `if effect_zone_cards.is_empty()
/// { break; }` (`game/scenario.rs`), so an empty declared policy never emits a
/// `SelectCards` action at all — it parks on the prompt and returns a stalled,
/// half-resolved state. Only `runner.act(..)` reaches the reducer.
///
/// CR 608.2d: the player announces the sacrifice choice while applying the
/// effect and "can't choose an option that's illegal or impossible" — with a
/// non-empty eligible pool, choosing to sacrifice NOTHING is exactly such an
/// illegal option. CR 701.21a supplies WHAT may be chosen: a permanent the
/// player controls, moved from the battlefield to its owner's graveyard. The
/// engine enforces that in `engine_resolution_choices.rs` via the `!up_to`
/// branch `chosen.len() != count`, which rejects a 0-card submission against
/// `count == 1` with `InvalidAction("Must select exactly 1 card(s), got 0")`.
///
/// REVERT-FAILING ASSERTIONS: the post-rejection half. After the legal
/// selection is submitted, `assert_zone(&chosen, Zone::Battlefield)` fails
/// without the completion-seam seed (the fodder IS sacrificed, but the CR
/// 118.12 rider never fires, leaving both chosen cards in the graveyard).
#[test]
fn victimize_interactive_path_rejects_empty_mandatory_sacrifice_selection() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder", "Spare"]);
    let (fodder, spare) = (battlefield[0], battlefield[1]);
    let chosen = [graveyard[0], graveyard[1]];

    // Commit the cast, then let the stack resolve until it parks on the
    // sacrifice prompt. `advance_until_stack_empty` deliberately breaks at any
    // non-`PutAtLibraryPosition` `EffectZoneChoice`, leaving it live for us.
    runner.cast(victimize).target_objects(&chosen).commit();
    runner.advance_until_stack_empty();

    // REACH-GUARD (structural, not "it left the hand"): the interactive prompt
    // genuinely exists, is owned by the caster, offers exactly the two
    // battlefield creatures, and is a MANDATORY choice of one.
    match runner.state().waiting_for.clone() {
        WaitingFor::EffectZoneChoice {
            player,
            ref cards,
            count,
            up_to,
            effect_kind,
            ..
        } => {
            assert_eq!(player, P0, "the caster chooses the sacrifice");
            assert_eq!(
                effect_kind,
                EffectKind::Sacrifice,
                "the parked prompt must be the sacrifice, not some other zone move"
            );
            assert!(!up_to, "CR 701.21a: `Sacrifice a creature` is not optional");
            assert_eq!(count, 1, "exactly one creature must be sacrificed");
            let mut offered = cards.clone();
            offered.sort();
            let mut expected = vec![fodder, spare];
            expected.sort();
            assert_eq!(
                offered, expected,
                "both battlefield creatures are eligible fodder"
            );
        }
        other => panic!("expected the interactive sacrifice EffectZoneChoice, got {other:?}"),
    }

    // THE POINT OF THIS TEST: an empty submission goes straight to the reducer
    // and is REFUSED. CR 608.2d — the player can't choose an option that's
    // illegal; with an eligible pool, declining the mandatory sacrifice is one.
    let rejection = runner.act(GameAction::SelectCards { cards: vec![] });
    match rejection {
        Err(EngineError::InvalidAction(message)) => {
            assert!(
                message.contains("exactly 1"),
                "CR 608.2d: the mandatory sacrifice must reject a 0-card selection \
                 with a count-mismatch diagnostic, got {message:?}"
            );
        }
        other => panic!(
            "CR 608.2d + CR 701.21a: an empty mandatory sacrifice selection must be \
             REJECTED while eligible creatures exist, got {other:?}"
        ),
    }

    // The refusal is inert: nothing was sacrificed, nothing was returned, and
    // the SAME prompt is still live and re-answerable (no wedge, no state drift).
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { count: 1, .. }
        ),
        "the rejected action must leave the prompt intact, found {:?}",
        runner.state().waiting_for
    );
    for &id in &[fodder, spare] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Battlefield,
            "rejected selection must not sacrifice anything ({id:?})"
        );
    }
    for &id in &chosen {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Graveyard,
            "rejected selection must not fire the CR 118.12 rider ({id:?})"
        );
    }

    // Now answer the SAME live prompt legally and let resolution finish. This
    // is the real-resolution half the old test never reached.
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("a one-creature selection satisfies the mandatory sacrifice");
    runner.advance_until_stack_empty();

    // CR 701.21a: the chosen fodder was sacrificed; the spare survives.
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&spare].zone, Zone::Battlefield);

    // CR 118.12: the cost was paid, so the rider fires on the interactive path —
    // both chosen cards return to the battlefield tapped. THIS is the
    // revert-failing assertion.
    for &id in &chosen {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Battlefield,
            "CR 118.12: chosen card {id:?} must return after the interactive sacrifice"
        );
        assert!(
            runner.state().objects[&id].tapped,
            "CR 118.12: chosen card {id:?} must return tapped"
        );
    }

    // CR 117.3b: the completed spell leaves a clean priority window behind — no
    // dangling prompt (mirrors V4 on the interactive path).
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Victimize must resolve to a clean priority window, halted at {:?}",
        runner.state().waiting_for
    );
}

/// V13 — INTERACTIVE-PATH POSITIVE TWIN OF THE V6 SUPPRESSION NEGATIVE.
///
/// V6 (`victimize_rider_suppressed_when_no_creature_to_sacrifice`) is the
/// engine's ONLY honest rider-suppression case: with an EMPTY battlefield there
/// is no eligible fodder, so the sacrifice COST cannot be paid at all and the
/// `If you do` clause is false (CR 118.12) — the rider stays silent. (CR 609.3
/// covers the sacrifice instruction itself doing only as much as possible, which
/// on an empty pool is nothing; it is the cost check, not partial execution,
/// that gates the rider.) Suppression cannot be staged on a fixture that HAS
/// eligible creatures — with a non-empty pool, declining is an illegal choice
/// (CR 608.2d) and a sacrifice must happen (CR 701.21a), which is exactly what
/// V12 proves the reducer enforces.
///
/// This test is the positive counterweight on the INTERACTIVE path: two
/// eligible creatures, a real selection, and therefore a rider that DOES fire.
/// Together with V6 it brackets the suppression boundary from both sides
/// (no pool → silent; pool + real selection → fires) without ever asserting the
/// illegal middle state of "eligible pool, nothing sacrificed".
#[test]
fn victimize_interactive_path_real_selection_fires_rider() {
    let Fixture {
        mut runner,
        victimize,
        graveyard,
        battlefield,
    } = setup(&["Grave One", "Grave Two"], &["Fodder", "Spare"]);
    let chosen = [graveyard[0], graveyard[1]];

    let outcome = runner
        .cast(victimize)
        .target_objects(&chosen)
        .effect_zone(&[battlefield[0]])
        .resolve();

    assert_ne!(
        outcome.zone_of(victimize),
        Zone::Hand,
        "reach-guard: Victimize must have been cast and resolved"
    );

    // The one difference from V12: a sacrifice actually happened...
    outcome.assert_zone(&[battlefield[0]], Zone::Graveyard);
    // ...so the sacrifice cost was paid, the CR 118.12 rider fires, and the
    // chosen cards return.
    outcome.assert_zone(&chosen, Zone::Battlefield);
}
