//! Regression: **Fireblast** exiled by an impulse draw (Wrenn's Resolve,
//! Experimental Synthesizer) — "You may sacrifice two Mountains rather than
//! pay this spell's mana cost."
//!
//! CR 118.9 + CR 601.2b: a spell's own printed alternative cost applies to any
//! cast that would otherwise pay its printed mana cost, not only to a cast from
//! hand. "You may play those cards" authorizes exactly that cast. Field report
//! (2026-09-20): with both Mountains tapped the exiled Fireblast was not
//! castable at all, while the same card in hand was.
//!
//! CR 118.9a: only one alternative cost applies to a spell, so the offer must
//! NOT reach a cast whose authority already replaces the mana cost ("without
//! paying its mana cost").

use engine::game::casting::{can_cast_object_now, current_casting_variant_choice_options};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    CardPlayMode, CastCostModifier, CastingPermission, ContinuousModification, Duration,
    ExileGrantCostProvenance, PlayFromExileProvenance, StaticDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::keywords::{Keyword, KeywordKind};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CastFrequency, CostModifyMode};
use engine::types::zones::{EtbTapState, Zone};
use engine::types::{CardId, ObjectId};

const FIREBLAST: &str = "You may sacrifice two Mountains rather than pay this spell's mana cost.\n\
Fireblast deals 4 damage to any target.";
const WRENNS_RESOLVE: &str = "Exile the top two cards of your library. Until the end of your next \
turn, you may play those cards.";
const MASSACRE: &str = "If an opponent controls a Plains and you control a Swamp, you may cast \
this spell without paying its mana cost.\nAll creatures get -2/-2 until end of turn.";
const FORCE_OF_WILL: &str = "You may pay 1 life and exile a blue card from your hand rather than pay this spell's mana cost.\n\
Counter target spell.";

fn fireblast_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 4,
    }
}

fn red_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

/// A plain impulse-draw grant ("you may play that card"), as Wrenn's Resolve and
/// Experimental Synthesizer leave on the exiled card.
fn impulse_grant(granted_to: PlayerId, modifier: Option<CastCostModifier>) -> CastingPermission {
    CastingPermission::PlayFromExile {
        provenance: PlayFromExileProvenance::Impulse,
        mode: CardPlayMode::Play,
        duration: Duration::Permanent,
        granted_to,
        frequency: CastFrequency::Unlimited,
        source_id: None,
        invalidation: None,
        exiled_by_ability_controller: None,
        mana_spend_permission: None,
        card_filter: None,
        single_use_group: None,
        single_use: false,
        cast_cost_modifier: modifier,
        alt_ability_cost: None,
        without_paying_mana_cost: false,
        land_enter_tapped: EtbTapState::Unspecified,
    }
}

fn free_exile_grant(granted_to: PlayerId) -> CastingPermission {
    CastingPermission::ExileWithAltCost {
        source_id: None,
        cost_provenance: ExileGrantCostProvenance::Alternative,
        cost: ManaCost::zero(),
        cast_transformed: false,
        constraint: None,
        granted_to: Some(granted_to),
        resolution_cleanup: None,
        duration: None,
        graveyard_replacement: None,
        mana_spend_permission: None,
        enters_with_counter: None,
        enters_with_modifications: Default::default(),
        cast_cost_modifier: None,
    }
}

fn exiled_fireblast(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    let mut card = scenario.add_spell_to_exile(owner, "Fireblast", true);
    card.from_oracle_text(FIREBLAST);
    card.with_mana_cost(fireblast_cost());
    card.id()
}

fn grant(runner: &mut GameRunner, card: ObjectId, permission: CastingPermission) {
    runner
        .state_mut()
        .objects
        .get_mut(&card)
        .unwrap()
        .casting_permissions
        .push(permission);
}

fn mountains_on_battlefield(runner: &GameRunner, player: PlayerId) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name == "Mountain" && state.objects[id].controller == player)
        .count()
}

/// Announce `spell` and walk the cast to its end. `pay_alternative` answers the
/// alternative-cost choice, which MUST then be offered (a cast that silently
/// pays the printed cost fails the row); `None` asserts it is never offered.
fn cast(runner: &mut GameRunner, spell: ObjectId, pay_alternative: Option<bool>) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce the spell");
    finish_cast(runner, pay_alternative);
}

