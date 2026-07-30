//! Repro probe for the Tergrid + Mirrormade report: a permanent with an
//! "enters as a copy" replacement (CR 614.12 / 707.9) is put onto the
//! battlefield by an EFFECT (reanimation / Tergrid), not by being cast. The
//! report was that the copy choice never fired on effect-driven entry.
//!
//! This drives the real cast → resolve → ChangeZone pipeline and observes
//! whether `WaitingFor::CopyTargetChoice` (or its accept prompt) is offered.

use engine::game::scenario::GameScenario;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const MIRRORMADE: &str =
    "You may have this enchantment enter as a copy of any artifact or enchantment on the battlefield.";

#[test]
fn copy_as_enters_offered_on_effect_driven_entry() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    // A copy target on the battlefield (a distinctly-named enchantment).
    let copy_target = scenario
        .add_creature(P0, "Copy Target Enchantment", 5, 5)
        .as_enchantment()
        .id();

    // Mirrormade sits in the OPPONENT's graveyard — Tergrid enters an opponent's
    // discarded permanent under YOUR control (controller override), so reproduce
    // that exact axis rather than an owner-controlled reanimation.
    let mirrormade = {
        let mut b = scenario.add_creature_to_graveyard(P1, "Mirrormade", 0, 1);
        b.from_oracle_text(MIRRORMADE).as_enchantment();
        b.id()
    };

    // Tergrid's exact effect: ChangeZone Graveyard→Battlefield, enters_under You,
    // from any graveyard — driven by an effect, NOT by casting Mirrormade.
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Tergrid Steal",
            false,
            "Put target enchantment card from a graveyard onto the battlefield under your control.",
        )
        .id();
    let _ = P1;

    let mut runner = scenario.build();
    for _ in 0..4 {
        let unit = engine::types::mana::ManaUnit::new(
            ManaType::Black,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        );
        runner.state_mut().players[0].mana_pool.add(unit);
    }

    let _ = copy_target;
    let outcome = runner
        .cast(reanimate)
        .target_objects(&[mirrormade])
        .resolve();

    let waiting = outcome.final_waiting_for().clone();

    // CR 614.12 + CR 707.9: the "enter as a copy" choice belongs to the
    // controller the permanent WILL have on the battlefield — P0, who is taking
    // control via the override — NOT P1, Mirrormade's owner. Before the fix the
    // choice went to the owner (an AI opponent who declines), so the copy never
    // happened from the controlling player's seat (the Tergrid report).
    let chooser = match &waiting {
        WaitingFor::ReplacementChoice { player, .. } => *player,
        WaitingFor::CopyTargetChoice { player, .. } => *player,
        other => panic!(
            "expected the copy-as-enters choice to be offered, got {other:?} \
             (mirrormade zone={:?})",
            runner.state().objects.get(&mirrormade).map(|o| o.zone)
        ),
    };
    assert_eq!(
        chooser, P0,
        "copy-as-enters choice must go to the new controller (P0 via the override), \
         not Mirrormade's owner (P1); got {chooser:?}"
    );
}
