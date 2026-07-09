//! Discriminating regression for **Connecting the Dots** (std impulse-exile
//! batch) — the activated ability:
//!
//! > {1}{R}, Discard your hand, Sacrifice this enchantment:
//! > Put all cards exiled with ~ into their owners' hands.
//!
//! "Put all cards exiled with ~ into their owners' hands" is a MASS move of the
//! source's linked-exile set (`TargetFilter::ExiledBySource`) to each card's
//! own owner's hand. It must lower to `Effect::ChangeZoneAll { origin: Exile,
//! destination: Hand, target: ExiledBySource }`. Before the fix the plural
//! possessive destination "into their owners' hands" was not a recognized
//! put-destination needle, so the whole clause fell to `Effect::Unimplemented`
//! and the exiled cards stayed in exile.
//!
//! DISCRIMINATOR: after resolution, both cards exiled with the source return to
//! their owners' hands. With the parse reverted the effect is `Unimplemented`
//! (a no-op), the cards stay in Exile, and the `== Hand` assertion flips.
//!
//! CR 400.3 + CR 110.1: a non-battlefield zone move keys the destination by each
//! object's owner.
//! CR 610.3: the linked-exile entries are consumed once the cards leave exile.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{AbilityKind, Effect, TargetFilter};
use engine::types::game_state::{ExileLink, ExileLinkKind};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CONNECTING_THE_DOTS_ACTIVATED: &str = "Put all cards exiled with ~ into their owners' hands.";

#[test]
fn connecting_the_dots_returns_all_exiled_with_source_to_owners_hands() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The enchantment on the battlefield is the exile source.
    let source = scenario.add_creature(P0, "Connecting the Dots", 0, 0).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();

    // Two cards exiled "with ~": one owned by P0, one owned by P1 (the source's
    // attack trigger exiles the active player's library cards — model two owners
    // to prove the per-owner destination routing).
    let p0_exiled = create_object(state, CardId(900), P0, "P0 Exiled".to_string(), Zone::Exile);
    let p1_exiled = create_object(state, CardId(901), P1, "P1 Exiled".to_string(), Zone::Exile);
    for exiled in [p0_exiled, p1_exiled] {
        state.exile_links.push(ExileLink {
            exiled_id: exiled,
            source_id: source,
            kind: ExileLinkKind::TrackedBySource,
        });
    }
    // A control card exiled by a DIFFERENT source must NOT be returned.
    let unrelated = create_object(
        state,
        CardId(902),
        P0,
        "Unrelated Exiled".to_string(),
        Zone::Exile,
    );

    // Same parser path the real card uses.
    let def = parse_effect_chain(CONNECTING_THE_DOTS_ACTIVATED, AbilityKind::Activated);
    assert!(
        matches!(
            &*def.effect,
            Effect::ChangeZoneAll {
                destination: Zone::Hand,
                target: TargetFilter::ExiledBySource,
                ..
            }
        ),
        "must lower to ChangeZoneAll(Exile->Hand, ExiledBySource), got {:?}",
        def.effect
    );

    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("mass exile-return must resolve");

    // DISCRIMINATOR: each linked-exile card is now in its OWN owner's hand.
    assert_eq!(
        runner.state().objects[&p0_exiled].zone,
        Zone::Hand,
        "P0's exiled-with-source card must return to P0's hand"
    );
    assert_eq!(
        runner.state().objects[&p1_exiled].zone,
        Zone::Hand,
        "P1's exiled-with-source card must return to P1's hand (per-owner routing)"
    );
    assert_eq!(
        runner.state().objects[&p0_exiled].owner,
        P0,
        "ownership is unchanged by the move"
    );
    assert_eq!(runner.state().objects[&p1_exiled].owner, P1);

    // NEGATIVE: a card exiled by a different source is untouched.
    assert_eq!(
        runner.state().objects[&unrelated].zone,
        Zone::Exile,
        "cards NOT exiled with this source must stay in exile"
    );
}