fn finish_cast(runner: &mut GameRunner, pay_alternative: Option<bool>) {
    let mut offered = false;
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                assert_eq!(
                    offered,
                    pay_alternative.is_some(),
                    "the printed alternative cost must be offered exactly when expected"
                );
                return;
            }
            WaitingFor::OptionalCostChoice { .. } => {
                offered = true;
                GameAction::DecideOptionalCost {
                    pay: pay_alternative.expect(
                        "CR 118.9a: this cast must not be offered the printed alternative cost",
                    ),
                }
            }
            other => engine::ai_support::legal_actions(runner.state())
                .into_iter()
                .find(|a| !matches!(a, GameAction::CancelCast))
                .unwrap_or_else(|| panic!("no legal action while waiting for {other:?}")),
        };
        runner.act(action).expect("cast step");
    }
    panic!("the cast did not finish: {:?}", runner.state().waiting_for);
}

/// The reported game: the REAL Wrenn's Resolve resolver exiles Fireblast, both
/// Mountains are tapped and the pool is empty.
#[test]
fn fireblast_exiled_by_impulse_draw_offers_the_mountain_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountains = [
        scenario.add_basic_land(P0, ManaColor::Red),
        scenario.add_basic_land(P0, ManaColor::Red),
    ];
    // Two red mana pay for Wrenn's Resolve; nothing is left for {4}{R}{R}.
    scenario.with_mana_pool(P0, red_mana(2));
    let resolve = {
        let mut card =
            scenario.add_spell_to_hand_from_oracle(P0, "Wrenn's Resolve", false, WRENNS_RESOLVE);
        card.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        });
        card.id()
    };
    let filler = scenario.add_spell_to_library_top(P0, "Filler", false).id();
    let fireblast = {
        let mut card = scenario.add_spell_to_library_top(P0, "Fireblast", true);
        card.from_oracle_text(FIREBLAST);
        card.with_mana_cost(fireblast_cost());
        card.id()
    };
    let mut runner = scenario.build();
    for id in mountains {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }

    runner.cast(resolve).resolve();
    let state = runner.state();
    assert_eq!(state.objects[&fireblast].zone, Zone::Exile);
    assert_eq!(state.objects[&filler].zone, Zone::Exile);
    assert!(state.players[0].mana_pool.is_empty());

    assert!(
        can_cast_object_now(runner.state(), P0, fireblast),
        "two Mountains make the exiled Fireblast castable with no mana"
    );
    cast(&mut runner, fireblast, Some(true));

    let state = runner.state();
    assert_eq!(state.stack.len(), 1, "Fireblast is on the stack");
    for id in mountains {
        assert_eq!(
            state.objects[&id].zone,
            Zone::Graveyard,
            "both Mountains were sacrificed"
        );
    }
}

/// Declining the alternative cost from exile pays the printed mana cost and
/// leaves the Mountains alone.
#[test]
fn declining_the_alternative_cost_from_exile_pays_the_printed_mana_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.with_mana_pool(P0, red_mana(6));
    let fireblast = exiled_fireblast(&mut scenario, P0);
    let mut runner = scenario.build();
    grant(&mut runner, fireblast, impulse_grant(P0, None));

    cast(&mut runner, fireblast, Some(false));

    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(mountains_on_battlefield(&runner, P0), 2);
    assert!(
        runner.state().players[0].mana_pool.is_empty(),
        "the printed {{4}}{{R}}{{R}} was paid"
    );
}

/// The printed sacrifice cost needs two Mountains even though its Oracle text
/// has no separate condition.
#[test]
fn one_mountain_does_not_make_the_exiled_fireblast_castable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    let fireblast = exiled_fireblast(&mut scenario, P0);
    let mut runner = scenario.build();
    grant(&mut runner, fireblast, impulse_grant(P0, None));

    assert!(runner.state().objects[&fireblast]
        .casting_permissions
        .iter()
        .any(|permission| matches!(permission, CastingPermission::PlayFromExile { .. })));
    assert!(!can_cast_object_now(runner.state(), P0, fireblast));

    let pool = &mut runner.state_mut().players[0].mana_pool;
    for unit in red_mana(6) {
        pool.add(unit);
    }
    assert!(can_cast_object_now(runner.state(), P0, fireblast));
    cast(&mut runner, fireblast, None);

    let state = runner.state();
    assert!(matches!(
        state.stack.back(),
        Some(StackEntry {
            id,
            kind: StackEntryKind::Spell {
                casting_variant: CastingVariant::Normal,
                ..
            },
            ..
        }) if *id == fireblast
    ));
    assert!(state.players[0].mana_pool.is_empty());
    assert_eq!(mountains_on_battlefield(&runner, P0), 1);
}

