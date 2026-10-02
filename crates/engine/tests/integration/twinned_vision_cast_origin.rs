//! Twinned Vision's owner-relative cast-origin self-replacement, through casting.

use engine::ai_support::legal_actions;
use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastingVariant, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim MTGJSON Oracle text, including the complete flashback cost.
const TWINNED_VISION: &str = "Draw a card. If this spell wasn't cast from your hand, draw two cards instead.\nFlashback—{1}{U/R}{U/R}, Discard a card. (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const TWINCAST: &str =
    "Copy target instant or sorcery spell. You may choose new targets for the copy.";
const COMMANDEER: &str = "You may exile two blue cards from your hand rather than pay this spell's mana cost.\nGain control of target noncreature spell. You may choose new targets for it. (If that spell is an artifact, enchantment, or planeswalker, the permanent enters under your control.)";
const FESTIVAL_OF_EMBERS: &str = "During your turn, you may cast instant and sorcery spells from your graveyard by paying 1 life in addition to their other costs.\nIf a card or token would be put into your graveyard from anywhere, exile it instead.\n{1}{R}: Sacrifice this enchantment.";

fn pool(colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn stage_library(scenario: &mut GameScenario, player: PlayerId) {
    for index in 0..8 {
        scenario.add_card_to_library_top(player, &format!("Draw witness {index}"));
    }
}

fn add_twinned(scenario: &mut GameScenario, zone: Zone) -> ObjectId {
    let mut card = match zone {
        Zone::Hand => scenario.add_spell_to_hand(P0, "Twinned Vision", true),
        Zone::Graveyard => scenario.add_spell_to_graveyard(P0, "Twinned Vision", true),
        other => panic!("unsupported fixture zone: {other:?}"),
    };
    card.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::BlueRed],
        generic: 1,
    })
    .from_oracle_text(TWINNED_VISION)
    .id()
}

/// CR 614.15 + CR 614.6: the hand-cast original performs only its one-card draw.
#[test]
fn twinned_vision_hand_cast_draws_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    let spell = add_twinned(&mut scenario, Zone::Hand);
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Red]));
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();

    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_zone(&[spell], Zone::Graveyard);
    assert!(outcome.state().stack.is_empty());
}

/// CR 702.34a + CR 701.9a: flashback pays mana and the selected discard, then exiles.
/// CR 614.15 + CR 614.6: the two-card self-replacement displaces the original draw.
#[test]
fn twinned_vision_flashback_draws_two_and_pays_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    let spell = add_twinned(&mut scenario, Zone::Graveyard);
    let discard = scenario.add_card_to_hand(P0, "Selected discard");
    let retained = scenario.add_card_to_hand(P0, "Retained card");
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Colorless, ManaType::Blue, ManaType::Red]),
    );
    let mut runner = scenario.build();

    let committed = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .pay_cost_with(&[discard])
        .commit();
    assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(committed.state().objects[&discard].zone, Zone::Graveyard);
    assert_eq!(committed.state().objects[&retained].zone, Zone::Hand);
    let outcome = committed.resolve();

    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&[spell], Zone::Exile);
    outcome.assert_zone(&[discard], Zone::Graveyard);
    outcome.assert_zone(&[retained], Zone::Hand);
}

/// CR 601.2a + CR 702.34a: a later flashback cast records its own graveyard origin.
#[test]
fn twinned_vision_flashback_after_hand_cast_uses_fresh_origin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    let spell = add_twinned(&mut scenario, Zone::Hand);
    let discard = scenario.add_card_to_hand(P0, "Recast discard");
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Red,
        ]),
    );
    let mut runner = scenario.build();
    let first = runner.cast(spell).resolve();
    first.assert_hand_drawn(P0, 1);
    first.assert_zone(&[spell], Zone::Graveyard);

    let second = runner
        .cast(spell)
        .casting_variant(CastingVariant::Flashback)
        .pay_cost_with(&[discard])
        .resolve();
    second.assert_hand_drawn(P0, 2);
    second.assert_zone(&[spell], Zone::Exile);
    second.assert_zone(&[discard], Zone::Graveyard);
}

