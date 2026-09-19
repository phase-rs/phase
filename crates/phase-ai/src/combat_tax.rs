//! Deciding whether to pay the aggregate combat tax imposed by UnlessPay combat
//! restrictions (Propaganda, Ghostly Prison, Sphere of Safety, Windborn Muse,
//! Norn's Annex) and their block-side twins.
//!
//! CR 508.1d + CR 509.1c: a player is never required to pay a combat tax, so the
//! engine's declaration-completion authority has to be told whether the
//! declaring seat intends to. [`plan_attack_tax`] and [`plan_block_tax`] make that
//! call BEFORE the declaration is submitted and return a
//! [`CombatTaxPosture`]: `Accept` keeps the taxed creatures, `Refuse` lets the
//! engine substitute its tax-free witness.
//!
//! The payment prompt that follows an `Accept` is answered by the engine-owned
//! `combat::pending_combat_tax_is_affordable`, not by a second judgement here.
//! Declining that prompt rebuilds the identical declare prompt, so an answer that
//! could disagree with the posture which opened it would loop. Keeping the
//! judgement in exactly one place is what makes the round trip terminate.

use std::collections::{HashMap, HashSet};

use engine::game::combat::{
    attack_tax_is_affordable, block_tax_is_affordable, compute_attack_tax, compute_block_tax,
    AttackTarget, CombatTaxPosture,
};
use engine::game::combat_damage::lethal_damage_needed;
use engine::game::mana_sources::activatable_mana_options;
use engine::types::game_state::{CombatTaxContext, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;

/// When declining the tax would remove this fraction or more of the declared
/// creatures, treat the decision as "would collapse the declaration" and add a
/// modest bias toward paying. 0.75 = if 3 of 4 attackers are taxed, declining is
/// structurally similar to declining combat.
const DECLARATION_COLLAPSE_FRACTION: f64 = 0.75;

/// Base bonus applied when expected damage through exceeds the tax total.
const DAMAGE_EXCEEDS_TAX_BONUS: f64 = 0.35;

/// Base penalty applied when the tax total exceeds expected damage through.
const TAX_EXCEEDS_DAMAGE_PENALTY: f64 = -0.45;

/// Penalty for paying when the tax would consume every mana source we have,
/// leaving us unable to interact on the opponent's turn.
const TAP_OUT_PENALTY: f64 = -0.2;

/// Aggro archetypes pay the attack tax more aggressively.
const AGGRO_AMP: f64 = 1.4;

/// Control archetypes conserve mana for interaction.
const CONTROL_DAMP: f64 = 0.6;

/// Reduced control dampening for blocking decisions, so a control seat does not
/// shed its blockers to a tax it can comfortably pay (issue #1541).
const BLOCKING_CONTROL_DAMP: f64 = 0.8;

/// Bonus for paying a block tax to keep valuable blockers.
const BLOCKER_VALUE_BONUS: f64 = 0.25;

/// Extra bias toward paying when declining would drop most of the declaration.
const COLLAPSE_BONUS: f64 = 0.15;

/// Choose the attack declaration and tax posture the AI is prepared to honour.
///
/// Returns the (possibly trimmed) proposal to submit and the posture to complete
/// it under. Most taxes in this class price each taxed attacker (Propaganda's
/// "{2} for each creature"), and CR 508.1h totals those prices into one
/// locked-in quote, so an alpha strike the AI
/// cannot afford in full is not abandoned: the weakest taxed attacker is dropped
/// and the smaller strike re-priced, until one is both worth its price and
/// affordable. An empty result hands the engine's tax-free witness the final say,
/// which is also what honours any must-attack requirement the trimming walked past.
pub(crate) fn plan_attack_tax(
    state: &GameState,
    player: PlayerId,
    features: &DeckFeatures,
    attacks: &[(ObjectId, AttackTarget)],
) -> (Vec<(ObjectId, AttackTarget)>, CombatTaxPosture) {
    let mut kept = attacks.to_vec();
    while !kept.is_empty() {
        // No quote left means trimming removed every taxed attacker, so what
        // remains attacks for free.
        let Some((total_cost, per_creature)) = compute_attack_tax(state, &kept) else {
            return (kept, CombatTaxPosture::Refuse);
        };
        // An attacker's stake is the damage it threatens to deal.
        let damage_at_stake = per_creature
            .iter()
            .map(|(id, _)| state.objects.get(id).and_then(|obj| obj.power).unwrap_or(0))
            .sum();
        let quote = TaxQuote {
            context: CombatTaxContext::Attacking,
            total_cost: &total_cost,
            damage_at_stake,
            taxed_count: per_creature.len(),
            total_declared: kept.len(),
        };
        // The judgement is cheap; the affordability probe clones the state to
        // simulate auto-tapping, so it only runs for a strike worth paying for.
        if is_worth_paying(state, player, features, &quote)
            && attack_tax_is_affordable(state, &kept)
        {
            return (kept, CombatTaxPosture::Accept);
        }

        let Some(weakest) = per_creature
            .iter()
            .map(|(id, _)| *id)
            // Tie-broken by id so the trim is deterministic across runs.
            .min_by_key(|id| (state.objects.get(id).and_then(|obj| obj.power), id.0))
        else {
            break;
        };
        kept.retain(|(id, _)| *id != weakest);
    }

    (Vec::new(), CombatTaxPosture::Refuse)
}

/// Choose the posture to complete a blocker proposal under.
///
/// CR 509.1c + CR 509.1f: `Accept` only when paying is worth it and the
/// defending seat can cover the quote, so the completion never opens a payment
/// prompt this seat would then be unable to answer with a payment. Blocks are not trimmed: an
/// unaccepted proposal falls back to the engine's tax-free witness whole.
pub(crate) fn plan_block_tax(
    state: &GameState,
    player: PlayerId,
    features: &DeckFeatures,
    assignments: &[(ObjectId, ObjectId)],
) -> CombatTaxPosture {
    let Some((total_cost, per_creature)) = compute_block_tax(state, assignments) else {
        return CombatTaxPosture::Refuse;
    };
    let taxed: HashSet<ObjectId> = per_creature.iter().map(|(blocker, _)| *blocker).collect();
    let damage_at_stake = block_tax_stake(state, assignments, &taxed);
    let quote = TaxQuote {
        context: CombatTaxContext::Blocking,
        total_cost: &total_cost,
        damage_at_stake,
        taxed_count: per_creature.len(),
        total_declared: assignments.len(),
    };
    if is_worth_paying(state, player, features, &quote)
        && block_tax_is_affordable(state, player, assignments)
    {
        return CombatTaxPosture::Accept;
    }
    CombatTaxPosture::Refuse
}

/// Damage a block tax decides: what changes if the taxed blockers drop out.
///
/// Each attacker is judged by the blockers it would keep:
/// - CR 509.1h + CR 510.1c: an attacker that keeps an untaxed blocker stays
///   blocked either way, so its taxed blockers are worth the damage they add
///   toward killing it (their own power, each blocker counted once).
/// - CR 509.1h + CR 510.1b: an attacker whose every blocker is taxed becomes
///   unblocked if they drop out, so its damage gets through. CR 702.19b: a
///   trampler already carries its excess past its blockers, so for it only the
///   lethal damage those blockers were absorbing is at stake.
fn block_tax_stake(
    state: &GameState,
    assignments: &[(ObjectId, ObjectId)],
    taxed: &HashSet<ObjectId>,
) -> i32 {
    let mut blockers_by_attacker: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    for (blocker, attacker) in assignments {
        blockers_by_attacker
            .entry(*attacker)
            .or_default()
            .push(*blocker);
    }

    // CR 510.1a: a creature with 0 or less power assigns no combat damage.
    let power_of = |id: &ObjectId| {
        state
            .objects
            .get(id)
            .and_then(|obj| obj.power)
            .unwrap_or(0)
            .max(0)
    };
    let mut counted_blockers: HashSet<ObjectId> = HashSet::new();
    let mut stake = 0;
    for (attacker, blockers) in &blockers_by_attacker {
        let keeps_untaxed_blocker = blockers.iter().any(|blocker| !taxed.contains(blocker));
        if keeps_untaxed_blocker {
            for blocker in blockers {
                if taxed.contains(blocker) && counted_blockers.insert(*blocker) {
                    stake += power_of(blocker);
                }
            }
            continue;
        }

        let attacker_power = power_of(attacker);
        let attacker_object = state.objects.get(attacker);
        let tramples = attacker_object.is_some_and(|obj| obj.has_keyword(&Keyword::Trample));
        if !tramples {
            stake += attacker_power;
            continue;
        }
        // CR 702.2c: any nonzero damage from a deathtouch source is lethal.
        let deathtouch = attacker_object.is_some_and(|obj| obj.has_keyword(&Keyword::Deathtouch));
        let absorbed: i32 = blockers
            .iter()
            .map(|blocker| lethal_damage_needed(state, *blocker, deathtouch) as i32)
            .sum();
        stake += attacker_power.min(absorbed);
    }
    stake
}

/// A combat-tax quote for one proposed declaration.
struct TaxQuote<'a> {
    context: CombatTaxContext,
    total_cost: &'a ManaCost,
    /// Damage the taxed creatures decide: dealt by taxed attackers, or stopped
    /// by taxed blockers. Each planner computes it for its side of combat.
    damage_at_stake: i32,
    /// How many creatures in the declaration the quote taxes.
    taxed_count: usize,
    /// Size of the whole declaration the quote was priced against.
    total_declared: usize,
}

