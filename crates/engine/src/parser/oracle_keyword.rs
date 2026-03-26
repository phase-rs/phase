use std::borrow::Cow;

use crate::types::keywords::{Keyword, WardCost};

/// CR 702.16 + CR 702.11f: Expand compound "X from A and from B" keyword lines.
/// Handles both "protection from X and from Y" and "hexproof from X and from Y"
/// by splitting into individual keyword entries.
pub(crate) fn expand_protection_parts<'a>(parts: &[&'a str]) -> Vec<Cow<'a, str>> {
    // Fast path: skip allocation when no expansion is needed
    if !parts.iter().any(|p| {
        let l = p.to_ascii_lowercase();
        l.contains(" and from ") || l.starts_with("from ") || l.starts_with("and from ")
    }) {
        return parts.iter().map(|&p| Cow::Borrowed(p)).collect();
    }

    let mut expanded: Vec<Cow<'a, str>> = Vec::new();
    // Track which keyword prefix we're expanding (None, "protection", or "hexproof")
    let mut active_prefix: Option<&'static str> = None;

    for &part in parts {
        let lower = part.to_ascii_lowercase();

        // Check for "protection from X and from Y" or "hexproof from X and from Y"
        // (prefix_with_space, emit_prefix_no_space) — strip the prefix+space, emit prefix without space
        let prefix_match = if lower.starts_with("protection from ") {
            Some("protection from")
        } else if lower.starts_with("hexproof from ") {
            Some("hexproof from")
        } else {
            None
        };

        if let Some(prefix) = prefix_match {
            // Strip "protection from " or "hexproof from " (prefix + space)
            let after = &lower[prefix.len() + 1..]; // +1 for the trailing space
                                                    // CR 702.11f / CR 702.16: split on " and from "
            for frag in after.split(" and from ") {
                expanded.push(Cow::Owned(format!("{prefix} {}", frag.trim())));
            }
            active_prefix = Some(prefix);
        } else if let Some(pfx) = active_prefix {
            if let Some(rest) = lower.strip_prefix("and from ") {
                // ", and from Zombies" — Oxford comma continuation
                expanded.push(Cow::Owned(format!("{pfx} {}", rest.trim())));
            } else if let Some(rest) = lower.strip_prefix("from ") {
                // ", from Werewolves" — comma continuation
                expanded.push(Cow::Owned(format!("{pfx} {}", rest.trim())));
            } else {
                active_prefix = None;
                expanded.push(Cow::Borrowed(part));
            }
        } else {
            expanded.push(Cow::Borrowed(part));
        }
    }
    expanded
}

/// Try to extract keywords from a keyword-only line (comma-separated).
/// Returns `Some(keywords)` if the entire line consists of recognizable keywords
/// AND at least one part matches an MTGJSON keyword name (preventing false positives
/// from standalone ability lines like "Equip {1}").
///
/// Returns only keywords not already covered by MTGJSON names — these are typically
/// parameterized keywords where MTGJSON lists the name (e.g. "Protection") but
/// Oracle text has the full form (e.g. "Protection from multicolored").
pub(crate) fn extract_keyword_line(
    line: &str,
    mtgjson_keyword_names: &[String],
) -> Option<Vec<Keyword>> {
    if mtgjson_keyword_names.is_empty() {
        return None;
    }

    let raw_parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if raw_parts.is_empty() {
        return None;
    }

    // CR 702.16: Expand "protection from X and from Y" into individual parts
    let parts = expand_protection_parts(&raw_parts);

    let mut any_mtgjson_match = false;
    let mut new_keywords = Vec::new();

    for part in &parts {
        let lower = part.to_lowercase();

        // Check if this part matches or extends an MTGJSON keyword name.
        // Exact match: "flying" == "flying"
        // Prefix match: "protection from multicolored" starts with "protection"
        let mtgjson_match = mtgjson_keyword_names.iter().any(|name| {
            lower == *name
                || lower.starts_with(&format!("{name} "))
                || lower.starts_with(&format!("{name}\u{2014}"))
        });

        if mtgjson_match {
            any_mtgjson_match = true;

            // Exact name match means MTGJSON already has the parsed keyword — skip
            if mtgjson_keyword_names.contains(&lower) {
                continue;
            }

            // Prefix match: Oracle text has more detail (e.g. "protection from red").
            // Extract the full parameterized keyword.
            if let Some(kw) = parse_keyword_from_oracle(&lower) {
                new_keywords.push(kw);
                continue;
            }
        }

        // Not an MTGJSON match — try parsing as any keyword (for keyword-only line validation)
        if let Some(kw) = parse_keyword_from_oracle(&lower) {
            if !matches!(kw, Keyword::Unknown(_)) {
                // Keywords not in MTGJSON (e.g., firebending) must be extracted here.
                // They also validate the line as a keyword line.
                any_mtgjson_match = true;
                new_keywords.push(kw);
                continue;
            }
        }

        // Unrecognized part — not a keyword line
        return None;
    }

    if any_mtgjson_match {
        Some(new_keywords)
    } else {
        None
    }
}

