use std::collections::HashSet;

use engine::game::game_object::GameObject;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use crate::deck_knowledge::remaining_deck_view;
use crate::eval::StrategicIntent;
use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

pub struct TutorPolicy;

impl TutorPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        if !matches!(ctx.candidate.action, GameAction::CastSpell { .. }) {
            return 0.0;
        }

        let Some(facts) = ctx.cast_facts() else {
            return 0.0;
        };
        if !facts.has_search_library() {
            return 0.0;
        }

        let remaining = remaining_deck_view(ctx.state, ctx.ai_player);
        if remaining.entries.is_empty() {
            return 0.0;
        }

        let available_mana = crate::zone_eval::available_mana(ctx.state, ctx.ai_player);
        let best_follow_up = remaining
            .entries
            .iter()
            .map(|entry| entry_score(entry, available_mana, ctx.strategic_intent(), ctx))
            .fold(0.0, f64::max);

        0.34 + best_follow_up * 0.8
    }
}

impl TacticalPolicy for TutorPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::Tutor
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("tutor_score"),
        }
    }
}

pub(crate) fn score_search_choice_cards(
    state: &GameState,
    ai_player: PlayerId,
    cards: &[ObjectId],
) -> Vec<(ObjectId, f64)> {
    let available_mana = crate::zone_eval::available_mana(state, ai_player);
    let intent = crate::eval::strategic_intent(state, ai_player);
    let mana_constrained = materially_mana_constrained_state(state, ai_player);
    let combo_targets = combo_missing_piece_names(state, ai_player);

    cards
        .iter()
        .filter_map(|&card_id| {
            let object = state.objects.get(&card_id)?;
            let base = tutor_object_score(object, available_mana, intent, mana_constrained);
            let combo_bonus = if combo_targets.contains(&object.name.as_str()) {
                COMBO_PIECE_TUTOR_BONUS
            } else {
                0.0
            };
            Some((card_id, base + combo_bonus))
        })
        .collect()
}

pub(crate) fn score_search_choice_selection(
    state: &GameState,
    ai_player: PlayerId,
    chosen: &[ObjectId],
) -> f64 {
    let available_mana = crate::zone_eval::available_mana(state, ai_player);
    let intent = crate::eval::strategic_intent(state, ai_player);
    let mana_constrained = materially_mana_constrained_state(state, ai_player);
    let combo_targets = combo_missing_piece_names(state, ai_player);
    let mut seen_names = HashSet::new();

    chosen
        .iter()
        .enumerate()
        .filter_map(|(index, object_id)| state.objects.get(object_id).map(|object| (index, object)))
        .map(|(index, object)| {
            let mut score = tutor_object_score(object, available_mana, intent, mana_constrained);
            if combo_targets.contains(&object.name.as_str()) {
                score += COMBO_PIECE_TUTOR_BONUS;
            }
            if !seen_names.insert(object.name.clone()) {
                score *= 0.7;
            }
            if index > 0 {
                score *= 0.88_f64.powi(index as i32);
            }
            score
        })
        .sum()
}

/// Score bump applied to a tutor target that closes a near-reachable combo
/// line. Sized to dominate the existing `tutor_object_score` cap of `0.8`,
/// so the combo-completing card is preferred over even the highest-EV
/// generic creature. Kept as a module-level constant so the rationale stays
/// adjacent to the call site rather than buried in [`super::PolicyPenalties`].
const COMBO_PIECE_TUTOR_BONUS: f64 = 1.5;

/// Returns the set of card names that, if tutored into the appropriate zone,
/// would complete a registered cEDH combo line for `ai_player`. Empty for
/// non-cEDH boards (combos with their normal pieces missing show up too,
/// but the names match cEDH staples — false positives are bounded by the
/// registry's curation).
fn combo_missing_piece_names(state: &GameState, ai_player: PlayerId) -> HashSet<&'static str> {
    let registry = crate::combo::ComboRegistry::default();
    registry
        .missing_pieces_for_near_reachable_lines(state, ai_player)
        .into_iter()
        .collect()
}

fn entry_score(
    entry: &engine::game::deck_loading::DeckEntry,
    available_mana: u32,
    intent: StrategicIntent,
    ctx: &PolicyContext<'_>,
) -> f64 {
    let card = &entry.card;
    tutor_face_score(
        card,
        available_mana,
        intent,
        materially_mana_constrained(ctx),
    )
}

