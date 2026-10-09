//! #9514 — a self cost reduction that follows a quoted grant on an activated
//! ability must stay on the activated ability.
//!
//! Llanowar Greenwidow's graveyard ability ends with a quoted grant followed by
//! "This ability costs {1} less to activate for each basic land type among lands
//! you control." The clause splitter kept the reduction sentence attached to the
//! closed quote, so it was swallowed into the granted static's text and the
//! ability always cost the full {7}{G}.
//!
//! CR 602.2b: an activated ability's activation cost is the analog of a spell's
//! mana cost. CR 601.2f: the total cost is that cost minus all cost reductions.
//!
//! The test drives the real activation pipeline from the graveyard with exactly
//! the reduced mana available, so it fails if the reduction is dropped. It then
//! destroys the returned card to prove the quoted grant's replacement is live on
//! it (CR 614.1a: "exile it instead"; CR 611.2a: the grant states no duration).

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Oracle text (MTGJSON `AtomicCards.json`) for the activated line.
const GREENWIDOW_ABILITY: &str = "Domain — {7}{G}: Return this card from your graveyard to the \
battlefield tapped. It gains \"If this permanent would leave the battlefield, exile it instead \
of putting it anywhere else.\" This ability costs {1} less to activate for each basic land type \
among lands you control.";

fn mana(mana_type: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

/// Destroy `object` through the production zone-change hub so a granted
/// Moved→Exile replacement, if live, is consulted. Mirrors
/// `issue_6566_granted_leave_exile::destroy`.
fn destroy(runner: &mut GameRunner, object: ObjectId) {
    let destroy = ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        },
        vec![TargetRef::Object(object)],
        object,
        P0,
    );
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &destroy, &mut events, 0).expect("destroy resolves");
}

#[test]
fn greenwidow_graveyard_ability_is_reduced_per_basic_land_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let greenwidow = scenario
        .add_creature_to_graveyard(P0, "Llanowar Greenwidow", 6, 6)
        .from_oracle_text(GREENWIDOW_ABILITY)
        .id();
    // Three basic land types (Forest, Island, Mountain). The lands are tapped so
    // they only count toward domain and cannot fund the activation themselves.
    let lands = [ManaColor::Green, ManaColor::Blue, ManaColor::Red]
        .map(|color| scenario.add_basic_land(P0, color));
    // CR 601.2f: {7}{G} minus {1} per basic land type ({3}) leaves {4}{G} —
    // exactly what the pool holds, so the unreduced cost cannot be paid.
    let mut pool = mana(ManaType::Colorless, 4);
    pool.extend(mana(ManaType::Green, 1));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    for land in lands {
        runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
    }

    let outcome = runner.activate(greenwidow, 0).resolve();
    runner.advance_until_stack_empty();

    assert!(
        outcome.state().players[0].mana_pool.mana.is_empty(),
        "the reduced cost {{4}}{{G}} spends the whole pool"
    );
    assert_eq!(runner.state().objects[&greenwidow].zone, Zone::Battlefield);
    assert!(
        runner.state().objects[&greenwidow].tapped,
        "the card returns to the battlefield tapped"
    );

    // CR 614.1a: the granted "exile it instead of putting it anywhere else"
    // replacement lives on the returned card, so destroying it exiles it.
    destroy(&mut runner, greenwidow);
    assert_eq!(runner.state().objects[&greenwidow].zone, Zone::Exile);
}