/// CR 601.2a + CR 112.2: the caster of an impulse cast — the spell's controller,
/// who pays its alternative cost (CR 118.9) — is the GRANTEE. An opponent's
/// Fireblast exiled for P0 (Stolen Strategy class) is cast by P0 with P0's
/// Mountains; the owner, who holds no grant, cannot cast it.
#[test]
fn grantee_casts_an_opponents_exiled_fireblast_with_their_own_mountains() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P1, ManaColor::Red);
    scenario.add_basic_land(P1, ManaColor::Red);
    let fireblast = exiled_fireblast(&mut scenario, P1);
    let mut runner = scenario.build();
    grant(&mut runner, fireblast, impulse_grant(P0, None));

    assert!(can_cast_object_now(runner.state(), P0, fireblast));
    assert!(!can_cast_object_now(runner.state(), P1, fireblast));
    cast(&mut runner, fireblast, Some(true));

    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(
        mountains_on_battlefield(&runner, P0),
        0,
        "the caster's Mountains pay"
    );
    assert_eq!(
        mountains_on_battlefield(&runner, P1),
        2,
        "the owner's Mountains are untouched"
    );
}

/// The other printed-option kind: "you may cast this spell without paying its
/// mana cost" (Massacre, Fierce Guardianship) reaches the impulse cast too.
#[test]
fn printed_free_cast_condition_reaches_the_impulse_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Black);
    scenario.add_basic_land(P1, ManaColor::White);
    let massacre = {
        let mut card = scenario.add_spell_to_exile(P0, "Massacre", false);
        card.from_oracle_text(MASSACRE);
        card.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 2,
        });
        card.id()
    };
    let mut runner = scenario.build();
    grant(&mut runner, massacre, impulse_grant(P0, None));

    assert!(can_cast_object_now(runner.state(), P0, massacre));
    cast(&mut runner, massacre, Some(true));
    assert_eq!(runner.state().stack.len(), 1);
}

#[test]
fn massacre_without_its_printed_condition_pays_ordinary_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    let massacre = scenario
        .add_spell_to_exile(P0, "Massacre", false)
        .from_oracle_text(MASSACRE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 2,
        })
        .id();
    let mut runner = scenario.build();
    grant(&mut runner, massacre, impulse_grant(P0, None));
    assert!(can_cast_object_now(runner.state(), P0, massacre));
    cast(&mut runner, massacre, None);
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.id),
        Some(massacre)
    );
}

/// Force of Will's printed cost combines a life payment with exiling a blue
/// hand card. This follows the same impulse authority as Fireblast.
#[test]
fn impulse_force_of_will_pays_its_composite_printed_alternative() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let force = scenario
        .add_spell_to_exile(P0, "Force of Will", true)
        .from_oracle_text(FORCE_OF_WILL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let pitch = scenario
        .add_creature_to_hand(P0, "Blue Pitch Card", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let mut runner = scenario.build();
    grant(&mut runner, force, impulse_grant(P0, None));
    let target = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(900_196),
        P1,
        "Target Spell".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    runner.state_mut().stack.push_back(StackEntry {
        id: target,
        source_id: target,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(900_196),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    assert!(can_cast_object_now(runner.state(), P0, force));
    let life_before = runner.state().players[0].life;
    let commit = runner
        .cast(force)
        .accept_optional()
        .target_objects(&[target])
        .pay_cost_with(&[pitch])
        .commit();
    assert_eq!(
        commit.state().stack.back().map(|entry| entry.id),
        Some(force)
    );
    assert_eq!(commit.state().players[0].life, life_before - 1);
    assert_eq!(commit.state().objects[&pitch].zone, Zone::Exile);
}

#[test]
fn force_of_will_without_a_blue_pitch_card_keeps_the_ordinary_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
            .collect(),
    );
    let force = scenario
        .add_spell_to_exile(P0, "Force of Will", true)
        .from_oracle_text(FORCE_OF_WILL)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 3,
        })
        .id();
    let mut runner = scenario.build();
    grant(&mut runner, force, impulse_grant(P0, None));
    let target = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(900_197),
        P1,
        "Target Spell".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&target)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Instant];
    runner.state_mut().stack.push_back(StackEntry {
        id: target,
        source_id: target,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(900_197),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    assert!(can_cast_object_now(runner.state(), P0, force));
    let commit = runner.cast(force).target_objects(&[target]).commit();
    assert_eq!(
        commit.state().stack.back().map(|entry| entry.id),
        Some(force)
    );
    assert!(commit.state().players[0].mana_pool.is_empty());
}

/// CR 118.9a: a cast authorized "without paying its mana cost" already uses an
/// alternative cost, so the printed one is not offered on top of it.
#[test]
fn free_cast_from_exile_does_not_also_offer_the_printed_alternative_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    let fireblast = exiled_fireblast(&mut scenario, P0);
    let mut runner = scenario.build();
    grant(&mut runner, fireblast, free_exile_grant(P0));

    cast(&mut runner, fireblast, None);

    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(mountains_on_battlefield(&runner, P0), 2);
}

