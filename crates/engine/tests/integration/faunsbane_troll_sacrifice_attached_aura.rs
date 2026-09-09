//! Faunsbane Troll — "{1}, Sacrifice an Aura attached to this creature: This
//! creature fights target creature you don't control. ..."
//!
//! CR 303.4 (an Aura is attached to the object its enchant ability allows) +
//! CR 301.5 (the Equipment twin, Ronin, Shadow Stalker) + CR 601.2h via
//! CR 602.2b (the activation cost is paid with permanents matching its filter;
//! CR 601.2h is written about casting and CR 602.2b extends it to an activated
//! ability's cost) + CR 701.21a (sacrifice).
//!
//! THE REPORTED DEFECT. `parse_target`'s remainder was discarded where the
//! sacrifice cost's filter is built, so "attached to ~" vanished and the cost
//! filter shipped as a bare `Typed{[Subtype("Aura")]}`. That is a WIDENING: any
//! Aura on the battlefield satisfied it, including an Aura enchanting a creature
//! this player does not control. Ronin, Shadow Stalker carries the identical
//! shape with Equipment. Both riders are now consumed into
//! `FilterProp::AttachedToSource`, whose matcher is "this object's `attached_to`
//! is the filter source".
//!
//! These tests drive the production read-out (`ai_support::legal_actions_full`
//! and `activation_block_reasons`, which route through
//! `casting::activation_cost_block_reason` → `find_eligible_sacrifice_targets` →
//! `matches_target_filter`) and the real activation pipeline, never a parsed
//! filter shape — `oracle_cost.rs`'s own unit tests pin the shape.

use engine::ai_support::{activation_block_reasons, legal_actions_full};
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::AbilityBlockKind;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Only the activated ability, so it is ability index 0. The enters trigger that
/// creates the Monster Role token is immaterial here — the fixture places the
/// Aura directly — and omitting it keeps a token-creation path out of a test
/// about a cost filter.
const FAUNSBANE_ABILITY: &str = "{1}, Sacrifice an Aura attached to this creature: \
     This creature fights target creature you don't control. Activate only as a sorcery.";

/// Ability indices the engine actually OFFERS for `id`.
fn offered_indices(runner: &GameRunner, id: ObjectId) -> Vec<usize> {
    let (_, _, grouped) = legal_actions_full(runner.state());
    let mut out: Vec<usize> = grouped
        .get(&id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| match a {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } if *source_id == id => Some(*ability_index),
            _ => None,
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The block kinds published for `id`'s ability 0.
fn block_kinds(runner: &GameRunner, id: ObjectId) -> Vec<AbilityBlockKind> {
    activation_block_reasons(runner.state())
        .get(&id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|e| e.ability_index == 0)
        .map(|e| e.reason.kind)
        .collect()
}

/// Build the board: the Troll, a second creature of the same controller, one
/// Aura, and {1} in the pool. The Aura is attached to `on_troll ? troll : bear`.
/// Returns `(runner, troll, bear, aura, opponent_creature)`.
fn board(on_troll: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let troll = scenario
        .add_creature_from_oracle(P0, "Faunsbane Troll", 4, 4, FAUNSBANE_ABILITY)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // CR 303.4 + CR 704.5m: an Aura is an enchantment with the Aura subtype and
    // an enchant ability, and an Aura attached to an ILLEGAL object is put into
    // its owner's graveyard by the next SBA check. So the fixture is built
    // through the same builder the shipped Aura fixtures use
    // (`issue_4956_gift_of_immortality_reattach.rs`) — a real Aura whose enchant
    // ability legally accepts a creature — rather than a bare enchantment given
    // an `attached_to` by hand, which would not survive to be sacrificed.
    let aura = scenario
        .add_creature(P0, "Sentinel's Eyes", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(
            engine::types::ability::TargetFilter::Typed(
                engine::types::ability::TypedFilter::creature(),
            ),
        ))
        .id();
    let prey = scenario.add_creature(P1, "Prey", 1, 1).id();
    // CR 602.2b / CR 601.2g: {1} for the mana component of the activation cost.
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
    let host = if on_troll { troll } else { bear };
    attach_to(runner.state_mut(), aura, host);

    // FIXTURE GUARDS — the board really holds what the assertions assume, so an
    // empty or mis-built board cannot make them vacuously true.
    let state = runner.state();
    assert_eq!(
        state.objects[&aura].zone,
        Zone::Battlefield,
        "the Aura must be on the battlefield to be a sacrifice candidate"
    );
    assert!(
        state.objects[&aura]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Aura"),
        "the fixture permanent must actually be an Aura: {:?}",
        state.objects[&aura].card_types
    );
    assert_eq!(
        state.objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "the Aura must be attached to the intended host"
    );
    assert_eq!(
        state.objects[&troll].zone,
        Zone::Battlefield,
        "the source must be on the battlefield"
    );
    assert!(
        !state.objects[&troll].abilities.is_empty(),
        "the Troll's activated ability must have parsed onto the object"
    );
    (runner, troll, bear, aura, prey)
}

/// THE DEFECT. The only Aura on the battlefield enchants a DIFFERENT creature, so
/// nothing satisfies "an Aura attached to this creature" and the ability must not
/// be activatable at all.
///
/// Revert-failing: drop `fold_attached_to_source_rider` (or restore the discarded
/// `parse_target` remainder) and the cost filter is a bare `Typed{[Aura]}` again —
/// the Aura on the Bears qualifies, the ability is OFFERED, and the
/// `CostNotPayableNow` read-out disappears.
#[test]
fn faunsbane_troll_cannot_pay_with_an_aura_on_another_creature() {
    let (runner, troll, bear, aura, _prey) = board(false);

    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(bear)),
        "precondition: the Aura is on the Bears, not the Troll"
    );
    assert_eq!(
        offered_indices(&runner, troll),
        Vec::<usize>::new(),
        "no Aura is attached to the Troll, so its ability has no legal sacrifice"
    );
    assert!(
        block_kinds(&runner, troll).contains(&AbilityBlockKind::CostNotPayableNow),
        "the ability must be published as cost-blocked, got {:?}",
        block_kinds(&runner, troll)
    );
}

/// PAIRED CONTROL. The same board with the Aura moved onto the Troll: the ability
/// is offered, the activation pays with that Aura, and the Aura is sacrificed.
///
/// This is what makes the negative test above a discriminator rather than proof
/// that the ability is simply broken.
#[test]
fn faunsbane_troll_pays_with_the_aura_attached_to_it() {
    let (mut runner, troll, _bear, aura, prey) = board(true);

    assert_eq!(
        offered_indices(&runner, troll),
        vec![0],
        "with an Aura attached to the source the ability must be offered"
    );
    assert!(
        !block_kinds(&runner, troll).contains(&AbilityBlockKind::CostNotPayableNow),
        "a payable cost must not be published as cost-blocked, got {:?}",
        block_kinds(&runner, troll)
    );

    let outcome = runner
        .activate(troll, 0)
        .target_object(prey)
        .pay_with(&[aura])
        .resolve();
    let state = outcome.state();
    assert_eq!(
        state.objects[&aura].zone,
        Zone::Graveyard,
        "the attached Aura is the sacrifice that paid the cost"
    );
    assert_eq!(
        state.objects[&troll].zone,
        Zone::Battlefield,
        "the source itself is not the sacrifice"
    );
}
