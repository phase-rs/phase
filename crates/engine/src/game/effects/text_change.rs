//! CR 612.1: Interactive text-changing effect (`Effect::ChangeTextWords`).
//!
//! At resolution the controller of the effect chooses one concrete
//! `(category, from, to)` substitution to install as a Layer-3 text-changing
//! continuous effect on the targeted object. The engine pre-computes every legal
//! option so the choice is a single index (`GameAction::ChooseTextWordReplacement`).

use std::collections::BTreeSet;

use crate::game::text_substitution::collect_present_words;
use crate::types::ability::{
    BasicLandType, Effect, EffectError, EffectKind, ResolvedAbility, TargetRef, TextWord,
    TextWordCategory,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, TextWordReplacementOption, WaitingFor};
use crate::types::mana::ManaColor;

/// CR 612.1: Resolve `Effect::ChangeTextWords`. Enumerates every legal
/// `(category, from, to)` option for the target (CR 612.2) and pauses on
/// `WaitingFor::TextWordReplacement` for the controller to pick one. If the
/// target is gone (CR 608.2b) or no legal substitution exists (CR 609.3), the
/// effect does nothing.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ChangeTextWords {
        allowed_categories,
        excluded_to,
        duration,
        ..
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "expected ChangeTextWords effect".to_string(),
        ));
    };

    // CR 608.2b: the effect needs a legal object target still in its zone.
    let target = ability.targets.iter().find_map(|t| match t {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    });
    let Some(target) = target.filter(|id| state.objects.contains_key(id)) else {
        events.push(resolved_event(ability));
        return Ok(());
    };

    let options = build_options(state, target, allowed_categories, excluded_to);

    // CR 609.3: no legal substitution exists — do as much as possible (nothing).
    if options.is_empty() {
        events.push(resolved_event(ability));
        return Ok(());
    }

    state.waiting_for = WaitingFor::TextWordReplacement {
        player: ability.controller,
        source: ability.source_id,
        target,
        options,
        duration: duration.clone(),
    };
    events.push(resolved_event(ability));
    Ok(())
}

fn resolved_event(ability: &ResolvedAbility) -> GameEvent {
    GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    }
}

/// CR 612.1 + CR 612.2: Build every legal `(category, from, to)` option: `from`
/// ranges over the category words actually present on the target; `to` ranges
/// over that category's full word set minus `excluded_to`, and `to != from`.
fn build_options(
    state: &GameState,
    target: crate::types::identifiers::ObjectId,
    allowed_categories: &[TextWordCategory],
    excluded_to: &[TextWord],
) -> Vec<TextWordReplacementOption> {
    let Some(target_obj) = state.objects.get(&target) else {
        return Vec::new();
    };

    let mut options = Vec::new();
    for &category in allowed_categories {
        let mut present = collect_present_words(target_obj, category);
        // CR 205.3m + CR 612.2: a creature-type `from` must be a real creature
        // type; the walker over-reports non-basic subtypes, so intersect with the
        // live creature-type set.
        if category == TextWordCategory::CreatureType {
            let creature_types: BTreeSet<&String> = state.all_creature_types.iter().collect();
            present.retain(|w| match w {
                TextWord::CreatureType(name) => creature_types.contains(name),
                _ => true,
            });
        }
        let to_words = category_all_words(state, category);
        for from in &present {
            for to in &to_words {
                if to == from || excluded_to.contains(to) {
                    continue;
                }
                options.push(TextWordReplacementOption {
                    category,
                    label: format!("{} → {}", from.label(), to.label()),
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
    }
    options
}

/// CR 612.2: The full set of words of a category, from which `to` is chosen.
fn category_all_words(state: &GameState, category: TextWordCategory) -> Vec<TextWord> {
    match category {
        TextWordCategory::ColorWord => ManaColor::ALL.iter().map(|c| TextWord::Color(*c)).collect(),
        TextWordCategory::BasicLandType => BasicLandType::all()
            .iter()
            .map(|lt| TextWord::BasicLandType(*lt))
            .collect(),
        TextWordCategory::CreatureType => state
            .all_creature_types
            .iter()
            .map(|s| TextWord::CreatureType(s.clone()))
            .collect(),
    }
}