/// CR 702.21a: Parse a non-mana ward cost from the em-dash remainder.
/// Handles "pay N life", "discard a card", "sacrifice a permanent/creature/etc."
fn parse_ward_cost(cost_text: &str) -> Option<Keyword> {
    let lower = cost_text.trim().trim_end_matches('.').to_lowercase();

    // "pay N life"
    if let Some(rest) = lower.strip_prefix("pay ") {
        if let Some(life_str) = rest.strip_suffix(" life") {
            if let Ok(n) = life_str.trim().parse::<i32>() {
                return Some(Keyword::Ward(WardCost::PayLife(n)));
            }
        }
    }

    // "discard a card" / "discard two cards" etc.
    if lower.starts_with("discard") {
        return Some(Keyword::Ward(WardCost::DiscardCard));
    }

    // "sacrifice a permanent" / "sacrifice a creature" / etc.
    if lower.starts_with("sacrifice") {
        return Some(Keyword::Ward(WardCost::SacrificeAPermanent));
    }

    // CR 702.21a + CR 701.67: "waterbend {N}" — ward cost paid via waterbend mechanic.
    if let Some(rest) = lower.strip_prefix("waterbend") {
        let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(rest.trim());
        return Some(Keyword::Ward(WardCost::Waterbend(cost)));
    }

    // Fall back to mana cost parsing
    let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_text.trim());
    Some(Keyword::Ward(WardCost::Mana(cost)))
}

