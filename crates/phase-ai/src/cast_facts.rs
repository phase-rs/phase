use engine::ai_support::copy_target_filter;
use engine::game::game_object::GameObject;
#[cfg(test)]
use engine::types::ability::TapStateChange;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, BounceSelection, Effect, EffectScope, ReplacementDefinition,
    TargetFilter, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
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

    /// Build an EffectProfile from every effect a card face can produce — its
    /// abilities (spell and activated alike) and the `execute` chains of all its
    /// triggered abilities. Unlike [`cast_facts_for_object`], which keeps only
    /// immediate-ETB trigger effects, this includes attack/dies/upkeep/etc. triggers
    /// too, since draft-pick evaluation cares about a card's whole effect surface.
    pub fn from_face(face: &CardFace) -> Self {
        Self::from_effects(&collect_face_effects(face))
    }
}

/// Collect every effect reachable from a card face's abilities and triggered
/// abilities (recursing into modal/sub/else branches via [`collect_definition_effects`]).
/// Replacement effects are intentionally excluded — most face-level replacements are
/// ETB-tapped / enters-with-counters plumbing that the [`EffectProfile`] flags would
/// not classify anyway.
pub fn collect_face_effects(face: &CardFace) -> Vec<&Effect> {
    face.abilities
        .iter()
        .flat_map(collect_definition_effects)
        .chain(
            face.triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref())
                .flat_map(collect_definition_effects),
        )
        .collect()
}

/// How a cast candidate pays for itself.
///
/// A cast policy that prices a candidate off `CastFacts::mana_value` is reading
/// the PRINTED cost (CR 202.1). That is only what the player actually pays in
/// the `Printed` mode — the other two modes replace the mana cost outright
/// (CR 118.9), so any affordability or sequencing judgement built on the
/// printed mana value is unsound for them and must consult this first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CastCostMode {
    /// CR 601.2f: the card's printed mana cost, as modified by cost increases
    /// and reductions.
    Printed,
    /// CR 118.9: an alternative cost is paid instead of the mana cost —
    /// CR 702.94a miracle, CR 702.35a madness, CR 702.190a sneak,
    /// CR 702.188a web-slinging.
    ///
    /// The carried cost is the keyword's own cost as printed on the object; it
    /// deliberately does NOT go through `resolve_keyword_mana_cost`, so a
    /// self-referential form (`ManaCost::SelfManaCost` and friends) is passed
    /// through unresolved, and a granted-keyword cast whose keyword is not on
    /// the object falls back to `ManaCost::SelfManaCost`. Consumers today only
    /// discriminate `Free` from the rest, so the exact shard list is
    /// informational; resolve it at the call site before pricing against it.
    Alternative(ManaCost),
    /// CR 118.9 + CR 107.3b: cast "without paying its mana cost" — no mana is
    /// paid at all, and the only legal choice for an undefined X is 0.
    Free,
}

/// Card-level facts for spells: wraps EffectProfile with card-specific data
/// (mana value, ETB triggers, replacements). Available for every member of the
/// cast family (see [`is_cast_family_action`]).
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
    /// The copy-source filter of an enter-as-copy replacement (Clone /
    /// Phantasmal Image class), when this candidate carries one.
    ///
    /// A copy source is CHOSEN while the permanent enters, not targeted, so
    /// this is deliberately kept out of `requires_targets_in_immediate_etb`:
    /// that flag's consumers answer it with `find_legal_targets`, which is the
    /// wrong authority here (it enumerates legal *targets* on the battlefield,
    /// while a copy source may live in a graveyard or exile and ignores
    /// hexproof/shroud). Resolve this filter with
    /// `engine::ai_support::find_copy_targets`, the same enumeration the
    /// replacement pipeline uses.
    pub requires_copy_source_on_entry: Option<&'a TargetFilter>,
    /// CR 118.9: which cost this candidate actually pays. Derived from the
    /// candidate ACTION, so [`cast_facts_for_object`] (which has no action)
    /// reports [`CastCostMode::Printed`]; [`cast_facts_for_action`] overrides
    /// it for the alternative and free members of the cast family.
    pub cost_mode: CastCostMode,
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

