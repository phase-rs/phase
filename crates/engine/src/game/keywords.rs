use std::str::FromStr;

use crate::game::combat::{AttackTarget, AttackerInfo};
use crate::game::game_object::GameObject;
use crate::game::players;
use crate::game::zones;
use crate::types::ability::{AbilityCost, NinjutsuVariant};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{FlashbackCost, Keyword, KeywordKind, ProtectionTarget};
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// Check if a game object has a specific keyword, using discriminant-based matching
/// for simple keywords (ignoring associated data for parameterized variants).
pub fn has_keyword(obj: &GameObject, keyword: &Keyword) -> bool {
    obj.keywords
        .iter()
        .any(|k| std::mem::discriminant(k) == std::mem::discriminant(keyword))
}

pub fn has_keyword_kind(obj: &GameObject, kind: KeywordKind) -> bool {
    obj.keywords.iter().any(|keyword| keyword.kind() == kind)
}

pub fn object_has_effective_keyword_kind(
    state: &GameState,
    object_id: ObjectId,
    kind: KeywordKind,
) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    if obj.zone == Zone::Battlefield {
        return obj.keywords.iter().any(|keyword| keyword.kind() == kind);
    }

    crate::game::off_zone_characteristics::off_zone_has_keyword_kind(state, object_id, kind)
}

pub fn effective_flashback_cost(state: &GameState, object_id: ObjectId) -> Option<FlashbackCost> {
    let keyword = effective_keyword_for_object(state, object_id, KeywordKind::Flashback)?;
    match keyword {
        Keyword::Flashback(cost) => match cost {
            FlashbackCost::Mana(mana_cost) => Some(FlashbackCost::Mana(resolve_keyword_mana_cost(
                state, object_id, &mana_cost,
            ))),
            FlashbackCost::NonMana(ability_cost) => Some(FlashbackCost::NonMana(ability_cost)),
        },
        _ => None,
    }
}

pub fn effective_escape_data(state: &GameState, object_id: ObjectId) -> Option<(ManaCost, u32)> {
    let keyword = effective_keyword_for_object(state, object_id, KeywordKind::Escape)?;
    match keyword {
        Keyword::Escape { cost, exile_count } => Some((
            resolve_keyword_mana_cost(state, object_id, &cost),
            exile_count,
        )),
        _ => None,
    }
}

fn effective_keyword_for_object(
    state: &GameState,
    object_id: ObjectId,
    kind: KeywordKind,
) -> Option<Keyword> {
    let obj = state.objects.get(&object_id)?;
    if obj.zone == Zone::Battlefield {
        return obj
            .keywords
            .iter()
            .find(|keyword| keyword.kind() == kind)
            .cloned();
    }

    crate::game::off_zone_characteristics::effective_off_zone_keyword(state, object_id, kind)
}

fn resolve_keyword_mana_cost(state: &GameState, object_id: ObjectId, cost: &ManaCost) -> ManaCost {
    match cost {
        ManaCost::SelfManaCost => state
            .objects
            .get(&object_id)
            .map(|obj| obj.mana_cost.clone())
            .unwrap_or(ManaCost::NoCost),
        _ => cost.clone(),
    }
}

/// Convenience: check for Flying.
/// CR 702.9a: A creature with flying can't be blocked except by creatures with flying or reach.
pub fn has_flying(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Flying)
}

/// Convenience: check for Haste.
/// CR 702.10a: A creature with haste can attack and activate abilities with {T} the turn it enters.
pub fn has_haste(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Haste)
}

/// Convenience: check for Flash.
pub fn has_flash(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Flash)
}

/// CR 702.11a: Hexproof — can't be the target of spells or abilities opponents control.
pub fn has_hexproof(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Hexproof)
}

/// CR 702.18a: Shroud — can't be the target of spells or abilities.
pub fn has_shroud(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Shroud)
}

