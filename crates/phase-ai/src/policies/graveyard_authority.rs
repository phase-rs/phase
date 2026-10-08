//! Graveyard-permission announcement policy.
//!
//! CR 601.2a + CR 601.2b: when several graveyard permissions could authorize a
//! cast, the caster announces which one they use (Muldrotha, the Gravetide,
//! 2020-11-10 ruling), and the engine offers one casting option per
//! permission. The permissions differ in what the cast commits to: a per-turn
//! slot (Muldrotha's per type, Lurrus's and Exploration Broodship's once), an
//! extra cost (Broodship's land), a counter the permanent enters with
//! (Leonardo's finality counter), or where the card goes afterwards. This
//! policy compares the options of one casting method (the printed cost, Blitz
//! or Bestow, on one face) by those commitments only; which METHOD to use is
//! left to the rest of the scoring.
//!
//! An option that another option of its method matches or beats on every
//! commitment, and beats on one, is rejected. The rest are scored by what
//! they give up, in card-equivalents (the `graveyard_authority_*` weights in
//! `PolicyPenalties`, and `self_cost::real_self_cost` for an extra cost): a
//! finality counter, an extra cost, a spent slot per other
//! graveyard card it could still admit this turn (capped; a small idle cost when
//! it would admit none), a destination rider.

use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastingVariant, CastingVariantChoiceOption, CastingVariantFace, GameState, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::statics::{CastCostMode, CastFrequency};

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::config::PolicyPenalties;
use crate::features::DeckFeatures;

/// The casting method an option announces a permission for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Method {
    Printed,
    Blitz,
    Bestow,
}

/// What casting under one announced permission commits the cast to.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Commitment {
    method: Method,
    face: CastingVariantFace,
    mana: u32,
    extra: f64,
    counter: f64,
    slot: f64,
    destination: f64,
}

impl Commitment {
    fn axes(&self) -> [f64; 5] {
        [
            f64::from(self.mana),
            self.extra,
            self.counter,
            self.slot,
            self.destination,
        ]
    }

    fn same_group(&self, other: &Commitment) -> bool {
        self.method == other.method && self.face == other.face
    }

    /// No worse on every axis and strictly better on one.
    fn dominates(&self, other: &Commitment) -> bool {
        let (mine, theirs) = (self.axes(), other.axes());
        mine.iter().zip(theirs.iter()).all(|(a, b)| a <= b)
            && mine.iter().zip(theirs.iter()).any(|(a, b)| a < b)
    }

    /// What the option gives up, in card-equivalents. Mana is not priced here:
    /// phase-ai has no mana-to-card valuation. The options of one method and
    /// face share the spell's mana cost, but a permission can add mana of its
    /// own (`Additional(Mana)`), so `mana` carries both and dominance compares
    /// it: an option costing more mana and nothing less is rejected.
    fn given_up(&self) -> f64 {
        self.extra + self.counter + self.slot + self.destination
    }
}

/// The mana value of the mana legs of a permission's extra cost: a fixed mana
/// cost, a dynamic generic amount, every leg of a composite, the cheapest
/// alternative of a choice. Non-mana legs count 0 here.
fn permission_mana(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    cost: &engine::types::ability::AbilityCost,
) -> u32 {
    use engine::types::ability::AbilityCost;
    match cost {
        AbilityCost::Mana { cost } => cost.mana_value(),
        AbilityCost::ManaDynamic { quantity } => {
            engine::game::quantity::resolve_quantity(state, quantity, player, object_id).max(0)
                as u32
        }
        AbilityCost::Composite { costs } => costs
            .iter()
            .map(|leg| permission_mana(state, player, object_id, leg))
            .sum(),
        // Dormant: the engine never offers a graveyard permission whose extra
        // cost contains a choice (it can't pay one), so no option reaches this
        // arm. Kept as a conservative estimate should that change.
        AbilityCost::OneOf { costs } => costs
            .iter()
            .map(|leg| permission_mana(state, player, object_id, leg))
            .min()
            .unwrap_or(0),
        _ => 0,
    }
}

