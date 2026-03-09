use std::convert::Infallible;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::mana::ManaColor;

/// What a Protection keyword protects from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtectionTarget {
    Color(ManaColor),
    CardType(String),
    Quality(String),
}

/// All MTG keywords as typed enum variants.
/// Simple (unit) variants for keywords with no parameters.
/// Parameterized variants carry associated data (cost strings, amounts, etc.).
/// Unknown captures any unrecognized keyword string for forward compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Keyword {
    // Evasion / Combat
    Flying,
    FirstStrike,
    DoubleStrike,
    Trample,
    Deathtouch,
    Lifelink,
    Vigilance,
    Haste,
    Reach,
    Defender,
    Menace,
    Indestructible,
    Hexproof,
    Shroud,
    Flash,
    Fear,
    Intimidate,
    Skulk,
    Shadow,
    Horsemanship,

    // Damage modification
    Wither,
    Infect,
    Afflict,

    // Triggered abilities
    Prowess,
    Undying,
    Persist,
    Cascade,
    Exalted,
    Flanking,
    Evolve,
    Extort,
    Exploit,
    Explore,
    Ascend,
    Dredge(u32),
    Modular(u32),
    Renown(u32),
    Fabricate(u32),
    Annihilator(u32),
    Bushido(u32),
    Tribute(u32),
    Soulbond,
    Unearth(String),

    // Cost reduction / alternative costs
    Convoke,
    Delve,
    Devoid,

    // Creature type / characteristics
    Changeling,

    // Phase / zone
    Phasing,

    // Combat triggers
    Battlecry,
    Decayed,
    Unleash,
    Riot,
    Afterlife(u32),

    // Equipment / attachment
    Reconfigure(String),
    LivingWeapon,
    TotemArmor,
    Bestow(String),

    // Graveyard
    Embalm(String),
    Eternalize(String),

    // Token / counter
    Fading(u32),
    Vanishing(u32),

    // Parameterized keywords
    Protection(ProtectionTarget),
    Kicker(String),
    Cycling(String),
    Flashback(String),
    Ward(String),
    Equip(String),
    Landwalk(String),
    Rampage(u32),
    Absorb(u32),
    Crew(u32),
    Partner(Option<String>),
    Companion(String),
    Ninjutsu(String),

    // Additional common keywords
    Prowl(String),
    Morph(String),
    Megamorph(String),
    Madness(String),
    Dash(String),
    Emerge(String),
    Escape(String),
    Evoke(String),
    Foretell(String),
    Mutate(String),
    Disturb(String),
    Disguise(String),
    Blitz(String),
    Overload(String),
    Spectacle(String),
    Surge(String),
    Encore(String),
    Buyback(String),
    Echo(String),
    Outlast(String),
    Scavenge(String),
    Fortify(String),
    Prototype(String),
    Plot(String),
    Craft(String),
    Offspring(String),
    Impending(String),

    // Simple keywords (no params)
    Banding,
    Cumulative,
    Epic,
    Fuse,
    Gravestorm,
    Haunt,
    Hideaway,
    Improvise,
    Ingest,
    Melee,
    Mentor,
    Myriad,
    Provoke,
    Rebound,
    Retrace,
    Ripple,
    SplitSecond,
    Storm,
    Suspend,
    Totem,
    Warp,
    Gift,
    Ravenous,
    Daybound,
    Nightbound,
    Enlist,
    ReadAhead,
    Compleated,
    Conspire,
    Demonstrate,
    Dethrone,
    DoubleTeam,
    LivingMetal,
    Poisonous(u32),
    Bloodthirst(u32),
    Amplify(u32),
    Graft(u32),
    Devour(u32),

    /// Fallback for unrecognized keywords.
    Unknown(String),
}

impl FromStr for Keyword {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Split on first colon for parameterized keywords
        let (name, param) = match s.find(':') {
            Some(idx) => (&s[..idx], Some(s[idx + 1..].to_string())),
            None => (s, None),
        };

        let name_lower = name.to_ascii_lowercase();