/// Convenience: check for Indestructible.
/// CR 702.12a: A permanent with indestructible can't be destroyed.
pub fn has_indestructible(obj: &GameObject) -> bool {
    obj.keywords.contains(&Keyword::Indestructible)
}

/// CR 702.16b: Returns true if target's protection prevents interaction from source.
pub fn protection_prevents_from(target: &GameObject, source: &GameObject) -> bool {
    for kw in &target.keywords {
        if let Keyword::Protection(ref pt) = kw {
            if source_matches_protection_target(pt, target, source) {
                return true;
            }
        }
    }
    false
}

pub fn source_matches_protection_target(
    protection: &ProtectionTarget,
    protected: &GameObject,
    source: &GameObject,
) -> bool {
    match protection {
        ProtectionTarget::Color(color) => source.color.contains(color),
        ProtectionTarget::CardType(type_name) => source_matches_card_type(source, type_name),
        ProtectionTarget::Quality(quality) => source_matches_quality(source, quality),
        ProtectionTarget::Multicolored => source.color.len() > 1,
        ProtectionTarget::ChosenColor => protected
            .chosen_color()
            .is_some_and(|color| source.color.contains(&color)),
        // CR 702.16j: "Protection from everything" — protection from each object
        // regardless of the source's characteristic values.
        ProtectionTarget::Everything => true,
    }
}

pub fn source_matches_card_type(source: &GameObject, type_name: &str) -> bool {
    use crate::types::card_type::CoreType;

    let core = &source.card_types.core_types;
    match type_name {
        "artifacts" | "artifact" => core.contains(&CoreType::Artifact),
        "creatures" | "creature" => core.contains(&CoreType::Creature),
        "enchantments" | "enchantment" => core.contains(&CoreType::Enchantment),
        "instants" | "instant" => core.contains(&CoreType::Instant),
        "sorceries" | "sorcery" => core.contains(&CoreType::Sorcery),
        "planeswalkers" | "planeswalker" => core.contains(&CoreType::Planeswalker),
        "lands" | "land" => core.contains(&CoreType::Land),
        _ => false,
    }
}

pub fn source_matches_quality(source: &GameObject, quality: &str) -> bool {
    match quality {
        "monocolored" => source.color.len() == 1,
        "multicolored" => source.color.len() > 1,
        _ => false,
    }
}

/// Batch parse keyword strings into typed Keyword values.
/// Used when creating GameObjects from parsed card data.
pub fn parse_keywords(keyword_strings: &[String]) -> Vec<Keyword> {
    keyword_strings
        .iter()
        .map(|s| Keyword::from_str(s).unwrap())
        .collect()
}

/// CR 702.49: Check if the current phase allows activation of a Ninjutsu-family variant.
pub fn ninjutsu_timing_ok(phase: &Phase, variant: &NinjutsuVariant) -> bool {
    match variant {
        // CR 702.49a/d: Ninjutsu/CommanderNinjutsu can be activated during declare blockers step or later
        NinjutsuVariant::Ninjutsu
        | NinjutsuVariant::CommanderNinjutsu
        | NinjutsuVariant::WebSlinging => {
            matches!(phase, Phase::DeclareBlockers | Phase::CombatDamage)
        }
        // Sneak: declare blockers step only
        NinjutsuVariant::Sneak => matches!(phase, Phase::DeclareBlockers),
    }
}

/// CR 702.49: Return the creatures that can be returned for this variant.
/// - Ninjutsu/Sneak: unblocked attackers controlled by `player`
/// - WebSlinging: any tapped creature controlled by `player`
pub fn returnable_creatures_for_variant(
    state: &GameState,
    player: PlayerId,
    variant: &NinjutsuVariant,
) -> Vec<ObjectId> {
    match variant {
        NinjutsuVariant::Ninjutsu | NinjutsuVariant::CommanderNinjutsu | NinjutsuVariant::Sneak => {
            super::combat::unblocked_attackers(state)
                .into_iter()
                .filter(|&id| {
                    state
                        .objects
                        .get(&id)
                        .is_some_and(|o| o.controller == player)
                })
                .collect()
        }
        NinjutsuVariant::WebSlinging => state
            .objects
            .values()
            .filter(|o| {
                o.controller == player
                    && o.zone == Zone::Battlefield
                    && o.tapped
                    && o.card_types
                        .core_types
                        .contains(&crate::types::card_type::CoreType::Creature)
            })
            .map(|o| o.id)
            .collect(),
    }
}

