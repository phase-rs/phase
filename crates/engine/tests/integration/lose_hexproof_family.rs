//! CR 702.11e: losing hexproof removes every hexproof-from ability.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{TargetFilter, TargetRef, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{HexproofFilter, Keyword, KeywordKind, ProtectionTarget, WardCost};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SHADOWSPEAR: &str = "Equipped creature gets +1/+1 and has trample and lifelink.\n{1}: Permanents your opponents control lose hexproof and indestructible until end of turn.\nEquip {2}";
const KNIGHT_OF_GRACE: &str = "First strike (This creature deals combat damage before creatures without first strike.)\nHexproof from black (This creature can't be the target of black spells or abilities your opponents control.)\nThis creature gets +1/+0 as long as any player controls a black permanent.";

fn mana(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|ty| ManaUnit::new(*ty, ObjectId(0), false, vec![]))
        .collect()
}

fn spear(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "Shadowspear", SHADOWSPEAR)
        .id()
}

fn qualified() -> Keyword {
    Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Black))
}

fn complete(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "resolution must empty the stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must return to priority: {:?}",
        runner.state().waiting_for
    );
}

fn has(runner: &GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state().objects[&id].keywords.contains(keyword)
}

fn lost_family(runner: &GameRunner, id: ObjectId) {
    assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    assert!(
        !runner.state().objects[&id]
            .keywords
            .iter()
            .any(|kw| kw.kind() == KeywordKind::Hexproof),
        "lose hexproof must remove the entire family from {id:?}"
    );
}

fn targetable(
    runner: &GameRunner,
    source: ObjectId,
    controller: PlayerId,
    target: ObjectId,
) -> bool {
    find_legal_targets(
        runner.state(),
        &TargetFilter::Typed(TypedFilter::creature()),
        controller,
        source,
    )
    .contains(&TargetRef::Object(target))
}

#[test]
fn lose_hexproof_shadowspear_all_qualities_and_targeting() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Black,
            ManaType::Black,
        ]),
    );
    let spear = spear(&mut scenario);
    let knight = scenario
        .add_creature_from_oracle(P1, "Knight of Grace", 2, 2, KNIGHT_OF_GRACE)
        .from_oracle_text_with_keywords(&["First strike", "Hexproof from black"], KNIGHT_OF_GRACE)
        .with_color(vec![ManaColor::White])
        .id();
    let plain = scenario
        .add_creature(P1, "Plain Hexproof", 2, 2)
        .hexproof()
        .id();
    let indestructible = scenario
        .add_creature(P1, "Indestructible", 2, 2)
        .indestructible()
        .id();
    let ordinary = scenario.add_creature(P1, "Unprotected", 2, 2).id();
    // Synthetic augmentation exercises multiple qualities and unrelated abilities together.
    let ward = Keyword::Ward(WardCost::Mana(ManaCost::generic(2)));
    let protection = Keyword::Protection(ProtectionTarget::Color(ManaColor::Red));
    let many = scenario
        .add_creature(P1, "Multiple Qualities", 2, 2)
        .with_keyword(qualified())
        .with_keyword(Keyword::HexproofFrom(HexproofFilter::Color(
            ManaColor::Blue,
        )))
        .with_keyword(Keyword::HexproofFrom(HexproofFilter::CardType(
            "Artifact".into(),
        )))
        .with_keyword(Keyword::HexproofFrom(HexproofFilter::Quality(
            "multicolored".into(),
        )))
        .with_keyword(Keyword::HexproofFrom(HexproofFilter::ChosenColor))
        .with_keyword(ward.clone())
        .with_keyword(protection.clone())
        .with_keyword(Keyword::Shroud)
        .first_strike()
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    let mut runner = scenario.build();
    assert!(has(&runner, knight, &qualified()));
    assert!(has(&runner, knight, &Keyword::FirstStrike));
    assert_eq!(
        runner.state().objects[&knight].color,
        vec![ManaColor::White]
    );
    assert_eq!(
        runner.state().objects[&murder].color,
        vec![ManaColor::Black]
    );
    assert!(!targetable(&runner, murder, P0, knight));
    assert!(targetable(&runner, murder, P0, ordinary));
    assert!(targetable(&runner, murder, P1, knight));
    assert!(has(&runner, plain, &Keyword::Hexproof));
    assert!(has(&runner, indestructible, &Keyword::Indestructible));

    runner.activate(spear, 0).resolve();
    complete(&runner);
    // Positive controls are checked before the base-discriminating assertion.
    assert!(!has(&runner, plain, &Keyword::Hexproof));
    assert!(!has(&runner, indestructible, &Keyword::Indestructible));
    assert!(has(&runner, knight, &Keyword::FirstStrike));
    for keyword in [ward, protection, Keyword::Shroud, Keyword::FirstStrike] {
        assert!(has(&runner, many, &keyword));
    }
    // CR 702.11d/e: the same black source becomes legal only after family removal.
    assert!(
        targetable(&runner, murder, P0, knight),
        "after Shadowspear resolves, black Murder must be able to target Knight of Grace"
    );
    for id in [knight, plain, many] {
        lost_family(&runner, id);
    }
    runner.cast(murder).target_object(knight).resolve();
    complete(&runner);
    assert_eq!(runner.state().objects[&knight].zone, Zone::Graveyard);
}