        // If there's a param, try parameterized keywords first
        if let Some(ref p) = param {
            match name_lower.as_str() {
                "protection" => return Ok(Keyword::Protection(parse_protection_target(p))),
                "kicker" => return Ok(Keyword::Kicker(p.clone())),
                "cycling" => return Ok(Keyword::Cycling(p.clone())),
                "flashback" => return Ok(Keyword::Flashback(p.clone())),
                "ward" => return Ok(Keyword::Ward(p.clone())),
                "equip" => return Ok(Keyword::Equip(p.clone())),
                "landwalk" => return Ok(Keyword::Landwalk(p.clone())),
                "rampage" => return Ok(Keyword::Rampage(p.parse().unwrap_or(1))),
                "bushido" => return Ok(Keyword::Bushido(p.parse().unwrap_or(1))),
                "absorb" => return Ok(Keyword::Absorb(p.parse().unwrap_or(1))),
                "fading" => return Ok(Keyword::Fading(p.parse().unwrap_or(0))),
                "vanishing" => return Ok(Keyword::Vanishing(p.parse().unwrap_or(0))),
                "crew" => return Ok(Keyword::Crew(p.parse().unwrap_or(1))),
                "partner" => return Ok(Keyword::Partner(Some(p.clone()))),
                "companion" => return Ok(Keyword::Companion(p.clone())),
                "ninjutsu" => return Ok(Keyword::Ninjutsu(p.clone())),
                "dredge" => return Ok(Keyword::Dredge(p.parse().unwrap_or(1))),
                "modular" => return Ok(Keyword::Modular(p.parse().unwrap_or(1))),
                "renown" => return Ok(Keyword::Renown(p.parse().unwrap_or(1))),
                "fabricate" => return Ok(Keyword::Fabricate(p.parse().unwrap_or(1))),
                "annihilator" => return Ok(Keyword::Annihilator(p.parse().unwrap_or(1))),
                "tribute" => return Ok(Keyword::Tribute(p.parse().unwrap_or(1))),
                "afterlife" => return Ok(Keyword::Afterlife(p.parse().unwrap_or(1))),
                "reconfigure" => return Ok(Keyword::Reconfigure(p.clone())),
                "bestow" => return Ok(Keyword::Bestow(p.clone())),
                "embalm" => return Ok(Keyword::Embalm(p.clone())),
                "eternalize" => return Ok(Keyword::Eternalize(p.clone())),
                "unearth" => return Ok(Keyword::Unearth(p.clone())),
                "prowl" => return Ok(Keyword::Prowl(p.clone())),
                "morph" => return Ok(Keyword::Morph(p.clone())),
                "megamorph" => return Ok(Keyword::Megamorph(p.clone())),
                "madness" => return Ok(Keyword::Madness(p.clone())),
                "dash" => return Ok(Keyword::Dash(p.clone())),
                "emerge" => return Ok(Keyword::Emerge(p.clone())),
                "escape" => return Ok(Keyword::Escape(p.clone())),
                "evoke" => return Ok(Keyword::Evoke(p.clone())),
                "foretell" => return Ok(Keyword::Foretell(p.clone())),
                "mutate" => return Ok(Keyword::Mutate(p.clone())),
                "disturb" => return Ok(Keyword::Disturb(p.clone())),
                "disguise" => return Ok(Keyword::Disguise(p.clone())),
                "blitz" => return Ok(Keyword::Blitz(p.clone())),
                "overload" => return Ok(Keyword::Overload(p.clone())),
                "spectacle" => return Ok(Keyword::Spectacle(p.clone())),
                "surge" => return Ok(Keyword::Surge(p.clone())),
                "encore" => return Ok(Keyword::Encore(p.clone())),
                "buyback" => return Ok(Keyword::Buyback(p.clone())),
                "echo" => return Ok(Keyword::Echo(p.clone())),
                "outlast" => return Ok(Keyword::Outlast(p.clone())),
                "scavenge" => return Ok(Keyword::Scavenge(p.clone())),
                "fortify" => return Ok(Keyword::Fortify(p.clone())),
                "prototype" => return Ok(Keyword::Prototype(p.clone())),
                "plot" => return Ok(Keyword::Plot(p.clone())),
                "craft" => return Ok(Keyword::Craft(p.clone())),
                "offspring" => return Ok(Keyword::Offspring(p.clone())),
                "impending" => return Ok(Keyword::Impending(p.clone())),
                "poisonous" => return Ok(Keyword::Poisonous(p.parse().unwrap_or(1))),
                "bloodthirst" => return Ok(Keyword::Bloodthirst(p.parse().unwrap_or(1))),
                "amplify" => return Ok(Keyword::Amplify(p.parse().unwrap_or(1))),
                "graft" => return Ok(Keyword::Graft(p.parse().unwrap_or(1))),
                "devour" => return Ok(Keyword::Devour(p.parse().unwrap_or(1))),
                "afflict" => return Ok(Keyword::Afflict),
                _ => return Ok(Keyword::Unknown(s.to_string())),
            }
        }