/// CR 601.2 + CR 118.9: is this action a *cast* — the plain announcement, or one
/// of the dedicated alternative/free-cast action variants?
///
/// Single membership authority for the cast family, shared by `cast_facts`,
/// `decision_kind` routing and the cast policies. Non-announcement cast-shaped
/// siblings are deliberately excluded: `Foretell` and `PlayFaceDown` put a card
/// into a zone rather than onto the stack, `ActivateNinjutsu` is an activated
/// ability, and `CastPreparedCopy`/`CastParadigmCopy` create stack copies whose
/// characteristics do not come from a hand object.
pub(crate) fn is_cast_family_action(action: &GameAction) -> bool {
    matches!(
        action,
        GameAction::CastSpell { .. }
            | GameAction::CastSpellForFree { .. }
            | GameAction::CastSpellAsMiracle { .. }
            | GameAction::CastSpellAsMadness { .. }
            | GameAction::CastSpellAsSneak { .. }
            | GameAction::CastSpellAsWebSlinging { .. }
    )
}

pub fn cast_object_for_action<'a>(
    state: &'a GameState,
    action: &GameAction,
    player: PlayerId,
) -> Option<&'a GameObject> {
    if !is_cast_family_action(action) {
        return None;
    }
    // `GameAction::source_object()` is the engine's exhaustive action→object
    // map (a new action variant is a compile error there), so the whole family
    // resolves without re-enumerating which field each variant carries the
    // object in.
    let object = action
        .source_object()
        .and_then(|object_id| state.objects.get(&object_id));
    match action {
        // CR 601.2a: only the plain announcement carries the card identity
        // alongside the object id. Keep the cross-check, and the hand fallback
        // for a candidate whose object id no longer resolves.
        GameAction::CastSpell { card_id, .. } => object
            .filter(|object| object.card_id == *card_id)
            .or_else(|| {
                state.players[player.0 as usize]
                    .hand
                    .iter()
                    .filter_map(|object_id| state.objects.get(object_id))
                    .find(|object| object.card_id == *card_id)
            }),
        _ => object,
    }
}

pub fn cast_facts_for_action<'a>(
    state: &'a GameState,
    action: &GameAction,
    player: PlayerId,
) -> Option<CastFacts<'a>> {
    let object = cast_object_for_action(state, action, player)?;
    Some(CastFacts {
        cost_mode: cast_cost_mode(action, object),
        ..cast_facts_for_object(object)
    })
}

/// CR 118.9: which cost the candidate action pays, read off the action variant
/// and the object's own keyword.
fn cast_cost_mode(action: &GameAction, object: &GameObject) -> CastCostMode {
    // CR 118.9 + CR 107.3b: "without paying its mana cost" — nothing is paid.
    if matches!(action, GameAction::CastSpellForFree { .. }) {
        return CastCostMode::Free;
    }
    if !matches!(
        action,
        GameAction::CastSpellAsMiracle { .. }
            | GameAction::CastSpellAsMadness { .. }
            | GameAction::CastSpellAsSneak { .. }
            | GameAction::CastSpellAsWebSlinging { .. }
    ) {
        return CastCostMode::Printed;
    }
    let cost = object
        .keywords
        .iter()
        .find_map(|keyword| match (action, keyword) {
            // CR 702.94a / CR 702.35a / CR 702.190a / CR 702.188a: each of these
            // keywords carries the alternative cost paid instead of the mana cost.
            (GameAction::CastSpellAsMiracle { .. }, Keyword::Miracle(cost))
            | (GameAction::CastSpellAsMadness { .. }, Keyword::Madness(cost))
            | (GameAction::CastSpellAsSneak { .. }, Keyword::Sneak(cost))
            | (GameAction::CastSpellAsWebSlinging { .. }, Keyword::WebSlinging(cost)) => {
                Some(cost.clone())
            }
            _ => None,
        })
        .unwrap_or(ManaCost::SelfManaCost);
    CastCostMode::Alternative(cost)
}

