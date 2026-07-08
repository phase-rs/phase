//! CR 105.2 + CR 611.3a (#4590 review [HIGH]): Heroic Defiance gates its +3/+3
//! on the ENCHANTED CREATURE's color ("…unless IT shares a color…"), not the
//! Aura's. A red Aura on a lone white creature, with red the most common color,
//! must still pump the creature (white does not share red) — proving the
//! condition is recipient-scoped, not source-scoped. The earlier source-scoped
//! evaluation read the Aura's color and would (wrongly) withhold the grant.

use engine::game::derived::derive_display_state;
use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const HEROIC_DEFIANCE: &str = "Enchant creature\nEnchanted creature gets +3/+3 \
     unless it shares a color with the most common color among all permanents or \
     a color tied for most common.";

fn power_toughness(
    runner: &engine::game::scenario::GameRunner,
    id: engine::types::identifiers::ObjectId,
) -> (i32, i32) {
    let obj = runner.state().objects.get(&id).expect("object present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

#[test]
fn heroic_defiance_gates_on_enchanted_creature_color_not_aura() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The enchanted creature is the only WHITE permanent.
    let creature = scenario.add_creature(P0, "Lone White", 1, 1).id();
    // The Aura (Heroic Defiance) is RED — the most common color — so a
    // source-scoped check would read the Aura's color and withhold the grant.
    let aura = scenario
        .add_creature(P0, "Heroic Defiance", 0, 0)
        .from_oracle_text(HEROIC_DEFIANCE)
        .as_enchantment()
        .id();
    // Two more red permanents make RED strictly most common (3 red vs 1 white).
    let red1 = scenario.add_creature(P0, "Red One", 1, 1).id();
    let red2 = scenario.add_creature(P0, "Red Two", 1, 1).id();

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&creature).unwrap().base_color = vec![ManaColor::White];
        for id in [aura, red1, red2] {
            s.objects.get_mut(&id).unwrap().base_color = vec![ManaColor::Red];
        }
    }
    attach_to(runner.state_mut(), aura, creature);
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());

    // White creature does NOT share the most-common color (red): the "unless" is
    // false, so the grant applies, 1/1 -> 4/4. A source-scoped read of the red
    // Aura would see it shares red and leave the creature at 1/1.
    assert_eq!(
        power_toughness(&runner, creature),
        (4, 4),
        "recipient-scoped: white creature doesn't share most-common red -> +3/+3"
    );

    // Recolor the creature RED: now it shares the most-common color, the "unless"
    // is satisfied, and the grant is withheld — back to 1/1.
    runner
        .state_mut()
        .objects
        .get_mut(&creature)
        .unwrap()
        .base_color = vec![ManaColor::Red];
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());
    assert_eq!(
        power_toughness(&runner, creature),
        (1, 1),
        "recipient now shares most-common red -> grant withheld"
    );
}