        // Simple (unit) keywords -- case-insensitive match
        match name_lower.as_str() {
            "flying" => Ok(Keyword::Flying),
            "first strike" => Ok(Keyword::FirstStrike),
            "double strike" => Ok(Keyword::DoubleStrike),
            "trample" => Ok(Keyword::Trample),
            "deathtouch" => Ok(Keyword::Deathtouch),
            "lifelink" => Ok(Keyword::Lifelink),
            "vigilance" => Ok(Keyword::Vigilance),
            "haste" => Ok(Keyword::Haste),
            "reach" => Ok(Keyword::Reach),
            "defender" => Ok(Keyword::Defender),
            "menace" => Ok(Keyword::Menace),
            "indestructible" => Ok(Keyword::Indestructible),
            "hexproof" => Ok(Keyword::Hexproof),
            "shroud" => Ok(Keyword::Shroud),
            "flash" => Ok(Keyword::Flash),
            "fear" => Ok(Keyword::Fear),
            "intimidate" => Ok(Keyword::Intimidate),
            "skulk" => Ok(Keyword::Skulk),
            "shadow" => Ok(Keyword::Shadow),
            "horsemanship" => Ok(Keyword::Horsemanship),
            "wither" => Ok(Keyword::Wither),
            "infect" => Ok(Keyword::Infect),
            "afflict" => Ok(Keyword::Afflict),
            "prowess" => Ok(Keyword::Prowess),
            "undying" => Ok(Keyword::Undying),
            "persist" => Ok(Keyword::Persist),
            "cascade" => Ok(Keyword::Cascade),
            "convoke" => Ok(Keyword::Convoke),
            "delve" => Ok(Keyword::Delve),
            "devoid" => Ok(Keyword::Devoid),
            "exalted" => Ok(Keyword::Exalted),
            "flanking" => Ok(Keyword::Flanking),
            "changeling" => Ok(Keyword::Changeling),
            "phasing" => Ok(Keyword::Phasing),
            "battlecry" | "battle cry" => Ok(Keyword::Battlecry),
            "decayed" => Ok(Keyword::Decayed),
            "unleash" => Ok(Keyword::Unleash),
            "riot" => Ok(Keyword::Riot),
            "living weapon" => Ok(Keyword::LivingWeapon),
            "totem armor" => Ok(Keyword::TotemArmor),
            "evolve" => Ok(Keyword::Evolve),
            "extort" => Ok(Keyword::Extort),
            "exploit" => Ok(Keyword::Exploit),
            "explore" => Ok(Keyword::Explore),
            "ascend" => Ok(Keyword::Ascend),
            "soulbond" => Ok(Keyword::Soulbond),
            "partner" => Ok(Keyword::Partner(None)),
            "banding" => Ok(Keyword::Banding),
            "epic" => Ok(Keyword::Epic),
            "fuse" => Ok(Keyword::Fuse),
            "gravestorm" => Ok(Keyword::Gravestorm),
            "haunt" => Ok(Keyword::Haunt),
            "improvise" => Ok(Keyword::Improvise),
            "ingest" => Ok(Keyword::Ingest),
            "melee" => Ok(Keyword::Melee),
            "mentor" => Ok(Keyword::Mentor),
            "myriad" => Ok(Keyword::Myriad),
            "provoke" => Ok(Keyword::Provoke),
            "rebound" => Ok(Keyword::Rebound),
            "retrace" => Ok(Keyword::Retrace),
            "split second" => Ok(Keyword::SplitSecond),
            "storm" => Ok(Keyword::Storm),
            "suspend" => Ok(Keyword::Suspend),
            "gift" => Ok(Keyword::Gift),
            "ravenous" => Ok(Keyword::Ravenous),
            "daybound" => Ok(Keyword::Daybound),
            "nightbound" => Ok(Keyword::Nightbound),
            "enlist" => Ok(Keyword::Enlist),
            "read ahead" => Ok(Keyword::ReadAhead),
            "compleated" => Ok(Keyword::Compleated),
            "conspire" => Ok(Keyword::Conspire),
            "demonstrate" => Ok(Keyword::Demonstrate),
            "dethrone" => Ok(Keyword::Dethrone),
            "double team" => Ok(Keyword::DoubleTeam),
            "living metal" => Ok(Keyword::LivingMetal),
            _ => Ok(Keyword::Unknown(s.to_string())),
        }
    }
}