/// Does paying this quote beat letting its creatures drop out of combat?
///
/// Paying earns a bias from comparing the damage at stake to the tax (scaled by
/// deck archetype), minus a penalty for tapping out of interaction, plus
/// bonuses for keeping a declaration from collapsing and for keeping blockers.
/// Declining earns the opposite of the damage bias.
fn is_worth_paying(
    state: &GameState,
    player: PlayerId,
    features: &DeckFeatures,
    quote: &TaxQuote<'_>,
) -> bool {
    let tax_mana_value = quote.total_cost.mana_value();
    let expected_damage = quote.damage_at_stake;
    let tax = tax_mana_value as i32;

    let archetype_mod = archetype_multiplier(features, quote.context.clone());
    let damage_bias = if expected_damage > tax {
        DAMAGE_EXCEEDS_TAX_BONUS * archetype_mod
    } else if expected_damage < tax {
        TAX_EXCEEDS_DAMAGE_PENALTY / archetype_mod.max(0.01)
    } else {
        0.0
    };

    let available = count_untapped_mana_sources(state, player);
    let tap_out_penalty = if available > 0 && available.saturating_sub(tax_mana_value) == 0 {
        TAP_OUT_PENALTY
    } else {
        0.0
    };

    let collapse_fraction = if quote.total_declared > 0 {
        quote.taxed_count as f64 / quote.total_declared as f64
    } else {
        0.0
    };
    let collapse_bonus =
        if collapse_fraction >= DECLARATION_COLLAPSE_FRACTION && expected_damage > 0 {
            COLLAPSE_BONUS
        } else {
            0.0
        };

    let blocker_value_bonus = match quote.context {
        CombatTaxContext::Blocking => BLOCKER_VALUE_BONUS,
        CombatTaxContext::Attacking => 0.0,
    };

    let pay_value = damage_bias + tap_out_penalty + collapse_bonus + blocker_value_bonus;
    let decline_value = -damage_bias;
    pay_value > decline_value
}

