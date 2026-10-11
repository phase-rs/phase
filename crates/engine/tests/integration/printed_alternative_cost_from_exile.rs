//! Regression: **Fireblast** exiled by an impulse draw (Wrenn's Resolve,
//! Experimental Synthesizer) — "If you control two or more Mountains, you may
//! sacrifice two Mountains rather than pay this spell's mana cost."
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

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    CardPlayMode, CastCostModifier, CastingPermission, Duration, ExileGrantCostProvenance,
    PlayFromExileProvenance,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CastFrequency, CostModifyMode};
use engine::types::zones::{EtbTapState, Zone};
use engine::types::ObjectId;

const FIREBLAST: &str = "If you control two or more Mountains, you may sacrifice two Mountains \
rather than pay this spell's mana cost.\nFireblast deals 4 damage to any target.";
const WRENNS_RESOLVE: &str = "Exile the top two cards of your library. Until the end of your next \
turn, you may play those cards.";
const MASSACRE: &str = "If an opponent controls a Plains and you control a Swamp, you may cast \
this spell without paying its mana cost.\nAll creatures get -2/-2 until end of turn.";

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
        land_enter_tapped: EtbTapState::Unspecified,
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
    let mut offered = false;
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("announce the spell");
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
    assert!(state.players[0].mana_pool.mana.is_empty());

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
        runner.state().players[0].mana_pool.mana.is_empty(),
        "the printed {{4}}{{R}}{{R}} was paid"
    );
}

/// The printed condition still gates the offer: one Mountain is not enough.
#[test]
fn one_mountain_does_not_make_the_exiled_fireblast_castable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_basic_land(P0, ManaColor::Red);
    let fireblast = exiled_fireblast(&mut scenario, P0);
    let mut runner = scenario.build();
    grant(&mut runner, fireblast, impulse_grant(P0, None));

    assert!(!can_cast_object_now(runner.state(), P0, fireblast));
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
    grant(
        &mut runner,
        fireblast,
        CastingPermission::ExileWithAltCost {
            source_id: None,
            cost_provenance: ExileGrantCostProvenance::Alternative,
            cost: ManaCost::zero(),
            cast_transformed: false,
            constraint: None,
            granted_to: Some(P0),
            resolution_cleanup: None,
            duration: None,
            graveyard_replacement: None,
            mana_spend_permission: None,
            enters_with_counter: None,
            enters_with_modifications: Vec::new(),
            cast_cost_modifier: None,
        },
    );

    cast(&mut runner, fireblast, None);

    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(mountains_on_battlefield(&runner, P0), 2);
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
}
