//! Reach-source pricing: what one engine-issued cast or activation does to one
//! opponent's life total, what it does to the AI's, and what mana it commits.
//!
//! Every value here is an ESTIMATE used only to propose lines; the reducer
//! certifies a line before it is played (see the module docs in `reach`). The
//! estimate is still built from the engine's own authorities — target legality,
//! player protection, effect-recipient scopes, quantity resolution, effective
//! spell cost — so the proposals it makes are the ones the reducer will accept.

use std::collections::HashSet;
use std::iter::successors;

use engine::game::ability_utils::modal_spell_mode_ability_refs;
use engine::game::casting::{effective_spell_cost, spell_objects_available_to_cast};
use engine::game::effects::matches_player_scope;
use engine::game::filter::player_matches_target_filter_in_state;
use engine::game::game_object::GameObject;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::mana_abilities::is_mana_ability;
use engine::game::quantity::try_resolve_quantity_in_source_context;
use engine::game::static_abilities::player_protection_from;
use engine::game::targeting::player_is_legal_target;
use engine::game::{extract_mana_leg, max_x_value};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CostCategory, Effect, PlayerFilter, QuantityExpr,
    QuantityRef, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::player::PlayerId;

use super::LineStep;
use crate::search::MAX_ACTIVATIONS_PER_SOURCE_PER_TURN;

/// A life change of `fixed + per_x * X`, where X is the value announced for the
/// source's own `{X}` (CR 107.3a). Every other quantity is resolved against the
/// live state before the line search runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Linear {
    pub(super) fixed: u32,
    pub(super) per_x: u32,
}

impl Linear {
    pub(super) fn at(self, x: u32) -> u32 {
        self.fixed + self.per_x * x
    }

    pub(super) fn plus(self, other: Linear) -> Linear {
        Linear {
            fixed: self.fixed + other.fixed,
            per_x: self.per_x + other.per_x,
        }
    }

    fn is_zero(self) -> bool {
        self.fixed == 0 && self.per_x == 0
    }

    /// Larger of two single-recipient amounts. A per-X amount outranks any
    /// fixed one because the line search can always size X upward.
    fn max(self, other: Linear) -> Linear {
        if (other.per_x, other.fixed) > (self.per_x, self.fixed) {
            other
        } else {
            self
        }
    }
}

/// Life the AI gains when one source resolves (CR 119.3). `Unread` when the
/// source can give the AI life by a route this pricing does not read — an
/// unreadable amount, a recipient it does not resolve, lifelink damage
/// (CR 702.15b) — so the line search must not reject a line on the AI's life
/// total alone; the reducer decides the order things resolve in and whether
/// the AI survives each step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Gain {
    Read(Linear),
    Unread,
}

impl Default for Gain {
    fn default() -> Self {
        Gain::Read(Linear::default())
    }
}

impl Gain {
    pub(super) fn plus(self, other: Gain) -> Gain {
        match (self, other) {
            (Gain::Read(left), Gain::Read(right)) => Gain::Read(left.plus(right)),
            _ => Gain::Unread,
        }
    }
}

/// Life each side loses — and the life the AI gains — when one source
/// resolves.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LifeLoss {
    pub(super) opponent: Linear,
    pub(super) controller: Linear,
    pub(super) controller_gain: Gain,
}

/// When a source's first step can be started, which orders a line (CR 117.1a):
/// a noninstant spell needs an empty stack, so those go first; instant-speed
/// spells and activations can follow while earlier steps wait on the stack;
/// anything whose damage scales with X goes last so it absorbs the mana the
/// rest of the line leaves over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum StepOrder {
    SorcerySpeed,
    InstantSpeed,
    Activation,
    ScalesWithX,
}

/// The mana a source commits, read from the engine only once the damage could
/// matter: a cast's total cost goes through the engine's full cast
/// preparation, which is too costly to run for every burn spell at every
/// priority decision.
#[derive(Debug, Clone)]
pub(super) enum ManaCommitment {
    /// CR 601.2f: the engine's total cost to cast `object_id`, plus `extra` —
    /// a Spree mode's additional cost (CR 702.172a) or the mana leg of the
    /// follow-up activation of the permanent being cast (CR 602.2b).
    Cast {
        object_id: ObjectId,
        extra: ManaCost,
    },
    /// CR 602.2b: the mana leg of an activation cost.
    Activation(ManaCost),
}

