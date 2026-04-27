use engine::game::game_object::GameObject;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

/// Effect-level classification flags shared across spells and activated abilities.
/// Built from any ability's effect chain — no card-level assumptions.
#[derive(Debug, Clone, Default)]
pub struct EffectProfile {
    pub has_search_library: bool,
    pub has_reveal_hand_or_discard: bool,
    pub has_draw: bool,
    pub has_token_creation: bool,
    pub has_counter_spell: bool,
    pub has_direct_removal_text: bool,
    pub has_mass_damage_or_mass_shrink_text: bool,
}

impl EffectProfile {
    /// Build an EffectProfile by scanning a flat list of effects.
    pub fn from_effects(effects: &[&Effect]) -> Self {
        Self {
            has_search_library: effects
                .iter()
                .any(|e| matches!(e, Effect::SearchLibrary { .. })),
            has_reveal_hand_or_discard: effects
                .iter()
                .any(|e| matches!(e, Effect::RevealHand { .. } | Effect::DiscardCard { .. })),
            has_draw: effects.iter().any(|e| matches!(e, Effect::Draw { .. })),
            has_token_creation: effects.iter().any(|e| matches!(e, Effect::Token { .. })),
            has_counter_spell: effects.iter().any(|e| matches!(e, Effect::Counter { .. })),
            has_direct_removal_text: effects.iter().any(|e| is_direct_removal(e)),
            has_mass_damage_or_mass_shrink_text: effects
                .iter()
                .any(|e| is_mass_damage_or_shrink(e)),
        }
    }
}

/// Card-level facts for spells: wraps EffectProfile with card-specific data
/// (mana value, ETB triggers, replacements). Only available for CastSpell actions.
#[derive(Debug, Clone)]
pub struct CastFacts<'a> {
    pub object: &'a GameObject,
    pub primary_effects: Vec<&'a AbilityDefinition>,
    pub immediate_etb_triggers: Vec<&'a TriggerDefinition>,
    pub immediate_replacements: Vec<&'a ReplacementDefinition>,
    pub mana_value: u32,
    pub profile: EffectProfile,
    pub requires_targets_in_spell_text: bool,
    pub requires_targets_in_immediate_etb: bool,
}

impl<'a> CastFacts<'a> {
    // Delegate EffectProfile fields for backward compatibility with existing call sites.
    pub fn has_search_library(&self) -> bool {
        self.profile.has_search_library
    }
    pub fn has_reveal_hand_or_discard(&self) -> bool {
        self.profile.has_reveal_hand_or_discard
    }
    pub fn has_draw(&self) -> bool {
        self.profile.has_draw
    }
    pub fn has_token_creation(&self) -> bool {
        self.profile.has_token_creation
    }
    pub fn has_counter_spell(&self) -> bool {
        self.profile.has_counter_spell
    }
    pub fn has_direct_removal_text(&self) -> bool {
        self.profile.has_direct_removal_text
    }
    pub fn has_mass_damage_or_mass_shrink_text(&self) -> bool {
        self.profile.has_mass_damage_or_mass_shrink_text
    }

    pub fn immediate_effects(&self) -> Vec<&'a Effect> {
        let mut effects = Vec::new();
        for ability in collect_unique_immediate_abilities_from_parts(
            &self.primary_effects,
            &self.immediate_etb_triggers,
            &self.immediate_replacements,
        ) {
            effects.extend(collect_definition_effects(ability));
        }
        effects
    }

    pub fn is_creature(&self) -> bool {
        self.object
            .card_types
            .core_types
            .contains(&CoreType::Creature)
    }

    pub fn is_planeswalker(&self) -> bool {
        self.object
            .card_types
            .core_types
            .contains(&CoreType::Planeswalker)
    }

    pub fn is_enchantment(&self) -> bool {
        self.object
            .card_types
            .core_types
            .contains(&CoreType::Enchantment)
    }
}

pub fn cast_object_for_action<'a>(
    state: &'a GameState,
    action: &GameAction,
    player: PlayerId,
) -> Option<&'a GameObject> {
    match action {
        GameAction::CastSpell {
            object_id, card_id, ..
        } => state
            .objects
            .get(object_id)
            .filter(|object| object.card_id == *card_id)
            .or_else(|| {
                state.players[player.0 as usize]
                    .hand
                    .iter()
                    .filter_map(|object_id| state.objects.get(object_id))
                    .find(|object| object.card_id == *card_id)
            }),
        _ => None,
    }
}

pub fn cast_facts_for_action<'a>(
    state: &'a GameState,
    action: &GameAction,
    player: PlayerId,
) -> Option<CastFacts<'a>> {
    cast_object_for_action(state, action, player).map(cast_facts_for_object)
}