fn commitment(
    state: &GameState,
    penalties: &PolicyPenalties,
    player: PlayerId,
    object_id: ObjectId,
    option: &CastingVariantChoiceOption,
) -> Option<Commitment> {
    let authority = option.authority.as_ref()?;
    let method = match option.variant {
        CastingVariant::GraveyardPermission { .. } => Method::Printed,
        CastingVariant::Blitz => Method::Blitz,
        CastingVariant::Bestow => Method::Bestow,
        _ => return None,
    };
    // The permission's additional cost, priced by the shared self-cost
    // estimator (life per point, cards per discard, the permanent a sacrifice
    // consumes), so a cheaper rider is preferred over a dearer one.
    // A permission's additional MANA is paid on top of the option's mana cost;
    // phase-ai has no mana-to-card valuation to price it with, so it joins the
    // mana axis dominance compares (and the self-cost estimator prices it 0).
    let extra_mana = match &authority.extra_cost {
        Some(extra) if extra.mode == CastCostMode::Additional => {
            permission_mana(state, player, object_id, &extra.cost)
        }
        _ => 0,
    };
    let extra = match &authority.extra_cost {
        Some(extra) if extra.mode == CastCostMode::Additional => {
            super::self_cost::real_self_cost(state, player, object_id, &extra.cost, penalties)
        }
        _ => 0.0,
    };
    let counter = match authority.enters_with_counter {
        Some(CounterType::Finality) => penalties.graveyard_authority_finality_cost,
        _ => 0.0,
    };
    let slot = if authority.frequency == CastFrequency::Unlimited {
        0.0
    } else {
        let demand = engine::game::casting::graveyard_slot_demand(
            state,
            player,
            object_id,
            &authority.announcement,
        );
        if demand == 0 {
            penalties.graveyard_authority_idle_slot
        } else {
            (penalties.graveyard_authority_slot_per_demand * f64::from(demand))
                .min(penalties.graveyard_authority_slot_cap)
        }
    };
    let destination = if authority.graveyard_destination_replacement.is_some() {
        penalties.graveyard_authority_destination
    } else {
        0.0
    };
    Some(Commitment {
        method,
        face: option.face,
        mana: option.mana_cost.mana_value() + extra_mana,
        extra,
        counter,
        slot,
        destination,
    })
}

/// The commitments of a casting menu's options, `None` for an option that
/// announces no graveyard permission.
fn commitments(
    state: &GameState,
    penalties: &PolicyPenalties,
    player: PlayerId,
    object_id: ObjectId,
    options: &[CastingVariantChoiceOption],
) -> Vec<Option<Commitment>> {
    options
        .iter()
        .map(|option| commitment(state, penalties, player, object_id, option))
        .collect()
}

fn dominated(all: &[Option<Commitment>], index: usize) -> bool {
    let Some(me) = &all[index] else {
        return false;
    };
    all.iter().enumerate().any(|(other, them)| {
        other != index
            && them
                .as_ref()
                .is_some_and(|them| them.same_group(me) && them.dominates(me))
    })
}

/// The option to announce among those sharing `options[anchor]`'s method and
/// face: the undominated one that gives up least (ties keep menu order).
fn best_in_group(all: &[Option<Commitment>], anchor: usize) -> Option<usize> {
    let group = all.get(anchor)?.as_ref()?;
    all.iter()
        .enumerate()
        .filter_map(|(index, c)| {
            c.as_ref()
                .filter(|c| c.same_group(group) && !dominated(all, index))
                .map(|c| (index, c.given_up()))
        })
        .min_by(|(ia, a), (ib, b)| a.total_cmp(b).then(ia.cmp(ib)))
        .map(|(index, _)| index)
}

/// CR 601.2a + CR 601.2b: when every option of the AI's casting menu
/// announces a permission for the same method, only the announcement is left
/// to choose, and it is decided here without search.
pub(crate) fn same_method_announcement(
    state: &GameState,
    penalties: &PolicyPenalties,
    ai_player: PlayerId,
) -> Option<GameAction> {
    let WaitingFor::CastingVariantChoice {
        player,
        object_id,
        options,
        ..
    } = &state.waiting_for
    else {
        return None;
    };
    if *player != ai_player || options.len() < 2 {
        return None;
    }
    let all = commitments(state, penalties, *player, *object_id, options);
    let first = all.first()?.as_ref()?;
    if !all
        .iter()
        .all(|c| c.as_ref().is_some_and(|c| c.same_group(first)))
    {
        return None;
    }
    best_in_group(&all, 0).map(|index| GameAction::ChooseCastingVariant { index })
}

/// The fallback answer to a casting menu: the first option's method, under
/// the permission best to announce for it.
pub(crate) fn fallback_announcement(
    state: &GameState,
    penalties: &PolicyPenalties,
    options: &[CastingVariantChoiceOption],
) -> Option<GameAction> {
    let WaitingFor::CastingVariantChoice {
        player, object_id, ..
    } = &state.waiting_for
    else {
        return None;
    };
    let all = commitments(state, penalties, *player, *object_id, options);
    best_in_group(&all, 0).map(|index| GameAction::ChooseCastingVariant { index })
}

pub struct GraveyardAuthorityPolicy;