/// One way to take life from the opponent, before its mana is priced.
#[derive(Debug, Clone)]
pub(super) struct ReachSource {
    /// Engine actions that realise the damage. The first is engine-issued in
    /// the state the source was read from; a second, when present, is the
    /// activation of the permanent the first one casts (CR 117.3c + CR 602.2).
    pub(super) steps: Vec<LineStep>,
    pub(super) loss: LifeLoss,
    pub(super) commitment: ManaCommitment,
    pub(super) order: StepOrder,
}

impl ReachSource {
    /// Resolve the mana this source commits, or `None` when the engine says
    /// the spell can't be cast after all.
    pub(super) fn priced(&self, state: &GameState, ai_player: PlayerId) -> Option<PricedSource> {
        let mana = match &self.commitment {
            ManaCommitment::Cast { object_id, extra } => {
                effective_spell_cost(state, ai_player, *object_id)?.plus(extra)
            }
            ManaCommitment::Activation(mana) => mana.clone(),
        };
        Some(PricedSource {
            steps: self.steps.clone(),
            loss: self.loss,
            mana,
            order: self.order,
        })
    }
}

/// A reach source with the mana it commits. `{X}` shards stay symbolic; the
/// solver sizes X (CR 107.3a).
#[derive(Debug, Clone)]
pub(super) struct PricedSource {
    pub(super) steps: Vec<LineStep>,
    pub(super) loss: LifeLoss,
    pub(super) mana: ManaCost,
    pub(super) order: StepOrder,
}

impl PricedSource {
    pub(super) fn x_shards(&self) -> u32 {
        x_shard_count(&self.mana)
    }
}

/// Price every engine-issued cast and activation in `issued` that can take
/// life from `opponent`.
pub(super) fn reach_sources(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    issued: &[GameAction],
) -> Vec<ReachSource> {
    let mut sources = Vec::new();
    let mut casts_seen = HashSet::new();
    for action in issued {
        match action {
            GameAction::CastSpell { object_id, .. } if casts_seen.insert(*object_id) => {
                sources.extend(cast_source(state, ai_player, opponent, action, *object_id));
            }
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => push_activation_sources(
                state,
                ai_player,
                opponent,
                action,
                *source_id,
                *ability_index,
                &mut sources,
            ),
            _ => {}
        }
    }
    sources
}

/// The most mana the AI could spend on a line: the engine's X-affordability
/// authority priced for a bare `{X}` (CR 107.3a), which counts every untapped
/// source's real yield and the floating pool — never less than the plain
/// count of untapped mana sources. It is a board-wide sweep, so callers reach
/// it only once the damage alone could already be lethal.
pub(super) fn mana_capacity(state: &GameState, ai_player: PlayerId) -> u32 {
    let bare_x = ManaCost::Cost {
        shards: vec![ManaCostShard::X],
        generic: 0,
    };
    crate::zone_eval::available_mana(state, ai_player)
        .max(max_x_value(state, ai_player, &bare_x, None))
}

/// Whether the AI holds anything that could take life from `opponent` once
/// the stack settles: a priced source, a spell or ability it already has on
/// the stack (CR 405.1), every spell it has a route to cast from any zone
/// (hand, command zone, and graveyard or exile permissions such as flashback,
/// CR 702.34a), and the activated abilities of its permanents.
///
/// This is deliberately STRUCTURAL — it asks whether an instruction can reach
/// the opponent at all, never how much it would take. Before the stack
/// resolves no amount bounds the settled position: CR 608.2h reads a
/// quantity only when its effect is applied, so earlier stack work can change
/// it (a Krenko activation adding the Goblins a pending Goblin War Strike
/// counts); a resolving spell can reach a zone it is cast from again
/// (CR 608.2n + CR 702.34a); repetition and alternative branches decide how
/// many times an instruction runs; and the opponent's own stack work can cost
/// them life. Only the reducer's settlement reads those, so this gate decides
/// only whether that settlement is worth simulating.
pub(super) fn holds_reach(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    priced: &[ReachSource],
) -> bool {
    if !priced.is_empty() {
        return true;
    }
    let ai_stack = || {
        state
            .stack
            .iter()
            .filter(move |entry| entry.controller == ai_player)
    };
    if ai_stack().any(|entry| {
        entry
            .ability()
            .is_some_and(|ability| resolved_reaches(state, ai_player, opponent, ability))
    }) {
        return true;
    }
    let castable = spell_objects_available_to_cast(state, ai_player).into_iter();
    let battlefield = state.battlefield.iter().copied().filter(|object_id| {
        state
            .objects
            .get(object_id)
            .is_some_and(|object| object.controller == ai_player)
    });
    let on_stack = ai_stack().map(|entry| entry.source_id);
    let mut seen: HashSet<ObjectId> = HashSet::new();
    castable
        .chain(battlefield)
        .chain(on_stack)
        .filter(|object_id| seen.insert(*object_id))
        .filter_map(|object_id| state.objects.get(&object_id))
        .any(|object| object_reaches(state, ai_player, opponent, object))
}