/// Build an EffectProfile for any action — spells, activated abilities, or target
/// selection contexts. For spells, this delegates to CastFacts (which includes ETB
/// triggers and replacements). For activated abilities, it scans the specific
/// ability's effect chain directly.
pub fn effect_profile_for_action(
    state: &GameState,
    action: &GameAction,
    player: PlayerId,
) -> Option<EffectProfile> {
    match action {
        GameAction::CastSpell { .. } => {
            cast_facts_for_action(state, action, player).map(|facts| facts.profile)
        }
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => {
            let object = state.objects.get(source_id)?;
            let ability = object.abilities.get(*ability_index)?;
            let effects: Vec<_> = collect_definition_effects(ability);
            Some(EffectProfile::from_effects(&effects))
        }
        _ => None,
    }
}

pub fn cast_facts_for_object(object: &GameObject) -> CastFacts<'_> {
    let primary_effects: Vec<_> = object
        .abilities
        .iter()
        .filter(|ability| ability.kind == AbilityKind::Spell)
        .collect();
    let immediate_etb_triggers: Vec<_> = object
        .trigger_definitions
        .iter_unchecked()
        .filter(|trigger| qualifies_immediate_etb(object, trigger))
        .collect();
    let immediate_replacements: Vec<_> = object
        .replacement_definitions
        .iter_unchecked()
        .filter(|replacement| qualifies_immediate_replacement(replacement))
        .collect();

    let all_effects: Vec<_> = collect_unique_immediate_abilities_from_parts(
        &primary_effects,
        &immediate_etb_triggers,
        &immediate_replacements,
    )
    .into_iter()
    .flat_map(collect_definition_effects)
    .collect();

    let requires_targets_in_spell_text = primary_effects.iter().any(|ability| {
        collect_definition_effects(ability)
            .into_iter()
            .any(effect_requires_targets)
    });
    let requires_targets_in_immediate_etb = immediate_etb_triggers.iter().any(|trigger| {
        trigger.execute.as_ref().is_some_and(|ability| {
            collect_definition_effects(ability)
                .into_iter()
                .any(effect_requires_targets)
        })
    });

    let profile = EffectProfile::from_effects(&all_effects);

    CastFacts {
        object,
        primary_effects,
        immediate_etb_triggers,
        immediate_replacements,
        mana_value: object.mana_cost.mana_value(),
        profile,
        requires_targets_in_spell_text,
        requires_targets_in_immediate_etb,
    }
}

pub(crate) fn collect_definition_effects(ability: &AbilityDefinition) -> Vec<&Effect> {
    let mut effects = Vec::new();
    push_ability_effects(&mut effects, ability);
    effects
}

fn push_ability_effects<'a>(effects: &mut Vec<&'a Effect>, ability: &'a AbilityDefinition) {
    effects.push(&ability.effect);
    if let Some(sub_ability) = &ability.sub_ability {
        push_ability_effects(effects, sub_ability);
    }
    if let Some(else_ability) = &ability.else_ability {
        push_ability_effects(effects, else_ability);
    }
    for mode_ability in &ability.mode_abilities {
        push_ability_effects(effects, mode_ability);
    }
}

fn collect_unique_immediate_abilities_from_parts<'a>(
    primary_effects: &[&'a AbilityDefinition],
    immediate_etb_triggers: &[&'a TriggerDefinition],
    immediate_replacements: &[&'a ReplacementDefinition],
) -> Vec<&'a AbilityDefinition> {
    let mut abilities = Vec::new();
    push_unique_abilities(&mut abilities, primary_effects.iter().copied());
    push_unique_abilities(
        &mut abilities,
        immediate_etb_triggers
            .iter()
            .filter_map(|trigger| trigger.execute.as_deref()),
    );
    push_unique_abilities(
        &mut abilities,
        immediate_replacements
            .iter()
            .filter_map(|replacement| replacement.execute.as_deref()),
    );
    abilities
}

fn push_unique_abilities<'a>(
    target: &mut Vec<&'a AbilityDefinition>,
    abilities: impl IntoIterator<Item = &'a AbilityDefinition>,
) {
    for ability in abilities {
        if !target.iter().any(|existing| **existing == *ability) {
            target.push(ability);
        }
    }
}

fn qualifies_immediate_etb(object: &GameObject, trigger: &TriggerDefinition) -> bool {
    is_permanent_spell(object)
        && trigger.mode == TriggerMode::ChangesZone
        && trigger.valid_card == Some(TargetFilter::SelfRef)
        && trigger.destination == Some(Zone::Battlefield)
        && trigger.execute.is_some()
}

fn qualifies_immediate_replacement(replacement: &ReplacementDefinition) -> bool {
    matches!(
        replacement.event,
        ReplacementEvent::ChangeZone | ReplacementEvent::Moved
    ) && replacement.valid_card == Some(TargetFilter::SelfRef)
        && replacement.destination_zone == Some(Zone::Battlefield)
}

fn is_permanent_spell(object: &GameObject) -> bool {
    object.card_types.core_types.iter().any(|core_type| {
        matches!(
            core_type,
            CoreType::Artifact
                | CoreType::Battle
                | CoreType::Creature
                | CoreType::Enchantment
                | CoreType::Land
                | CoreType::Planeswalker
        )
    })
}