impl TacticalPolicy for GraveyardAuthorityPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::GraveyardAuthority
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        // `decision_kind::classify` routes every `CastingVariantChoice` prompt
        // into the `ActivateAbility` bucket.
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        match &state.waiting_for {
            WaitingFor::CastingVariantChoice { options, .. }
                if options.iter().any(|option| option.authority.is_some()) =>
            {
                // activation-constant: announcement prompt gate, no deck scaling.
                Some(1.0)
            }
            _ => None,
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("graveyard_authority_na"));
        let WaitingFor::CastingVariantChoice {
            player,
            object_id,
            options,
            ..
        } = &ctx.decision.waiting_for
        else {
            return na();
        };
        if *player != ctx.ai_player {
            return na();
        }
        let GameAction::ChooseCastingVariant { index } = ctx.candidate.action else {
            return na();
        };
        let all = commitments(
            ctx.state,
            &ctx.config.policy_penalties,
            *player,
            *object_id,
            options,
        );
        let Some(Some(me)) = all.get(index) else {
            return na();
        };
        if dominated(&all, index) {
            return PolicyVerdict::reject(PolicyReason::new("graveyard_authority_dominated"));
        }
        let given_up = me.given_up();
        let reason = PolicyReason::new("graveyard_authority_given_up")
            .with_fact("given_up_milli", (given_up * 1000.0) as i64);
        let delta = -given_up;
        PolicyVerdict::score(delta, reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weights() -> PolicyPenalties {
        PolicyPenalties::default()
    }

    fn c(method: Method, extra: f64, counter: f64, slot: f64) -> Option<Commitment> {
        Some(Commitment {
            method,
            face: CastingVariantFace::Current,
            mana: 4,
            extra,
            counter,
            slot,
            destination: 0.0,
        })
    }

    /// Sabin's own permission (nothing given up) beside Muldrotha (a slot):
    /// Muldrotha's option is dominated and the own permission is announced.
    #[test]
    fn a_free_permission_dominates_a_slot() {
        let all = [
            c(Method::Blitz, 0.0, 0.0, 0.0),
            c(Method::Blitz, 0.0, 0.0, 0.05),
        ];
        assert!(dominated(&all, 1));
        assert!(!dominated(&all, 0));
        assert_eq!(best_in_group(&all, 1), Some(0));
    }

    /// Leonardo (finality counter) against Muldrotha: with no other graveyard
    /// card for Muldrotha's slot the slot is cheaper; with two, Leonardo is.
    #[test]
    fn leonardo_against_muldrotha_follows_slot_demand() {
        let idle = [
            c(
                Method::Blitz,
                0.0,
                weights().graveyard_authority_finality_cost,
                0.0,
            ),
            c(
                Method::Blitz,
                0.0,
                0.0,
                weights().graveyard_authority_idle_slot,
            ),
        ];
        assert_eq!(best_in_group(&idle, 0), Some(1));
        let wanted = [
            c(
                Method::Blitz,
                0.0,
                weights().graveyard_authority_finality_cost,
                0.0,
            ),
            c(
                Method::Blitz,
                0.0,
                0.0,
                2.0 * weights().graveyard_authority_slot_per_demand,
            ),
        ];
        assert_eq!(best_in_group(&wanted, 0), Some(0));
    }

    /// Broodship (land + slot) against Muldrotha (slot): Muldrotha unless its
    /// slot is worth more than a land.
    #[test]
    fn broodship_against_muldrotha_prefers_keeping_the_land() {
        let all = [
            c(
                Method::Blitz,
                1.0,
                0.0,
                weights().graveyard_authority_idle_slot,
            ),
            c(
                Method::Blitz,
                0.0,
                0.0,
                2.0 * weights().graveyard_authority_slot_per_demand,
            ),
        ];
        assert_eq!(best_in_group(&all, 0), Some(1));
        let all = [
            c(
                Method::Blitz,
                1.0,
                0.0,
                weights().graveyard_authority_idle_slot,
            ),
            c(
                Method::Blitz,
                0.0,
                0.0,
                weights().graveyard_authority_slot_cap,
            ),
        ];
        assert_eq!(best_in_group(&all, 0), Some(0));
    }

    /// On a real menu (a once-per-turn permission beside an unlimited one,
    /// both open to Grizzly Bears's printed cost), the AI announces the
    /// unlimited permission without search, and falls back to it too.
    #[test]
    fn a_real_menu_announces_the_permission_giving_up_least() {
        use engine::game::scenario::{GameScenario, P0};
        use engine::types::ability::{CardPlayMode, StaticDefinition, TargetFilter, TypedFilter};
        use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use engine::types::phase::Phase;
        use engine::types::statics::StaticMode;
        let permission = |frequency| {
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
                extra_cost: None,
                enters_with_counter: None,
                required_cast_keyword: None,
                pool: engine::types::statics::GraveyardPermissionPool::OwnGraveyard,
            })
            .affected(TargetFilter::Typed(TypedFilter::creature()))
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Once Permission", 1, 1)
            .with_static_definition(permission(CastFrequency::OncePerTurn));
        let unlimited = scenario
            .add_creature(P0, "Unlimited Permission", 1, 1)
            .with_static_definition(permission(CastFrequency::Unlimited))
            .id();
        let bears = scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let mut runner = scenario.build();
        for _ in 0..2 {
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::Green,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        let card_id = runner.state().objects[&bears].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: bears,
                card_id,
                targets: vec![],
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .expect("the cast starts");
        let WaitingFor::CastingVariantChoice { options, .. } = runner.state().waiting_for.clone()
        else {
            panic!("two permissions ask for the announcement");
        };
        let expected = options
            .iter()
            .position(|option| {
                option
                    .authority
                    .as_ref()
                    .is_some_and(|a| a.announcement.permission.source == unlimited)
            })
            .expect("the unlimited permission is offered");
        let action = GameAction::ChooseCastingVariant { index: expected };
        assert_eq!(
            same_method_announcement(runner.state(), &weights(), P0),
            Some(action.clone())
        );
        assert_eq!(
            fallback_announcement(runner.state(), &weights(), &options),
            Some(action)
        );
    }

    /// Two unlimited creature permissions on separate hosts whose extra costs
    /// are `dear` (scanned first) and `cheap` (second), with Grizzly Bears in
    /// the graveyard and plenty of green mana: the AI's announcement, and the
    /// menu index of the cheap permission.
    fn announce_between(
        dear: engine::types::ability::AbilityCost,
        cheap: engine::types::ability::AbilityCost,
    ) -> (Option<GameAction>, usize) {
        use engine::game::scenario::{GameScenario, P0};
        use engine::types::ability::{CardPlayMode, StaticDefinition, TargetFilter, TypedFilter};
        use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use engine::types::phase::Phase;
        use engine::types::statics::{CastExtraCost, StaticMode};
        let permission = |cost| {
            StaticDefinition::new(StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::Unlimited,
                play_mode: CardPlayMode::Cast,
                graveyard_destination_replacement: None,
                extra_cost: Some(CastExtraCost {
                    cost,
                    mode: CastCostMode::Additional,
                }),
                enters_with_counter: None,
                required_cast_keyword: None,
                pool: engine::types::statics::GraveyardPermissionPool::OwnGraveyard,
            })
            .affected(TargetFilter::Typed(TypedFilter::creature()))
        };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Dear Permission", 1, 1)
            .with_static_definition(permission(dear));
        let cheap_host = scenario
            .add_creature(P0, "Cheap Permission", 1, 1)
            .with_static_definition(permission(cheap))
            .id();
        let bears = scenario
            .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
            .with_mana_cost(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
            .id();
        let mut runner = scenario.build();
        for _ in 0..8 {
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::Green,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        let card_id = runner.state().objects[&bears].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: bears,
                card_id,
                targets: vec![],
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            })
            .expect("the cast starts");
        let WaitingFor::CastingVariantChoice { options, .. } = runner.state().waiting_for.clone()
        else {
            panic!("two permissions ask for the announcement");
        };
        let cheap_index = options
            .iter()
            .position(|option| {
                option
                    .authority
                    .as_ref()
                    .is_some_and(|a| a.announcement.permission.source == cheap_host)
            })
            .expect("the cheap permission is offered");
        assert!(cheap_index > 0, "the cheap permission is scanned second");
        (
            same_method_announcement(runner.state(), &weights(), P0),
            cheap_index,
        )
    }

    /// Two permissions whose extra costs differ only in amount (pay 10 life,
    /// scanned first; pay 1 life, second): the AI announces the cheaper one.
    #[test]
    fn a_cheaper_extra_cost_is_announced_even_when_scanned_second() {
        use engine::types::ability::{AbilityCost, QuantityExpr};
        let life = |value| AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value },
        };
        let (chosen, cheap) = announce_between(life(10), life(1));
        assert_eq!(
            chosen,
            Some(GameAction::ChooseCastingVariant { index: cheap })
        );
    }

    /// The same with permission-imposed MANA ({5} scanned first, {1} second):
    /// that mana is paid on top of the spell's cost, so the cheaper one is
    /// announced.
    #[test]
    fn cheaper_permission_mana_is_announced_even_when_scanned_second() {
        use engine::types::ability::AbilityCost;
        use engine::types::mana::ManaCost;
        let mana = |generic| AbilityCost::Mana {
            cost: ManaCost::generic(generic),
        };
        let (chosen, cheap) = announce_between(mana(5), mana(1));
        assert_eq!(
            chosen,
            Some(GameAction::ChooseCastingVariant { index: cheap })
        );
    }

    /// Options of different methods are never compared.
    #[test]
    fn methods_are_not_compared() {
        let all = [
            c(Method::Printed, 0.0, 0.0, 0.05),
            c(Method::Blitz, 0.0, 0.0, 0.0),
        ];
        assert!(!dominated(&all, 0));
        assert_eq!(best_in_group(&all, 0), Some(0));
    }
}