/// CR 702.49a-c: Resolve Ninjutsu-family activation.
///
/// Validates the activation, returns the specified creature to its owner's hand,
/// and puts the Ninjutsu creature onto the battlefield tapped and attacking the
/// same player/planeswalker as the returned creature (or active player's opponent
/// for WebSlinging when the returned creature isn't an attacker).
///
/// CR 702.49c: The creature is never "declared as an attacker" so it
/// does not fire "whenever ~ attacks" triggers.
pub fn activate_ninjutsu(
    state: &mut GameState,
    player: PlayerId,
    ninjutsu_card_id: CardId,
    creature_to_return: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), String> {
    // Find the ninjutsu card in player's hand or command zone (for commander ninjutsu)
    // CR 903.8: Commander tax applies to casting, not to ninjutsu activation.
    let p = &state.players[player.0 as usize];
    let ninjutsu_obj_id = p
        .hand
        .iter()
        .chain(state.command_zone.iter())
        .find(|&&obj_id| {
            state
                .objects
                .get(&obj_id)
                .is_some_and(|o| o.card_id == ninjutsu_card_id)
        })
        .copied()
        .ok_or("Ninjutsu-family card not in hand or command zone")?;

    // Determine which variant from the card's keywords
    let ninjutsu_obj = state
        .objects
        .get(&ninjutsu_obj_id)
        .ok_or("Ninjutsu-family card object not found")?;
    let variant = ninjutsu_family_variant(ninjutsu_obj)
        .ok_or("Card does not have a Ninjutsu-family keyword")?;

    // CR 702.49b: Extract the mana cost (validated after all other checks, paid before mutations)
    let mana_cost =
        ninjutsu_family_cost(ninjutsu_obj).ok_or("Ninjutsu-family card has no mana cost")?;

    // Validate timing
    if !ninjutsu_timing_ok(&state.phase, &variant) {
        return Err(format!(
            "{variant:?} can only be activated during the declare blockers step"
        ));
    }

    // Validate: must be in combat
    let combat = state.combat.as_ref().ok_or("No active combat")?;

    // Validate the creature to return based on variant
    let (defending_player, attack_target) = match variant {
        NinjutsuVariant::Ninjutsu | NinjutsuVariant::CommanderNinjutsu | NinjutsuVariant::Sneak => {
            // Must be an unblocked attacker
            let attacker_info = combat
                .attackers
                .iter()
                .find(|a| a.object_id == creature_to_return)
                .ok_or("Specified creature is not an attacker")?
                .clone();

            let is_blocked = combat
                .blocker_assignments
                .get(&creature_to_return)
                .is_some_and(|blockers| !blockers.is_empty());
            if is_blocked {
                return Err("Attacker is blocked".to_string());
            }

            (attacker_info.defending_player, attacker_info.attack_target)
        }
        NinjutsuVariant::WebSlinging => {
            // Must be a tapped creature controlled by player
            let obj = state
                .objects
                .get(&creature_to_return)
                .ok_or("Creature not found")?;
            // CR 702.49: WebSlinging requires a creature on the battlefield
            if obj.zone != Zone::Battlefield {
                return Err("WebSlinging requires a creature on the battlefield".to_string());
            }
            if !obj.tapped {
                return Err("WebSlinging requires a tapped creature".to_string());
            }
            if !obj
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Creature)
            {
                return Err("WebSlinging requires a creature".to_string());
            }

            // If the tapped creature is an attacker, inherit its attack target;
            // otherwise use the opponent of the active player
            combat
                .attackers
                .iter()
                .find(|a| a.object_id == creature_to_return)
                .map(|a| (a.defending_player, a.attack_target.clone()))
                .unwrap_or_else(|| {
                    // CR 702.49c + CR 508.4: Ninjutsu enters attacking same defender;
                    // fallback to first opponent in seat order (multiplayer-aware).
                    let pid = players::opponents(state, player)
                        .first()
                        .copied()
                        .unwrap_or(player);
                    (pid, AttackTarget::Player(pid))
                })
        }
    };

    // Validate: creature controlled by player
    let creature_obj = state
        .objects
        .get(&creature_to_return)
        .ok_or("Creature not found")?;
    if creature_obj.controller != player {
        return Err("You don't control that creature".to_string());
    }

    // CR 601.2f: Apply ability cost reduction from statics like Silver-Fur Master
    // CR 601.2f: All ninjutsu-family variants share the "ninjutsu" keyword for cost reduction.
    let effective_cost = apply_ability_cost_reduction(state, player, "ninjutsu", mana_cost);

    // CR 702.49b: Pay the ninjutsu-family mana cost (after all validation, before mutations)
    super::casting::pay_ability_cost(
        state,
        player,
        ninjutsu_obj_id,
        &AbilityCost::Mana {
            cost: effective_cost,
        },
        events,
    )
    .map_err(|e| e.to_string())?;

    // 1. Return creature to owner's hand
    zones::move_to_zone(state, creature_to_return, Zone::Hand, events);

    // Remove the returned creature from combat state if it was an attacker
    if let Some(combat) = state.combat.as_mut() {
        combat
            .attackers
            .retain(|a| a.object_id != creature_to_return);
        combat.blocker_assignments.remove(&creature_to_return);
    }

    // 2. Move Ninjutsu-family card from hand/command zone to battlefield
    zones::move_to_zone(state, ninjutsu_obj_id, Zone::Battlefield, events);

    // 3. Set tapped, entered_battlefield_turn (summoning sickness), ninjutsu variant tracking
    if let Some(obj) = state.objects.get_mut(&ninjutsu_obj_id) {
        obj.tapped = true;
        obj.entered_battlefield_turn = Some(state.turn_number);
        // CR 702.49: Track which ninjutsu-family variant was paid this turn
        obj.ninjutsu_variant_paid = Some((variant.clone(), state.turn_number));
    }

    // 4. CR 702.49c: Add to combat.attackers directly — do NOT use declare_attackers()
    //    This ensures no AttackersDeclared event fires, so no "whenever ~ attacks" triggers.
    if let Some(combat) = state.combat.as_mut() {
        combat.attackers.push(AttackerInfo::new(
            ninjutsu_obj_id,
            attack_target,
            defending_player,
        ));
    }

    // CR 702.49a: Emit event for "whenever you activate a ninjutsu ability" triggers.
    events.push(GameEvent::NinjutsuActivated {
        player_id: player,
        source_id: ninjutsu_obj_id,
    });

    state.layers_dirty = true;

    Ok(())
}