fn fixed_pt_value(value: &Option<engine::types::ability::PtValue>) -> i32 {
    match value {
        Some(engine::types::ability::PtValue::Fixed(value)) => *value,
        _ => 0,
    }
}

fn keyword_score(keywords: &[Keyword]) -> f64 {
    keywords
        .iter()
        .map(|keyword| match keyword {
            Keyword::Flying
            | Keyword::Trample
            | Keyword::Menace
            | Keyword::Lifelink
            | Keyword::Haste
            | Keyword::DoubleStrike => 0.05,
            _ => 0.0,
        })
        .sum::<f64>()
        .min(0.15)
}

fn materially_mana_constrained(ctx: &PolicyContext<'_>) -> bool {
    materially_mana_constrained_state(ctx.state, ctx.ai_player)
}

fn is_stabilizer(card: &engine::types::card::CardFace) -> bool {
    card.card_type.core_types.contains(&CoreType::Creature)
        && (fixed_pt_value(&card.toughness) >= 4
            || card.abilities.iter().any(|ability| {
                matches!(
                    *ability.effect,
                    engine::types::ability::Effect::Destroy { .. }
                        | engine::types::ability::Effect::DealDamage { .. }
                        | engine::types::ability::Effect::Draw { .. }
                )
            }))
}

fn is_push_card(card: &engine::types::card::CardFace) -> bool {
    (card.card_type.core_types.contains(&CoreType::Creature)
        && (fixed_pt_value(&card.power) >= 4
            || card.keywords.iter().any(|keyword| {
                matches!(keyword, Keyword::Flying | Keyword::Haste | Keyword::Trample)
            })))
        || card.abilities.iter().any(|ability| {
            matches!(
                *ability.effect,
                engine::types::ability::Effect::DealDamage { .. }
                    | engine::types::ability::Effect::Destroy { .. }
            )
        })
}

fn tutor_object_score(
    object: &GameObject,
    available_mana: u32,
    intent: StrategicIntent,
    mana_constrained: bool,
) -> f64 {
    let mana_value = object.mana_cost.mana_value();
    let castability = castability_score(mana_value, available_mana);
    let type_value = type_value_for_object(object, mana_constrained);
    let text_value = object
        .abilities
        .iter()
        .map(effect_text_value)
        .fold(0.0, f64::max);
    let intent_bonus = intent_bonus_for_object(object, intent, text_value);

    (castability + type_value + text_value + intent_bonus).min(0.8)
}

fn tutor_face_score(
    card: &engine::types::card::CardFace,
    available_mana: u32,
    intent: StrategicIntent,
    mana_constrained: bool,
) -> f64 {
    let mana_value = card.mana_cost.mana_value();
    let castability = castability_score(mana_value, available_mana);
    let type_value = type_value_for_face(card, mana_constrained);
    let text_value = card
        .abilities
        .iter()
        .map(effect_text_value)
        .fold(0.0, f64::max);
    let intent_bonus = intent_bonus_for_face(card, intent, text_value);

    (castability + type_value + text_value + intent_bonus).min(0.8)
}

fn castability_score(mana_value: u32, available_mana: u32) -> f64 {
    if mana_value <= available_mana {
        0.16
    } else if mana_value <= available_mana + 2 {
        0.09
    } else {
        0.03
    }
}

fn type_value_for_object(object: &GameObject, mana_constrained: bool) -> f64 {
    if object.card_types.core_types.contains(&CoreType::Creature) {
        let power = object.power.unwrap_or(0);
        let toughness = object.toughness.unwrap_or(0);
        ((power + toughness) as f64 / 10.0).min(0.35) + keyword_score(&object.keywords)
    } else if object
        .card_types
        .core_types
        .contains(&CoreType::Planeswalker)
    {
        0.34
    } else if object.card_types.core_types.contains(&CoreType::Land) {
        if mana_constrained {
            0.28
        } else {
            0.02
        }
    } else {
        0.08
    }
}

