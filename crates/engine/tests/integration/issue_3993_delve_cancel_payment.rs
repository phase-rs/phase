//! Regression for issue #3993: cancelling during delve mana payment leaves the
//! delved graveyard cards where they were.
//!
//! CR 601.2h + CR 733.1: the delve exile is paid with the total cost, so a
//! cancelled cast has no exile to undo.
//!
//! https://github.com/phase-rs/phase/issues/3993

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    CastPermissionConstraint, CastingPermission, Comparator, ExileGrantCostProvenance, PlayerScope,
    QuantityExpr, QuantityRef, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ConvokeMode, ShardChoice, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::support::shared_card_db;

const DELVE_DRAW_ORACLE: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\n\
Draw a card.";

fn mana_pool(generic: usize, red: usize) -> Vec<ManaUnit> {
    let mut pool = Vec::new();
    for _ in 0..generic {
        pool.push(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..red {
        pool.push(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    }
    pool
}

#[test]
fn issue_3993_cancel_during_delve_payment_returns_graveyard_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Delve Draw", false, DELVE_DRAW_ORACLE)
        .id();
    let delve_a = scenario.add_spell_to_graveyard(P0, "Old Bolt", true).id();
    let delve_b = scenario.add_spell_to_graveyard(P0, "Old Shock", true).id();
    scenario.with_mana_pool(P0, mana_pool(3, 1));

    let mut runner = scenario.build();

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("begin casting delve spell");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment {
            convoke_mode: Some(ConvokeMode::Delve),
            ..
        }
    ));

    for gy_id in [delve_a, delve_b] {
        runner
            .act(GameAction::TapForConvoke {
                object_id: gy_id,
                mana_type: ManaType::Colorless,
            })
            .expect("delve graveyard card");
        assert_eq!(
            runner.state().objects[&gy_id].zone,
            Zone::Graveyard,
            "CR 601.2h: a delve selection exiles nothing before the cost is paid"
        );
    }

    runner
        .act(GameAction::CancelCast)
        .expect("cancel during delve payment");

    for gy_id in [delve_a, delve_b] {
        assert_eq!(
            runner.state().objects[&gy_id].zone,
            Zone::Graveyard,
            "cancelled delve payment must return the card to the graveyard"
        );
    }
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "cancelled spell must return to hand"
    );
    assert!(
        runner.state().stack.is_empty(),
        "cancelled spell must be removed from the stack"
    );
    assert!(
        !runner
            .state()
            .cards_exiled_with_source_this_turn
            .get(&spell)
            .is_some_and(|ids| ids.contains(&delve_a) || ids.contains(&delve_b)),
        "delve exile-with-source tracking must be cleared on cancel"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .mana
            .iter()
            .all(|unit| !unit.is_convoke_payment()),
        "delve mana markers must be removed from the pool on cancel"
    );
}

const CRUISE_ORACLE: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\n\
Draw three cards.";

fn cruise_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 7,
    }
}

fn graveyard_names(runner: &GameRunner) -> Vec<String> {
    runner.state().players[P0.0 as usize]
        .graveyard
        .iter()
        .map(|id| runner.state().objects[id].name.clone())
        .collect()
}

fn delve_marker_count(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .mana
        .iter()
        .filter(|unit| unit.is_convoke_payment())
        .count()
}

fn cast_manual(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin casting Treasure Cruise");
}

fn delve(runner: &mut GameRunner, card: ObjectId) {
    runner
        .act(GameAction::TapForConvoke {
            object_id: card,
            mana_type: ManaType::Colorless,
        })
        .expect("delve graveyard card");
}

fn assert_cancel_restored(runner: &GameRunner, spell: ObjectId, spell_zone: Zone, gy: &[&str]) {
    assert_eq!(graveyard_names(runner), gy);
    assert_eq!(runner.state().objects[&spell].zone, spell_zone);
    assert!(runner.state().stack.is_empty());
    assert_eq!(delve_marker_count(runner), 0);
}

