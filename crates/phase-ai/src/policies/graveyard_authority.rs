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
//! they give up, in card-equivalents: a finality counter 0.6, an extra cost
//! 1.0, a spent slot 0.4 per other graveyard card it could still admit this
//! turn (at most 1.5; 0.05 when it would admit none), a destination rider 0.3.

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
use crate::features::DeckFeatures;

const FINALITY_COST: f64 = 0.6;
const EXTRA_COST: f64 = 1.0;
const SLOT_COST_PER_DEMAND: f64 = 0.4;
const SLOT_COST_CAP: f64 = 1.5;
const IDLE_SLOT_COST: f64 = 0.05;
const DESTINATION_COST: f64 = 0.3;

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

    /// What the option gives up, in card-equivalents (mana is the method's
    /// shared cost and is compared only by dominance).
    fn given_up(&self) -> f64 {
        self.extra + self.counter + self.slot + self.destination
    }
}

fn commitment(
    state: &GameState,
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
    let extra = match &authority.extra_cost {
        Some(extra) if extra.mode == CastCostMode::Additional => EXTRA_COST,
        _ => 0.0,
    };
    let counter = match authority.enters_with_counter {
        Some(CounterType::Finality) => FINALITY_COST,
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
            IDLE_SLOT_COST
        } else {
            (SLOT_COST_PER_DEMAND * f64::from(demand)).min(SLOT_COST_CAP)
        }
    };
    let destination = if authority.graveyard_destination_replacement.is_some() {
        DESTINATION_COST
    } else {
        0.0
    };
    Some(Commitment {
        method,
        face: option.face,
        mana: option.mana_cost.mana_value(),
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
    player: PlayerId,
    object_id: ObjectId,
    options: &[CastingVariantChoiceOption],
) -> Vec<Option<Commitment>> {
    options
        .iter()
        .map(|option| commitment(state, player, object_id, option))
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
    let all = commitments(state, *player, *object_id, options);
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
    options: &[CastingVariantChoiceOption],
) -> Option<GameAction> {
    let WaitingFor::CastingVariantChoice {
        player, object_id, ..
    } = &state.waiting_for
    else {
        return None;
    };
    let all = commitments(state, *player, *object_id, options);
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
        let all = commitments(ctx.state, *player, *object_id, options);
        let Some(Some(me)) = all.get(index) else {
            return na();
        };
        if dominated(&all, index) {
            return PolicyVerdict::reject(PolicyReason::new("graveyard_authority_dominated"));
        }
        let given_up = me.given_up();
        let reason = PolicyReason::new("graveyard_authority_given_up")
            .with_fact("given_up_milli", (given_up * 1000.0) as i64);
        PolicyVerdict::score(-given_up, reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            c(Method::Blitz, 0.0, FINALITY_COST, 0.0),
            c(Method::Blitz, 0.0, 0.0, IDLE_SLOT_COST),
        ];
        assert_eq!(best_in_group(&idle, 0), Some(1));
        let wanted = [
            c(Method::Blitz, 0.0, FINALITY_COST, 0.0),
            c(Method::Blitz, 0.0, 0.0, 2.0 * SLOT_COST_PER_DEMAND),
        ];
        assert_eq!(best_in_group(&wanted, 0), Some(0));
    }

    /// Broodship (land + slot) against Muldrotha (slot): Muldrotha unless its
    /// slot is worth more than a land.
    #[test]
    fn broodship_against_muldrotha_prefers_keeping_the_land() {
        let all = [
            c(Method::Blitz, EXTRA_COST, 0.0, IDLE_SLOT_COST),
            c(Method::Blitz, 0.0, 0.0, 2.0 * SLOT_COST_PER_DEMAND),
        ];
        assert_eq!(best_in_group(&all, 0), Some(1));
        let all = [
            c(Method::Blitz, EXTRA_COST, 0.0, IDLE_SLOT_COST),
            c(Method::Blitz, 0.0, 0.0, SLOT_COST_CAP),
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
            same_method_announcement(runner.state(), P0),
            Some(action.clone())
        );
        assert_eq!(
            fallback_announcement(runner.state(), &options),
            Some(action)
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