/// Resolve the exact activated-ability definition represented by an action.
///
/// Production appends runtime-granted abilities after printed abilities, so
/// indexing `GameObject::abilities` directly is not authoritative for an
/// `ActivateAbility` candidate. Reuse the engine's enumerated index space.
pub fn effective_activated_ability(
    state: &GameState,
    action: &GameAction,
) -> Option<AbilityDefinition> {
    let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = action
    else {
        return None;
    };
    engine::game::casting::activated_ability_definitions(state, *source_id)
        .into_iter()
        .find_map(|(index, ability)| (index == *ability_index).then_some(ability))
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
        GameAction::ActivateAbility { .. } => {
            let ability = effective_activated_ability(state, action)?;
            let effects: Vec<_> = collect_definition_effects(&ability);
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
        .map(|entry| &entry.definition)
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

    // Enter-as-copy replacements (Clone / Phantasmal Image class). Read the
    // filter off the replacement's own execute chain with the engine's copy
    // accessor so the AI and the replacement pipeline agree on what a copy
    // source is; the presence check itself belongs to the consuming policy.
    let requires_copy_source_on_entry = immediate_replacements
        .iter()
        .filter_map(|replacement| replacement.execute.as_deref())
        .find_map(copy_target_filter);

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
        requires_copy_source_on_entry,
        // No action in hand here — `cast_facts_for_action` refines this.
        cost_mode: CastCostMode::Printed,
    }
}

/// CR 700.2a: whether an ability walk descends into `mode_abilities`, the
/// branches a modal spell or ability has NOT chosen yet.
///
/// `All` is the cast-commit reading (CR 601.2b — at announcement no mode is
/// chosen, so a cast candidate is priced against everything the card can do).
/// `RootOnly` is the activation-step reading: `WaitingFor::AbilityModeChoice`
/// is a separate decision that scores the chosen modes on their own, so reading
/// every printed mode as a conjunction at the activation step charges an
/// Umezawa's Jitte activation with a combat trick and a no-target whiff even
/// when the intended mode is "gain 2 life".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeWalk {
    All,
    RootOnly,
}

pub(crate) fn collect_definition_effects(ability: &AbilityDefinition) -> Vec<&Effect> {
    collect_definition_effects_with(ability, ModeWalk::All)
}

/// [`collect_definition_effects`] with the unchosen-mode branch under caller
/// control. One walker, two readings — never a second traversal to drift.
pub(crate) fn collect_definition_effects_with(
    ability: &AbilityDefinition,
    modes: ModeWalk,
) -> Vec<&Effect> {
    let mut effects = Vec::new();
    push_ability_effects(&mut effects, ability, modes);
    effects
}