/// Treasure Cruise in hand, `gy` in the graveyard (in order), optionally a Forest and a Dimir Signet.
fn cruise_in_hand(
    gy: &[&str],
    with_signet: bool,
) -> (
    GameRunner,
    ObjectId,
    Vec<ObjectId>,
    Option<(ObjectId, ObjectId)>,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cruise = scenario
        .add_spell_to_hand_from_oracle(P0, "Treasure Cruise", false, CRUISE_ORACLE)
        .from_oracle_text_with_keywords(&["Delve"], CRUISE_ORACLE)
        .with_mana_cost(cruise_cost())
        .id();
    let ids = gy
        .iter()
        .map(|name| scenario.add_spell_to_graveyard(P0, name, true).id())
        .collect();
    let lands = with_signet.then(|| {
        let forest = scenario.add_basic_land(P0, ManaColor::Green);
        let signet = scenario
            .add_artifact_from_oracle(P0, "Dimir Signet", "{1}, {T}: Add {U}{B}.")
            .id();
        (forest, signet)
    });
    (scenario.build(), cruise, ids, lands)
}

/// Treasure Cruise in exile castable only while its mana value is at most the
/// graveyard size, so delving the graveyard away fails the finalize re-check.
fn cruise_in_exile(constraint_value: QuantityExpr) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cruise = scenario
        .add_spell_to_exile(P0, "Treasure Cruise", false)
        .from_oracle_text_with_keywords(&["Delve"], CRUISE_ORACLE)
        .with_mana_cost(cruise_cost())
        .id();
    let ids = ["Lightning Bolt", "Island", "Shock"]
        .iter()
        .map(|name| scenario.add_spell_to_graveyard(P0, name, true).id())
        .collect();
    let mut pool = mana_pool(5, 0);
    pool.push(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&cruise)
        .expect("cruise exists")
        .casting_permissions
        .push(CastingPermission::ExileWithAltCost {
            source_id: None,
            cost_provenance: ExileGrantCostProvenance::Alternative,
            cost: cruise_cost(),
            cast_transformed: false,
            constraint: Some(CastPermissionConstraint::ManaValue {
                comparator: Comparator::LE,
                value: constraint_value,
            }),
            granted_to: Some(P0),
            resolution_cleanup: None,
            duration: None,
            graveyard_replacement: None,
            mana_spend_permission: None,
            enters_with_counter: None,
            enters_with_modifications: Vec::new(),
            cast_cost_modifier: None,
        });
    (runner, cruise, ids)
}

#[test]
fn rejected_finalize_control_fixed_constraint_casts() {
    let (mut runner, cruise, ids) = cruise_in_exile(QuantityExpr::Fixed { value: 8 });
    cast_manual(&mut runner, cruise);
    delve(&mut runner, ids[0]);
    delve(&mut runner, ids[2]);

    runner
        .act(GameAction::PassPriority)
        .expect("constraint satisfied, cast completes");
    assert_eq!(runner.state().objects[&cruise].zone, Zone::Stack);
}

const DELVE_DIVIDED_X_ORACLE: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\n\
~ deals X damage divided as you choose among any number of targets.";

#[test]
fn rejected_finalize_keeps_selection_and_cancel_moves_nothing() {
    let (mut runner, cruise, ids) = cruise_in_exile(QuantityExpr::Ref {
        qty: QuantityRef::GraveyardSize {
            player: PlayerScope::Controller,
        },
    });
    cast_manual(&mut runner, cruise);
    delve(&mut runner, ids[0]);
    delve(&mut runner, ids[2]);

    runner
        .act(GameAction::PassPriority)
        .expect_err("finalize re-check rejects the cast");
    assert_eq!(
        runner
            .state()
            .pending_cast
            .as_ref()
            .map(|p| p.delved_cards.len()),
        Some(2)
    );
    assert_eq!(delve_marker_count(&runner), 2);

    runner.act(GameAction::CancelCast).expect("cancel cast");
    assert_cancel_restored(
        &runner,
        cruise,
        Zone::Exile,
        &["Lightning Bolt", "Island", "Shock"],
    );
}

