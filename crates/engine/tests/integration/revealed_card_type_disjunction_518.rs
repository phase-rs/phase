//! Regression (issue #518): a printed card-type DISJUNCTION in an
//! "If it's a[n] X or Y card, …" reveal gate must retain BOTH legs.
//!
//! Before the fix the reveal-gate body reduced the parsed type phrase to its
//! LAST word — `take_until(" card")` yields "creature or land", then
//! `rsplit(' ').next()` kept only "land" — so the condition lowered to
//! `RevealedHasCardType { card_types: [Land] }`. The evaluator matches
//! `card_types` with `any` (CR 205.2b), so revealing a CREATURE failed the gate
//! and the card's rider never fired.
//!
//! `track_down_draws_on_revealed_creature` is the REVERT DISCRIMINATOR: it
//! drives Track Down's real Oracle text through the cast pipeline with a
//! creature on top and asserts the card is drawn. With the parser change
//! reverted the gate is `[Land]` only, the revealed creature fails it, and no
//! card is drawn — the assertion flips. `track_down_draws_on_revealed_land` is
//! the sibling control: it passes both before and after the fix, which is what
//! proves the creature test discriminates the parser change rather than the
//! scenario simply being miswired.
//!
//! Track Down carries this suite rather than Hidetsugu and Kairi because
//! Hidetsugu's rider is an OPTIONAL during-resolution free cast that does not
//! currently reach the player for EITHER printed type — a defect separate from
//! this parser fix, pinned by the last test in this file.
//!
//! CR references:
//! - CR 205.2a / CR 205.2b — the card-type list, and "objects satisfy the
//!   criteria for any effect that applies to any of their card types" (why a
//!   printed disjunction lowers to an any-matched set).
//! - CR 608.2c — the "If it's a [type] card" resolution-time gate.
//! - CR 700.4 — "dies" means put into a graveyard from the battlefield.

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (verified against `client/public/card-data.json`).
const TRACK_DOWN: &str = "Scry 3, then reveal the top card of your library. If it's a creature or land card, draw a card.";

const HIDETSUGU: &str = "Flying\nWhen Hidetsugu and Kairi enters, draw three cards, then put two cards from your hand on top of your library in any order.\nWhen Hidetsugu and Kairi dies, exile the top card of your library. Target opponent loses life equal to its mana value. If it's an instant or sorcery card, you may cast it without paying its mana cost.";

const DESTROY: &str = "Destroy target creature.";

/// Cast Track Down with a single card of `core_type` on top of P0's library and
/// return the net cards drawn.
///
/// The library is reduced to exactly the staged card so "the top card of your
/// library" is deterministic; scry (CR 701.22a) is auto-answered by the driver,
/// which keeps looked-at cards on top.
fn track_down_cards_drawn(core_type: CoreType) -> i64 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let track_down = scenario
        .add_spell_to_hand_from_oracle(P0, "Track Down", false, TRACK_DOWN)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let top = scenario.add_card_to_library_top(P0, "Staged Top Card");
    let mut runner = scenario.build();

    {
        let obj = runner.state_mut().objects.get_mut(&top).unwrap();
        obj.card_types.core_types = vec![core_type];
        obj.base_card_types = obj.card_types.clone();
    }
    {
        let player = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap();
        player.library.retain(|&id| id == top);
        assert_eq!(
            player.library.len(),
            1,
            "precondition: library must hold exactly the staged top card"
        );
    }

    let outcome = runner.cast(track_down).resolve();
    outcome.hand_drawn(P0)
}

/// CR 608.2c + CR 205.2b: revealing a CREATURE must satisfy the
/// "creature or land card" gate and draw a card.
///
/// REVERT DISCRIMINATOR — with the disjunction fix reverted the gate retains
/// only `[Land]`, the revealed creature fails it, and no card is drawn.
#[test]
fn track_down_draws_on_revealed_creature() {
    assert_eq!(
        track_down_cards_drawn(CoreType::Creature),
        1,
        "a revealed creature must satisfy the \"creature or land card\" gate"
    );
}

/// SIBLING CONTROL: revealing a LAND also satisfies the gate and draws.
///
/// This passed both before and after the fix (the pre-fix gate retained exactly
/// the `land` leg), so its continued success proves the creature test above
/// discriminates the parser change rather than the scenario being miswired.
#[test]
fn track_down_draws_on_revealed_land() {
    assert_eq!(
        track_down_cards_drawn(CoreType::Land),
        1,
        "a revealed land must satisfy the \"creature or land card\" gate"
    );
}

/// NEGATIVE: a card matching NEITHER printed leg must not draw.
///
/// Pairs with the two positives above as the reach-guard set — together they
/// prove the gate is genuinely evaluating both legs rather than having been
/// widened into an always-true condition.
#[test]
fn track_down_does_not_draw_on_revealed_instant() {
    assert_eq!(
        track_down_cards_drawn(CoreType::Instant),
        0,
        "a revealed instant matches neither printed leg and must not draw"
    );
}

/// DOCUMENTED SEPARATE DEFECT (not fixed by this PR).
///
/// Hidetsugu and Kairi's dies trigger resolves its first two clauses correctly —
/// the top card is exiled and the targeted opponent loses life equal to its mana
/// value, which proves the anaphoric subject binds. But the third clause, an
/// OPTIONAL during-resolution free cast (`CastFromZone { target: ParentTarget,
/// driver: DuringResolution, optional: true }`), never reaches the player:
/// resolution completes to a clean `Priority` window with the card still in
/// exile and no cast offered — for an INSTANT and a SORCERY alike.
///
/// Because the free cast is unreachable for BOTH printed types it cannot
/// discriminate this parser fix, and asserting on it as though it did would be
/// vacuous (foot-gun 6 in the `card-test` skill). This test pins the CURRENT
/// behavior so the separate defect stays visible and regression-tracked; when
/// that path is fixed, this expectation flips deliberately.
#[test]
fn hidetsugu_free_cast_currently_unreachable_for_either_type() {
    for top_is_instant in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let hidetsugu = scenario
            .add_creature_from_oracle(P0, "Hidetsugu and Kairi", 5, 4, HIDETSUGU)
            .with_subtypes(vec!["Ogre", "Demon", "Dragon"])
            .id();
        let top: ObjectId = scenario
            .add_spell_to_library_top(P0, "Staged Top Card", top_is_instant)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let destroy = scenario
            .add_spell_to_hand_from_oracle(P0, "Murder", true, DESTROY)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        {
            let player = runner
                .state_mut()
                .players
                .iter_mut()
                .find(|p| p.id == P0)
                .unwrap();
            player.library.retain(|&id| id == top);
        }

        runner.cast(destroy).target_objects(&[hidetsugu]).resolve();

        // POSITIVE reach-guards: the trigger really did fire and resolve.
        // Without these the negative below would pass even if nothing happened.
        assert_eq!(
            runner.state().objects[&hidetsugu].zone,
            Zone::Graveyard,
            "reach-guard: Hidetsugu must have died (CR 700.4)"
        );
        assert_eq!(
            runner.state().objects[&top].zone,
            Zone::Exile,
            "reach-guard: the dies trigger must have exiled the top card"
        );

        // CURRENT (defective) behavior: the optional free cast never reaches the
        // player, so the exiled card stays in exile instead of going to the
        // stack — for an instant and a sorcery alike.
        assert_ne!(
            runner.state().objects[&top].zone,
            Zone::Stack,
            "pinning the separate during-resolution free-cast defect \
             (top_is_instant={top_is_instant})"
        );
    }
}
