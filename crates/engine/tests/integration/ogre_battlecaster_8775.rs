//! Issue #8775: Ogre Battlecaster — "Whenever this creature attacks, you may
//! cast target instant or sorcery card from your graveyard by paying {R}{R} in
//! addition to its other costs. If that spell would be put into a graveyard,
//! exile it instead. When you cast that spell, this creature gets +X/+0 until
//! end of turn, where X is that spell's mana value."
//!
//! CR 608.2g: "If an effect specifically instructs or allows a player to cast a
//! spell during resolution, they do so by following the steps in rules
//! 601.2a–i, except no player receives priority after it's cast." The chosen
//! card is cast AS THE ATTACK TRIGGER RESOLVES — in the declare attackers
//! step, a sorcery included — not under a permission exercised later at
//! sorcery speed. The paid form of "cast target … card from your graveyard"
//! used to be lowered as such a lingering permission (the free form, Torrential
//! Gearhulk's, was already cast during resolution); the parser now marks it
//! `CastFromZoneDriver::DuringResolution` and the resolver's paid branch opens
//! the `CastOffer::GraveyardPaidCast` it always had for that driver.
//!
//! X is the cast spell's mana value: the issue as first filed claimed X
//! resolved to 0, which was a stand-in Bolt with no mana cost. The Bolt here
//! carries `{R}`.
//!
//! "By paying {R}{R} in addition to its other costs" is an additional cost of
//! that cast (CR 601.2b): accepting the offer pays the printed cost AND the
//! {R}{R} — three Mountains for a Bolt. The parser used to drop the clause.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::cast_this_way_gate_8721::{
    accept_offer_and_pay, offered_card, settle_attack_trigger, to_declare_attackers,
};
use super::rules::AttackTarget;

const OGRE_BATTLECASTER: &str = "First strike\n\
Whenever this creature attacks, you may cast target instant or sorcery card from your graveyard \
by paying {R}{R} in addition to its other costs. If that spell would be put into a graveyard, \
exile it instead. When you cast that spell, this creature gets +X/+0 until end of turn, where X \
is that spell's mana value.";

/// Lands `player` controls that are tapped — the mana the accepted cast
/// took from the untapped basics the scenario supplied.
fn tapped_lands(
    runner: &engine::game::scenario::GameRunner,
    player: engine::types::player::PlayerId,
) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.controller == player
                && object.tapped
                && object
                    .card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Land)
        })
        .count()
}

fn power(
    runner: &mut engine::game::scenario::GameRunner,
    id: engine::types::identifiers::ObjectId,
) -> Option<i32> {
    engine::game::layers::evaluate_layers(runner.state_mut());
    runner.state().objects[&id].power
}

/// CR 608.2g + CR 603.7: the instant is cast while the attack trigger resolves,
/// Ogre is pumped by the spell's mana value while still in combat, and the
/// cast card is exiled by the rider instead of returning to the graveyard.
#[test]
fn ogre_is_pumped_by_the_mana_value_of_the_spell_cast_as_its_trigger_resolves() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ogre = scenario
        .add_creature_from_oracle(P0, "Ogre Battlecaster", 3, 3, OGRE_BATTLECASTER)
        .id();
    for _ in 0..6 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();

    to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[(ogre, AttackTarget::Player(P1))])
        .expect("Ogre must be a legal attacker");
    settle_attack_trigger(&mut runner, true);
    assert_eq!(
        offered_card(&runner),
        Some(bolt),
        "the resolving trigger offers the chosen card for casting now"
    );
    assert_eq!(
        power(&mut runner, ogre),
        Some(3),
        "reach guard: nothing is pumped before the cast"
    );

    assert_eq!(
        tapped_lands(&runner, P0),
        0,
        "reach guard: nothing paid yet"
    );
    accept_offer_and_pay(&mut runner);
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == bolt),
        "reach guard: the accepted Bolt is on the stack"
    );
    assert_eq!(
        tapped_lands(&runner, P0),
        3,
        "CR 601.2b: the Bolt's {{R}} plus the printed {{R}}{{R}} in addition — three Mountains"
    );
    assert_eq!(
        runner.state().phase,
        Phase::DeclareAttackers,
        "CR 608.2g: the cast happened inside the trigger's resolution, in combat"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().phase,
        Phase::DeclareAttackers,
        "reach guard: still in combat"
    );
    assert_eq!(
        power(&mut runner, ogre),
        Some(4),
        "X is the cast spell's mana value: Lightning Bolt, mana value 1, pumps Ogre to 4/3"
    );
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Exile,
        "the rider exiles the cast card instead of putting it into the graveyard"
    );
}

/// CR 608.2g: "Timing permissions based on the card's type are ignored" — a
/// SORCERY offered by the attack trigger is cast in the declare attackers step.
/// Under the lingering permission this class used to get, the sorcery waited
/// for the second main phase and Ogre attacked unpumped.
#[test]
fn a_sorcery_offered_by_the_attack_trigger_is_cast_in_combat() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let ogre = scenario
        .add_creature_from_oracle(P0, "Ogre Battlecaster", 3, 3, OGRE_BATTLECASTER)
        .id();
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    // A sorcery with mana value 2 (Cruel Edict's cost); its effect is not
    // what is measured here.
    let edict = scenario
        .add_spell_to_graveyard(P0, "Cruel Edict", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();

    to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[(ogre, AttackTarget::Player(P1))])
        .expect("Ogre must be a legal attacker");
    settle_attack_trigger(&mut runner, true);
    assert_eq!(
        offered_card(&runner),
        Some(edict),
        "the resolving trigger offers the chosen sorcery for casting now"
    );

    accept_offer_and_pay(&mut runner);
    assert!(
        runner.state().stack.iter().any(|entry| entry.id == edict),
        "the sorcery is on the stack in the declare attackers step (CR 608.2g)"
    );
    assert_eq!(
        tapped_lands(&runner, P0),
        4,
        "{{1}}{{B}} plus the printed {{R}}{{R}} in addition — four lands"
    );
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
    runner.advance_until_stack_empty();

    assert_eq!(
        power(&mut runner, ogre),
        Some(5),
        "X is the sorcery's mana value: mana value 2 pumps Ogre to 5/3, still in combat"
    );
    assert_eq!(runner.state().phase, Phase::DeclareAttackers);
}