/// CR 702.66a: a selected card whose marker pays no generic mana is not exiled.
#[test]
fn commit_exiles_only_selected_cards_that_paid_generic_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Delve One", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(engine::types::keywords::Keyword::Delve)
        .id();
    let [a, b] = ["Delve Fuel A", "Delve Fuel B"]
        .map(|name| scenario.add_spell_to_graveyard(P0, name, true).id());
    let mut runner = scenario.build();
    cast_manual(&mut runner, spell);
    delve(&mut runner, a);
    delve(&mut runner, b);
    assert_eq!(delve_marker_count(&runner), 2);

    runner.act(GameAction::PassPriority).expect("commit");

    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
    let exiled: Vec<ObjectId> = [a, b]
        .into_iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Exile)
        .collect();
    assert_eq!(exiled.len(), 1, "one generic mana, one exiled card");
    assert_eq!(graveyard_names(&runner).len(), 1);
    assert_eq!(delve_marker_count(&runner), 0);
    assert_eq!(
        runner.state().cards_exiled_with_source_this_turn[&spell],
        exiled
    );
}

#[test]
fn cancel_after_signet_activation_leaves_selected_fuel_in_graveyard() {
    let (mut runner, cruise, ids, lands) = cruise_in_hand(&["Lightning Bolt", "Shock"], true);
    let (forest, signet) = lands.expect("signet fixture");
    cast_manual(&mut runner, cruise);

    let (_, _, grouped) = legal_actions_full(runner.state());
    let selection = grouped
        .get(&forest)
        .into_iter()
        .flatten()
        .find_map(|action| match action {
            GameAction::TapLandForMana { selection } => Some(selection.clone()),
            _ => None,
        })
        .expect("engine offers a Forest tap");
    runner
        .act(GameAction::TapLandForMana { selection })
        .expect("tap Forest");
    delve(&mut runner, ids[0]);
    delve(&mut runner, ids[1]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: signet,
            ability_index: 0,
        })
        .expect("activate Dimir Signet");

    let marker_sources: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .mana_pool
        .mana
        .iter()
        .filter(|unit| unit.is_convoke_payment())
        .map(|unit| unit.source_id)
        .collect();
    assert_eq!(
        marker_sources,
        [ids[1], ids[0]],
        "Signet activation must have reordered the delve markers"
    );

    assert_eq!(graveyard_names(&runner), ["Lightning Bolt", "Shock"]);

    runner.act(GameAction::CancelCast).expect("cancel cast");
    assert_cancel_restored(&runner, cruise, Zone::Hand, &["Lightning Bolt", "Shock"]);
}

/// CR 601.2h: selecting non-adjacent fuel moves nothing, so the graveyard order
/// is the pre-cast order at every step and after the cancel.
#[test]
fn selecting_non_adjacent_fuel_keeps_graveyard_order_through_cancel() {
    let (mut runner, cruise, ids, _) =
        cruise_in_hand(&["Lightning Bolt", "Island", "Shock"], false);
    cast_manual(&mut runner, cruise);
    delve(&mut runner, ids[0]);
    delve(&mut runner, ids[2]);
    for id in [ids[0], ids[2]] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    assert_eq!(
        graveyard_names(&runner),
        ["Lightning Bolt", "Island", "Shock"]
    );

    runner.act(GameAction::CancelCast).expect("cancel cast");
    assert_cancel_restored(
        &runner,
        cruise,
        Zone::Hand,
        &["Lightning Bolt", "Island", "Shock"],
    );
}