/// Detect which NinjutsuVariant a game object has, if any.
fn ninjutsu_family_variant(obj: &GameObject) -> Option<NinjutsuVariant> {
    for kw in &obj.keywords {
        match kw {
            Keyword::Ninjutsu(_) => return Some(NinjutsuVariant::Ninjutsu),
            Keyword::CommanderNinjutsu(_) => return Some(NinjutsuVariant::CommanderNinjutsu),
            Keyword::Sneak(_) => return Some(NinjutsuVariant::Sneak),
            Keyword::WebSlinging(_) => return Some(NinjutsuVariant::WebSlinging),
            _ => {}
        }
    }
    None
}

/// CR 702.49b: Extract the mana cost for a ninjutsu-family keyword on this object.
fn ninjutsu_family_cost(obj: &GameObject) -> Option<ManaCost> {
    for kw in &obj.keywords {
        match kw {
            Keyword::Ninjutsu(c)
            | Keyword::CommanderNinjutsu(c)
            | Keyword::Sneak(c)
            | Keyword::WebSlinging(c) => return Some(c.clone()),
            _ => {}
        }
    }
    None
}

/// CR 601.2f: Scan battlefield for ReduceAbilityCost statics that reduce the cost
/// of a specific ability type, and apply the reduction to the given mana cost.
/// `ability_keyword` is the lowered keyword name to match (e.g., "ninjutsu", "equip").
fn apply_ability_cost_reduction(
    state: &GameState,
    player: PlayerId,
    ability_keyword: &str,
    mut cost: ManaCost,
) -> ManaCost {
    for &bf_id in &state.battlefield {
        let Some(bf_obj) = state.objects.get(&bf_id) else {
            continue;
        };
        if bf_obj.controller != player {
            continue;
        }
        for static_def in &bf_obj.static_definitions {
            if let StaticMode::ReduceAbilityCost {
                ref keyword,
                amount,
            } = static_def.mode
            {
                if keyword == ability_keyword {
                    if let ManaCost::Cost {
                        ref mut generic, ..
                    } = cost
                    {
                        *generic = generic.saturating_sub(amount);
                    }
                }
            }
        }
    }
    cost
}

