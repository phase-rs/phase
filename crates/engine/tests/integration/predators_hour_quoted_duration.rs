//! Regression (#9213): Predators' Hour — "Until end of turn, creatures you
//! control gain menace and "Whenever this creature deals combat damage to a
//! player, exile the top card of that player's library face down. You may look
//! at and play that card for as long as it remains exiled, and you may spend
//! mana as though it were mana of any color to cast that spell."" — granted
//! only menace. The trailing-duration scan started inside the quoted ability,
//! read "for as long as it remains exiled, …" as the granting clause's
//! duration and cut the quote in half, so the granted trigger (with its grant
//! and concession) was lost; coverage showed only generic swallow warnings.
//!
//! Fix: a duration that starts inside a quoted granted ability is that
//! ability's own text (CR 113.1a). The test drives the real cast, combat
//! damage and trigger, then casts the exiled {G} sorcery with a Swamp
//! (CR 609.4b).

use engine::ai_support::legal_actions;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PREDATORS_HOUR: &str = "Until end of turn, creatures you control gain menace and \
\"Whenever this creature deals combat damage to a player, exile the top card of that player's \
library face down. You may look at and play that card for as long as it remains exiled, and you \
may spend mana as though it were mana of any color to cast that spell.\"";

#[test]
fn granted_trigger_exiles_and_lets_the_card_be_cast_with_any_color() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..5 {
        scenario.add_card_to_library_top(P1, &format!("Filler {i}"));
    }
    let green = {
        let mut b = scenario.add_spell_to_library_top(P1, "Green Sorcery", false);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        });
        b.id()
    };
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Own Filler {i}"));
    }
    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let swamps = [
        scenario.add_basic_land(P0, ManaColor::Black),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let hour = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Predators' Hour", false, PREDATORS_HOUR);
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&hour].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: hour,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");
    runner.resolve_top();
    assert!(
        runner.state().objects[&attacker].has_keyword(&Keyword::Menace),
        "reach guard: the spell resolved and granted menace"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("DeclareAttackers accepted");
    let outcome = runner.combat_damage();
    assert_eq!(outcome.life_delta(P1), -2, "the Bears dealt combat damage");

    assert_eq!(
        runner.state().objects[&green].zone,
        Zone::Exile,
        "the granted trigger exiled the top card of the damaged player's library"
    );
    assert!(runner.state().objects[&green].face_down, "exiled face down");

    runner.advance_to_phase(Phase::PostCombatMain);
    assert!(
        legal_actions(runner.state()).iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == green)
        ),
        "the exiled card is offered as a legal cast with Swamps"
    );
    let card_id = runner.state().objects[&green].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: green,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the exiled card is cast with a Swamp");
    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while the exiled card resolves: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&green].zone,
        Zone::Graveyard,
        "the sorcery resolved into its owner's graveyard"
    );
    assert_eq!(
        swamps
            .iter()
            .filter(|swamp| runner.state().objects[swamp].tapped)
            .count(),
        1,
        "exactly one Swamp paid for {{G}}"
    );
}
