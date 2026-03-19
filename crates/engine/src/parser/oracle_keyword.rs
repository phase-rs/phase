use crate::types::keywords::Keyword;

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

    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.is_empty() {
        return None;
    }

    let mut any_mtgjson_match = false;
    let mut new_keywords = Vec::new();

    for part in &parts {
        let lower = part.to_lowercase();

        // Check if this part matches or extends an MTGJSON keyword name.
        // Exact match: "flying" == "flying"
        // Prefix match: "protection from multicolored" starts with "protection"
        let mtgjson_match = mtgjson_keyword_names
            .iter()
            .any(|name| lower == *name || lower.starts_with(&format!("{name} ")));

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

/// Parse a keyword from Oracle text format (natural language) into a `Keyword`.
///
/// Oracle text uses space-separated format: "protection from red", "ward {2}",
/// "flashback {2}{U}". Converts to the colon format that `FromStr` expects,
/// handling the "from" preposition used by protection keywords.
pub(crate) fn parse_keyword_from_oracle(text: &str) -> Option<Keyword> {
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
        "channel",
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
    ];
    keyword_costs.iter().any(|kw| {
        lower.starts_with(kw)
            && (lower.len() == kw.len()
                || lower.as_bytes().get(kw.len()) == Some(&b' ')
                || lower.as_bytes().get(kw.len()) == Some(&b'\t'))
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

    #[test]
    fn is_keyword_cost_line_rejects_trigger_text() {
        // "when you cycle a card" is trigger text, not a keyword cost line
        assert!(!is_keyword_cost_line("when you cycle a card"));
        assert!(!is_keyword_cost_line(
            "whenever you cycle or discard a card"
        ));
    }
}