fn parse_protection_target(s: &str) -> ProtectionTarget {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "white" => ProtectionTarget::Color(ManaColor::White),
        "blue" => ProtectionTarget::Color(ManaColor::Blue),
        "black" => ProtectionTarget::Color(ManaColor::Black),
        "red" => ProtectionTarget::Color(ManaColor::Red),
        "green" => ProtectionTarget::Color(ManaColor::Green),
        _ if lower.starts_with("from ") => ProtectionTarget::Quality(s.to_string()),
        _ => ProtectionTarget::CardType(s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_keywords() {
        assert_eq!(Keyword::from_str("Flying").unwrap(), Keyword::Flying);
        assert_eq!(Keyword::from_str("flying").unwrap(), Keyword::Flying);
        assert_eq!(Keyword::from_str("FLYING").unwrap(), Keyword::Flying);
        assert_eq!(Keyword::from_str("Haste").unwrap(), Keyword::Haste);
        assert_eq!(
            Keyword::from_str("Deathtouch").unwrap(),
            Keyword::Deathtouch
        );
        assert_eq!(
            Keyword::from_str("Indestructible").unwrap(),
            Keyword::Indestructible
        );
        assert_eq!(Keyword::from_str("Hexproof").unwrap(), Keyword::Hexproof);
        assert_eq!(Keyword::from_str("Shroud").unwrap(), Keyword::Shroud);
        assert_eq!(Keyword::from_str("Flash").unwrap(), Keyword::Flash);
    }

    #[test]
    fn parse_multi_word_keywords() {
        assert_eq!(
            Keyword::from_str("First Strike").unwrap(),
            Keyword::FirstStrike
        );
        assert_eq!(
            Keyword::from_str("first strike").unwrap(),
            Keyword::FirstStrike
        );
        assert_eq!(
            Keyword::from_str("Double Strike").unwrap(),
            Keyword::DoubleStrike
        );
        assert_eq!(
            Keyword::from_str("Living Weapon").unwrap(),
            Keyword::LivingWeapon
        );
        assert_eq!(
            Keyword::from_str("Totem Armor").unwrap(),
            Keyword::TotemArmor
        );
        assert_eq!(
            Keyword::from_str("Split Second").unwrap(),
            Keyword::SplitSecond
        );
        assert_eq!(Keyword::from_str("Battle Cry").unwrap(), Keyword::Battlecry);
    }

    #[test]
    fn parse_parameterized_keywords() {
        assert_eq!(
            Keyword::from_str("Kicker:1G").unwrap(),
            Keyword::Kicker("1G".to_string())
        );
        assert_eq!(
            Keyword::from_str("Cycling:2").unwrap(),
            Keyword::Cycling("2".to_string())
        );
        assert_eq!(
            Keyword::from_str("Flashback:3BB").unwrap(),
            Keyword::Flashback("3BB".to_string())
        );
        assert_eq!(
            Keyword::from_str("Ward:2").unwrap(),
            Keyword::Ward("2".to_string())
        );
        assert_eq!(
            Keyword::from_str("Equip:3").unwrap(),
            Keyword::Equip("3".to_string())
        );
        assert_eq!(Keyword::from_str("Crew:3").unwrap(), Keyword::Crew(3));
        assert_eq!(Keyword::from_str("Rampage:2").unwrap(), Keyword::Rampage(2));
    }

    #[test]
    fn parse_protection_variants() {
        assert_eq!(
            Keyword::from_str("Protection:Red").unwrap(),
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Red))
        );
        assert_eq!(
            Keyword::from_str("Protection:from everything").unwrap(),
            Keyword::Protection(ProtectionTarget::Quality("from everything".to_string()))
        );
        assert_eq!(
            Keyword::from_str("Protection:Artifacts").unwrap(),
            Keyword::Protection(ProtectionTarget::CardType("Artifacts".to_string()))
        );
    }

    #[test]
    fn parse_partner_variants() {
        assert_eq!(
            Keyword::from_str("Partner").unwrap(),
            Keyword::Partner(None)
        );
        assert_eq!(
            Keyword::from_str("Partner:Brallin, Skyshark Rider").unwrap(),
            Keyword::Partner(Some("Brallin, Skyshark Rider".to_string()))
        );
    }

    #[test]
    fn parse_unknown_keyword() {
        assert_eq!(
            Keyword::from_str("NotARealKeyword").unwrap(),
            Keyword::Unknown("NotARealKeyword".to_string())
        );
    }

    #[test]
    fn keyword_never_fails() {
        // FromStr returns Result<Self, Infallible> -- always Ok
        assert!(Keyword::from_str("").unwrap() == Keyword::Unknown("".to_string()));
        assert!(Keyword::from_str("xyz:abc").unwrap() == Keyword::Unknown("xyz:abc".to_string()));
    }

    #[test]
    fn keyword_serialization_roundtrip() {
        let keywords = vec![
            Keyword::Flying,
            Keyword::Kicker("1G".to_string()),
            Keyword::Protection(ProtectionTarget::Color(ManaColor::Blue)),
            Keyword::Unknown("CustomKeyword".to_string()),
        ];
        let json = serde_json::to_string(&keywords).unwrap();
        let deserialized: Vec<Keyword> = serde_json::from_str(&json).unwrap();
        assert_eq!(keywords, deserialized);
    }

    #[test]
    fn keyword_count_over_fifty() {
        // Ensure we have 50+ keyword variants (excluding Unknown)
        let test_keywords = vec![
            "Flying",
            "First Strike",
            "Double Strike",
            "Trample",
            "Deathtouch",
            "Lifelink",
            "Vigilance",
            "Haste",
            "Reach",
            "Defender",
            "Menace",
            "Indestructible",
            "Hexproof",
            "Shroud",
            "Flash",
            "Fear",
            "Intimidate",
            "Skulk",
            "Shadow",
            "Horsemanship",
            "Wither",
            "Infect",
            "Afflict",
            "Prowess",
            "Undying",
            "Persist",
            "Cascade",
            "Convoke",
            "Delve",
            "Devoid",
            "Exalted",
            "Flanking",
            "Changeling",
            "Phasing",
            "Battle Cry",
            "Decayed",
            "Unleash",
            "Riot",
            "Living Weapon",
            "Totem Armor",
            "Evolve",
            "Extort",
            "Exploit",
            "Explore",
            "Ascend",
            "Soulbond",
            "Partner",
            "Banding",
            "Epic",
            "Fuse",
            "Improvise",
            "Ingest",
            "Melee",
            "Mentor",
            "Myriad",
        ];
        let mut non_unknown = 0;
        for kw in &test_keywords {
            let parsed = Keyword::from_str(kw).unwrap();
            if !matches!(parsed, Keyword::Unknown(_)) {
                non_unknown += 1;
            }
        }
        assert!(
            non_unknown >= 50,
            "Expected 50+ known keywords, got {non_unknown}"
        );
    }
}