fn type_value_for_face(card: &engine::types::card::CardFace, mana_constrained: bool) -> f64 {
    if card.card_type.core_types.contains(&CoreType::Creature) {
        let power = fixed_pt_value(&card.power);
        let toughness = fixed_pt_value(&card.toughness);
        ((power + toughness) as f64 / 10.0).min(0.35) + keyword_score(&card.keywords)
    } else if card.card_type.core_types.contains(&CoreType::Planeswalker) {
        0.34
    } else if card.card_type.core_types.contains(&CoreType::Land) {
        if mana_constrained {
            0.28
        } else {
            0.02
        }
    } else {
        0.08
    }
}

fn effect_text_value(ability: &engine::types::ability::AbilityDefinition) -> f64 {
    match &*ability.effect {
        engine::types::ability::Effect::Destroy { .. }
        | engine::types::ability::Effect::DealDamage { .. }
        | engine::types::ability::Effect::Counter { .. } => 0.22,
        engine::types::ability::Effect::Draw { .. } => 0.16,
        engine::types::ability::Effect::SearchLibrary { .. } => 0.14,
        _ => 0.0,
    }
}

fn intent_bonus_for_object(object: &GameObject, intent: StrategicIntent, text_value: f64) -> f64 {
    match intent {
        StrategicIntent::Stabilize if is_stabilizer_object(object) => 0.16,
        StrategicIntent::PushLethal if is_push_object(object) => 0.16,
        StrategicIntent::Develop => 0.08,
        StrategicIntent::PreserveAdvantage if text_value > 0.15 => 0.08,
        _ => 0.0,
    }
}

fn intent_bonus_for_face(
    card: &engine::types::card::CardFace,
    intent: StrategicIntent,
    text_value: f64,
) -> f64 {
    match intent {
        StrategicIntent::Stabilize if is_stabilizer(card) => 0.16,
        StrategicIntent::PushLethal if is_push_card(card) => 0.16,
        StrategicIntent::Develop => 0.08,
        StrategicIntent::PreserveAdvantage if text_value > 0.15 => 0.08,
        _ => 0.0,
    }
}

fn materially_mana_constrained_state(state: &GameState, ai_player: PlayerId) -> bool {
    crate::zone_eval::available_mana(state, ai_player) < 4
        && state.players[ai_player.0 as usize]
            .hand
            .iter()
            .filter_map(|object_id| state.objects.get(object_id))
            .all(|object| !object.card_types.core_types.contains(&CoreType::Land))
}

fn is_stabilizer_object(object: &GameObject) -> bool {
    object.card_types.core_types.contains(&CoreType::Creature)
        && (object.toughness.unwrap_or(0) >= 4
            || object.abilities.iter().any(|ability| {
                matches!(
                    *ability.effect,
                    engine::types::ability::Effect::Destroy { .. }
                        | engine::types::ability::Effect::DealDamage { .. }
                        | engine::types::ability::Effect::Draw { .. }
                )
            }))
}