/// Whether anything `object` can do — its spell, any mode of it, or one of its
/// activated abilities — can take life from `opponent`, whatever the amount.
pub(super) fn object_reaches(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    object: &GameObject,
) -> bool {
    let reaches = |ability: &AbilityDefinition| {
        let mut nodes = Vec::new();
        definition_tree(ability, &mut nodes);
        chain_loss(state, ai_player, opponent, object.id, nodes.into_iter()).reaches
    };
    modal_spell_mode_ability_refs(object).any(reaches)
        || object.abilities.iter().any(|ability| {
            ability.kind == AbilityKind::Activated && !is_mana_ability(ability) && reaches(ability)
        })
}

/// CR 602.2b: an ability whose only cost is mana, with no activation limit,
/// can be activated again for as long as the mana lasts.
fn is_repeatable(ability: &AbilityDefinition) -> bool {
    ability.cost_categories() == [CostCategory::ManaOnly]
        && activation_mana(ability).mana_value() > 0
        && ability.activation_restrictions.is_empty()
}

/// Price an issued cast: a burn spell directly, or a permanent whose own
/// activated ability deals the damage once it resolves.
fn cast_source(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    action: &GameAction,
    object_id: ObjectId,
) -> Option<ReachSource> {
    let object = state.objects.get(&object_id)?;
    if let Some(spell) = spell_reach(state, ai_player, opponent, object) {
        let order = if spell.loss.opponent.per_x > 0 {
            StepOrder::ScalesWithX
        } else if is_instant_speed(state, object) {
            StepOrder::InstantSpeed
        } else {
            StepOrder::SorcerySpeed
        };
        return Some(ReachSource {
            steps: vec![LineStep {
                action: action.clone(),
                mode: spell.mode,
                x: None,
            }],
            loss: spell.loss,
            commitment: ManaCommitment::Cast {
                object_id,
                extra: spell.mode_cost,
            },
            order,
        });
    }

    let follow_up = permanent_follow_up(state, ai_player, opponent, object)?;
    let order = if is_instant_speed(state, object) {
        StepOrder::InstantSpeed
    } else {
        StepOrder::SorcerySpeed
    };
    Some(ReachSource {
        steps: vec![
            LineStep {
                action: action.clone(),
                mode: None,
                x: None,
            },
            LineStep {
                action: GameAction::ActivateAbility {
                    source_id: object_id,
                    ability_index: follow_up.ability_index,
                },
                mode: None,
                x: None,
            },
        ],
        loss: follow_up.loss,
        commitment: ManaCommitment::Cast {
            object_id,
            extra: follow_up.mana,
        },
        order,
    })
}

/// Price an issued activation. A mana-only cost with no activation limit can
/// be paid again as long as mana lasts, so it is offered once per remaining
/// activation the search's loop guard would still admit this turn.
fn push_activation_sources(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    action: &GameAction,
    source_id: ObjectId,
    ability_index: usize,
    sources: &mut Vec<ReachSource>,
) {
    let Some(object) = state.objects.get(&source_id) else {
        return;
    };
    let Some(ability) = object.abilities.get(ability_index) else {
        return;
    };
    if is_mana_ability(ability) {
        return;
    }
    let loss = ability_loss(state, ai_player, opponent, object, ability);
    if loss.opponent.is_zero() {
        return;
    }
    let mana = activation_mana(ability);
    let order = if loss.opponent.per_x > 0 {
        StepOrder::ScalesWithX
    } else {
        StepOrder::Activation
    };
    let copies = if is_repeatable(ability) && order != StepOrder::ScalesWithX {
        let used = state
            .activated_abilities_this_turn
            .get(&(source_id, ability_index))
            .copied()
            .unwrap_or(0);
        MAX_ACTIVATIONS_PER_SOURCE_PER_TURN
            .saturating_sub(used)
            .max(1)
    } else {
        1
    };
    for _ in 0..copies {
        sources.push(ReachSource {
            steps: vec![LineStep {
                action: action.clone(),
                mode: None,
                x: None,
            }],
            loss,
            commitment: ManaCommitment::Activation(mana.clone()),
            order,
        });
    }
}

