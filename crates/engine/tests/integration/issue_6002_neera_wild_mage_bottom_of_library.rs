//! CR 608.2c — Neera, Wild Mage's cast trigger must move the
//! TRIGGERING SPELL to the bottom of its owner's library, never Neera herself.
//!
//! Oracle (read from the pool export): "Whenever you cast a spell, you may put
//! it on the bottom of its owner's library. If you do, reveal cards from the
//! top of your library until you reveal a nonland card. You may cast that card
//! without paying its mana cost. Then put all revealed cards not cast this way
//! on the bottom of your library in a random order. This ability triggers only
//! once each turn."
//!
//! Issue #6002 (Discord report): accepting the first "you may put it on the
//! bottom of its owner's library" moved Neera herself to the library instead
//! of the spell that was cast.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const NEERA: &str = "Whenever you cast a spell, you may put it on the bottom of its owner's library. If you do, reveal cards from the top of your library until you reveal a nonland card. You may cast that card without paying its mana cost. Then put all revealed cards not cast this way on the bottom of your library in a random order. This ability triggers only once each turn.";

/// A self-contained life-gain spell body — its resolution is the ONLY thing
/// that can move P0's life total, so a life delta of 0 proves the spell never
/// resolved (i.e. it was bottomed, not cast normally).
const GAIN: &str = "You gain 3 life.";

fn cast_spell(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner
        .state()
        .objects
        .get(&spell)
        .expect("spell object exists")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast must be accepted");
}

/// Accept the first "you may put it on the bottom" prompt, decline the
/// second "you may cast that card" prompt, drain trigger ordering, and stop
/// at stack-empty (or any prompt not modeled here).
fn drive_accept_bottom_decline_cast(runner: &mut GameRunner) {
    let mut first_optional_seen = false;
    for _ in 0..64 {
        let wf = runner.state().waiting_for.clone();
        match wf {
            WaitingFor::OptionalEffectChoice { .. } => {
                let accept = !first_optional_seen;
                first_optional_seen = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional decision must be accepted");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// RED before the fix: accepting "you may put it on the bottom of its owner's
/// library" bottomed Neera (the ability's source) instead of the spell that
/// triggered the ability.
#[test]
fn neera_bottoms_the_cast_spell_not_herself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let neera = scenario
        .add_creature_from_oracle(P0, "Neera, Wild Mage", 2, 7, NEERA)
        .id();
    scenario.add_card_to_library_top(P0, "P0 Lib Filler");
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P0, "Divergent Bolt", true, GAIN)
        .id();
    let mut runner = scenario.build();

    let life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("player exists")
        .life;

    cast_spell(&mut runner, bolt);
    drive_accept_bottom_decline_cast(&mut runner);

    assert_eq!(
        runner.state().objects.get(&bolt).map(|o| o.zone),
        Some(Zone::Library),
        "the cast spell must be put on the bottom of its owner's library"
    );
    assert_eq!(
        runner.state().objects.get(&neera).map(|o| o.zone),
        Some(Zone::Battlefield),
        "Neera herself must stay on the battlefield — she is not the trigger's referent"
    );
    let life_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("player exists")
        .life;
    assert_eq!(
        life_after, life_before,
        "the bottomed spell must never resolve, so life must not change"
    );
}
