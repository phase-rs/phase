//! Issue #4943: Dreadhorde Arcanist — "Whenever this creature attacks, you may
//! cast target instant or sorcery card with mana value less than or equal to
//! this creature's power from your graveyard without paying its mana cost."
//!
//! CR 608.2h: "If an effect requires information from the game (such as the
//! number of creatures on the battlefield), the answer is determined only
//! once, when the effect is applied." The ceiling "less than or equal to this
//! creature's power" is such information. `cast_from_zone::resolve` used to
//! hand the live `Ref { Power { Source } }` to the during-resolution cast,
//! where finalization re-read it without the trigger's source context and got
//! 0 — so a Bolt with a real mana cost (1) failed "1 ≤ 0" and stayed in the
//! graveyard, while the stand-in Bolts of the older tests (mana value 0)
//! passed. The ceiling is now frozen once, as the trigger resolves, for every
//! route the resolver takes (the lingering-permission route already did).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::cast_this_way_gate_8721::{settle_attack_trigger, to_declare_attackers};
use super::rules::AttackTarget;

const DREADHORDE_ARCANIST: &str = "Trample\n\
Whenever this creature attacks, you may cast target instant or sorcery card with mana value less \
than or equal to this creature's power from your graveyard without paying its mana cost. If that \
spell would be put into your graveyard, exile it instead.";

/// A 1-power Arcanist and a Bolt with mana value 1: the ceiling is met, the
/// Bolt is cast as the trigger resolves and the rider exiles it afterwards.
#[test]
fn arcanists_ceiling_is_frozen_when_its_trigger_resolves() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let arcanist = scenario
        .add_creature_from_oracle(P0, "Dreadhorde Arcanist", 1, 3, DREADHORDE_ARCANIST)
        .id();
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
        .declare_attackers(&[(arcanist, AttackTarget::Player(P1))])
        .expect("Arcanist must be a legal attacker");
    settle_attack_trigger(&mut runner, true);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Exile,
        "the Bolt (mana value 1 ≤ power 1) was cast as the trigger resolved and exiled by the \
         rider — a ceiling re-read as 0 at finalization leaves it in the graveyard"
    );
}

/// The ceiling tracks the source's power: a 2-power Arcanist casts a
/// mana-value-2 card. Together with the test above this pins the frozen
/// VALUE — a ceiling frozen at some constant would fail one of the two.
#[test]
fn arcanists_ceiling_is_its_power_as_the_trigger_resolves() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let arcanist = scenario
        .add_creature_from_oracle(P0, "Dreadhorde Arcanist", 2, 3, DREADHORDE_ARCANIST)
        .id();
    let spear = scenario
        .add_spell_to_graveyard(P0, "Searing Spear", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        })
        .id();
    let mut runner = scenario.build();

    to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[(arcanist, AttackTarget::Player(P1))])
        .expect("Arcanist must be a legal attacker");
    settle_attack_trigger(&mut runner, true);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&spear].zone,
        Zone::Exile,
        "mana value 2 ≤ power 2: cast as the trigger resolved and exiled by the rider"
    );
}