/// A spell's resolution reach and, for a modal spell, the mode that gets it.
struct SpellReach {
    loss: LifeLoss,
    mode: Option<usize>,
    /// CR 702.172a: a Spree mode's additional cost.
    mode_cost: ManaCost,
}

/// What resolving `object` as a spell takes from `opponent`. A modal spell is
/// priced by its single most damaging mode (CR 700.2a); one that must choose
/// more than one mode is left to ordinary scoring.
fn spell_reach(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    object: &GameObject,
) -> Option<SpellReach> {
    let modes: Vec<&AbilityDefinition> = modal_spell_mode_ability_refs(object).collect();
    match &object.modal {
        None => {
            // CR 608.2c: a non-modal spell follows its one spell ability's
            // instructions in order.
            let [ability] = modes[..] else {
                return None;
            };
            let loss = ability_loss(state, ai_player, opponent, object, ability);
            (!loss.opponent.is_zero()).then(|| SpellReach {
                loss,
                mode: None,
                mode_cost: ManaCost::zero(),
            })
        }
        Some(modal) if modal.min_choices <= 1 => modes
            .iter()
            .enumerate()
            .map(|(index, ability)| {
                (
                    index,
                    ability_loss(state, ai_player, opponent, object, ability),
                )
            })
            .filter(|(_, loss)| !loss.opponent.is_zero())
            .max_by(|(left_index, left), (right_index, right)| {
                (left.opponent.per_x, left.opponent.fixed)
                    .cmp(&(right.opponent.per_x, right.opponent.fixed))
                    .then_with(|| right.controller.fixed.cmp(&left.controller.fixed))
                    .then_with(|| right_index.cmp(left_index))
            })
            .map(|(index, loss)| SpellReach {
                loss,
                mode: Some(index),
                mode_cost: modal
                    .mode_costs
                    .get(index)
                    .cloned()
                    .unwrap_or_else(ManaCost::zero),
            }),
        Some(_) => None,
    }
}

/// A permanent's activated damage ability usable as soon as the permanent
/// resolves.
struct FollowUp {
    ability_index: usize,
    loss: LifeLoss,
    mana: ManaCost,
}

/// The best activated ability of a permanent card that its controller can
/// activate right after casting it, with no resource but mana and the
/// permanent itself: mana, tapping it, sacrificing it, or loyalty. A creature
/// that must tap needs haste (CR 302.6).
fn permanent_follow_up(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    object: &GameObject,
) -> Option<FollowUp> {
    // CR 110.4: only a permanent card stays around to activate afterwards.
    let core = &object.card_types.core_types;
    if core
        .iter()
        .any(|kind| matches!(kind, CoreType::Instant | CoreType::Sorcery | CoreType::Land))
    {
        return None;
    }
    let is_creature = core.contains(&CoreType::Creature);
    let hasty = object_has_effective_keyword_kind(state, object.id, KeywordKind::Haste);
    object
        .abilities
        .iter()
        .enumerate()
        .filter(|(_, ability)| ability.kind == AbilityKind::Activated && !is_mana_ability(ability))
        .filter(|(_, ability)| {
            let categories = ability.cost_categories();
            categories.iter().all(|category| match category {
                CostCategory::ManaOnly | CostCategory::PaysLoyalty => true,
                CostCategory::TapsSelf => !is_creature || hasty,
                // CR 701.21a: the permanent itself is the only one sacrificed.
                CostCategory::SacrificesPermanent => ability
                    .cost
                    .as_ref()
                    .is_some_and(AbilityCost::sacrifices_only_source),
                _ => false,
            })
        })
        .map(|(ability_index, ability)| FollowUp {
            ability_index,
            loss: ability_loss(state, ai_player, opponent, object, ability),
            mana: activation_mana(ability),
        })
        .filter(|follow_up| !follow_up.loss.opponent.is_zero())
        .max_by(|left, right| {
            (left.loss.opponent.per_x, left.loss.opponent.fixed)
                .cmp(&(right.loss.opponent.per_x, right.loss.opponent.fixed))
                .then_with(|| right.mana.mana_value().cmp(&left.mana.mana_value()))
        })
}