/// CR 601.2h: a selection that left the graveyard before the cost is paid cannot pay.
#[test]
fn commit_rejects_selection_that_left_the_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand(P0, "Delve One", true)
        .with_mana_cost(ManaCost::generic(1))
        .with_keyword(engine::types::keywords::Keyword::Delve)
        .id();
    let fuel = scenario.add_spell_to_graveyard(P0, "Delve Fuel", true).id();
    let mut runner = scenario.build();
    cast_manual(&mut runner, spell);
    delve(&mut runner, fuel);
    let mut events = Vec::new();
    engine::game::zone_pipeline::move_object_for_test(
        runner.state_mut(),
        engine::game::zone_pipeline::ZoneMoveRequest::effect(fuel, Zone::Exile, fuel),
        &mut events,
    );

    let error = runner
        .act(GameAction::PassPriority)
        .expect_err("the selected card is no longer in the graveyard");
    assert!(format!("{error:?}").contains("no longer in your graveyard"));
    assert!(runner.state().pending_cast.is_some());
    assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);

    runner.act(GameAction::CancelCast).expect("cancel cast");
    assert_eq!(delve_marker_count(&runner), 0);
    assert_eq!(runner.state().objects[&fuel].zone, Zone::Exile);
}

const DELVE_EVEN_SPLIT_ORACLE: &str =
    "Delve (Each card you exile from your graveyard while casting this spell pays for {1}.)\n\
~ deals X damage divided evenly, rounded down, among any number of targets.";

struct DistributionWitness {
    runner: GameRunner,
    spell: ObjectId,
    gy: Vec<ObjectId>,
    victims: Vec<ObjectId>,
    rows_before_cast: usize,
}

/// `{X}{shard}{1}` delve spell cast with X = 0 and the first `delved` graveyard cards
/// selected, ready for the post-payment step; `[Lightning Bolt, Island, Shock]` is the
/// graveyard.
fn delve_volley_payment(
    oracle: &str,
    shard: ManaCostShard,
    victims: usize,
    delved: usize,
) -> DistributionWitness {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Delve Volley", false, oracle)
        .from_oracle_text_with_keywords(&["Delve"], oracle)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, shard],
            generic: 1,
        })
        .id();
    let gy: Vec<ObjectId> = ["Lightning Bolt", "Island", "Shock"]
        .iter()
        .map(|name| scenario.add_spell_to_graveyard(P0, name, true).id())
        .collect();
    let victims: Vec<ObjectId> = (0..victims)
        .map(|i| scenario.add_creature(P1, &format!("Victim {i}"), 3, 3).id())
        .collect();
    scenario.with_mana_pool(P0, mana_pool(0, 1));
    let mut runner = scenario.build();
    let rows_before_cast = runner.state().zone_changes_this_turn.len();

    cast_manual(&mut runner, spell);
    runner
        .act(GameAction::ChooseX { value: 0 })
        .expect("announce X = 0");
    for &fuel in &gy[..delved] {
        delve(&mut runner, fuel);
    }
    DistributionWitness {
        runner,
        spell,
        gy,
        victims,
        rows_before_cast,
    }
}

impl DistributionWitness {
    fn pass(&mut self, shard: ManaCostShard) {
        self.runner
            .act(GameAction::PassPriority)
            .expect("finish payment");
        let phyrexian_prompted = matches!(
            self.runner.state().waiting_for,
            WaitingFor::PhyrexianPayment { .. }
        );
        assert_eq!(phyrexian_prompted, shard == ManaCostShard::PhyrexianRed);
        if phyrexian_prompted {
            self.runner
                .act(GameAction::SubmitPhyrexianChoices {
                    choices: vec![ShardChoice::PayMana],
                })
                .expect("pay the Phyrexian shard with mana");
        }
    }

    fn at_prompt(shard: ManaCostShard) -> Self {
        let mut witness = delve_volley_payment(DELVE_DIVIDED_X_ORACLE, shard, 0, 2);
        witness.pass(shard);
        assert!(
            matches!(
                witness.runner.state().waiting_for,
                WaitingFor::DistributeAmong { .. }
            ),
            "payment must end at the post-payment distribution, got {:?}",
            witness.runner.state().waiting_for
        );
        witness
    }
}

