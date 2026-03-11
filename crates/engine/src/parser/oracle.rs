use serde::{Deserialize, Serialize};

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, StaticDefinition,
    TriggerDefinition,
};
use crate::types::keywords::Keyword;

use super::oracle_cost::parse_oracle_cost;
use super::oracle_effect::parse_effect_chain;
use super::oracle_replacement::parse_replacement_line;
use super::oracle_static::parse_static_line;
use super::oracle_trigger::parse_trigger_line;
use super::oracle_util::strip_reminder_text;

/// Collected parsed abilities from Oracle text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedAbilities {
    pub abilities: Vec<AbilityDefinition>,
    pub triggers: Vec<TriggerDefinition>,
    pub statics: Vec<StaticDefinition>,
    pub replacements: Vec<ReplacementDefinition>,
}

/// Parse Oracle text into structured ability definitions.
///
/// Splits on newlines, strips reminder text, then classifies each line
/// according to a priority table (keywords, enchant, equip, activated,
/// triggered, static, replacement, spell effect, modal, loyalty, etc.).
pub fn parse_oracle_text(
    oracle_text: &str,
    card_name: &str,
    keywords: &[Keyword],
    types: &[String],
    subtypes: &[String],
) -> ParsedAbilities {
    let _ = subtypes; // reserved for future use

    let is_spell = types.iter().any(|t| t == "Instant" || t == "Sorcery");

    let mut result = ParsedAbilities {
        abilities: Vec::new(),
        triggers: Vec::new(),
        statics: Vec::new(),
        replacements: Vec::new(),
    };

    let lines: Vec<&str> = oracle_text.split('\n').collect();
    let mut i = 0;

    while i < lines.len() {
        let raw_line = lines[i].trim();
        if raw_line.is_empty() {
            i += 1;
            continue;
        }

        let line = strip_reminder_text(raw_line);
        if line.is_empty() {
            // Priority 14: entirely parenthesized reminder text
            i += 1;
            continue;
        }

        // Priority 1: keyword-only line
        if is_keyword_only_line(&line, keywords) {
            i += 1;
            continue;
        }

        let lower = line.to_lowercase();

        // Priority 2: "Enchant {filter}" — skip (handled externally)
        if lower.starts_with("enchant ") && !lower.starts_with("enchanted ") {
            i += 1;
            continue;
        }

        // Priority 3: "Equip {cost}" / "Equip — {cost}" (but not "Equipped ...")
        if lower.starts_with("equip") && !lower.starts_with("equipped") {
            if let Some(ability) = try_parse_equip(&line) {
                result.abilities.push(ability);
                i += 1;
                continue;
            }
        }

        // Priority 10: Modal "Choose one —" / "Choose two —" etc.
        if lower.starts_with("choose ") && (lower.contains(" —") || lower.contains(" -")) {
            let mut bullets = Vec::new();
            let mut j = i + 1;
            while j < lines.len() {
                let bullet = lines[j].trim();
                if let Some(stripped) = bullet.strip_prefix('•') {
                    bullets.push(stripped.trim().to_string());
                    j += 1;
                } else {
                    break;
                }
            }
            // Parse each bullet as a spell effect
            for bullet in &bullets {
                let def = parse_effect_chain(bullet, AbilityKind::Spell);
                result.abilities.push(def);
            }
            if bullets.is_empty() {
                result.abilities.push(make_unimplemented(&line));
            }
            i = j;
            continue;
        }

        // Priority 11: Planeswalker loyalty abilities: +N:, −N:, 0:, [+N]:, [−N]:, [0]:
        if let Some(ability) = try_parse_loyalty_line(&line) {
            result.abilities.push(ability);
            i += 1;
            continue;
        }

        // Priority 4: Activated ability — contains ":" with cost-like prefix
        if let Some(colon_pos) = find_activated_colon(&line) {
            let cost_text = line[..colon_pos].trim();
            let effect_text = line[colon_pos + 1..].trim();
            let cost = parse_oracle_cost(cost_text);
            let mut def = parse_effect_chain(effect_text, AbilityKind::Activated);
            def.cost = Some(cost);
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Priority 5-6: Triggered abilities — starts with When/Whenever/At
        if lower.starts_with("when ") || lower.starts_with("whenever ") || lower.starts_with("at ")
        {
            let trigger = parse_trigger_line(&line, card_name);
            result.triggers.push(trigger);
            i += 1;
            continue;
        }

        // Priority 7: Static/continuous patterns
        if is_static_pattern(&lower) {
            if let Some(static_def) = parse_static_line(&line) {
                result.statics.push(static_def);
                i += 1;
                continue;
            }
        }

        // Priority 8: Replacement patterns
        if is_replacement_pattern(&lower) {
            if let Some(rep_def) = parse_replacement_line(&line, card_name) {
                result.replacements.push(rep_def);
                i += 1;
                continue;
            }
        }

        // Priority 9: Imperative verb for instants/sorceries
        if is_spell {
            let def = parse_effect_chain(&line, AbilityKind::Spell);
            result.abilities.push(def);
            i += 1;
            continue;
        }

        // Priority 12: Roman numeral chapters (saga) — skip
        if is_saga_chapter(&lower) {
            i += 1;
            continue;
        }

        // Priority 13: Kicker/Multikicker — skip (handled by keywords)
        if lower.starts_with("kicker") || lower.starts_with("multikicker") {
            i += 1;
            continue;
        }

        // Priority 15: Fallback
        result.abilities.push(make_unimplemented(&line));
        i += 1;
    }

    result
}

/// Check if a line consists entirely of known keywords (comma-separated).
/// If at least one part matches a provided keyword, and the remaining parts
/// look like keyword-like text (e.g., "protection from X"), skip the whole line.
fn is_keyword_only_line(line: &str, keywords: &[Keyword]) -> bool {
    if keywords.is_empty() {
        return false;
    }

    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.is_empty() {
        return false;
    }

    // Build a set of lowercase keyword names from the provided keywords slice
    let keyword_names: Vec<String> = keywords.iter().map(keyword_display_name).collect();

    // Check that every part is either a known keyword or a keyword-like phrase
    let mut any_matched = false;
    for part in &parts {
        let lower = part.to_lowercase();
        if keyword_names.iter().any(|name| name == &lower) {
            any_matched = true;
            continue;
        }
        // "protection from ..." is a keyword phrase — always treat as keyword-like
        if lower.starts_with("protection from") {
            continue;
        }
        // Try parsing as a keyword via FromStr to catch common keywords not in the slice
        let parsed: Keyword = lower.parse().unwrap();
        if !matches!(parsed, Keyword::Unknown(_)) {
            continue;
        }
        // Unknown part — not a keyword line
        return false;
    }

    any_matched
}

/// Get a lowercase display name for a keyword variant.
fn keyword_display_name(keyword: &Keyword) -> String {
    match keyword {
        Keyword::Flying => "flying".to_string(),
        Keyword::FirstStrike => "first strike".to_string(),
        Keyword::DoubleStrike => "double strike".to_string(),
        Keyword::Trample => "trample".to_string(),
        Keyword::Deathtouch => "deathtouch".to_string(),
        Keyword::Lifelink => "lifelink".to_string(),
        Keyword::Vigilance => "vigilance".to_string(),
        Keyword::Haste => "haste".to_string(),
        Keyword::Reach => "reach".to_string(),
        Keyword::Defender => "defender".to_string(),
        Keyword::Menace => "menace".to_string(),
        Keyword::Indestructible => "indestructible".to_string(),
        Keyword::Hexproof => "hexproof".to_string(),
        Keyword::Shroud => "shroud".to_string(),
        Keyword::Flash => "flash".to_string(),
        Keyword::Fear => "fear".to_string(),
        Keyword::Intimidate => "intimidate".to_string(),
        Keyword::Skulk => "skulk".to_string(),
        Keyword::Shadow => "shadow".to_string(),
        Keyword::Horsemanship => "horsemanship".to_string(),
        Keyword::Wither => "wither".to_string(),
        Keyword::Infect => "infect".to_string(),
        Keyword::Afflict => "afflict".to_string(),
        Keyword::Prowess => "prowess".to_string(),
        Keyword::Undying => "undying".to_string(),
        Keyword::Persist => "persist".to_string(),
        Keyword::Cascade => "cascade".to_string(),
        Keyword::Convoke => "convoke".to_string(),
        Keyword::Delve => "delve".to_string(),
        Keyword::Devoid => "devoid".to_string(),
        Keyword::Exalted => "exalted".to_string(),
        Keyword::Flanking => "flanking".to_string(),
        Keyword::Changeling => "changeling".to_string(),
        Keyword::Phasing => "phasing".to_string(),
        Keyword::Battlecry => "battlecry".to_string(),
        Keyword::Decayed => "decayed".to_string(),
        Keyword::Unleash => "unleash".to_string(),
        Keyword::Riot => "riot".to_string(),
        Keyword::LivingWeapon => "living weapon".to_string(),
        Keyword::TotemArmor => "totem armor".to_string(),
        Keyword::Evolve => "evolve".to_string(),
        Keyword::Extort => "extort".to_string(),
        Keyword::Exploit => "exploit".to_string(),
        Keyword::Explore => "explore".to_string(),
        Keyword::Ascend => "ascend".to_string(),
        Keyword::Soulbond => "soulbond".to_string(),
        Keyword::Banding => "banding".to_string(),
        Keyword::Cumulative => "cumulative".to_string(),
        Keyword::Epic => "epic".to_string(),
        Keyword::Fuse => "fuse".to_string(),
        Keyword::Gravestorm => "gravestorm".to_string(),
        Keyword::Haunt => "haunt".to_string(),
        Keyword::Hideaway => "hideaway".to_string(),
        Keyword::Improvise => "improvise".to_string(),
        Keyword::Ingest => "ingest".to_string(),
        Keyword::Melee => "melee".to_string(),
        Keyword::Mentor => "mentor".to_string(),
        Keyword::Myriad => "myriad".to_string(),
        Keyword::Provoke => "provoke".to_string(),
        Keyword::Rebound => "rebound".to_string(),
        Keyword::Retrace => "retrace".to_string(),
        Keyword::Ripple => "ripple".to_string(),
        Keyword::SplitSecond => "split second".to_string(),
        Keyword::Storm => "storm".to_string(),
        Keyword::Suspend => "suspend".to_string(),
        Keyword::Totem => "totem".to_string(),
        Keyword::Warp => "warp".to_string(),
        Keyword::Gift => "gift".to_string(),
        Keyword::Ravenous => "ravenous".to_string(),
        Keyword::Daybound => "daybound".to_string(),
        Keyword::Nightbound => "nightbound".to_string(),
        Keyword::Enlist => "enlist".to_string(),
        Keyword::ReadAhead => "read ahead".to_string(),
        Keyword::Compleated => "compleated".to_string(),
        Keyword::Conspire => "conspire".to_string(),
        Keyword::Demonstrate => "demonstrate".to_string(),
        Keyword::Dethrone => "dethrone".to_string(),
        Keyword::DoubleTeam => "double team".to_string(),
        Keyword::LivingMetal => "living metal".to_string(),
        // Parameterized keywords — return just the base name
        Keyword::Dredge(_) => "dredge".to_string(),
        Keyword::Modular(_) => "modular".to_string(),
        Keyword::Renown(_) => "renown".to_string(),
        Keyword::Fabricate(_) => "fabricate".to_string(),
        Keyword::Annihilator(_) => "annihilator".to_string(),
        Keyword::Bushido(_) => "bushido".to_string(),
        Keyword::Tribute(_) => "tribute".to_string(),
        Keyword::Afterlife(_) => "afterlife".to_string(),
        Keyword::Fading(_) => "fading".to_string(),
        Keyword::Vanishing(_) => "vanishing".to_string(),
        Keyword::Rampage(_) => "rampage".to_string(),
        Keyword::Absorb(_) => "absorb".to_string(),
        Keyword::Crew(_) => "crew".to_string(),
        Keyword::Poisonous(_) => "poisonous".to_string(),
        Keyword::Bloodthirst(_) => "bloodthirst".to_string(),
        Keyword::Amplify(_) => "amplify".to_string(),
        Keyword::Graft(_) => "graft".to_string(),
        Keyword::Devour(_) => "devour".to_string(),
        Keyword::Protection(_) => "protection".to_string(),
        Keyword::Kicker(_) => "kicker".to_string(),
        Keyword::Cycling(_) => "cycling".to_string(),
        Keyword::Flashback(_) => "flashback".to_string(),
        Keyword::Ward(_) => "ward".to_string(),
        Keyword::Equip(_) => "equip".to_string(),
        Keyword::Landwalk(_) => "landwalk".to_string(),
        Keyword::Partner(_) => "partner".to_string(),
        Keyword::Companion(_) => "companion".to_string(),
        Keyword::Ninjutsu(_) => "ninjutsu".to_string(),
        Keyword::Enchant(_) => "enchant".to_string(),
        Keyword::EtbCounter { .. } => "etb counter".to_string(),
        Keyword::Reconfigure(_) => "reconfigure".to_string(),
        Keyword::Bestow(_) => "bestow".to_string(),
        Keyword::Embalm(_) => "embalm".to_string(),
        Keyword::Eternalize(_) => "eternalize".to_string(),
        Keyword::Unearth(_) => "unearth".to_string(),
        Keyword::Prowl(_) => "prowl".to_string(),
        Keyword::Morph(_) => "morph".to_string(),
        Keyword::Megamorph(_) => "megamorph".to_string(),
        Keyword::Madness(_) => "madness".to_string(),
        Keyword::Dash(_) => "dash".to_string(),
        Keyword::Emerge(_) => "emerge".to_string(),
        Keyword::Escape(_) => "escape".to_string(),
        Keyword::Evoke(_) => "evoke".to_string(),
        Keyword::Foretell(_) => "foretell".to_string(),
        Keyword::Mutate(_) => "mutate".to_string(),
        Keyword::Disturb(_) => "disturb".to_string(),
        Keyword::Disguise(_) => "disguise".to_string(),
        Keyword::Blitz(_) => "blitz".to_string(),
        Keyword::Overload(_) => "overload".to_string(),
        Keyword::Spectacle(_) => "spectacle".to_string(),
        Keyword::Surge(_) => "surge".to_string(),
        Keyword::Encore(_) => "encore".to_string(),
        Keyword::Buyback(_) => "buyback".to_string(),
        Keyword::Echo(_) => "echo".to_string(),
        Keyword::Outlast(_) => "outlast".to_string(),
        Keyword::Scavenge(_) => "scavenge".to_string(),
        Keyword::Fortify(_) => "fortify".to_string(),
        Keyword::Prototype(_) => "prototype".to_string(),
        Keyword::Plot(_) => "plot".to_string(),
        Keyword::Craft(_) => "craft".to_string(),
        Keyword::Offspring(_) => "offspring".to_string(),
        Keyword::Impending(_) => "impending".to_string(),
        Keyword::Unknown(s) => s.to_lowercase(),
    }
}

/// Try to parse "Equip {cost}" or "Equip — {cost}" lines.
fn try_parse_equip(line: &str) -> Option<AbilityDefinition> {
    let lower = line.to_lowercase();
    if !lower.starts_with("equip") {
        return None;
    }
    let rest = line[5..].trim();
    // Strip leading "—" or "- "
    let cost_text = rest
        .strip_prefix('—')
        .or_else(|| rest.strip_prefix('-'))
        .unwrap_or(rest)
        .trim();

    if cost_text.is_empty() {
        return None;
    }

    let cost = parse_oracle_cost(cost_text);
    Some(AbilityDefinition {
        kind: AbilityKind::Activated,
        effect: Effect::Attach {
            target: crate::types::ability::TargetFilter::Typed {
                card_type: Some(crate::types::ability::TypeFilter::Creature),
                subtype: None,
                controller: Some(crate::types::ability::ControllerRef::You),
                properties: vec![],
            },
        },
        cost: Some(cost),
        sub_ability: None,
        duration: None,
        description: Some(line.to_string()),
        target_prompt: None,
        sorcery_speed: true,
    })
}

/// Try to parse a planeswalker loyalty line: "+N:", "−N:", "0:", "[+N]:", "[−N]:", "[0]:"
fn try_parse_loyalty_line(line: &str) -> Option<AbilityDefinition> {
    let trimmed = line.trim();

    // Try bracket format first: [+2]: ..., [−1]: ..., [0]: ...
    if trimmed.starts_with('[') {
        if let Some(bracket_end) = trimmed.find(']') {
            let inner = &trimmed[1..bracket_end];
            let after_bracket = trimmed[bracket_end + 1..].trim();
            if let Some(effect_text) = after_bracket.strip_prefix(':') {
                if let Some(amount) = parse_loyalty_number(inner) {
                    let effect_text = effect_text.trim();
                    let mut def = parse_effect_chain(effect_text, AbilityKind::Activated);
                    def.cost = Some(AbilityCost::Loyalty { amount });
                    return Some(def);
                }
            }
        }
    }

    // Try bare format: +2: ..., −1: ..., 0: ...
    if let Some(colon_pos) = trimmed.find(':') {
        let prefix = &trimmed[..colon_pos];
        if let Some(amount) = parse_loyalty_number(prefix) {
            // Verify it looks like a loyalty prefix (starts with +, −, –, -, or is "0")
            let first_char = prefix.trim().chars().next()?;
            if first_char == '+' || first_char == '−' || first_char == '–' || first_char == '-' || prefix.trim() == "0" {
                let effect_text = trimmed[colon_pos + 1..].trim();
                let mut def = parse_effect_chain(effect_text, AbilityKind::Activated);
                def.cost = Some(AbilityCost::Loyalty { amount });
                return Some(def);
            }
        }
    }

    None
}

/// Parse a loyalty number string like "+2", "−3", "0", "-1".
fn parse_loyalty_number(s: &str) -> Option<i32> {
    let s = s.trim();
    // Normalize Unicode minus signs
    let normalized = s.replace(['−', '–'], "-");
    // "+N" → positive
    if let Some(rest) = normalized.strip_prefix('+') {
        return rest.parse::<i32>().ok();
    }
    // "-N" or bare number
    normalized.parse::<i32>().ok()
}

/// Find the position of ":" that indicates an activated ability cost/effect split.
/// The left side must look like a cost (contains "{", or starts with cost-like words,
/// or is a loyalty marker).
fn find_activated_colon(line: &str) -> Option<usize> {
    let colon_pos = line.find(':')?;
    let prefix = &line[..colon_pos];
    let lower_prefix = prefix.to_lowercase().trim().to_string();

    // Contains mana symbols
    if prefix.contains('{') {
        return Some(colon_pos);
    }

    // Starts with cost-like words
    let cost_starters = [
        "sacrifice",
        "discard",
        "pay",
        "remove",
        "exile",
        "tap",
        "untap",
    ];
    if cost_starters
        .iter()
        .any(|s| lower_prefix.starts_with(s))
    {
        return Some(colon_pos);
    }

    None
}

/// Check if a line looks like a static/continuous ability.
fn is_static_pattern(lower: &str) -> bool {
    lower.contains("gets +")
        || lower.contains("gets -")
        || lower.contains("get +")
        || lower.contains("get -")
        || lower.contains("has ")
        || lower.contains("can't be blocked")
        || lower.contains("can't attack")
}

/// Check if a line looks like a replacement effect.
fn is_replacement_pattern(lower: &str) -> bool {
    lower.contains("would ")
        || lower.contains("prevent all")
        || lower.contains("can't be prevented")
        || lower.contains("enters the battlefield tapped")
        || lower.contains("enters tapped")
}

/// Check if a line is a saga chapter (e.g. "I —", "II —", "III —").
fn is_saga_chapter(lower: &str) -> bool {
    let trimmed = lower.trim();
    // Saga chapter lines start with roman numerals followed by "—" or "-"
    for prefix in &["i —", "ii —", "iii —", "iv —", "v —", "i -", "ii -", "iii -", "iv -", "v -"]
    {
        if trimmed.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Create an Unimplemented fallback ability.
fn make_unimplemented(line: &str) -> AbilityDefinition {
    AbilityDefinition {
        kind: AbilityKind::Spell,
        effect: Effect::Unimplemented {
            name: "unknown".to_string(),
            description: Some(line.to_string()),
        },
        cost: None,
        sub_ability: None,
        duration: None,
        description: Some(line.to_string()),
        target_prompt: None,
        sorcery_speed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(
        text: &str,
        name: &str,
        kw: &[Keyword],
        types: &[&str],
        subtypes: &[&str],
    ) -> ParsedAbilities {
        let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
        let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
        parse_oracle_text(text, name, kw, &types, &subtypes)
    }

    #[test]
    fn lightning_bolt_spell_effect() {
        let r = parse(
            "Lightning Bolt deals 3 damage to any target.",
            "Lightning Bolt",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Spell);
    }

    #[test]
    fn llanowar_elves_mana_ability() {
        let r = parse(
            "{T}: Add {G}.",
            "Llanowar Elves",
            &[],
            &["Creature"],
            &["Elf", "Druid"],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
    }

    #[test]
    fn murder_spell_destroy() {
        let r = parse(
            "Destroy target creature.",
            "Murder",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Spell);
    }

    #[test]
    fn counterspell_spell_counter() {
        let r = parse(
            "Counter target spell.",
            "Counterspell",
            &[],
            &["Instant"],
            &[],
        );
        assert_eq!(r.abilities.len(), 1);
    }

    #[test]
    fn bonesplitter_static_plus_equip() {
        let r = parse(
            "Equipped creature gets +2/+0.\nEquip {1}",
            "Bonesplitter",
            &[],
            &["Artifact"],
            &["Equipment"],
        );
        assert_eq!(r.statics.len(), 1);
        assert_eq!(r.abilities.len(), 1); // equip ability
    }

    #[test]
    fn rancor_enchant_static_trigger() {
        let r = parse(
            "Enchant creature\nEnchanted creature gets +2/+0 and has trample.\nWhen Rancor is put into a graveyard from the battlefield, return Rancor to its owner's hand.",
            "Rancor",
            &[],
            &["Enchantment"],
            &["Aura"],
        );
        // Enchant line skipped (priority 2)
        assert_eq!(r.statics.len(), 1);
        assert_eq!(r.triggers.len(), 1);
    }

    #[test]
    fn goblin_chainwhirler_etb_trigger() {
        let r = parse(
            "First strike\nWhen Goblin Chainwhirler enters the battlefield, it deals 1 damage to each opponent and each creature and planeswalker they control.",
            "Goblin Chainwhirler",
            &[Keyword::FirstStrike],
            &["Creature"],
            &["Goblin", "Warrior"],
        );
        assert_eq!(r.triggers.len(), 1);
        assert_eq!(r.abilities.len(), 0); // keyword line skipped
    }

    #[test]
    fn baneslayer_angel_keywords_only() {
        let r = parse(
            "Flying, first strike, lifelink, protection from Demons and from Dragons",
            "Baneslayer Angel",
            &[Keyword::Flying, Keyword::FirstStrike, Keyword::Lifelink],
            &["Creature"],
            &["Angel"],
        );
        // Keywords line should be mostly skipped; protection clause may produce unimplemented
        // The key assertion: no activated abilities, no triggers
        assert_eq!(r.abilities.len(), 0);
        assert_eq!(r.triggers.len(), 0);
    }

    #[test]
    fn questing_beast_mixed() {
        let r = parse(
            "Vigilance, deathtouch, haste\nQuesting Beast can't be blocked by creatures with power 2 or less.\nCombat damage that would be dealt by creatures you control can't be prevented.\nWhenever Questing Beast deals combat damage to a planeswalker, it deals that much damage to target planeswalker that player controls.",
            "Questing Beast",
            &[Keyword::Vigilance, Keyword::Deathtouch, Keyword::Haste],
            &["Creature"],
            &["Beast"],
        );
        assert_eq!(r.abilities.len(), 0); // keyword line skipped
        // Should have static, replacement, and trigger
        assert!(r.statics.len() + r.replacements.len() + r.triggers.len() >= 2);
    }

    #[test]
    fn jace_loyalty_abilities() {
        let r = parse(
            "+2: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n0: Draw three cards, then put two cards from your hand on top of your library in any order.\n\u{2212}1: Return target creature to its owner's hand.\n\u{2212}12: Exile all cards from target player's library, then that player shuffles their hand into their library.",
            "Jace, the Mind Sculptor",
            &[],
            &["Planeswalker"],
            &["Jace"],
        );
        assert_eq!(r.abilities.len(), 4);
        // All should be activated with loyalty costs
        for ab in &r.abilities {
            assert_eq!(ab.kind, AbilityKind::Activated);
        }
    }

    #[test]
    fn forest_reminder_text_only() {
        let r = parse("({T}: Add {G}.)", "Forest", &[], &["Land"], &["Forest"]);
        // Reminder text should be stripped/skipped
        assert_eq!(r.abilities.len(), 0);
    }

    #[test]
    fn mox_pearl_mana_ability() {
        let r = parse("{T}: Add {W}.", "Mox Pearl", &[], &["Artifact"], &[]);
        assert_eq!(r.abilities.len(), 1);
        assert_eq!(r.abilities[0].kind, AbilityKind::Activated);
    }
}