/// CR 602.2b: the mana an activation commits, from the engine's cost-leg
/// extractor.
fn activation_mana(ability: &AbilityDefinition) -> ManaCost {
    ability
        .cost
        .as_ref()
        .and_then(extract_mana_leg)
        .map(|(mana, _)| mana)
        .unwrap_or_else(ManaCost::zero)
}

/// CR 117.1a: an instant, or a spell with flash (CR 702.8a), can be cast
/// whenever its controller has priority.
fn is_instant_speed(state: &GameState, object: &GameObject) -> bool {
    object.card_types.core_types.contains(&CoreType::Instant)
        || object_has_effective_keyword_kind(state, object.id, KeywordKind::Flash)
}

/// A chain's life loss and AI life gain, and whether any of its instructions
/// can take life from the opponent at all — whatever the amount, and even when
/// the amount could not be read.
struct ChainLoss {
    loss: LifeLoss,
    reaches: bool,
}

/// Where the amounts of one chain instruction are read from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AmountContext {
    /// A printed definition priced before it is cast or activated: a bare X is
    /// worth one point per mana announced for it (CR 107.3a), and anything else
    /// must be readable from the source's present context.
    Printed,
    /// Work already on the stack (CR 405.1). CR 608.2h: its amounts are read
    /// only when its effect is applied, after everything above it resolves,
    /// so none is read here — the walk answers only whether it reaches.
    Pending,
}

/// One instruction of an effect chain, as the pricing walk reads it — shared
/// by a card's printed definition and a stack entry's resolved ability.
struct ChainNode<'a> {
    effect: &'a Effect,
    player_scope: Option<&'a PlayerFilter>,
    context: AmountContext,
    /// CR 608.2c: the instruction runs a number of times decided as it
    /// resolves ("for each red card exiled this way"), so one run's amount
    /// is not what it takes.
    repeats: bool,
}

/// Life each side loses when `ability` (and its unconditional sub-ability
/// chain) resolves with `opponent` chosen for its player target.
fn ability_loss(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    source: &GameObject,
    ability: &AbilityDefinition,
) -> LifeLoss {
    let nodes = successors(Some(ability), |node| node.sub_ability.as_deref()).map(definition_node);
    chain_loss(state, ai_player, opponent, source.id, nodes).loss
}

fn definition_node(node: &AbilityDefinition) -> ChainNode<'_> {
    ChainNode {
        effect: &node.effect,
        player_scope: node.player_scope.as_ref(),
        context: AmountContext::Printed,
        repeats: node.repeat_for.is_some() || node.repeat_until.is_some(),
    }
}

/// Every instruction of a printed definition — its sub-ability chain and every
/// alternative (`else`) branch along it (CR 608.2c), in written order.
fn definition_tree<'a>(node: &'a AbilityDefinition, out: &mut Vec<ChainNode<'a>>) {
    out.push(definition_node(node));
    if let Some(sub) = node.sub_ability.as_deref() {
        definition_tree(sub, out);
    }
    if let Some(other) = node.else_ability.as_deref() {
        definition_tree(other, out);
    }
}

/// Every instruction of a stack entry's resolved ability, including every
/// alternative branch along its chain (CR 608.2c).
fn resolved_tree<'a>(node: &'a ResolvedAbility, out: &mut Vec<ChainNode<'a>>) {
    out.push(ChainNode {
        effect: &node.effect,
        player_scope: node.player_scope.as_ref(),
        context: AmountContext::Pending,
        repeats: node.repeat_for.is_some() || node.repeat_until.is_some(),
    });
    if let Some(sub) = node.sub_ability.as_deref() {
        resolved_tree(sub, out);
    }
    if let Some(other) = node.else_ability.as_deref() {
        resolved_tree(other, out);
    }
}