/// CR 707.10: an uncast spell copy satisfies "wasn't cast from your hand".
#[test]
fn twinned_vision_hand_cast_copy_draws_three_total() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    let spell = add_twinned(&mut scenario, Zone::Hand);
    let twincast = scenario
        .add_spell_to_hand_from_oracle(P0, "Twincast", true, TWINCAST)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        pool(&[
            ManaType::Colorless,
            ManaType::Red,
            ManaType::Blue,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    let mut original = runner.cast(spell).commit();
    assert_eq!(original.state().objects[&spell].zone, Zone::Stack);

    // Twinned has no targets: CopyRetarget is unreachable, so resolve directly.
    let outcome = original.cast(twincast).target_object(spell).resolve();
    let copy = outcome
        .events()
        .iter()
        .find_map(|event| match event {
            GameEvent::SpellCopied {
                original_id,
                object_id,
                controller,
                ..
            } if *original_id == spell && *controller == P0 => Some(*object_id),
            _ => None,
        })
        .expect("Twincast must create a copy of the committed Twinned spell");
    assert_ne!(copy, spell, "the copy must have its own source object");
    // CR 707.10a: the distinct copied object ceases to exist after leaving the stack.
    assert!(!outcome.state().objects.contains_key(&copy));
    outcome.assert_hand_drawn(P0, 3);
    outcome.assert_zone(&[spell, twincast], Zone::Graveyard);
    assert!(outcome.state().stack.is_empty());
}

/// CR 109.5 + CR 400.3: "your hand" refers to the resolving controller's hand.
#[test]
fn twinned_vision_stolen_hand_spell_draws_two_for_new_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stage_library(&mut scenario, P0);
    stage_library(&mut scenario, P1);
    let spell = add_twinned(&mut scenario, Zone::Hand);
    let commandeer = scenario
        .add_spell_to_hand_from_oracle(P1, "Commandeer", true, COMMANDEER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 5,
        })
        .id();
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Red]));
    scenario.with_mana_pool(
        P1,
        pool(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    let mut original = runner.cast(spell).commit();
    assert_eq!(original.state().objects[&spell].owner, P0);
    assert_eq!(original.state().objects[&spell].controller, P0);
    original.act(GameAction::PassPriority).unwrap();
    let mut response = original.cast(commandeer).target_object(spell).commit();
    let mut optional_declined = false;
    let mut control_changed = false;
    for _ in 0..12 {
        if response.state().objects[&commandeer].zone != Zone::Stack
            && matches!(response.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let result = match response.state().waiting_for {
            WaitingFor::Priority { .. } => response.act(GameAction::PassPriority).unwrap(),
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_declined = true;
                response
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .unwrap()
            }
            ref other => panic!("unexpected Commandeer prompt: {other:?}"),
        };
        control_changed |= result.events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ControllerChanged { object_id, old_controller, new_controller }
                    if *object_id == spell && *old_controller == P0 && *new_controller == P1
            )
        });
    }
    assert!(
        optional_declined,
        "Commandeer's retarget rider must be reached"
    );
    assert!(
        control_changed,
        "Commandeer must emit the spell's control change"
    );
    assert_eq!(response.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(response.state().objects[&spell].owner, P0);
    assert_eq!(response.state().objects[&spell].controller, P1);
    // Commandeer's commit baseline accounts for its departure from P1's hand.
    let outcome = response.resolve();
    outcome.assert_hand_drawn(P1, 2);
    outcome.assert_hand_drawn(P0, 0);
    outcome.assert_zone(&[spell, commandeer], Zone::Graveyard);
}

/// CR 702.34a + CR 601.2h: flashback requires a discard even when mana is available.
#[test]
fn twinned_vision_flashback_requires_a_discardable_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_twinned(&mut scenario, Zone::Graveyard);
    scenario.with_mana_pool(
        P0,
        pool(&[ManaType::Colorless, ManaType::Blue, ManaType::Red]),
    );
    let runner = scenario.build();
    assert!(spell_objects_available_to_cast(runner.state(), P0).contains(&spell));
    assert!(!can_cast_object_now(runner.state(), P0, spell));
    assert!(!legal_actions(runner.state()).iter().any(|action| matches!(
        action,
        GameAction::CastSpell { object_id, .. } if *object_id == spell
    )));
}

/// CR 601.2a: another permission still records a graveyard cast, without flashback.
#[test]
fn twinned_vision_nonflashback_graveyard_permission_draws_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    stage_library(&mut scenario, P0);
    scenario
        .add_creature(P0, "Festival of Embers", 0, 0)
        .as_enchantment()
        .from_oracle_text(FESTIVAL_OF_EMBERS);
    let spell = add_twinned(&mut scenario, Zone::Graveyard);
    scenario.with_mana_pool(P0, pool(&[ManaType::Colorless, ManaType::Red]));
    let mut runner = scenario.build();
    assert!(runner.state().players[0].hand.is_empty());
    assert!(can_cast_object_now(runner.state(), P0, spell));
    let committed = runner.cast(spell).commit();
    assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
    let outcome = committed.resolve();
    outcome.assert_life_delta(P0, -1);
    outcome.assert_hand_drawn(P0, 2);
    outcome.assert_zone(&[spell], Zone::Exile);
}