fn is_push_object(object: &GameObject) -> bool {
    (object.card_types.core_types.contains(&CoreType::Creature)
        && (object.power.unwrap_or(0) >= 4
            || object.keywords.iter().any(|keyword| {
                matches!(keyword, Keyword::Flying | Keyword::Haste | Keyword::Trample)
            })))
        || object.abilities.iter().any(|ability| {
            matches!(
                *ability.effect,
                engine::types::ability::Effect::DealDamage { .. }
                    | engine::types::ability::Effect::Destroy { .. }
            )
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, QuantityExpr, TargetFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::game_state::{GameState, PlayerDeckPool, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    #[test]
    fn scores_tutor_when_remaining_deck_has_strong_follow_up() {
        let mut state = GameState::new_two_player(42);
        state.phase = engine::types::phase::Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.deck_pools.push(PlayerDeckPool {
            player: PlayerId(0),
            current_main: std::sync::Arc::new(vec![engine::game::deck_loading::DeckEntry {
                card: CardFace {
                    name: "Deck Titan".to_string(),
                    mana_cost: ManaCost::Cost {
                        shards: Vec::new(),
                        generic: 4,
                    },
                    card_type: CardType {
                        supertypes: Vec::new(),
                        core_types: vec![CoreType::Creature],
                        subtypes: Vec::new(),
                    },
                    power: Some(PtValue::Fixed(6)),
                    toughness: Some(PtValue::Fixed(6)),
                    ..Default::default()
                },
                count: 1,
            }]),
            ..Default::default()
        });
        for index in 0..4 {
            let land = create_object(
                &mut state,
                CardId(100 + index),
                PlayerId(0),
                format!("Swamp {index}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&land)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }

        let tutor = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Tutor".to_string(),
            Zone::Hand,
        );
        Arc::make_mut(&mut state.objects.get_mut(&tutor).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::SearchLibrary {
                    filter: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    reveal: false,
                    target_player: None,
                    selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
                    split: None,
                    source_zones: vec![engine::types::zones::Zone::Library],
                },
            ),
        );

        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: tutor,
                card_id: CardId(10),
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: vec![candidate.clone()],
        };
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        assert!(
            TutorPolicy.score(&ctx) > 0.5,
            "expected tutor to score positively with a strong remaining deck"
        );
    }

    #[test]
    fn search_choice_prefers_strongest_single_target() {
        let mut state = GameState::new_two_player(42);

        let titan = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Titan".to_string(),
            Zone::Library,
        );
        {
            let titan_obj = state.objects.get_mut(&titan).unwrap();
            titan_obj.card_types.core_types.push(CoreType::Creature);
            titan_obj.power = Some(6);
            titan_obj.toughness = Some(6);
            titan_obj.keywords.push(Keyword::Flying);
        }

        let land = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Swamp".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let scored = score_search_choice_cards(&state, PlayerId(0), &[titan, land]);
        let titan_score = scored
            .iter()
            .find(|(id, _)| *id == titan)
            .map(|(_, score)| *score)
            .expect("titan score");
        let land_score = scored
            .iter()
            .find(|(id, _)| *id == land)
            .map(|(_, score)| *score)
            .expect("land score");

        assert!(titan_score > land_score);
    }

    #[test]
    fn search_choice_combination_applies_redundancy_discount() {
        let mut state = GameState::new_two_player(42);

        let first = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Titan".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(22),
            PlayerId(0),
            "Titan".to_string(),
            Zone::Library,
        );
        let removal = create_object(
            &mut state,
            CardId(23),
            PlayerId(0),
            "Answer".to_string(),
            Zone::Library,
        );
        for object_id in [first, second] {
            let object = state.objects.get_mut(&object_id).unwrap();
            object.card_types.core_types.push(CoreType::Creature);
            object.power = Some(6);
            object.toughness = Some(6);
        }
        Arc::make_mut(&mut state.objects.get_mut(&removal).unwrap().abilities).push(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                },
            ),
        );

        let duplicate_score = score_search_choice_selection(&state, PlayerId(0), &[first, second]);
        let mixed_score = score_search_choice_selection(&state, PlayerId(0), &[first, removal]);

        assert!(mixed_score > duplicate_score);
    }

    /// Tutor target inference: when Heliod is on the AI's battlefield,
    /// Walking Ballista in the library must outscore an otherwise-stronger
    /// generic creature because grabbing it completes the registered
    /// Heliod+Ballista combo line.
    #[test]
    fn search_choice_prefers_combo_piece_over_generic_threat() {
        let mut state = GameState::new_two_player(42);

        // Heliod, Sun-Crowned on the AI's battlefield → near-reachable line.
        let heliod = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Heliod, Sun-Crowned".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&heliod)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // The library candidates: a generic flying 6/6 (high tutor score) and
        // Walking Ballista (the missing combo piece, a 0/0 by default).
        let titan = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Titan".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&titan).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(6);
            obj.toughness = Some(6);
            obj.keywords.push(Keyword::Flying);
        }
        let ballista = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Walking Ballista".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&ballista)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let scored = score_search_choice_cards(&state, PlayerId(0), &[titan, ballista]);
        let titan_score = scored
            .iter()
            .find(|(id, _)| *id == titan)
            .map(|(_, s)| *s)
            .expect("titan score");
        let ballista_score = scored
            .iter()
            .find(|(id, _)| *id == ballista)
            .map(|(_, s)| *s)
            .expect("ballista score");

        assert!(
            ballista_score > titan_score,
            "expected combo-piece bonus to outscore generic threat; ballista={ballista_score}, titan={titan_score}"
        );
    }
}