/// CR 118.9a + CR 601.2b: the cost belongs to the elected authority even
/// when both permissions function. The selector elects the alternative-cost
/// authority in either vector order.
#[test]
fn two_live_exile_grants_never_borrow_the_sibling_impulse_option() {
    for free_first in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_basic_land(P0, ManaColor::Red);
        scenario.add_basic_land(P0, ManaColor::Red);
        let fireblast = exiled_fireblast(&mut scenario, P0);
        let mut runner = scenario.build();
        let grants = if free_first {
            [free_exile_grant(P0), impulse_grant(P0, None)]
        } else {
            [impulse_grant(P0, None), free_exile_grant(P0)]
        };
        for permission in grants {
            grant(&mut runner, fireblast, permission);
        }

        assert!(can_cast_object_now(runner.state(), P0, fireblast));
        cast(&mut runner, fireblast, None);
        assert_eq!(
            runner.state().stack.back().map(|entry| entry.id),
            Some(fireblast)
        );
        assert_eq!(mountains_on_battlefield(&runner, P0), 2);
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    let fireblast = exiled_fireblast(&mut scenario, P0);
    let mut runner = scenario.build();
    grant(&mut runner, fireblast, impulse_grant(P0, None));
    cast(&mut runner, fireblast, Some(true));
    assert_eq!(mountains_on_battlefield(&runner, P0), 0);
}

/// A used earlier grant cannot veto the functioning impulse grant elected
/// after it. The positive cast reaches the cost prompt and pays both Mountains.
#[test]
fn used_sibling_does_not_veto_the_live_impulse_grant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    let fireblast = exiled_fireblast(&mut scenario, P0);
    let other = scenario.add_spell_to_exile(P0, "Other", true).id();
    let mut runner = scenario.build();
    let mut used = impulse_grant(P0, None);
    if let CastingPermission::PlayFromExile {
        frequency,
        source_id,
        alt_ability_cost,
        ..
    } = &mut used
    {
        *frequency = CastFrequency::OncePerTurn;
        *source_id = Some(fireblast);
        *alt_ability_cost = Some(engine::types::ability::AbilityCost::Mana {
            cost: ManaCost::zero(),
        });
    }
    grant(&mut runner, fireblast, used);
    runner
        .state_mut()
        .exile_play_permissions_used
        .insert(fireblast);
    let mut filtered = impulse_grant(P0, None);
    if let CastingPermission::PlayFromExile { card_filter, .. } = &mut filtered {
        *card_filter = Some(TargetFilter::SpecificObject { id: other });
    }
    grant(&mut runner, fireblast, filtered);
    grant(&mut runner, fireblast, impulse_grant(P0, None));

    assert!(can_cast_object_now(runner.state(), P0, fireblast));
    cast(&mut runner, fireblast, Some(true));
    assert_eq!(mountains_on_battlefield(&runner, P0), 0);
}

fn face_down_fireblast_board(
    mana: usize,
    mountains: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mountains: Vec<ObjectId> = (0..mountains)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Red))
        .collect();
    scenario.with_mana_pool(P0, red_mana(mana));
    let fireblast = exiled_fireblast(&mut scenario, P0);
    scenario
        .add_creature(P0, "Morph Grantor", 1, 1)
        .with_static_definition(
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: fireblast })
                .modifications(vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Morph(ManaCost::generic(4).into()),
                }]),
        );
    let mut runner = scenario.build();
    for &mountain in &mountains {
        runner
            .state_mut()
            .objects
            .get_mut(&mountain)
            .unwrap()
            .tapped = true;
    }
    grant(&mut runner, fireblast, impulse_grant(P0, None));
    assert!(engine::game::keywords::object_has_effective_keyword_kind(
        runner.state(),
        fireblast,
        KeywordKind::Morph,
    ));
    (runner, fireblast, mountains)
}