/// CR 702.49b: Look up the ninjutsu-family mana cost for a card eligible for activation.
pub fn ninjutsu_family_cost_for_card(
    state: &GameState,
    player: PlayerId,
    card_id: CardId,
) -> Option<ManaCost> {
    // Reuse activatable_cards to validate card is eligible (DRY — same zone traversal)
    if !ninjutsu_family_activatable_cards(state, player)
        .iter()
        .any(|(c, _)| *c == card_id)
    {
        return None;
    }
    // Look up the object to extract cost
    state
        .objects
        .values()
        .find(|o| o.card_id == card_id && o.owner == player)
        .and_then(ninjutsu_family_cost)
}

/// Returns the CardIds + variant of activatable Ninjutsu-family cards.
/// Checks the player's hand for all variants, plus the command zone for CommanderNinjutsu.
pub fn ninjutsu_family_activatable_cards(
    state: &GameState,
    player: PlayerId,
) -> Vec<(CardId, NinjutsuVariant)> {
    let p = &state.players[player.0 as usize];
    let mut results: Vec<(CardId, NinjutsuVariant)> = p
        .hand
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            let variant = ninjutsu_family_variant(obj)?;
            Some((obj.card_id, variant))
        })
        .collect();

    // CR 702.49d: CommanderNinjutsu can also be activated from the command zone.
    for &obj_id in &state.command_zone {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.owner != player {
                continue;
            }
            if let Some(variant) = ninjutsu_family_variant(obj) {
                if matches!(variant, NinjutsuVariant::CommanderNinjutsu) {
                    results.push((obj.card_id, variant));
                }
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_obj() -> GameObject {
        GameObject::new(
            ObjectId(1),
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        )
    }

    #[test]
    fn has_keyword_simple_match() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        assert!(has_keyword(&obj, &Keyword::Flying));
        assert!(!has_keyword(&obj, &Keyword::Haste));
    }

    #[test]
    fn has_keyword_discriminant_matching() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Kicker(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        }));
        // Discriminant match -- doesn't care about the param value
        assert!(has_keyword(
            &obj,
            &Keyword::Kicker(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Red],
            })
        ));
        assert!(!has_keyword(
            &obj,
            &Keyword::Cycling(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            })
        ));
    }

    #[test]
    fn convenience_functions() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Flying);
        obj.keywords.push(Keyword::Haste);
        obj.keywords.push(Keyword::Flash);
        obj.keywords.push(Keyword::Hexproof);
        obj.keywords.push(Keyword::Shroud);
        obj.keywords.push(Keyword::Indestructible);

        assert!(has_flying(&obj));
        assert!(has_haste(&obj));
        assert!(has_flash(&obj));
        assert!(has_hexproof(&obj));
        assert!(has_shroud(&obj));
        assert!(has_indestructible(&obj));
    }

    #[test]
    fn protection_from_instants_prevents_damage() {
        let mut protected = make_obj();
        protected
            .keywords
            .push(Keyword::Protection(ProtectionTarget::CardType(
                "instants".to_string(),
            )));

        let mut source = make_obj();
        source
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Instant);

        assert!(protection_prevents_from(&protected, &source));
    }

    #[test]
    fn parse_keywords_known() {
        let strings = vec![
            "Flying".to_string(),
            "Haste".to_string(),
            "Deathtouch".to_string(),
        ];
        let parsed = parse_keywords(&strings);
        assert_eq!(
            parsed,
            vec![Keyword::Flying, Keyword::Haste, Keyword::Deathtouch]
        );
    }

    #[test]
    fn parse_keywords_parameterized() {
        let strings = vec!["Kicker:1G".to_string(), "Ward:2".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(
            parsed[0],
            Keyword::Kicker(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            })
        );
        assert_eq!(
            parsed[1],
            Keyword::Ward(crate::types::keywords::WardCost::Mana(ManaCost::Cost {
                generic: 2,
                shards: vec![],
            }))
        );
    }

    #[test]
    fn parse_keywords_unknown() {
        let strings = vec!["NotReal".to_string()];
        let parsed = parse_keywords(&strings);
        assert_eq!(parsed[0], Keyword::Unknown("NotReal".to_string()));
    }

    #[test]
    fn has_keyword_method_on_game_object() {
        let mut obj = make_obj();
        obj.keywords.push(Keyword::Indestructible);
        assert!(obj.has_keyword(&Keyword::Indestructible));
        assert!(!obj.has_keyword(&Keyword::Flying));
    }

    use crate::game::combat::{AttackerInfo, CombatState};
    use crate::game::zones::create_object;
    use crate::types::events::GameEvent;
    use crate::types::game_state::GameState;

    fn setup_ninjutsu_scenario() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::DeclareBlockers;

        // Create an attacker on battlefield
        let attacker_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.tapped = true;
            obj.entered_battlefield_turn = Some(1); // no summoning sickness
        }

        // Set up combat state with attacker unblocked
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker_id, PlayerId(1))],
            ..Default::default()
        });

        // Create Ninjutsu creature in hand
        let ninja_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ninja of the Deep Hours".to_string(),
            crate::types::zones::Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&ninja_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.keywords.push(Keyword::Ninjutsu(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Blue],
            }));
            obj.base_keywords = obj.keywords.clone();
        }

        // Add mana for ninjutsu activation cost ({1}{U})
        for color in [ManaType::Blue, ManaType::Colorless] {
            state.players[0].mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }

        (state, attacker_id, ninja_id)
    }

    #[test]
    fn ninjutsu_returns_attacker_to_hand() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        let mut events = Vec::new();

        activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        )
        .expect("activation should succeed");

        // Attacker should be in hand
        let attacker = state.objects.get(&attacker_id).unwrap();
        assert_eq!(
            attacker.zone,
            crate::types::zones::Zone::Hand,
            "Attacker should be returned to hand"
        );
    }

    #[test]
    fn ninjutsu_creature_enters_battlefield_tapped_attacking() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        let mut events = Vec::new();

        activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        )
        .expect("activation should succeed");

        // Ninjutsu creature should be on battlefield, tapped, attacking
        let ninja = state.objects.get(&ninja_id).unwrap();
        assert_eq!(ninja.zone, crate::types::zones::Zone::Battlefield);
        assert!(ninja.tapped, "Ninjutsu creature should be tapped");
        assert_eq!(
            ninja.entered_battlefield_turn,
            Some(state.turn_number),
            "Should have summoning sickness"
        );

        // Should be in combat attackers
        let combat = state.combat.as_ref().unwrap();
        assert!(
            combat.attackers.iter().any(|a| a.object_id == ninja_id),
            "Ninjutsu creature should be in attackers list"
        );
        // Should be attacking same player as returned attacker
        let ninja_attacker = combat
            .attackers
            .iter()
            .find(|a| a.object_id == ninja_id)
            .unwrap();
        assert_eq!(
            ninja_attacker.defending_player,
            PlayerId(1),
            "Should attack same player"
        );
    }

    #[test]
    fn ninjutsu_creature_does_not_fire_attack_triggers() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        let mut events = Vec::new();

        activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        )
        .expect("activation should succeed");

        // CR 702.49c: No AttackersDeclared event should be emitted for the Ninjutsu creature
        let has_attackers_declared = events
            .iter()
            .any(|e| matches!(e, GameEvent::AttackersDeclared { .. }));
        assert!(
            !has_attackers_declared,
            "No AttackersDeclared event should fire for Ninjutsu creature"
        );
    }

    #[test]
    fn ninjutsu_fails_if_attacker_is_blocked() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;

        // Add a blocker assignment
        let blocker_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Wall".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        state
            .combat
            .as_mut()
            .unwrap()
            .blocker_assignments
            .insert(attacker_id, vec![blocker_id]);

        let mut events = Vec::new();
        let result = activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        );
        assert!(result.is_err(), "Should fail when attacker is blocked");
    }

    #[test]
    fn ninjutsu_fails_without_combat() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        state.combat = None; // Remove combat

        let mut events = Vec::new();
        let result = activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        );
        assert!(result.is_err(), "Should fail without active combat");
    }

    #[test]
    fn ninjutsu_activation_fails_without_mana() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        // Clear all mana
        state.players[0].mana_pool.clear();

        let mut events = Vec::new();
        let result = activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        );
        assert!(result.is_err(), "Should fail without mana");

        // Verify no zone changes occurred — creature still on battlefield, ninja still in hand
        let attacker = state.objects.get(&attacker_id).unwrap();
        assert_eq!(
            attacker.zone,
            Zone::Battlefield,
            "Attacker should not have moved"
        );
        let ninja = state.objects.get(&ninja_id).unwrap();
        assert_eq!(ninja.zone, Zone::Hand, "Ninja should still be in hand");
    }

    #[test]
    fn ninjutsu_activation_deducts_mana() {
        let (mut state, attacker_id, ninja_id) = setup_ninjutsu_scenario();
        let ninja_card_id = state.objects.get(&ninja_id).unwrap().card_id;
        let mut events = Vec::new();

        activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ninja_card_id,
            attacker_id,
            &mut events,
        )
        .expect("activation should succeed");

        // Mana pool should be empty after paying {1}{U}
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "Mana pool should be empty after ninjutsu payment"
        );
    }

    #[test]
    fn webslinging_rejects_non_battlefield_creature() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.turn_number = 2;
        state.phase = crate::types::phase::Phase::DeclareBlockers;

        // Create a tapped creature in the graveyard (not on battlefield)
        let creature_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Graveyard Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.tapped = true;
        }

        // Set up minimal combat state
        state.combat = Some(CombatState {
            attackers: vec![],
            ..Default::default()
        });

        // Create WebSlinging card in hand
        let ws_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "WebSlinger".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&ws_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.keywords.push(Keyword::WebSlinging(ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Blue],
            }));
            obj.base_keywords = obj.keywords.clone();
        }

        // Add mana for WebSlinging cost
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(0),
            snow: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        let ws_card_id = state.objects.get(&ws_id).unwrap().card_id;
        let mut events = Vec::new();
        let result = activate_ninjutsu(
            &mut state,
            PlayerId(0),
            ws_card_id,
            creature_id,
            &mut events,
        );
        assert!(
            result.is_err(),
            "Should reject non-battlefield creature for WebSlinging"
        );
        assert!(
            result.unwrap_err().contains("battlefield"),
            "Error should mention battlefield"
        );
    }
}