/// Whether a spell or ability already on the stack can take life from
/// `opponent` when it resolves (CR 608.2).
pub(super) fn resolved_reaches(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    ability: &ResolvedAbility,
) -> bool {
    let mut nodes = Vec::new();
    resolved_tree(ability, &mut nodes);
    chain_loss(
        state,
        ai_player,
        opponent,
        ability.source_id,
        nodes.into_iter(),
    )
    .reaches
}

/// Life each side loses, and the AI gains, when a chain of instructions from
/// `source_id` resolves with `opponent` chosen for its player target.
///
/// One targeted amount is counted — the largest — since a single target slot
/// is all the line driver aims at the opponent; non-targeted recipients
/// ("each player", "each opponent") add on top. An amount this walk cannot
/// read — pending work, a repeated instruction, damage dealt by an object
/// other than the source, a context it cannot resolve — is not counted, but
/// the instruction still marks the chain as reaching the opponent.
fn chain_loss<'a>(
    state: &GameState,
    ai_player: PlayerId,
    opponent: PlayerId,
    source_id: ObjectId,
    nodes: impl Iterator<Item = ChainNode<'a>>,
) -> ChainLoss {
    // CR 702.16e: protection prevents damage from a source with the stated
    // quality. CR 120.3b + CR 702.90b: damage from an infect source gives
    // poison counters instead of costing life.
    let damage_lands = || {
        !player_protection_from(state, opponent, Some(source_id))
            && !object_has_effective_keyword_kind(state, source_id, KeywordKind::Infect)
    };
    // CR 702.15b: damage from a lifelink source gains its controller life.
    let lifelink = object_has_effective_keyword_kind(state, source_id, KeywordKind::Lifelink);
    let targetable = |filter: &TargetFilter| {
        player_matches_target_filter_in_state(
            state,
            filter,
            opponent,
            Some(ai_player),
            Some(source_id),
        ) && player_is_legal_target(state, opponent, source_id, ai_player)
    };
    let in_scope = |scope: &PlayerFilter, player: PlayerId| {
        matches_player_scope(state, player, scope, ai_player, source_id)
    };
    let read = |amount: &QuantityExpr, node: &ChainNode<'_>| match node.context {
        AmountContext::Printed if !node.repeats => {
            linear_amount(state, ai_player, source_id, amount)
        }
        AmountContext::Printed | AmountContext::Pending => None,
    };
    let mut targeted = Linear::default();
    let mut loss = LifeLoss::default();
    let mut reaches = false;

    for node in nodes {
        match node.effect {
            // CR 120.3a: damage dealt to a player costs that player that much
            // life. A `damage_source` override makes some other object the
            // source (CR 120.3), whose characteristics are not priced here.
            Effect::DealDamage {
                amount,
                target,
                damage_source: None,
                ..
            } if damage_lands() && targetable(target) => {
                reaches = true;
                if lifelink {
                    loss.controller_gain = Gain::Unread;
                }
                if let Some(amount) = read(amount, &node) {
                    targeted = targeted.max(amount);
                }
            }
            // CR 120.7: another object is the source of this damage (Soul's
            // Fire: "target creature you control deals damage …"), so its
            // amount and its source's characteristics are not this walk's to
            // read.
            Effect::DealDamage {
                target,
                damage_source: Some(_),
                ..
            } if targetable(target) => {
                reaches = true;
                loss.controller_gain = Gain::Unread;
            }
            Effect::DamageAll {
                player_filter: Some(_),
                damage_source: Some(_),
                ..
            } => {
                reaches = true;
                loss.controller_gain = Gain::Unread;
            }
            Effect::DamageEachPlayer {
                amount,
                player_filter,
            }
            | Effect::DamageAll {
                amount,
                player_filter: Some(player_filter),
                damage_source: None,
                ..
            } => {
                let hits_opponent = in_scope(player_filter, opponent) && damage_lands();
                let hits_controller = in_scope(player_filter, ai_player);
                reaches |= hits_opponent;
                if lifelink && (hits_opponent || hits_controller) {
                    loss.controller_gain = Gain::Unread;
                }
                if let Some(amount) = read(amount, &node) {
                    if hits_opponent {
                        loss.opponent = loss.opponent.plus(amount);
                    }
                    if hits_controller {
                        loss.controller = loss.controller.plus(amount);
                    }
                }
            }
            // CR 119.3: life loss — not damage, so protection and infect do
            // not stop it.
            Effect::LoseLife {
                amount,
                target: Some(target),
            } if targetable(target) => {
                reaches = true;
                if let Some(amount) = read(amount, &node) {
                    targeted = targeted.max(amount);
                }
            }
            Effect::LoseLife {
                amount,
                target: None,
            } => {
                // "Each opponent loses N life" iterates its player scope, each
                // player losing in turn; without a scope or target, the
                // controller loses it.
                let (hits_opponent, hits_controller) = match node.player_scope {
                    Some(scope) => (in_scope(scope, opponent), in_scope(scope, ai_player)),
                    None => (false, true),
                };
                reaches |= hits_opponent;
                if let Some(amount) = read(amount, &node) {
                    if hits_opponent {
                        loss.opponent = loss.opponent.plus(amount);
                    }
                    if hits_controller {
                        loss.controller = loss.controller.plus(amount);
                    }
                }
            }
            // CR 119.3: life gained. The AI is the recipient when the
            // instruction names its controller — inside a player scope, the
            // player it is iterating. A targeted recipient is aimed at the
            // opponent by the driver; any other recipient is not resolved
            // here, so the gain is left unread.
            Effect::GainLife { amount, player } => {
                let to_ai = match (player, node.player_scope) {
                    (TargetFilter::Controller, None) => Some(true),
                    (TargetFilter::Controller, Some(scope)) => Some(in_scope(scope, ai_player)),
                    _ => None,
                };
                let gain = match to_ai {
                    Some(false) => Gain::default(),
                    Some(true) => read(amount, &node).map_or(Gain::Unread, Gain::Read),
                    None => Gain::Unread,
                };
                loss.controller_gain = loss.controller_gain.plus(gain);
            }
            effect if changes_life_unpriced(effect) => {
                reaches = true;
                loss.controller_gain = Gain::Unread;
            }
            _ => {}
        }
    }
    loss.opponent = loss.opponent.plus(targeted);
    ChainLoss { loss, reaches }
}

