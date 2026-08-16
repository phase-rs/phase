//! dq-f: Squirming Emergence — the target's mana-value clause TRAILS its zone
//! clause ("target nonland permanent card in your graveyard with mana value less
//! than or equal to the number of permanent cards in your graveyard"). Before the
//! zone-then-mana-value second pass in `parse_type_phrase_with_ctx`, the trailing
//! "with mana value …" clause was dropped, so every nonland permanent card in the
//! graveyard was a legal target regardless of its mana value.
//!
//! This drives the real cast pipeline: with four permanent cards in the graveyard
//! (permanent-card count = 4), only cards with mana value <= 4 may be targeted.
//! The mana-value-5 card is the discriminator — it is legal iff the fix is
//! reverted.
//!
//! Oracle text (verbatim, Scryfall — Squirming Emergence, DSK):
//!   "Fathomless descent — Return to the battlefield target nonland permanent
//!    card in your graveyard with mana value less than or equal to the number of
//!    permanent cards in your graveyard."

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const SQUIRMING_EMERGENCE: &str = "Fathomless descent — Return to the battlefield target nonland permanent card in your graveyard with mana value less than or equal to the number of permanent cards in your graveyard.";

fn gy_creature(scenario: &mut GameScenario, name: &str, mana_value: u32) -> ObjectId {
    let mut b = scenario.add_creature_to_graveyard(P0, name, 1, 1);
    b.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: mana_value,
    });
    b.id()
}

#[test]
fn squirming_emergence_targets_only_within_permanent_card_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Four nonland permanent (creature) cards in P0's graveyard → the dynamic
    // "number of permanent cards in your graveyard" resolves to 4. The on-stack
    // sorcery is not a permanent card and is not in the graveyard, so it does not
    // inflate the count.
    let mv2 = gy_creature(&mut scenario, "MV Two", 2);
    let f1 = gy_creature(&mut scenario, "MV One", 1);
    // MV3 card: also within the threshold (a legal target) and, together with the
    // other three, makes the permanent-card count exactly 4.
    let f2 = gy_creature(&mut scenario, "MV Three", 3);
    let mv5 = gy_creature(&mut scenario, "MV Five", 5);

    let mut spell = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Squirming Emergence",
        false,
        SQUIRMING_EMERGENCE,
    );
    spell.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = spell.id();

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Squirming Emergence must be accepted");

    // Targets are chosen at CR 601.2c, before the (trivial {0}) cost. Step through
    // any priority/mana windows until the single ChangeZone target slot surfaces.
    let mut reached = None;
    for _ in 0..8 {
        if let WaitingFor::TargetSelection {
            target_slots,
            selection,
            ..
        } = runner.state().waiting_for.clone()
        {
            reached = Some((target_slots, selection));
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    let (target_slots, selection) =
        reached.expect("cast must halt at target selection for the reanimation slot");
    let legal = &target_slots[selection.current_slot].legal_targets;

    // Non-vacuous positive reach-guard: at least two cards satisfy the filter, so
    // the slot genuinely surfaces legal targets (not an empty/short-circuited set).
    assert!(
        legal.contains(&TargetRef::Object(mv2)),
        "MV2 card (<= 4) must be a legal target, got {legal:?}"
    );
    assert!(
        legal.contains(&TargetRef::Object(f1)),
        "MV1 card (<= 4) must be a legal target, got {legal:?}"
    );
    assert!(
        legal.contains(&TargetRef::Object(f2)),
        "MV3 card (<= 4) must be a legal target, got {legal:?}"
    );
    // Discriminator — reverting the zone-then-mana-value second pass drops the
    // "with mana value …" clause, making every graveyard permanent card legal.
    assert!(
        !legal.contains(&TargetRef::Object(mv5)),
        "MV5 card (> permanent-card count 4) must NOT be a legal target, got {legal:?}"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(mv2)),
        })
        .expect("choosing the MV2 card must be accepted");

    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    assert!(
        runner.state().battlefield.contains(&mv2),
        "the chosen MV2 card must be returned to the battlefield"
    );
    assert!(
        !runner.state().players[0].graveyard.contains(&mv2),
        "the reanimated card must have left the graveyard"
    );
}