/// CR 601.2h + CR 733.1: the post-payment distribution (opened only because X = 0
/// leaves the pool empty at target selection) precedes the delve exile, so a cancel
/// there restores the graveyard order and moves nothing.
fn delve_fuel_stays_until_post_payment_distribution_cancels(shard: ManaCostShard) {
    let mut witness = DistributionWitness::at_prompt(shard);
    for fuel in &witness.gy[..2] {
        assert_eq!(witness.runner.state().objects[fuel].zone, Zone::Graveyard);
    }
    assert_eq!(delve_marker_count(&witness.runner), 0);

    witness
        .runner
        .act(GameAction::CancelCast)
        .expect("cancel cast");

    assert_cancel_restored(
        &witness.runner,
        witness.spell,
        Zone::Hand,
        &["Lightning Bolt", "Island", "Shock"],
    );
    assert_eq!(
        witness.runner.state().zone_changes_this_turn.len(),
        witness.rows_before_cast
    );
    assert!(witness.runner.state().exile_links.is_empty());
}

/// CR 601.2h + CR 702.66a: completing the distribution exiles only the selected
/// card whose marker paid the one generic mana.
fn delve_fuel_exiles_when_post_payment_distribution_completes(shard: ManaCostShard) {
    let mut witness = DistributionWitness::at_prompt(shard);

    witness
        .runner
        .act(GameAction::DistributeAmong {
            distribution: vec![],
        })
        .expect("empty division for X = 0");

    let exiled: Vec<ObjectId> = witness.gy[..2]
        .iter()
        .copied()
        .filter(|id| witness.runner.state().objects[id].zone == Zone::Exile)
        .collect();
    assert_eq!(exiled.len(), 1, "one generic mana, one exiled card");
    assert_eq!(graveyard_names(&witness.runner).len(), 2);
    assert_eq!(delve_marker_count(&witness.runner), 0);
    assert_eq!(
        witness.runner.state().cards_exiled_with_source_this_turn[&witness.spell],
        exiled
    );
}

#[test]
fn delve_fuel_stays_until_post_payment_distribution_paid_with_mana() {
    delve_fuel_stays_until_post_payment_distribution_cancels(ManaCostShard::Red);
    delve_fuel_exiles_when_post_payment_distribution_completes(ManaCostShard::Red);
}

#[test]
fn delve_fuel_stays_until_post_payment_distribution_after_phyrexian_choice() {
    delve_fuel_stays_until_post_payment_distribution_cancels(ManaCostShard::PhyrexianRed);
    delve_fuel_exiles_when_post_payment_distribution_completes(ManaCostShard::PhyrexianRed);
}

/// CR 601.2d + CR 601.2h: an even split needs no prompt, so the delve exile is
/// paid by the same commit that puts the spell on the stack. The ability holds its
/// targets already, which is the only shape that skips the post-payment prompt.
#[test]
fn even_split_damage_commit_exiles_delve_fuel_and_casts() {
    let mut witness = delve_volley_payment(DELVE_EVEN_SPLIT_ORACLE, ManaCostShard::Red, 2, 1);
    witness
        .runner
        .state_mut()
        .pending_cast
        .as_mut()
        .expect("payment window")
        .ability
        .targets = witness
        .victims
        .iter()
        .copied()
        .map(TargetRef::Object)
        .collect();

    witness
        .runner
        .act(GameAction::PassPriority)
        .expect("finish payment");

    let fuel = witness.gy[0];
    assert_eq!(
        witness.runner.state().objects[&witness.spell].zone,
        Zone::Stack
    );
    assert_eq!(witness.runner.state().objects[&fuel].zone, Zone::Exile);
    assert_eq!(delve_marker_count(&witness.runner), 0);
}

