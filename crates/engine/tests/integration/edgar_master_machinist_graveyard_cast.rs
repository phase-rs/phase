//! Edgar, Master Machinist (#9770): "Once during each of your turns, you may
//! cast an artifact spell from your graveyard. If you cast a spell this way,
//! that artifact enters tapped."

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EDGAR: &str = "Once during each of your turns, you may cast an artifact spell from your graveyard. If you cast a spell this way, that artifact enters tapped.\nTools — Whenever Edgar attacks, it gets +X/+0 until end of turn, where X is the greatest mana value among artifacts you control.";

fn try_cast(r: &mut GameRunner, id: ObjectId) -> bool {
    let card_id = r.state().objects[&id].card_id;
    r.act(GameAction::CastSpell {
        object_id: id,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .is_ok()
}

fn resolve_stack(r: &mut GameRunner) {
    for _ in 0..16 {
        if matches!(r.state().waiting_for, WaitingFor::Priority { .. })
            && r.state().stack.is_empty()
        {
            return;
        }
        r.act(GameAction::PassPriority).expect("pass");
    }
}

#[test]
fn edgar_graveyard_artifact_enters_tapped_once_per_turn() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_creature_from_oracle(P0, "Edgar, Master Machinist", 2, 4, EDGAR);
    let first = s
        .add_creature_to_graveyard(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id();
    let second = s
        .add_creature_to_graveyard(P0, "Memnite", 1, 1)
        .as_artifact_creature()
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut r = s.build();
    let first_cast = try_cast(&mut r, first);
    resolve_stack(&mut r);
    let first_zone = r.state().objects[&first].zone;
    let first_tapped = r.state().objects[&first].tapped;
    let second_cast = try_cast(&mut r, second);
    // CR 601.3: Edgar permits one graveyard artifact cast per turn; the rider
    // makes it enter tapped.
    assert_eq!(
        (first_cast, first_zone, first_tapped, second_cast),
        (true, Zone::Battlefield, true, false),
        "(first cast accepted, first zone, first tapped, second cast accepted)"
    );
}
