use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::TacticalPolicy;
use super::strategy_helpers::is_own_main_phase;
use crate::deck_profile::DeckArchetype;

/// Rewards casting spells that have synergy with existing board presence.
///
/// Uses the pre-computed `SynergyGraph` (tribal, sacrifice, graveyard,
/// spellcast axes) and supplements with runtime tribal overlap detection
/// against creatures currently on the battlefield.
pub struct SynergyCastingPolicy;

impl TacticalPolicy for SynergyCastingPolicy {
    fn archetype_scale(&self, archetype: DeckArchetype) -> f64 {
        match archetype {
            DeckArchetype::Aggro => 1.0,
            DeckArchetype::Control => 1.0,
            DeckArchetype::Midrange => 1.0,
            DeckArchetype::Ramp => 1.0,
            DeckArchetype::Combo => 2.0,
        }
    }

    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        if !is_own_main_phase(ctx) {
            return 0.0;
        }

        let GameAction::CastSpell { card_id, .. } = &ctx.candidate.action else {
            return 0.0;
        };

        let object = ctx
            .state
            .objects
            .values()
            .find(|obj| obj.card_id == *card_id);

        let Some(object) = object else {
            return 0.0;
        };

        let max_bonus = ctx.config.policy_penalties.synergy_casting_bonus;

        // Axis 1: Pre-computed synergy score from the deck graph.
        let graph_score = ctx.context.synergy_graph.card_score(&object.name);

        // Axis 2: Runtime tribal overlap — does this creature share a subtype
        // with creatures already on our battlefield?
        let tribal_bonus = if object.card_types.core_types.contains(&CoreType::Creature) {
            tribal_overlap_bonus(ctx, &object.card_types.subtypes)
        } else {
            0.0
        };

        // Combine both axes, capped at max_bonus.
        let raw = graph_score * 0.5 * max_bonus + tribal_bonus * 0.5 * max_bonus;
        raw.min(max_bonus)
    }
}

/// Proportion of the AI's battlefield creatures that share at least one
/// subtype with the incoming spell. Returns 0.0-1.0.
fn tribal_overlap_bonus(ctx: &PolicyContext<'_>, spell_subtypes: &[String]) -> f64 {
    if spell_subtypes.is_empty() {
        return 0.0;
    }

    let mut matching = 0u32;
    let mut total = 0u32;

    for &id in ctx.state.battlefield.iter() {
        let Some(obj) = ctx.state.objects.get(&id) else {
            continue;
        };
        if obj.controller != ctx.ai_player
            || obj.zone != Zone::Battlefield
            || !obj.card_types.core_types.contains(&CoreType::Creature)
        {
            continue;
        }
        total += 1;
        if obj
            .card_types
            .subtypes
            .iter()
            .any(|st| spell_subtypes.contains(st))
        {
            matching += 1;
        }
    }

    if total == 0 {
        return 0.0;
    }

    matching as f64 / total as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;

    fn setup_main_phase() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state
    }

    fn add_creature_to_battlefield(
        state: &mut GameState,
        player: PlayerId,
        name: &str,
        subtypes: Vec<&str>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(200),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes = subtypes.into_iter().map(String::from).collect();
        obj.controller = player;
        id
    }

    fn add_creature_to_hand(
        state: &mut GameState,
        name: &str,
        subtypes: Vec<&str>,
    ) -> (ObjectId, CardId) {
        let card_id = CardId(50);
        let obj_id = create_object(state, card_id, PlayerId(0), name.to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes = subtypes.into_iter().map(String::from).collect();
        obj.mana_cost = ManaCost::generic(2);
        (obj_id, card_id)
    }

    fn make_decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        }
    }

    #[test]
    fn tribal_overlap_gives_bonus() {
        let mut state = setup_main_phase();
        // Two Elves on battlefield
        add_creature_to_battlefield(&mut state, PlayerId(0), "Llanowar Elves", vec!["Elf"]);
        add_creature_to_battlefield(&mut state, PlayerId(0), "Elvish Mystic", vec!["Elf"]);

        // Casting another Elf
        let (obj_id, card_id) = add_creature_to_hand(&mut state, "Elvish Archdruid", vec!["Elf"]);

        let config = AiConfig::default();
        let decision = make_decision();
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: obj_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = SynergyCastingPolicy.score(&ctx);
        assert!(
            score > 0.0,
            "Casting an Elf with Elves on board should give bonus, got {score}"
        );
    }

    #[test]
    fn returns_zero_outside_main_phase() {
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::BeginCombat;
        state.active_player = PlayerId(0);
        add_creature_to_battlefield(&mut state, PlayerId(0), "Llanowar Elves", vec!["Elf"]);
        let (obj_id, card_id) = add_creature_to_hand(&mut state, "Elvish Mystic", vec!["Elf"]);

        let config = AiConfig::default();
        let decision = make_decision();
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: obj_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        assert_eq!(
            SynergyCastingPolicy.score(&ctx),
            0.0,
            "Should return 0.0 outside main phase"
        );
    }

    #[test]
    fn no_tribal_overlap_with_different_types() {
        let mut state = setup_main_phase();
        // Goblins on battlefield
        add_creature_to_battlefield(&mut state, PlayerId(0), "Goblin Guide", vec!["Goblin"]);

        // Casting an Elf (no overlap)
        let (obj_id, card_id) = add_creature_to_hand(&mut state, "Llanowar Elves", vec!["Elf"]);

        let config = AiConfig::default();
        let decision = make_decision();
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: obj_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = SynergyCastingPolicy.score(&ctx);
        assert!(
            score < 0.01,
            "No tribal overlap should give near-zero bonus, got {score}"
        );
    }
}