fn effect_requires_targets(effect: &Effect) -> bool {
    match effect {
        Effect::Destroy { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Tap { target }
        | Effect::Untap { target }
        | Effect::Bounce { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::PhaseOut { target }
        | Effect::Fight { target, .. }
        | Effect::Goad { target }
        | Effect::ChangeZone { target, .. }
        | Effect::Connive { target, .. }
        | Effect::Suspect { target, .. }
        | Effect::ForceBlock { target, .. }
        | Effect::Exploit { target, .. }
        | Effect::Attach { target, .. }
        | Effect::GivePlayerCounter { target, .. }
        | Effect::BecomeCopy { target, .. }
        | Effect::ExtraTurn { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::DoublePT { target, .. }
        | Effect::PreventDamage { target, .. }
        | Effect::Animate { target, .. }
        | Effect::AddCounter { target, .. } => !matches!(target, TargetFilter::None),
        Effect::RevealHand { target, .. } => !matches!(target, TargetFilter::None),
        _ => false,
    }
}

pub(crate) fn is_direct_removal(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Destroy { .. }
            | Effect::DealDamage { .. }
            | Effect::Bounce { .. }
            | Effect::Counter { .. }
            | Effect::Fight { .. }
            | Effect::DestroyAll { .. }
            | Effect::DamageAll { .. }
            | Effect::DiscardCard { .. }
    ) || matches!(
        effect,
        Effect::ChangeZone {
            destination: Zone::Exile | Zone::Graveyard,
            ..
        }
    )
}

pub(crate) fn is_mass_damage_or_shrink(effect: &Effect) -> bool {
    matches!(effect, Effect::DestroyAll { .. } | Effect::DamageAll { .. })
        || matches!(
            effect,
            Effect::Pump {
                power: engine::types::ability::PtValue::Fixed(power),
                toughness: engine::types::ability::PtValue::Fixed(toughness),
                target: TargetFilter::Any,
            } if *power < 0 || *toughness < 0
        )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use engine::game::game_object::GameObject;
    use engine::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;

    fn make_object() -> GameObject {
        let mut object = GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Hand,
        );
        object.card_types.core_types.push(CoreType::Creature);
        object.mana_cost = ManaCost::Cost {
            shards: Vec::new(),
            generic: 4,
        };
        object
    }

    #[test]
    fn includes_only_qualifying_etb_triggers() {
        let mut object = make_object();
        object.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                    },
                )),
        );
        object
            .trigger_definitions
            .push(
                TriggerDefinition::new(TriggerMode::Phase).execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: engine::types::ability::TargetFilter::Controller,
                    },
                )),
            );

        let facts = cast_facts_for_object(&object);
        assert_eq!(facts.immediate_etb_triggers.len(), 1);
        assert!(facts.has_draw());
    }

    #[test]
    fn includes_only_qualifying_replacements() {
        let mut object = make_object();
        object.replacement_definitions.push(ReplacementDefinition {
            event: ReplacementEvent::ChangeZone,
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Tap {
                    target: TargetFilter::SelfRef,
                },
            ))),
            mode: engine::types::ability::ReplacementMode::Mandatory,
            valid_card: Some(TargetFilter::SelfRef),
            description: None,
            condition: None,
            destination_zone: Some(Zone::Battlefield),
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: Default::default(),
            quantity_modification: None,
            token_owner_scope: None,
            valid_player: None,
            is_consumed: false,
            expires_at_eot: false,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        });
        object.replacement_definitions.push(ReplacementDefinition {
            destination_zone: None,
            ..object.replacement_definitions[0].clone()
        });

        let facts = cast_facts_for_object(&object);
        assert_eq!(facts.immediate_replacements.len(), 1);
    }

    #[test]
    fn dedupes_structurally_identical_immediate_effects() {
        let mut object = make_object();
        let draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        Arc::make_mut(&mut object.abilities).push(draw.clone());
        let mut trigger_draw = draw.clone();
        trigger_draw.kind = AbilityKind::Spell;
        object.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(trigger_draw),
        );

        let facts = cast_facts_for_object(&object);
        let draw_count = facts
            .immediate_effects()
            .into_iter()
            .filter(|effect| matches!(effect, Effect::Draw { .. }))
            .count();
        assert_eq!(draw_count, 1);
    }

    #[test]
    fn excludes_non_spell_primary_abilities() {
        let mut object = make_object();
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        ));
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        ));

        let facts = cast_facts_for_object(&object);
        assert_eq!(facts.primary_effects.len(), 1);
        assert!(matches!(
            *facts.primary_effects[0].effect,
            Effect::DealDamage { .. }
        ));
    }

    #[test]
    fn preserves_structurally_distinct_immediate_branches() {
        let mut object = make_object();
        let draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        let mut draw_with_else = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        draw_with_else.else_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        )));
        Arc::make_mut(&mut object.abilities).push(draw);
        object.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(draw_with_else),
        );

        let facts = cast_facts_for_object(&object);
        let draw_count = facts
            .immediate_effects()
            .into_iter()
            .filter(|effect| matches!(effect, Effect::Draw { .. }))
            .count();
        assert_eq!(draw_count, 3);
    }
}
