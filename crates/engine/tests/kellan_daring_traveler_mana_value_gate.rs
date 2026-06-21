//! Discriminating runtime regression for **Kellan, Daring Traveler** (std
//! long-tail batch):
//!
//! > Whenever Kellan attacks, reveal the top card of your library. If it's a
//! > creature card with mana value 3 or less, put it into your hand. Otherwise,
//! > you may put it into your graveyard.
//!
//! The residual gap was the "with mana value 3 or less" tail on the revealed-card
//! gate: the parser dropped it (`Effect::Unimplemented("with mana value 3 or
//! less")`), and even when carried as `RevealedHasCardType.additional_filter`,
//! the runtime evaluator's `Some(_) => true` arm IGNORED any non-chosen-type
//! filter property — so the mana-value bound was never enforced.
//!
//! The fix parses the tail into `FilterProp::Cmc { LE, 3 }` as the gate's
//! `additional_filter`, AND routes generic `additional_filter` props through the
//! shared filter evaluator (`matches_target_filter`) against the revealed card.
//!
//! Modeled as an opponent-cast trigger (the proven reveal-conditional harness,
//! cf. issue #3127) rather than an attack trigger so the test isolates the
//! mana-value gate being fixed here. The gate semantics are identical — the
//! `RevealedHasCardType` condition runs the same regardless of trigger kind.
//!
//! DISCRIMINATOR (`reveal_creature_above_mv_threshold_does_not_go_to_hand`): a
//! creature card with mana value 4 (> 3) must NOT be put into the hand. With the
//! Cmc filter dropped or ignored (the pre-fix `Some(_) => true`), the gate would
//! pass and the card WOULD go to hand — the assertion below flips on revert.
//!
//! CR 202.3: mana value. CR 701.20a/b: reveal keeps the card on top of the
//! library. CR 608.2c: instructions resolve in order.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KELLAN_REVEAL: &str = "Whenever an opponent casts a spell, reveal the top card of your \
library. If it's a creature card with mana value 3 or less, put it into your hand. Otherwise, \
you may put it into your graveyard.";

/// Build the reveal source on P0's battlefield with a single staged top card,
/// have P1 cast a spell to fire the trigger, and resolve through the real stack
/// pipeline. Returns the runner and the staged card's id.
fn run_reveal(stage_top: impl FnOnce(&mut GameRunner) -> ObjectId) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Reveal Source", 0, 0)
        .as_enchantment()
        .from_oracle_text(KELLAN_REVEAL);
    let opponent_spell = scenario
        .add_creature_to_hand(P1, "Opponent Bear", 2, 2)
        .id();

    let mut runner = scenario.build();
    let revealed = stage_top(&mut runner);

    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let card_id = runner
        .state()
        .objects
        .get(&opponent_spell)
        .expect("spell")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: opponent_spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("opponent cast should succeed");
    runner.advance_until_stack_empty();
    (runner, revealed)
}

/// Seat a creature card with the given mana value on the very top of P0's
/// library.
fn stage_library_creature(runner: &mut GameRunner, mana_value: u8) -> ObjectId {
    let id = engine::game::zones::create_object(
        runner.state_mut(),
        engine::types::identifiers::CardId(900 + mana_value as u64),
        P0,
        format!("Library Creature MV{mana_value}"),
        Zone::Library,
    );
    {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.mana_cost = ManaCost::generic(mana_value as u32);
    }
    let mut events = Vec::new();
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
    id
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects.get(&id).expect("object").zone
}

/// Positive case: a creature card with mana value 3 (<= 3) is put into the hand.
#[test]
fn reveal_creature_at_or_below_mv_threshold_goes_to_hand() {
    let (runner, revealed) = run_reveal(|runner| stage_library_creature(runner, 3));
    assert_eq!(
        zone_of(&runner, revealed),
        Zone::Hand,
        "a creature card with mana value 3 (<= 3) must be put into the hand"
    );
}

/// DISCRIMINATOR: a creature card with mana value 4 (> 3) fails the gate, so the
/// Otherwise branch applies — it is NOT in the hand. With the mana-value filter
/// dropped/ignored, this card would wrongly land in the hand.
#[test]
fn reveal_creature_above_mv_threshold_does_not_go_to_hand() {
    let (runner, revealed) = run_reveal(|runner| stage_library_creature(runner, 4));
    assert_ne!(
        zone_of(&runner, revealed),
        Zone::Hand,
        "a creature card with mana value 4 (> 3) must FAIL the gate and NOT be \
         put into the hand (revert-discriminating: pre-fix the ignored Cmc \
         filter let it through to the hand)"
    );
}