/// Parse a keyword from Oracle text format (natural language) into a `Keyword`.
///
/// Oracle text uses space-separated format: "protection from red", "ward {2}",
/// "flashback {2}{U}". Converts to the colon format that `FromStr` expects,
/// handling the "from" preposition used by protection keywords.
pub(crate) fn parse_keyword_from_oracle(text: &str) -> Option<Keyword> {
    use crate::types::keywords::PartnerType;

    // CR 702.124: Partner variant keywords — must come BEFORE generic "partner" match.
    // MTGJSON sends Character Select, Friends Forever, and generic Partner all as keyword "Partner".
    // Oracle text em-dash suffix disambiguates them.
    if text.starts_with("partner\u{2014}character select") {
        return Some(Keyword::Partner(PartnerType::CharacterSelect));
    }
    if text.starts_with("partner\u{2014}friends forever") {
        return Some(Keyword::Partner(PartnerType::FriendsForever));
    }
    if text.starts_with("choose a background") {
        return Some(Keyword::Partner(PartnerType::ChooseABackground));
    }
    if text.starts_with("doctor\u{2019}s companion") || text.starts_with("doctor's companion") {
        return Some(Keyword::Partner(PartnerType::DoctorsCompanion));
    }
    // CR 702.124c: "Partner with [Name]" — handled at the build_oracle_face level
    // via MTGJSON keyword detection. Skip here to avoid producing a duplicate with
    // incorrect casing from the lowered oracle text.
    if text.starts_with("partner with ") {
        return None;
    }

    // First try direct parse (handles simple keywords like "flying")
    let direct: Keyword = text.parse().unwrap();
    if !matches!(direct, Keyword::Unknown(_)) {
        return Some(direct);
    }

    // CR 702.29: Typecycling — "{subtype}cycling {cost}" e.g. "plainscycling {2}"
    // Guard: subtype prefix must be a single word (no spaces) to avoid false positives.
    if let Some(cycling_pos) = text.find("cycling") {
        if cycling_pos > 0 {
            let subtype = &text[..cycling_pos];
            if !subtype.contains(' ') {
                let cost_str = text[cycling_pos + "cycling".len()..].trim();
                if !cost_str.is_empty() {
                    let colon_form = format!("typecycling:{subtype}:{cost_str}");
                    let parsed: Keyword = colon_form.parse().unwrap();
                    if !matches!(parsed, Keyword::Unknown(_)) {
                        return Some(parsed);
                    }
                }
            }
        }
    }

    // CR 702.21a: Ward with non-mana costs uses em-dash separator (U+2014).
    // "ward—pay N life", "ward—discard a card", "ward—sacrifice a permanent"
    if let Some(rest) = text.strip_prefix("ward\u{2014}") {
        return parse_ward_cost(rest);
    }

    // CR 702.74a: "hideaway N" — parameterized keyword.
    if let Some(rest) = text.strip_prefix("hideaway ") {
        if let Ok(n) = rest.trim().parse::<u32>() {
            return Some(Keyword::Hideaway(n));
        }
    }

    // CR 702.87a: "level up {cost}" — two-word keyword name.
    if let Some(rest) = text.strip_prefix("level up ") {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
            return Some(Keyword::LevelUp(cost));
        }
    }

    // CR 701.57a: "discover N"
    if let Some(rest) = text.strip_prefix("discover ") {
        if let Ok(n) = rest.trim().parse::<u32>() {
            return Some(Keyword::Discover(n));
        }
    }

    // Gift keyword: "gift a card", "gift a treasure", "gift a food", "gift a tapped fish"
    if let Some(rest) = text.strip_prefix("gift a ") {
        use crate::types::keywords::GiftKind;
        let kind = match rest.trim() {
            "card" => GiftKind::Card,
            "treasure" => GiftKind::Treasure,
            "food" => GiftKind::Food,
            "tapped fish" => GiftKind::TappedFish,
            _ => return None,
        };
        return Some(Keyword::Gift(kind));
    }

    // CR 702.49d: Commander ninjutsu — multi-word keyword name (like "level up").
    if let Some(rest) = text.strip_prefix("commander ninjutsu ") {
        let cost_str = rest.trim();
        if !cost_str.is_empty() {
            let cost = crate::database::mtgjson::parse_mtgjson_mana_cost(cost_str);
            return Some(Keyword::CommanderNinjutsu(cost));
        }
    }

    // For parameterized keywords, find the first space to split name from parameter.
    // Oracle format: "protection from multicolored" → name="protection", rest="from multicolored"
    // Oracle format: "ward {2}" → name="ward", rest="{2}"
    let space_idx = text.find(' ')?;
    let name = &text[..space_idx];
    let rest = text[space_idx + 1..].trim();

    // Strip "from" preposition (used by protection keywords)
    let param = rest.strip_prefix("from ").unwrap_or(rest);

    let colon_form = format!("{name}:{param}");
    let parsed: Keyword = colon_form.parse().unwrap();
    if matches!(parsed, Keyword::Unknown(_)) {
        return None;
    }
    Some(parsed)
}