/// CR 601.2a + CR 702.66a: a delve spell cast from the graveyard is already on
/// the stack, so it cannot exile itself to pay for its own cost. Hogaak is cast
/// from the graveyard (Manual payment) with two other graveyard cards as fuel.
#[test]
fn delve_spell_cast_from_graveyard_cannot_select_itself() {
    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_spell_to_graveyard(P0, "Pre A", true);
    let hogaak = scenario.add_real_card(P0, "Hogaak, Arisen Necropolis", Zone::Graveyard, db);
    let fuel = scenario.add_spell_to_graveyard(P0, "Fuel", true).id();
    let mut runner = scenario.build();

    cast_manual(&mut runner, hogaak);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    let selectable: Vec<ObjectId> = legal_actions_full(runner.state())
        .0
        .iter()
        .filter_map(|action| match action {
            GameAction::TapForConvoke { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert!(
        selectable.contains(&fuel),
        "other graveyard cards stay selectable"
    );
    assert!(
        !selectable.contains(&hogaak),
        "the spell cannot delve itself"
    );

    let gy_before = graveyard_names(&runner);
    runner
        .act(GameAction::TapForConvoke {
            object_id: hogaak,
            mana_type: ManaType::Colorless,
        })
        .expect_err("selecting the spell being cast is rejected");
    assert_eq!(graveyard_names(&runner), gy_before);
    assert_eq!(delve_marker_count(&runner), 0);
}

/// Real Treasure Cruise in the graveyard with flashback (its own mana cost),
/// `others` other graveyard cards, and `blue` + `colorless` mana in the pool.
fn cruise_flashback_from_graveyard(
    others: usize,
    colorless: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    cruise_flashback_with(others, colorless, false)
}

/// As `cruise_flashback_from_graveyard`; `convoke_bear` also grants the spell
/// Convoke and puts one untapped creature on the battlefield.
fn cruise_flashback_with(
    others: usize,
    colorless: usize,
    convoke_bear: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    use engine::types::keywords::{FlashbackCost, Keyword};

    let db = shared_card_db().expect("card db");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if convoke_bear {
        scenario.add_creature(P0, "Bear", 2, 2);
    }
    let cruise = scenario.add_real_card(P0, "Treasure Cruise", Zone::Graveyard, db);
    let fuel = (0..others)
        .map(|i| {
            scenario
                .add_spell_to_graveyard(P0, &format!("Fuel {i}"), true)
                .id()
        })
        .collect();
    let mut pool = mana_pool(colorless, 0);
    pool.push(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    scenario.with_mana_pool(P0, pool);
    let mut runner = scenario.build();

    let object = runner.state_mut().objects.get_mut(&cruise).expect("cruise");
    assert!(
        object
            .keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Delve)),
        "real Treasure Cruise carries Delve"
    );
    let flashback = Keyword::Flashback(FlashbackCost::Mana(object.mana_cost.clone()));
    object.base_keywords.push(flashback.clone());
    object.keywords.push(flashback);
    if convoke_bear {
        object.base_keywords.push(Keyword::Convoke);
        object.keywords.push(Keyword::Convoke);
    }
    (runner, cruise, fuel)
}

fn cast_offered(runner: &GameRunner, spell: ObjectId) -> bool {
    legal_actions_full(runner.state()).0.iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell),
    )
}

/// CR 601.2a + CR 702.66a: the affordability preview must not count the spell
/// being cast from the graveyard as its own delve fuel. Cost {7}{U} against
/// {U} + 4 colorless: two other graveyard cards reach 7 mana only if the spell
/// counts itself; a third real fuel card makes the cast genuinely payable.
#[test]
fn delve_affordability_preview_excludes_spell_cast_from_graveyard() {
    let (runner, cruise, _) = cruise_flashback_from_graveyard(3, 4);
    assert!(
        cast_offered(&runner, cruise),
        "control: three real fuel cards make the cast payable"
    );

    let (runner, cruise, _) = cruise_flashback_from_graveyard(2, 4);
    assert!(
        !cast_offered(&runner, cruise),
        "two real fuel cards cannot pay; the spell is not its own fuel"
    );
}