fn push_ability_effects<'a>(
    effects: &mut Vec<&'a Effect>,
    ability: &'a AbilityDefinition,
    modes: ModeWalk,
) {
    effects.push(&ability.effect);
    if let Some(sub_ability) = &ability.sub_ability {
        push_ability_effects(effects, sub_ability, modes);
    }
    if let Some(else_ability) = &ability.else_ability {
        push_ability_effects(effects, else_ability, modes);
    }
    if modes == ModeWalk::All {
        for mode_ability in &ability.mode_abilities {
            push_ability_effects(effects, mode_ability, modes);
        }
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
        // CR 115.1 + Whitemane Lion ruling: A non-targeted Bounce ("return a
        // creature you control") does NOT use the word "target" — the
        // controller chooses at resolution time via `EffectZoneChoice`, so
        // the spell does not need a target slot to be cast. Targeted Bounce
        // (with "target") follows the standard target-required predicate.
        Effect::Bounce {
            target, selection, ..
        } => {
            matches!(selection, BounceSelection::Targeted) && !matches!(target, TargetFilter::None)
        }
        Effect::Destroy { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Counter { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::PhaseOut { target }
        | Effect::Fight { target, .. }
        | Effect::Goad { target }
        | Effect::ChangeZone { target, .. }
        | Effect::Connive { target, .. }
        | Effect::ForceBlock { target, .. }
        | Effect::Exploit { target, .. }
        | Effect::Attach { target, .. }
        | Effect::GivePlayerCounter { target, .. }
        | Effect::BecomeCopy { target, .. }
        | Effect::ExtraTurn { target, .. }
        | Effect::SkipNextStep { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::RemoveAllDamage { target, .. }
        | Effect::DoublePT { target, .. }
        | Effect::PreventDamage { target, .. }
        | Effect::Animate { target, .. }
        // CR 113.1a + CR 611.2: the donor whose activated abilities are gained
        // (Quicksilver Elemental) is a real declared target.
        | Effect::GainActivatedAbilitiesOfTarget { target, .. }
        | Effect::PutCounter { target, .. } => !matches!(target, TargetFilter::None),
        Effect::RevealHand { target, .. } => !matches!(target, TargetFilter::None),
        // CR 701.26a/b: only single-permanent tap/untap declares a target. The
        // mass (`All`) scope falls through to `false`, matching the legacy
        // `TapAll`/`UntapAll`.
        Effect::SetTapState {
            scope: EffectScope::Single,
            target,
            ..
        } => !matches!(target, TargetFilter::None),
        // CR 701.60a: only single-permanent suspect/unsuspect declares a target.
        // The mass (`All`) scope (e.g. Absolving Lammasu, "all suspected
        // creatures are no longer suspected") is a non-targeting population
        // effect — its filter is not a selectable target, so it falls through to
        // `false` (mirrors `SetTapState`'s `Single`/`All` split).
        Effect::Suspect {
            scope: EffectScope::Single,
            target,
            ..
        }
        | Effect::Unsuspect {
            scope: EffectScope::Single,
            target,
            ..
        } => !matches!(target, TargetFilter::None),
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
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, TargetFilter, TypedFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::{CardId, ObjectId};

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
    fn effective_activation_uses_runtime_granted_index_space() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Granted Equipment".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .keywords
            .push(Keyword::Equip(ManaCost::generic(2)));
        let action = GameAction::ActivateAbility {
            source_id,
            ability_index: 0,
        };

        let ability = effective_activated_ability(&state, &action)
            .expect("runtime-granted equip ability must use index zero");
        assert_eq!(ability.kind, AbilityKind::Activated);
        assert!(matches!(ability.effect.as_ref(), Effect::Attach { .. }));
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
        object.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::ChangeZone)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                        state: TapStateChange::Tap,
                    },
                ))
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield),
        );
        object.replacement_definitions.push(ReplacementDefinition {
            destination_zone: None,
            ..object.replacement_definitions[0].clone()
        });

        let facts = cast_facts_for_object(&object);
        assert_eq!(facts.immediate_replacements.len(), 1);
    }

    /// Clone / Phantasmal Image shape: one `Moved`-to-battlefield replacement
    /// whose execute is `BecomeCopy`.
    fn enter_as_copy_replacement(target: TargetFilter) -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::BecomeCopy {
                    target,
                    recipient: TargetFilter::SelfRef,
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: Vec::new(),
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination_zone(Zone::Battlefield)
    }

    #[test]
    fn enter_as_copy_replacement_sets_copy_source_fact() {
        let mut object = make_object();
        object
            .replacement_definitions
            .push(enter_as_copy_replacement(TargetFilter::Typed(
                TypedFilter::creature(),
            )));

        let facts = cast_facts_for_object(&object);

        assert_eq!(
            facts.requires_copy_source_on_entry,
            Some(&TargetFilter::Typed(TypedFilter::creature()))
        );
        // The copy source is chosen as the permanent enters, not targeted — the
        // target-driven ETB gate must not claim it.
        assert!(!facts.requires_targets_in_immediate_etb);
    }

    #[test]
    fn non_copy_replacement_does_not() {
        let mut object = make_object();
        object.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: EffectScope::Single,
                        state: TapStateChange::Tap,
                    },
                ))
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield),
        );

        let facts = cast_facts_for_object(&object);

        assert_eq!(facts.immediate_replacements.len(), 1);
        assert!(facts.requires_copy_source_on_entry.is_none());
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
                excess: None,
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

    // CR 701.60a: mass un-designation ("all suspected creatures are no longer
    // suspected", Absolving Lammasu) is a non-targeting population effect, so it
    // must NOT be scored as target-requiring. Only `EffectScope::Single`
    // (targeted/anaphoric "suspect target creature" / "it's no longer
    // suspected") declares a target, mirroring the engine's `target_filter()`
    // and the `SetTapState` `Single`/`All` split.
    #[test]
    fn mass_unsuspect_is_not_target_requiring() {
        // The non-None filter is identical across scopes (the mass clause still
        // carries its population filter); only `scope` distinguishes them, so a
        // pass proves the scope gate — not the filter — drives the decision.
        let single = Effect::Unsuspect {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
        };
        let mass = Effect::Unsuspect {
            target: TargetFilter::Any,
            scope: EffectScope::All,
        };
        assert!(
            effect_requires_targets(&single),
            "single-scope Unsuspect must be target-requiring"
        );
        assert!(
            !effect_requires_targets(&mass),
            "mass Unsuspect{{All}} (Absolving Lammasu) must not be target-requiring"
        );
    }

    #[test]
    fn mass_suspect_is_not_target_requiring() {
        let single = Effect::Suspect {
            target: TargetFilter::Any,
            scope: EffectScope::Single,
        };
        let mass = Effect::Suspect {
            target: TargetFilter::Any,
            scope: EffectScope::All,
        };
        assert!(
            effect_requires_targets(&single),
            "single-scope Suspect must be target-requiring"
        );
        assert!(
            !effect_requires_targets(&mass),
            "mass Suspect{{All}} must not be target-requiring"
        );
    }

    /// CR 601.2 + CR 118.9: every member of the cast family resolves to the card
    /// being cast, and reports the cost it actually pays. Before the cast-family
    /// widening only `CastSpell` resolved and the other five returned `None`, so
    /// every cast policy scored them as if they were not casts at all.
    #[test]
    fn cast_facts_resolve_for_every_cast_family_variant() {
        let mut state = GameState::new_two_player(42);
        let oid = create_object(
            &mut state,
            CardId(4242),
            PlayerId(0),
            "Family Caster".to_string(),
            Zone::Hand,
        );
        let card_id = CardId(4242);
        {
            let object = state.objects.get_mut(&oid).unwrap();
            object.card_types.core_types.push(CoreType::Creature);
            object.mana_cost = ManaCost::generic(5);
            object.keywords.push(Keyword::Miracle(ManaCost::generic(1)));
            object.keywords.push(Keyword::Madness(ManaCost::generic(2)));
            object.keywords.push(Keyword::Sneak(ManaCost::generic(3)));
            object
                .keywords
                .push(Keyword::WebSlinging(ManaCost::generic(4)));
        }
        let returned = ObjectId(999);

        let cases: &[(GameAction, CastCostMode)] = &[
            (
                GameAction::CastSpell {
                    object_id: oid,
                    card_id,
                    targets: Vec::new(),
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                },
                CastCostMode::Printed,
            ),
            (
                GameAction::CastSpellForFree {
                    object_id: oid,
                    card_id,
                    source_id: ObjectId(1),
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                },
                CastCostMode::Free,
            ),
            (
                GameAction::CastSpellAsMiracle {
                    object_id: oid,
                    card_id,
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                },
                CastCostMode::Alternative(ManaCost::generic(1)),
            ),
            (
                GameAction::CastSpellAsMadness {
                    object_id: oid,
                    card_id,
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                },
                CastCostMode::Alternative(ManaCost::generic(2)),
            ),
            (
                GameAction::CastSpellAsSneak {
                    hand_object: oid,
                    card_id,
                    creature_to_return: returned,
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                },
                CastCostMode::Alternative(ManaCost::generic(3)),
            ),
            (
                GameAction::CastSpellAsWebSlinging {
                    hand_object: oid,
                    card_id,
                    creature_to_return: returned,
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                },
                CastCostMode::Alternative(ManaCost::generic(4)),
            ),
        ];

        for (action, expected_mode) in cases {
            assert!(
                is_cast_family_action(action),
                "{action:?} must be in the cast family"
            );
            let facts = cast_facts_for_action(&state, action, PlayerId(0))
                .unwrap_or_else(|| panic!("{action:?} must resolve cast facts"));
            assert_eq!(facts.object.id, oid, "{action:?} resolved the wrong object");
            // The printed mana value stays the card's own (CR 202.3); the cost
            // mode is what tells a policy whether that number is being paid.
            assert_eq!(facts.mana_value, 5, "{action:?}");
            assert_eq!(&facts.cost_mode, expected_mode, "{action:?}");
        }
    }

    /// `GameAction::source_object()` answers for activations too, so the family
    /// membership gate — not the object lookup — is what keeps an activated
    /// ability out of the cast-facts population.
    #[test]
    fn activate_ability_yields_no_cast_facts() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Activatable".to_string(),
            Zone::Battlefield,
        );
        let action = GameAction::ActivateAbility {
            source_id,
            ability_index: 0,
        };
        assert_eq!(action.source_object(), Some(source_id));
        assert!(!is_cast_family_action(&action));
        assert!(cast_object_for_action(&state, &action, PlayerId(0)).is_none());
        assert!(cast_facts_for_action(&state, &action, PlayerId(0)).is_none());
    }
}