/// Get a lowercase display name for a keyword variant.
pub fn keyword_display_name(keyword: &Keyword) -> String {
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
        Keyword::HexproofFrom(_) => "hexproof from".to_string(),
        Keyword::Shroud => "shroud".to_string(),
        Keyword::Flash => "flash".to_string(),
        Keyword::Fear => "fear".to_string(),
        Keyword::Intimidate => "intimidate".to_string(),
        Keyword::Skulk => "skulk".to_string(),
        Keyword::Shadow => "shadow".to_string(),
        Keyword::Horsemanship => "horsemanship".to_string(),
        Keyword::Wither => "wither".to_string(),
        Keyword::Infect => "infect".to_string(),
        Keyword::Afflict(n) => format!("afflict {n}"),
        Keyword::Prowess => "prowess".to_string(),
        Keyword::Undying => "undying".to_string(),
        Keyword::Persist => "persist".to_string(),
        Keyword::Cascade => "cascade".to_string(),
        Keyword::Convoke => "convoke".to_string(),
        Keyword::Waterbend => "waterbend".to_string(),
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
        Keyword::Warp(_) => "warp".to_string(),
        Keyword::Sneak(_) => "sneak".to_string(),
        Keyword::WebSlinging(_) => "web-slinging".to_string(),
        Keyword::Mobilize(_) => "mobilize".to_string(),
        Keyword::Gift(_) => "gift".to_string(),
        Keyword::Discover(n) => format!("discover {n}"),
        Keyword::Spree => "spree".to_string(),
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
        Keyword::Firebending(_) => "firebending".to_string(),
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
        Keyword::Toxic(_) => "toxic".to_string(),
        Keyword::Saddle(_) => "saddle".to_string(),
        Keyword::Soulshift(_) => "soulshift".to_string(),
        Keyword::Backup(_) => "backup".to_string(),
        Keyword::Squad(_) => "squad".to_string(),
        Keyword::Typecycling { ref subtype, .. } => {
            format!("{}cycling", subtype.to_lowercase())
        }
        Keyword::Protection(_) => "protection".to_string(),
        Keyword::Kicker(_) => "kicker".to_string(),
        Keyword::Cycling(_) => "cycling".to_string(),
        Keyword::Flashback(_) => "flashback".to_string(),
        Keyword::Ward(_) => "ward".to_string(),
        Keyword::Equip(_) => "equip".to_string(),
        Keyword::Landwalk(_) => "landwalk".to_string(),
        Keyword::Partner(ref pt) => {
            use crate::types::keywords::PartnerType;
            match pt {
                PartnerType::Generic => "partner".to_string(),
                PartnerType::With(name) => format!("partner with {name}"),
                PartnerType::FriendsForever => "friends forever".to_string(),
                PartnerType::CharacterSelect => "character select".to_string(),
                PartnerType::DoctorsCompanion => "doctor's companion".to_string(),
                PartnerType::ChooseABackground => "choose a background".to_string(),
            }
        }
        Keyword::Companion(_) => "companion".to_string(),
        Keyword::Ninjutsu(_) => "ninjutsu".to_string(),
        Keyword::CommanderNinjutsu(_) => "commander ninjutsu".to_string(),
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
        Keyword::Escape { .. } => "escape".to_string(),
        Keyword::Harmonize(_) => "harmonize".to_string(),
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
        Keyword::LevelUp(_) => "level up".to_string(),
        Keyword::Hideaway(_) => "hideaway".to_string(),
        Keyword::Unknown(s) => s.to_lowercase(),
    }
}