/// Instructions that can change a player's life total — or put more spells on
/// the stack that might — through a route the pricing walk does not model:
/// per-source damage (CR 120.1), already-replaced damage, life totals set,
/// exchanged or redistributed (CR 119.5, CR 701.12), a player losing outright
/// (CR 104.3e), and copies or casts of further spells (CR 707.10, CR 601.2).
/// Pending work containing any of them is not priced.
fn changes_life_unpriced(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::EachDealsDamageEqualToPower { .. }
            | Effect::EachSourceDealsDamage { .. }
            | Effect::ApplyPostReplacementDamage { .. }
            | Effect::SetLifeTotal { .. }
            | Effect::ExchangeLifeWithStat { .. }
            | Effect::ExchangeLifeTotals { .. }
            | Effect::RedistributeLifeTotals
            | Effect::LoseTheGame { .. }
            | Effect::CopySpell { .. }
            | Effect::EpicCopy { .. }
            | Effect::CastCopyOfCard { .. }
            | Effect::CastFromZone { .. }
            | Effect::FreeCastFromZones { .. }
            | Effect::MiracleCast { .. }
            | Effect::MadnessCast { .. }
    )
}

/// Resolve a damage or life-loss amount: a bare `X` prices per announced point
/// (CR 107.3a); anything else must be resolvable from the source's present
/// context, otherwise the effect is not priced at all.
fn linear_amount(
    state: &GameState,
    ai_player: PlayerId,
    source_id: ObjectId,
    amount: &QuantityExpr,
) -> Option<Linear> {
    if amount.contains_x() {
        return matches!(
            amount,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { .. }
            }
        )
        .then_some(Linear { fixed: 0, per_x: 1 });
    }
    try_resolve_quantity_in_source_context(state, amount, ai_player, source_id).map(|value| {
        Linear {
            fixed: value.max(0) as u32,
            per_x: 0,
        }
    })
}

fn x_shard_count(cost: &ManaCost) -> u32 {
    match cost {
        ManaCost::Cost { shards, .. } => shards
            .iter()
            .filter(|shard| matches!(shard, ManaCostShard::X))
            .count() as u32,
        _ => 0,
    }
}