/// CR 601.2a + CR 702.66a: with Delve as the only tap-payment keyword the spell
/// is absent from the selectable fuel and a direct selection is rejected.
#[test]
fn pure_delve_spell_cast_from_graveyard_cannot_select_itself() {
    let (mut runner, cruise, fuel) = cruise_flashback_from_graveyard(3, 4);
    cast_manual(&mut runner, cruise);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    let selectable: Vec<ObjectId> = legal_actions_full(runner.state())
        .0
        .iter()
        .filter_map(|action| match action {
            GameAction::TapForConvoke { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert!(
        fuel.iter().all(|id| selectable.contains(id)),
        "other graveyard cards stay selectable"
    );
    assert!(
        !selectable.contains(&cruise),
        "the spell cannot delve itself"
    );

    let gy_before = graveyard_names(&runner);
    runner
        .act(GameAction::TapForConvoke {
            object_id: cruise,
            mana_type: ManaType::Colorless,
        })
        .expect_err("selecting the spell being cast is rejected");
    assert_eq!(graveyard_names(&runner), gy_before);
    assert_eq!(delve_marker_count(&runner), 0);
}

/// CR 601.2a + CR 702.66a: a delve-only spell cast from the graveyard with no
/// other graveyard card has no delve fuel, so Auto payment of an affordable
/// {7}{U} must not stop at a manual payment step.
#[test]
fn delve_only_spell_cast_from_graveyard_without_other_fuel_auto_pays() {
    let (mut runner, cruise, fuel) = cruise_flashback_from_graveyard(0, 7);
    assert!(fuel.is_empty(), "reach guard: no other graveyard card");
    let card_id = runner.state().objects[&cruise].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: cruise,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Treasure Cruise from the graveyard");
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
        "the spell is not its own delve fuel, so the convoke-mode gate stays closed"
    );
    assert_eq!(
        runner.state().objects[&cruise].zone,
        Zone::Stack,
        "reach guard: the pool paid the full cost"
    );
}

/// CR 601.2a + CR 702.66a: Delve composing with Convoke counts only other
/// graveyard cards. {7}{U} against {U} + 4 colorless + one creature needs two
/// real fuel cards; one fuel card plus the spell itself must not suffice.
#[test]
fn delve_composed_with_convoke_excludes_spell_cast_from_graveyard() {
    let (runner, cruise, _) = cruise_flashback_with(2, 4, true);
    assert!(
        cast_offered(&runner, cruise),
        "control: two real fuel cards + creature + 4 mana pay {{7}}{{U}}"
    );

    let (runner, cruise, _) = cruise_flashback_with(1, 4, true);
    assert!(
        !cast_offered(&runner, cruise),
        "one real fuel card cannot pay; the spell is not its own fuel"
    );
}

/// CR 601.2a + CR 702.66a: the X-value ceiling counts delve fuel without the
/// spell cast from the graveyard; cost {X}{U} with one other card allows X = 1.
#[test]
fn x_delve_max_excludes_spell_cast_from_graveyard() {
    use engine::types::keywords::{FlashbackCost, Keyword};

    let (mut runner, cruise, _) = cruise_flashback_from_graveyard(1, 0);
    let cost = ManaCost::Cost {
        shards: vec![ManaCostShard::X, ManaCostShard::Blue],
        generic: 0,
    };
    let object = runner.state_mut().objects.get_mut(&cruise).expect("cruise");
    object.mana_cost = cost.clone();
    for keyword in object
        .keywords
        .iter_mut()
        .chain(object.base_keywords.iter_mut())
    {
        if let Keyword::Flashback(FlashbackCost::Mana(flashback)) = keyword {
            *flashback = cost.clone();
        }
    }

    cast_manual(&mut runner, cruise);
    match &runner.state().waiting_for {
        WaitingFor::ChooseXValue { max, .. } => {
            assert_eq!(*max, 1, "max X must count only the one real fuel card")
        }
        other => panic!("expected ChooseXValue, got {other:?}"),
    }
}