#[test]
fn lose_hexproof_shadowspear_opponents_not_owner() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(P0, mana(&[ManaType::Colorless]));
    let spear = spear(&mut scenario);
    let stolen = scenario
        .add_creature(P0, "Owned Here Controlled There", 2, 2)
        .with_keyword(qualified())
        .controlled_by(P1)
        .id();
    let other_opponent = scenario
        .add_artifact_from_oracle(p2, "Protected Noncreature", "")
        .with_keyword(qualified())
        .id();
    let own = scenario
        .add_creature(P1, "Owned There Controlled Here", 2, 2)
        .with_keyword(qualified())
        .controlled_by(P0)
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&stolen].owner, P0);
    assert_eq!(runner.state().objects[&stolen].controller, P1);
    assert_eq!(runner.state().objects[&own].owner, P1);
    assert_eq!(runner.state().objects[&own].controller, P0);
    for id in [stolen, other_opponent, own] {
        assert!(has(&runner, id, &qualified()));
    }
    runner.activate(spear, 0).resolve();
    complete(&runner);
    assert!(has(&runner, own, &qualified()));
    for id in [stolen, other_opponent] {
        lost_family(&runner, id);
    }
}

#[test]
fn lose_hexproof_restores_after_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(P0, mana(&[ManaType::Colorless]));
    let spear = spear(&mut scenario);
    let protected = scenario
        .add_creature(P1, "Protected Survivor", 2, 2)
        .with_keyword(qualified())
        .hexproof()
        .indestructible()
        .id();
    let mut runner = scenario.build();
    let turn = runner.state().turn_number;
    for kw in [qualified(), Keyword::Hexproof, Keyword::Indestructible] {
        assert!(has(&runner, protected, &kw));
    }
    runner.activate(spear, 0).resolve();
    complete(&runner);
    lost_family(&runner, protected);
    assert!(!has(&runner, protected, &Keyword::Indestructible));
    runner.advance_to_end_step();
    assert_eq!(runner.state().phase, Phase::End);
    assert_eq!(runner.state().turn_number, turn);
    lost_family(&runner, protected);
    assert!(!has(&runner, protected, &Keyword::Indestructible));
    // CR 514.2 + CR 611.2a: restoration happens at cleanup, not the end step.
    runner.advance_to_upkeep();
    assert_eq!(runner.state().phase, Phase::Upkeep);
    assert_eq!(runner.state().turn_number, turn + 1);
    complete(&runner);
    for kw in [qualified(), Keyword::Hexproof, Keyword::Indestructible] {
        assert!(has(&runner, protected, &kw));
    }
}

#[test]
fn lose_hexproof_source_left_still_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(
        P0,
        mana(&[ManaType::Colorless, ManaType::Colorless, ManaType::White]),
    );
    let spear = spear(&mut scenario);
    let protected = scenario
        .add_creature(P1, "Protected Survivor", 2, 2)
        .with_keyword(qualified())
        .hexproof()
        .id();
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disenchant",
            true,
            "Destroy target artifact or enchantment.",
        )
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();
    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: spear,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().stack[0].source_id, spear);
    assert_eq!(runner.state().stack[0].controller, P0);
    let mut response = runner.cast(disenchant).target_object(spear).commit();
    assert_eq!(response.state().stack.len(), 2);
    assert_eq!(response.state().stack[1].source_id, disenchant);
    for _ in 0..4 {
        if response.state().objects[&spear].zone == Zone::Graveyard {
            break;
        }
        response.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(response.state().objects[&spear].zone, Zone::Graveyard);
    assert_eq!(response.state().stack.len(), 1);
    assert_eq!(response.state().stack[0].source_id, spear);
    assert!(response.state().objects[&protected]
        .keywords
        .contains(&qualified()));
    // CR 113.7a: the queued ability survives destruction of its source.
    response.resolve();
    complete(&runner);
    assert!(!has(&runner, protected, &Keyword::Hexproof));
    lost_family(&runner, protected);
}

#[test]
fn lose_hexproof_no_opponent_permanents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_mana_pool(P0, mana(&[ManaType::Colorless, ManaType::Colorless]));
    let spear = spear(&mut scenario);
    let own = scenario
        .add_creature(P0, "Own Protected Survivor", 2, 2)
        .with_keyword(qualified())
        .hexproof()
        .indestructible()
        .id();
    let mut runner = scenario.build();
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
    runner.activate(spear, 0).resolve();
    complete(&runner);
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    for kw in [qualified(), Keyword::Hexproof, Keyword::Indestructible] {
        assert!(has(&runner, own, &kw));
    }
}