/// Check if a line is a keyword with a cost (e.g., "Cycling {2}", "Flashback {3}{R}", "Crew 3").
/// These are handled by MTGJSON keywords and should be skipped by the Oracle parser.
pub(crate) fn is_keyword_cost_line(lower: &str) -> bool {
    let keyword_costs = [
        "cycling",
        "flashback",
        "crew",
        "ward",
        "equip", // already handled earlier but as safety
        "bestow",
        "embalm",
        "eternalize",
        "unearth",
        "commander ninjutsu",
        "ninjutsu",
        "prowl",
        "morph",
        "megamorph",
        "madness",
        "dash",
        "emerge",
        "escape",
        "evoke",
        "foretell",
        "mutate",
        "disturb",
        "disguise",
        "blitz",
        "overload",
        "spectacle",
        "surge",
        "encore",
        "buyback",
        "echo",
        "outlast",
        "scavenge",
        "fortify",
        "prototype",
        "plot",
        "craft",
        "offspring",
        "impending",
        "reconfigure",
        "suspend",
        "cumulative upkeep",
        "level up",
        "transfigure",
        "transmute",
        "forecast",
        "recover",
        "reinforce",
        "retrace",
        "adapt",
        "monstrosity",
        "affinity",
        "convoke",
        "waterbend",
        "delve",
        "improvise",
        "miracle",
        "splice",
        "entwine",
        "toxic",
        "saddle",
        "soulshift",
        "backup",
        "squad",
        "warp",
        "sneak",
        "web-slinging",
        "mobilize",
        "hideaway",
        "gift",
        "discover",
        "harmonize",
        "collect evidence",
        "mayhem",
        "more than meets the eye",
        "living weapon",
        "champion",
        "amplify",
        "bloodthirst",
        "tribute",
        "persist",
        "undying",
        "fabricate",
        "modular",
        "partner",
        "spree",
        "casualty",
        "bargain",
        "demonstrate",
        "strive",
        "exploit",
        "devoid",
    ];
    keyword_costs.iter().any(|kw| {
        lower.starts_with(kw)
            && (lower.len() == kw.len()
                || lower.as_bytes().get(kw.len()) == Some(&b' ')
                || lower.as_bytes().get(kw.len()) == Some(&b'\t')
                || lower[kw.len()..].starts_with('\u{2014}'))
    })
        // CR 702.29: Typecycling — first word ends in "cycling" but isn't "cycling" itself
        || lower
            .split_whitespace()
            .next()
            .is_some_and(|w| w.ends_with("cycling") && w != "cycling")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::mana::ManaCost;

    #[test]
    fn parse_keyword_from_oracle_toxic() {
        // CR 702.164: Toxic N — parameterized keyword from Oracle text
        let kw = parse_keyword_from_oracle("toxic 2").unwrap();
        assert_eq!(kw, Keyword::Toxic(2));
    }

    #[test]
    fn parse_keyword_from_oracle_saddle() {
        // CR 702.173: Saddle N
        let kw = parse_keyword_from_oracle("saddle 3").unwrap();
        assert_eq!(kw, Keyword::Saddle(3));
    }

    #[test]
    fn parse_keyword_from_oracle_soulshift() {
        // CR 702.46: Soulshift N
        let kw = parse_keyword_from_oracle("soulshift 7").unwrap();
        assert_eq!(kw, Keyword::Soulshift(7));
    }

    #[test]
    fn parse_keyword_from_oracle_backup() {
        // CR 702.165: Backup N
        let kw = parse_keyword_from_oracle("backup 1").unwrap();
        assert_eq!(kw, Keyword::Backup(1));
    }

    #[test]
    fn parse_keyword_from_oracle_squad() {
        // CR 702.157: Squad {cost}
        let kw = parse_keyword_from_oracle("squad {2}").unwrap();
        assert!(matches!(kw, Keyword::Squad(ManaCost::Cost { .. })));
    }

    #[test]
    fn parse_keyword_from_oracle_typecycling() {
        // CR 702.29: Typecycling — "plainscycling {2}" is typecycling, not regular cycling
        let kw = parse_keyword_from_oracle("plainscycling {2}").unwrap();
        assert!(matches!(kw, Keyword::Typecycling { .. }));
        if let Keyword::Typecycling { subtype, .. } = &kw {
            assert_eq!(subtype, "Plains");
        }

        // "forestcycling {1}{G}" — different subtype
        let kw2 = parse_keyword_from_oracle("forestcycling {1}{G}").unwrap();
        if let Keyword::Typecycling { subtype, .. } = &kw2 {
            assert_eq!(subtype, "Forest");
        }
    }

    #[test]
    fn parse_keyword_from_oracle_regular_cycling_not_typecycling() {
        // "cycling {2}" must remain regular Cycling, not Typecycling
        let kw = parse_keyword_from_oracle("cycling {2}").unwrap();
        assert!(matches!(kw, Keyword::Cycling(_)));
    }

    #[test]
    fn parse_keyword_from_oracle_protection_from_color() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // CR 702.16: "protection from red" parses to Protection(Color(Red))
        let kw = parse_keyword_from_oracle("protection from red").unwrap();
        assert_eq!(
            kw,
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Red))
        );

        let kw = parse_keyword_from_oracle("protection from blue").unwrap();
        assert_eq!(
            kw,
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Blue))
        );
    }

    #[test]
    fn parse_keyword_from_oracle_protection_from_chosen_color() {
        use crate::types::keywords::ProtectionTarget;

        // CR 702.16: "protection from the chosen color" parses to Protection(ChosenColor)
        let kw = parse_keyword_from_oracle("protection from the chosen color").unwrap();
        assert_eq!(kw, Keyword::Protection(ProtectionTarget::ChosenColor));
    }

    #[test]
    fn parse_keyword_from_oracle_gift_a_card() {
        use crate::types::keywords::GiftKind;
        let kw = parse_keyword_from_oracle("gift a card").unwrap();
        assert_eq!(kw, Keyword::Gift(GiftKind::Card));
    }

    #[test]
    fn parse_keyword_from_oracle_gift_a_treasure() {
        use crate::types::keywords::GiftKind;
        let kw = parse_keyword_from_oracle("gift a treasure").unwrap();
        assert_eq!(kw, Keyword::Gift(GiftKind::Treasure));
    }

    #[test]
    fn parse_keyword_from_oracle_gift_a_food() {
        use crate::types::keywords::GiftKind;
        let kw = parse_keyword_from_oracle("gift a food").unwrap();
        assert_eq!(kw, Keyword::Gift(GiftKind::Food));
    }

    #[test]
    fn parse_keyword_from_oracle_gift_a_tapped_fish() {
        use crate::types::keywords::GiftKind;
        let kw = parse_keyword_from_oracle("gift a tapped fish").unwrap();
        assert_eq!(kw, Keyword::Gift(GiftKind::TappedFish));
    }

    #[test]
    fn gift_is_keyword_cost_line() {
        assert!(is_keyword_cost_line("gift a card"));
        assert!(is_keyword_cost_line("gift a treasure"));
        assert!(is_keyword_cost_line("gift a tapped fish"));
    }

    #[test]
    fn is_keyword_cost_line_new_keywords() {
        assert!(is_keyword_cost_line("toxic 2"));
        assert!(is_keyword_cost_line("saddle 3"));
        assert!(is_keyword_cost_line("soulshift 7"));
        assert!(is_keyword_cost_line("backup 1"));
        assert!(is_keyword_cost_line("squad {2}"));
    }

    #[test]
    fn is_keyword_cost_line_typecycling() {
        // Typecycling lines should be recognized as keyword cost lines
        assert!(is_keyword_cost_line("plainscycling {2}"));
        assert!(is_keyword_cost_line("forestcycling {1}{G}"));
        assert!(is_keyword_cost_line("islandcycling {2}"));
        // Regular cycling still matches (existing behavior)
        assert!(is_keyword_cost_line("cycling {2}"));
    }

    // --- expand_protection_parts tests ---

    #[test]
    fn expand_protection_baneslayer_pattern() {
        // CR 702.16: "protection from Demons and from Dragons" → two Protection keywords
        let keywords = extract_keyword_line(
            "Flying, first strike, lifelink, protection from Demons and from Dragons",
            &[
                "flying".to_string(),
                "first strike".to_string(),
                "lifelink".to_string(),
                "protection".to_string(),
            ],
        )
        .unwrap();
        let protection_count = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .count();
        assert_eq!(
            protection_count, 2,
            "expected two separate Protection keywords"
        );
    }

    #[test]
    fn expand_protection_two_colors() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // CR 702.16: "protection from black and from red" → two color protections
        let keywords = extract_keyword_line(
            "Flying, protection from black and from red",
            &["flying".to_string(), "protection".to_string()],
        )
        .unwrap();
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Black
            )))
        );
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Red
            )))
        );
    }

    #[test]
    fn expand_protection_three_comma_continuation() {
        // CR 702.16: comma + Oxford comma continuation
        let keywords = extract_keyword_line(
            "First strike, protection from Vampires, from Werewolves, and from Zombies",
            &["first strike".to_string(), "protection".to_string()],
        )
        .unwrap();
        let protection_count = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .count();
        assert_eq!(
            protection_count, 3,
            "expected three separate Protection keywords"
        );
    }

    #[test]
    fn expand_protection_preserves_qualifier_text() {
        use crate::types::keywords::ProtectionTarget;

        // Emrakul pattern: qualifier text preserved after split
        let keywords = extract_keyword_line(
            "protection from spells and from permanents that were cast this turn",
            &["protection".to_string()],
        )
        .unwrap();
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::CardType(
                "spells".to_string()
            )))
        );
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::CardType(
                "permanents that were cast this turn".to_string()
            )))
        );
    }

    #[test]
    fn expand_protection_from_everything_no_split() {
        use crate::types::keywords::ProtectionTarget;

        // "protection from everything" — no " and from " present, no expansion
        let keywords =
            extract_keyword_line("protection from everything", &["protection".to_string()])
                .unwrap();
        assert_eq!(keywords.len(), 1);
        assert_eq!(
            keywords[0],
            Keyword::Protection(ProtectionTarget::CardType("everything".to_string()))
        );
    }

    #[test]
    fn expand_protection_single_no_expansion() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // Single protection — expansion is a no-op
        let keywords = extract_keyword_line(
            "Flying, protection from red",
            &["flying".to_string(), "protection".to_string()],
        )
        .unwrap();
        let prots: Vec<_> = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::Protection(_)))
            .collect();
        assert_eq!(prots.len(), 1);
        assert_eq!(
            prots[0],
            &Keyword::Protection(ProtectionTarget::Color(ManaColor::Red))
        );
    }

    #[test]
    fn expand_protection_non_protection_line_unchanged() {
        // Non-protection keyword line — all matched by MTGJSON, no extracted keywords
        let keywords = extract_keyword_line(
            "Flying, first strike, lifelink",
            &[
                "flying".to_string(),
                "first strike".to_string(),
                "lifelink".to_string(),
            ],
        )
        .unwrap();
        assert!(
            keywords.is_empty(),
            "all keywords matched by MTGJSON, none extracted"
        );
    }

    #[test]
    fn expand_protection_three_way_inline_and_from() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        // Three-way inline split: "protection from red and from blue and from green"
        let keywords = extract_keyword_line(
            "Flying, protection from red and from blue and from green",
            &["flying".to_string(), "protection".to_string()],
        )
        .unwrap();
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Red
            )))
        );
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Blue
            )))
        );
        assert!(
            keywords.contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Green
            )))
        );
    }

    #[test]
    fn is_keyword_cost_line_rejects_trigger_text() {
        // "when you cycle a card" is trigger text, not a keyword cost line
        assert!(!is_keyword_cost_line("when you cycle a card"));
        assert!(!is_keyword_cost_line(
            "whenever you cycle or discard a card"
        ));
    }

    #[test]
    fn is_keyword_cost_line_em_dash() {
        // CR 702.138: Escape uses em-dash separator — must be recognized
        assert!(is_keyword_cost_line(
            "escape\u{2014}{w}, exile two other cards from your graveyard."
        ));
    }

    #[test]
    fn parse_partner_variant_oracle_text() {
        use crate::types::keywords::PartnerType;

        // CR 702.124: Partner variant keywords from Oracle text
        let kw = parse_keyword_from_oracle(
            "partner\u{2014}character select (you can have two commanders if both have this ability.)",
        ).unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::CharacterSelect));

        let kw = parse_keyword_from_oracle(
            "partner\u{2014}friends forever (you can have two commanders if both have this ability.)",
        ).unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::FriendsForever));

        let kw = parse_keyword_from_oracle(
            "choose a background (you can have a background as a second commander.)",
        )
        .unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::ChooseABackground));

        let kw = parse_keyword_from_oracle(
            "doctor\u{2019}s companion (you can have two commanders if the other is the doctor.)",
        )
        .unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::DoctorsCompanion));

        // Also test with straight apostrophe
        let kw = parse_keyword_from_oracle("doctor's companion").unwrap();
        assert_eq!(kw, Keyword::Partner(PartnerType::DoctorsCompanion));
    }

    // --- CR 702.11f: hexproof from X and from Y expansion ---

    #[test]
    fn expand_hexproof_from_compound() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        // CR 702.11f: "hexproof from white and from black" → two HexproofFrom keywords
        let expanded = expand_protection_parts(&["hexproof from white and from black"]);
        assert!(expanded.len() == 2);
        assert_eq!(expanded[0], "hexproof from white");
        assert_eq!(expanded[1], "hexproof from black");

        // Through extract_keyword_line
        let keywords = extract_keyword_line(
            "hexproof from white and from black",
            &["hexproof".to_string()],
        )
        .unwrap();
        assert!(keywords.len() == 2);
        assert_eq!(
            keywords[0],
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::White))
        );
        assert_eq!(
            keywords[1],
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Black))
        );
    }

    #[test]
    fn hexproof_from_single_no_expansion() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        // Single hexproof-from — no expansion needed
        let keywords =
            extract_keyword_line("hexproof from red", &["hexproof".to_string()]).unwrap();
        let hf: Vec<_> = keywords
            .iter()
            .filter(|k| matches!(k, Keyword::HexproofFrom(_)))
            .collect();
        assert_eq!(hf.len(), 1);
        assert_eq!(
            hf[0],
            &Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red))
        );
    }

    #[test]
    fn hexproof_from_oracle_parses() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        // parse_keyword_from_oracle handles "hexproof from red"
        let kw = parse_keyword_from_oracle("hexproof from red").unwrap();
        assert_eq!(
            kw,
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red))
        );

        let kw = parse_keyword_from_oracle("hexproof from artifacts").unwrap();
        assert_eq!(
            kw,
            Keyword::HexproofFrom(HexproofFilter::CardType("artifacts".to_string()))
        );
    }
}
