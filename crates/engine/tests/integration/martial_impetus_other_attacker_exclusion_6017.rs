//! Issue #6017 — Martial Impetus: the attack-triggered +1/+1 must go on each
//! *other* attacking creature, never on the enchanted creature itself.
//!
//! Oracle:
//!   Enchant creature
//!   Enchanted creature gets +1/+1 and is goaded. (...)
//!   Whenever enchanted creature attacks, each other creature that's attacking
//!   one of your opponents gets +1/+1 until end of turn.
//!
//! The trigger body parses to `Effect::PumpAll(+1/+1)` whose population filter
//! is emitted as `Typed(Creature, [Another, Attacking{Opponent}])`. The generic
//! `FilterProp::Another` is *source-relative* and the ability's source is the
//! AURA — never a creature — so it would exclude nothing and the enchanted
//! creature (itself an attacker) would wrongly receive the buff.
//!
//! CR 303.4 + CR 301.5a: the anaphoric "other" on an attachment self-trigger
//! (`valid_card == AttachedTo`) refers to the ENCHANTED creature named by the
//! trigger, not the Aura. The parser retargets the mass-population `Another` to
//! an `AttachedTo` exclusion — `And[Typed(Creature,[Attacking]), Not(AttachedTo)]`
//! — so the enchanted host is excluded at runtime (`source.attached_to`) while
//! the generic source-relative `Another` used by costs (Bound by Moonsilver's
//! "Sacrifice another permanent") is left untouched.
//!
//! Drives the REAL parse -> attach -> declare-attackers -> trigger-resolution ->
//! layer pipeline and reads back effective power/toughness (a runtime test).

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const MARTIAL_IMPETUS: &str = "Enchant creature\n\
Enchanted creature gets +1/+1 and is goaded. (It attacks each combat if able and attacks a player other than you if able.)\n\
Whenever enchanted creature attacks, each other creature that's attacking one of your opponents gets +1/+1 until end of turn.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

#[test]
fn martial_impetus_attack_pump_excludes_enchanted_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The enchanted creature (base 2/2). Gets +1/+1 from the static anthem.
    let enchanted = scenario.add_creature(P0, "Enchanted Bear", 2, 2).id();

    // A second creature P0 controls (base 3/3) — a co-attacker that SHOULD get
    // the +1/+1 from the attack trigger.
    let co_attacker = scenario.add_creature(P0, "Co-Attacker", 3, 3).id();

    // The Aura carrying the real parse (static anthem + goad + attack trigger).
    let aura = scenario
        .add_creature(P0, "Martial Impetus", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(MARTIAL_IMPETUS)
        .id();

    let mut runner = scenario.build();

    // CR 303.4: attach the Aura to the enchanted creature (set the attachment
    // relation directly, as the Vow-of-Lightning aura regression does).
    {
        let state = runner.state_mut();
        state.objects.get_mut(&aura).unwrap().attached_to = Some(enchanted.into());
        state
            .objects
            .get_mut(&enchanted)
            .unwrap()
            .attachments
            .push(aura);
        state.layers_dirty.mark_full();
    }

    // Sanity: the static anthem is live before combat — enchanted is 3/3.
    assert_eq!(
        effective_pt(&mut runner, enchanted),
        (3, 3),
        "static 'Enchanted creature gets +1/+1': 2/2 -> 3/3"
    );

    // Both P0 creatures attack P1 (one of P0's opponents).
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (enchanted, AttackTarget::Player(P1)),
            (co_attacker, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");

    // Resolve the "enchanted creature attacks" trigger (the mass +1/+1 pump).
    runner.advance_until_stack_empty();

    // CR 303.4 + CR 611.2c: "each OTHER creature that's attacking" — the pump
    // must NOT hit the enchanted creature. It stays at 3/3 (static anthem only),
    // NOT 4/4.
    assert_eq!(
        effective_pt(&mut runner, enchanted),
        (3, 3),
        "enchanted creature must be excluded by 'each other creature' \
         (3/3 from anthem only, not 4/4)"
    );

    // The other attacker gets +1/+1: 3/3 -> 4/4.
    assert_eq!(
        effective_pt(&mut runner, co_attacker),
        (4, 4),
        "each other attacking creature gets +1/+1: 3/3 -> 4/4"
    );
}
