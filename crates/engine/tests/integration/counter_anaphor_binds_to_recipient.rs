//! CR 611.3a + CR 613.4c (#5929): a bare counter anaphor ("…for each slime
//! counter on them") inside a per-recipient continuous static names the object
//! RECEIVING the effect, not the static's source. Toxrill, the Corrosive parsed
//! it as `CountersOn { scope: Source }`, so the -1/-1 scaled off Toxrill's own
//! (always empty) slime pile and never applied.
//!
//! The two tests here pin both sides of the gate, and each fails on revert:
//!   * the anaphoric form must vary PER RECIPIENT, and
//!   * the explicit `~` form (Joraga Warcaller class) must keep naming the
//!     source — the far larger class a blanket rebind would break.

use engine::game::derived::derive_display_state;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

const TOXRILL: &str = "At the beginning of each end step, put a slime counter on \
     each creature you don't control.\nCreatures you don't control get -1/-1 for \
     each slime counter on them.";

const JORAGA_WARCALLER: &str =
    "Elf creatures you control get +1/+1 for each +1/+1 counter on Joraga Warcaller.";

fn power_toughness(
    runner: &engine::game::scenario::GameRunner,
    id: engine::types::identifiers::ObjectId,
) -> (i32, i32) {
    let obj = runner.state().objects.get(&id).expect("object present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

#[test]
fn toxrill_slime_penalty_scales_per_affected_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let toxrill = scenario
        .add_creature(P0, "Toxrill, the Corrosive", 4, 5)
        .from_oracle_text(TOXRILL)
        .id();
    // Two opponent creatures with DIFFERENT slime counts. A source-scoped read
    // gives both the same modifier; only a recipient-scoped read separates them.
    let slimed = scenario.add_creature(P1, "Slimed Bear", 4, 4).id();
    let clean = scenario.add_creature(P1, "Clean Bear", 4, 4).id();
    scenario.with_counter(slimed, CounterType::Generic("slime".to_string()), 2);
    // Counters on the SOURCE must not drive the effect — under the old
    // source-scoped binding these three would have set every recipient's
    // modifier to -3/-3.
    scenario.with_counter(toxrill, CounterType::Generic("slime".to_string()), 3);

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, slimed),
        (2, 2),
        "2 slime counters on this creature → -2/-2 on the 4/4"
    );
    assert_eq!(
        power_toughness(&runner, clean),
        (4, 4),
        "no slime counters on this creature → unmodified, even though Toxrill \
         itself carries three"
    );
    assert_eq!(
        power_toughness(&runner, toxrill),
        (4, 5),
        "Toxrill controls the static, so it is not among the creatures its \
         controller doesn't control"
    );
}

#[test]
fn explicit_self_named_counter_anthem_still_reads_the_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let warcaller = scenario
        .add_creature(P0, "Joraga Warcaller", 1, 1)
        .with_subtypes(vec!["Elf"])
        .from_oracle_text(JORAGA_WARCALLER)
        .id();
    let other_elf = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    scenario.with_counter(warcaller, CounterType::Plus1Plus1, 2);

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());
    derive_display_state(runner.state_mut());

    assert_eq!(
        power_toughness(&runner, other_elf),
        (3, 3),
        "\"counter on ~\" names the source outright: every Elf gets +2/+2 from \
         the Warcaller's own counters, even with none of its own"
    );
}