/// Count the mana sources the seat could tap for mana right now.
///
/// Delegates to `activatable_mana_options`, the engine's readiness check for
/// tapping a source for mana now. It excludes tapped sources, sources the seat
/// does not control, lands without a mana ability, and summoning-sick
/// creatures (CR 302.6); layers give a basic-typed land its intrinsic mana
/// ability (CR 305.6).
fn count_untapped_mana_sources(state: &GameState, player: PlayerId) -> u32 {
    state
        .battlefield
        .iter()
        .filter(|&&id| !activatable_mana_options(state, id, player).is_empty())
        .count() as u32
}

/// Deck archetype weighting: aggro decks push harder on paying the attack tax (so
/// their attack doesn't collapse); control decks conserve mana for interaction.
fn archetype_multiplier(features: &DeckFeatures, context: CombatTaxContext) -> f64 {
    let aggro = features.aggro_pressure.commitment.clamp(0.0, 1.0) as f64;
    let control = features.control.commitment.clamp(0.0, 1.0) as f64;

    match context {
        CombatTaxContext::Attacking => {
            1.0 + (AGGRO_AMP - 1.0) * aggro - (1.0 - CONTROL_DAMP) * control
        }
        // Reduced control dampening so a control seat keeps its blockers
        // (issue #1541).
        CombatTaxContext::Blocking => 1.0 - (1.0 - BLOCKING_CONTROL_DAMP) * control,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features_with(aggro: f32, control: f32) -> DeckFeatures {
        let mut features = DeckFeatures::default();
        features.aggro_pressure.commitment = aggro;
        features.control.commitment = control;
        features
    }

    /// CR 302.6 + CR 305.6: only sources that can actually be tapped for mana
    /// count. A summoning-sick creature cannot pay a {T} cost, and a land with no
    /// mana ability produces nothing; an untapped Forest counts.
    #[test]
    fn only_activatable_mana_sources_are_available() {
        use engine::game::scenario::{GameScenario, P0};
        use engine::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
        };
        use engine::types::mana::ManaColor;

        let mut scenario = GameScenario::new();
        scenario.add_basic_land(P0, ManaColor::Green);
        // A land with no rules text has no mana ability to activate.
        scenario.add_land_from_oracle(P0, "Blank Land", "");
        let elf = scenario
            .add_creature(P0, "Mana Elf", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            )
            .id();
        let mut runner = scenario.build();

        assert_eq!(
            count_untapped_mana_sources(runner.state(), P0),
            2,
            "the Forest and the elf count; the blank land does not"
        );
        runner
            .state_mut()
            .objects
            .get_mut(&elf)
            .unwrap()
            .summoning_sick = true;
        assert_eq!(
            count_untapped_mana_sources(runner.state(), P0),
            1,
            "only the Forest counts while the elf is summoning sick"
        );
    }

    /// The stake of each `block_tax_stake` case, with every blocker taxed
    /// unless it is listed in `untaxed`.
    fn stake_for(
        state: &GameState,
        assignments: &[(ObjectId, ObjectId)],
        untaxed: &[ObjectId],
    ) -> i32 {
        let taxed: HashSet<ObjectId> = assignments
            .iter()
            .map(|(blocker, _)| *blocker)
            .filter(|blocker| !untaxed.contains(blocker))
            .collect();
        block_tax_stake(state, assignments, &taxed)
    }

    /// CR 509.1h + CR 510.1b: an attacker whose only blocker is taxed gets
    /// through if that blocker drops out, so its full power is at stake.
    #[test]
    fn block_stake_is_the_power_that_gets_through() {
        use engine::game::scenario::{GameScenario, P0, P1};

        let mut scenario = GameScenario::new();
        let attacker = scenario.add_creature(P0, "Raider", 5, 5).id();
        let wall = scenario.add_creature(P1, "Wall", 0, 4).id();
        let runner = scenario.build();

        assert_eq!(stake_for(runner.state(), &[(wall, attacker)], &[]), 5);
    }

    /// CR 509.1h: an attacker that keeps an untaxed blocker stays blocked, so
    /// the taxed blocker is worth the damage it adds toward killing it.
    #[test]
    fn block_stake_for_a_gang_block_counts_the_taxed_blockers_power() {
        use engine::game::scenario::{GameScenario, P0, P1};

        let mut scenario = GameScenario::new();
        let attacker = scenario.add_creature(P0, "Brute", 4, 4).id();
        let taxed_bear = scenario.add_creature(P1, "Taxed Bear", 2, 2).id();
        let free_bear = scenario.add_creature(P1, "Free Bear", 2, 2).id();
        let runner = scenario.build();

        assert_eq!(
            stake_for(
                runner.state(),
                &[(taxed_bear, attacker), (free_bear, attacker)],
                &[free_bear],
            ),
            2
        );
    }

    /// CR 702.19b + CR 702.2c: a trampler's excess reaches the player whether
    /// or not the chump blocks, so only the lethal damage the blocker absorbs
    /// is at stake; with deathtouch, that is a single point.
    #[test]
    fn block_stake_for_a_trampler_is_the_lethal_damage_absorbed() {
        use engine::game::scenario::{GameScenario, P0, P1};

        let mut scenario = GameScenario::new();
        let trampler = scenario
            .add_creature(P0, "Stomper", 6, 6)
            .with_keyword(Keyword::Trample)
            .id();
        let deadly_trampler = {
            let mut builder = scenario.add_creature(P0, "Deadly Stomper", 6, 6);
            builder.with_keyword(Keyword::Trample);
            builder.with_keyword(Keyword::Deathtouch);
            builder.id()
        };
        let chump = scenario.add_creature(P1, "Chump", 0, 3).id();
        let runner = scenario.build();

        assert_eq!(stake_for(runner.state(), &[(chump, trampler)], &[]), 3);
        assert_eq!(
            stake_for(runner.state(), &[(chump, deadly_trampler)], &[]),
            1
        );
    }

    /// CR 702.19b: a trampler too weak to get past its blocker is stopped
    /// entirely, so its whole power (not the blocker's toughness) is at stake.
    #[test]
    fn block_stake_for_a_weak_trampler_is_capped_at_its_power() {
        use engine::game::scenario::{GameScenario, P0, P1};

        let mut scenario = GameScenario::new();
        let trampler = scenario
            .add_creature(P0, "Small Stomper", 2, 2)
            .with_keyword(Keyword::Trample)
            .id();
        let wall = scenario.add_creature(P1, "Wall", 0, 4).id();
        let runner = scenario.build();

        assert_eq!(stake_for(runner.state(), &[(wall, trampler)], &[]), 2);
    }

    /// CR 510.1a: an attacker with 0 or less power assigns no combat damage, so
    /// a negative-power attacker puts nothing at stake rather than a negative.
    #[test]
    fn block_stake_treats_negative_power_as_no_damage() {
        use engine::game::scenario::{GameScenario, P0, P1};

        let mut scenario = GameScenario::new();
        let shrunk = scenario.add_creature(P0, "Shrunk Raider", 0, 4).id();
        let guard = scenario.add_creature(P1, "Guard", 1, 1).id();
        let mut runner = scenario.build();
        runner.state_mut().objects.get_mut(&shrunk).unwrap().power = Some(-2);

        assert_eq!(stake_for(runner.state(), &[(guard, shrunk)], &[]), 0);
    }

    /// A taxed blocker assigned to two attackers that each keep an untaxed
    /// blocker adds its power once, not once per attacker.
    #[test]
    fn block_stake_counts_a_taxed_blocker_once_across_attackers() {
        use engine::game::scenario::{GameScenario, P0, P1};

        let mut scenario = GameScenario::new();
        let first = scenario.add_creature(P0, "First Brute", 4, 4).id();
        let second = scenario.add_creature(P0, "Second Brute", 4, 4).id();
        let taxed = scenario.add_creature(P1, "Taxed Guard", 3, 3).id();
        let free_first = scenario.add_creature(P1, "Free Guard A", 2, 2).id();
        let free_second = scenario.add_creature(P1, "Free Guard B", 2, 2).id();
        let runner = scenario.build();

        let assignments = [
            (taxed, first),
            (taxed, second),
            (free_first, first),
            (free_second, second),
        ];
        assert_eq!(
            stake_for(runner.state(), &assignments, &[free_first, free_second]),
            3
        );
    }

    /// A declaration mixing both cases sums them: the fully taxed attacker's
    /// power gets through, and the kept attacker's taxed blocker adds its own.
    #[test]
    fn block_stake_sums_a_mixed_declaration() {
        use engine::game::scenario::{GameScenario, P0, P1};

        let mut scenario = GameScenario::new();
        let exposed = scenario.add_creature(P0, "Exposed Raider", 5, 5).id();
        let held = scenario.add_creature(P0, "Held Raider", 4, 4).id();
        let lone_guard = scenario.add_creature(P1, "Lone Guard", 1, 1).id();
        let taxed_helper = scenario.add_creature(P1, "Taxed Helper", 2, 2).id();
        let free_guard = scenario.add_creature(P1, "Free Guard", 2, 2).id();
        let runner = scenario.build();

        let assignments = [
            (lone_guard, exposed),
            (taxed_helper, held),
            (free_guard, held),
        ];
        assert_eq!(
            stake_for(runner.state(), &assignments, &[free_guard]),
            5 + 2
        );
    }

    #[test]
    fn aggro_amplifies_the_attack_tax_bias_over_control() {
        let aggro = archetype_multiplier(&features_with(0.9, 0.0), CombatTaxContext::Attacking);
        let control = archetype_multiplier(&features_with(0.0, 0.9), CombatTaxContext::Attacking);
        assert!(
            aggro > control,
            "aggro amplifier {aggro} should exceed control {control}"
        );
    }

    /// Issue #1541: blocking dampens control less than attacking does, so a
    /// control seat is more willing to keep its blockers than to press an attack.
    #[test]
    fn control_is_damped_less_when_blocking_than_when_attacking() {
        let control = features_with(0.0, 0.9);
        let blocking = archetype_multiplier(&control, CombatTaxContext::Blocking);
        let attacking = archetype_multiplier(&control, CombatTaxContext::Attacking);
        assert!(
            blocking > attacking,
            "blocking multiplier {blocking} should exceed attacking {attacking}"
        );
    }
}