/// CR 702.37c + CR 118.9a: the two casting methods have independent costs.
/// Printed sacrifice keeps Normal available, but cannot pay the face-down {3}.
#[test]
fn late_preview_only_uses_the_printed_option_for_normal_casts() {
    let offers_face_down = |runner: &GameRunner, fireblast: ObjectId| {
        current_casting_variant_choice_options(runner.state(), P0, fireblast)
            .iter()
            .any(|option| option.variant == CastingVariant::FaceDown)
    };

    // Reach guard: with three mana and no Mountains the face-down {3} is the
    // only payable method, so this impulse grant does reach the face-down
    // cast from exile.
    let (mut runner, fireblast, _) = face_down_fireblast_board(3, 0);
    assert!(can_cast_object_now(runner.state(), P0, fireblast));
    cast(&mut runner, fireblast, None);
    assert!(runner.state().objects[&fireblast].face_down);
    assert_eq!(runner.state().objects[&fireblast].zone, Zone::Stack);
    assert!(runner.state().players[0].mana_pool.is_empty());

    // Same two Mountains, only the mana differs: the face-down option is
    // offered exactly when its own {3} is payable. With two mana the printed
    // sacrifice must not stand in for the face-down cost (the Normal-only
    // guard on the printed alternative).
    let (runner, fireblast, _) = face_down_fireblast_board(3, 2);
    assert!(offers_face_down(&runner, fireblast));
    let (mut runner, fireblast, mountains) = face_down_fireblast_board(2, 2);
    assert!(!offers_face_down(&runner, fireblast));
    assert!(can_cast_object_now(runner.state(), P0, fireblast));
    let card_id = runner.state().objects[&fireblast].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: fireblast,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("normal sacrifice cast is still legal");
    match &runner.state().waiting_for {
        WaitingFor::CastingVariantChoice { options, .. } => {
            assert!(options
                .iter()
                .all(|option| option.variant != CastingVariant::FaceDown));
            let invalid = options.len();
            let normal = options
                .iter()
                .position(|option| option.variant == CastingVariant::Normal)
                .expect("normal method survives");
            assert!(runner
                .act(GameAction::ChooseCastingVariant { index: invalid })
                .is_err());
            runner
                .act(GameAction::ChooseCastingVariant { index: normal })
                .expect("choose normal cast");
        }
        _ => {
            assert!(runner
                .act(GameAction::ChooseCastingVariant { index: 0 })
                .is_err());
        }
    }
    assert_eq!(runner.state().objects[&fireblast].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&fireblast].casting_permissions.len(),
        1
    );
    finish_cast(&mut runner, Some(true));
    assert_eq!(runner.state().objects[&fireblast].zone, Zone::Stack);
    for mountain in mountains {
        assert_eq!(runner.state().objects[&mountain].zone, Zone::Graveyard);
    }
}

/// Boundary pin: a grant carrying a CR 601.2f cost rider ("each spell cast this
/// way costs {1} more", Lightstall Inquisitor) is outside this change — the
/// rider's composition with a non-mana alternative cost is not modelled, so
/// the printed option stays unoffered rather than dropping the {1}.
#[test]
fn a_cost_rider_grant_keeps_the_printed_alternative_cost_unoffered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    let fireblast = exiled_fireblast(&mut scenario, P0);
    let mut runner = scenario.build();
    let raise = CastCostModifier::new(CostModifyMode::Raise, ManaCost::generic(1)).unwrap();
    grant(&mut runner, fireblast, impulse_grant(P0, Some(raise)));

    assert!(!can_cast_object_now(runner.state(), P0, fireblast));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.with_mana_pool(P0, red_mana(7));
    let fireblast = exiled_fireblast(&mut scenario, P0);
    let mut runner = scenario.build();
    let raise = CastCostModifier::new(CostModifyMode::Raise, ManaCost::generic(1)).unwrap();
    grant(&mut runner, fireblast, impulse_grant(P0, Some(raise)));
    assert!(can_cast_object_now(runner.state(), P0, fireblast));
    cast(&mut runner, fireblast, None);
    assert_eq!(
        runner.state().stack.back().map(|entry| entry.id),
        Some(fireblast)
    );
    assert_eq!(mountains_on_battlefield(&runner, P0), 2);
    assert!(runner.state().players[0].mana_pool.is_empty());
}
