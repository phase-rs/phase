use std::collections::HashMap;

use crate::types::card::{CardFace, CardLayout, CardRules};
use crate::types::card_type::CardType;
use crate::types::mana::{ManaColor, ManaCost};

use super::{card_type, mana_cost, ParseError};

struct CardFaceBuilder {
    name: Option<String>,
    mana_cost: Option<ManaCost>,
    card_type: Option<CardType>,
    power: Option<String>,
    toughness: Option<String>,
    loyalty: Option<String>,
    defense: Option<String>,
    oracle_text: Option<String>,
    non_ability_text: Option<String>,
    flavor_name: Option<String>,
    color_override: Option<Vec<ManaColor>>,
    keywords: Vec<String>,
    abilities: Vec<String>,
    triggers: Vec<String>,
    static_abilities: Vec<String>,
    replacements: Vec<String>,
    svars: HashMap<String, String>,
}

impl CardFaceBuilder {
    fn new() -> Self {
        Self {
            name: None,
            mana_cost: None,
            card_type: None,
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            color_override: None,
            keywords: Vec::new(),
            abilities: Vec::new(),
            triggers: Vec::new(),
            static_abilities: Vec::new(),
            replacements: Vec::new(),
            svars: HashMap::new(),
        }
    }

    fn build(self) -> Result<CardFace, ParseError> {
        let name = self
            .name
            .ok_or_else(|| ParseError::MissingField("name".to_string()))?;
        Ok(CardFace {
            name,
            mana_cost: self.mana_cost.unwrap_or_default(),
            card_type: self.card_type.unwrap_or_default(),
            power: self.power,
            toughness: self.toughness,
            loyalty: self.loyalty,
            defense: self.defense,
            oracle_text: self.oracle_text,
            non_ability_text: self.non_ability_text,
            flavor_name: self.flavor_name,
            color_override: self.color_override,
            keywords: self.keywords,
            abilities: self.abilities,
            triggers: self.triggers,
            static_abilities: self.static_abilities,
            replacements: self.replacements,
            svars: self.svars,
        })
    }
}

struct ParseState {
    cur_face: usize,
    alt_mode: Option<String>,
    meld_with: Option<String>,
    partner_with: Option<String>,
}

pub fn parse_card_file(content: &str) -> Result<CardRules, ParseError> {
    let mut faces = [CardFaceBuilder::new(), CardFaceBuilder::new()];
    let mut state = ParseState {
        cur_face: 0,
        alt_mode: None,
        meld_with: None,
        partner_with: None,
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        parse_line(trimmed, &mut faces, &mut state);
    }

    let [face0, face1] = faces;
    let layout = match state.alt_mode.as_deref() {
        None => CardLayout::Single(face0.build()?),
        Some(mode) => {
            let f0 = face0.build()?;
            let f1 = face1.build()?;
            match mode {
                "Split" => CardLayout::Split(f0, f1),
                "Flip" => CardLayout::Flip(f0, f1),
                "Transform" | "DoubleFaced" => CardLayout::Transform(f0, f1),
                "Meld" => CardLayout::Meld(f0, f1),
                "Adventure" => CardLayout::Adventure(f0, f1),
                "Modal" => CardLayout::Modal(f0, f1),
                "Omen" => CardLayout::Omen(f0, f1),
                _ => CardLayout::Single(f0), // Unknown mode falls back to single
            }
        }
    };

    Ok(CardRules {
        layout,
        meld_with: state.meld_with,
        partner_with: state.partner_with,
    })
}

fn parse_line(line: &str, faces: &mut [CardFaceBuilder; 2], state: &mut ParseState) {
    // Handle bare keywords (no colon)
    let Some((key, value)) = line.split_once(':') else {
        if line == "ALTERNATE" {
            state.cur_face = 1;
        }
        return;
    };

    let value = value.trim();
    let face = &mut faces[state.cur_face];

    match key.as_bytes().first() {
        Some(b'A') => match key {
            "A" => face.abilities.push(value.to_string()),
            "AlternateMode" => state.alt_mode = Some(value.to_string()),
            _ => {} // skip unknown
        },
        Some(b'C') => if key == "Colors" {
            let colors: Vec<ManaColor> =
                value.split(',').filter_map(parse_color).collect();
            if !colors.is_empty() {
                face.color_override = Some(colors);
            }
        },
        Some(b'D') => match key {
            "Defense" => face.defense = Some(value.to_string()),
            "DeckHints" | "DeckNeeds" | "DeckHas" => {} // deferred
            _ => {}
        },
        Some(b'F') => if key == "FlavorName" { face.flavor_name = Some(value.to_string()) },
        Some(b'K') => if key == "K" {
            for kw in value.split(',') {
                let kw = kw.trim();
                if !kw.is_empty() {
                    face.keywords.push(kw.to_string());
                }
            }
        },
        Some(b'L') => if key == "Loyalty" { face.loyalty = Some(value.to_string()) },
        Some(b'M') => match key {
            "ManaCost" => {
                if let Ok(cost) = mana_cost::parse(value) {
                    face.mana_cost = Some(cost);
                }
            }
            "MeldPair" => state.meld_with = Some(value.to_string()),
            _ => {}
        },
        Some(b'N') => if key == "Name" { face.name = Some(value.to_string()) },
        Some(b'O') => if key == "Oracle" { face.oracle_text = Some(value.to_string()) },
        Some(b'P') => match key {
            "PT" => {
                if let Some((p, t)) = value.split_once('/') {
                    face.power = Some(p.to_string());
                    face.toughness = Some(t.to_string());
                }
            }
            "PartnerWith" => state.partner_with = Some(value.to_string()),
            _ => {}
        },
        Some(b'R') => if key == "R" { face.replacements.push(value.to_string()) },
        Some(b'S') => match key {
            "S" => face.static_abilities.push(value.to_string()),
            "SVar" => {
                if let Some((var_name, var_value)) = value.split_once(':') {
                    face.svars
                        .insert(var_name.to_string(), var_value.to_string());
                }
            }
            _ => {}
        },
        Some(b'T') => match key {
            "T" => face.triggers.push(value.to_string()),
            "Types" => face.card_type = Some(card_type::parse(value)),
            "Text" => face.non_ability_text = Some(value.to_string()),
            _ => {}
        },
        _ => {} // silently skip unknown keys
    }
}

fn parse_color(s: &str) -> Option<ManaColor> {
    match s.trim() {
        "White" | "W" => Some(ManaColor::White),
        "Blue" | "U" => Some(ManaColor::Blue),
        "Black" | "B" => Some(ManaColor::Black),
        "Red" | "R" => Some(ManaColor::Red),
        "Green" | "G" => Some(ManaColor::Green),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::card_type::CoreType;
    use crate::types::mana::ManaCostShard;

    #[test]
    fn parse_lightning_bolt() {
        let input = "\
Name:Lightning Bolt
ManaCost:R
Types:Instant
A:SP$ DealDamage | Cost$ R | NumDmg$ 3 | ValidTgts$ Any
Oracle:Lightning Bolt deals 3 damage to any target.
SVar:Picture:https://example.com/bolt.jpg";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.name, "Lightning Bolt");
                assert_eq!(
                    face.mana_cost,
                    ManaCost::Cost {
                        shards: vec![ManaCostShard::Red],
                        generic: 0
                    }
                );
                assert_eq!(face.card_type.core_types, vec![CoreType::Instant]);
                assert_eq!(face.abilities.len(), 1);
                assert!(face.abilities[0].starts_with("SP$ DealDamage"));
                assert_eq!(
                    face.oracle_text.as_deref(),
                    Some("Lightning Bolt deals 3 damage to any target.")
                );
                assert_eq!(
                    face.svars.get("Picture").map(|s| s.as_str()),
                    Some("https://example.com/bolt.jpg")
                );
            }
            _ => panic!("Expected Single layout"),
        }
        assert!(rules.meld_with.is_none());
        assert!(rules.partner_with.is_none());
    }

    #[test]
    fn parse_creature_with_pt_and_keywords() {
        let input = "\
Name:Goblin Guide
ManaCost:R
Types:Creature Goblin Scout
PT:2/2
K:Haste
T:Mode$ ChangesZone | Origin$ Battlefield | Destination$ Battlefield | ValidCard$ Card.Self | Execute$ TrigDraw | TriggerDescription$ Whenever CARDNAME attacks, defending player reveals the top card of their library.
Oracle:Haste\\nWhenever Goblin Guide attacks, defending player reveals the top card of their library.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.name, "Goblin Guide");
                assert_eq!(face.power.as_deref(), Some("2"));
                assert_eq!(face.toughness.as_deref(), Some("2"));
                assert_eq!(face.keywords, vec!["Haste"]);
                assert_eq!(face.triggers.len(), 1);
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_adventure_bonecrusher_giant() {
        let input = "\
Name:Bonecrusher Giant
ManaCost:2 R
Types:Creature Giant
PT:4/3
K:Trample
Oracle:Trample\\nWhenever Bonecrusher Giant becomes the target of a spell, Bonecrusher Giant deals 2 damage to that spell's controller.
ALTERNATE
Name:Stomp
ManaCost:1 R
Types:Instant Adventure
A:SP$ DealDamage | Cost$ 1 R | NumDmg$ 2 | ValidTgts$ Any
AlternateMode:Adventure
Oracle:Deal 2 damage to any target. Damage can't be prevented this turn.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Adventure(main, adv) => {
                assert_eq!(main.name, "Bonecrusher Giant");
                assert_eq!(main.power.as_deref(), Some("4"));
                assert_eq!(main.toughness.as_deref(), Some("3"));
                assert_eq!(adv.name, "Stomp");
                assert_eq!(adv.card_type.core_types, vec![CoreType::Instant]);
            }
            _ => panic!("Expected Adventure layout, got {:?}", rules.layout),
        }
    }

    #[test]
    fn parse_transform_nicol_bolas() {
        let input = "\
Name:Nicol Bolas, the Ravager
ManaCost:1 U B R
Types:Legendary Creature Elder Dragon
PT:4/4
K:Flying
Oracle:Flying\\nWhen Nicol Bolas, the Ravager enters the battlefield, each opponent discards a card.
ALTERNATE
Name:Nicol Bolas, the Arisen
ManaCost:no cost
Types:Legendary Planeswalker Bolas
Loyalty:7
AlternateMode:DoubleFaced
Oracle:+2: Draw two cards.\\n-3: Nicol Bolas deals 10 damage to target creature or planeswalker.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Transform(front, back) => {
                assert_eq!(front.name, "Nicol Bolas, the Ravager");
                assert_eq!(back.name, "Nicol Bolas, the Arisen");
                assert_eq!(back.mana_cost, ManaCost::NoCost);
                assert_eq!(back.loyalty.as_deref(), Some("7"));
            }
            _ => panic!("Expected Transform layout, got {:?}", rules.layout),
        }
    }

    #[test]
    fn parse_split_fire_ice() {
        let input = "\
Name:Fire
ManaCost:1 R
Types:Instant
A:SP$ DealDamage | Cost$ 1 R | NumDmg$ 2
Oracle:Fire deals 2 damage divided as you choose among one or two targets.
ALTERNATE
Name:Ice
ManaCost:1 U
Types:Instant
A:SP$ Tap | Cost$ 1 U | ValidTgts$ Permanent
AlternateMode:Split
Oracle:Tap target permanent. Draw a card.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Split(a, b) => {
                assert_eq!(a.name, "Fire");
                assert_eq!(b.name, "Ice");
            }
            _ => panic!("Expected Split layout, got {:?}", rules.layout),
        }
    }

    #[test]
    fn parse_meld_gisela() {
        let input = "\
Name:Gisela, the Broken Blade
ManaCost:2 W W
Types:Legendary Creature Angel Horror
PT:4/3
K:Flying, First Strike, Lifelink
Oracle:Flying, first strike, lifelink\\nAt the beginning of your end step, if you both own and control Gisela and Bruna, exile them, then meld them.
ALTERNATE
Name:Brisela, Voice of Nightmares
ManaCost:no cost
Types:Legendary Creature Eldrazi Angel
PT:9/10
K:Flying, First Strike, Lifelink
AlternateMode:Meld
MeldPair:Bruna, the Fading Light
Oracle:Flying, first strike, lifelink\\nYour opponents can't cast spells with mana value 3 or less.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Meld(front, back) => {
                assert_eq!(front.name, "Gisela, the Broken Blade");
                assert_eq!(back.name, "Brisela, Voice of Nightmares");
            }
            _ => panic!("Expected Meld layout, got {:?}", rules.layout),
        }
        assert_eq!(rules.meld_with.as_deref(), Some("Bruna, the Fading Light"));
    }

    #[test]
    fn parse_flip_akki_lavarunner() {
        let input = "\
Name:Akki Lavarunner
ManaCost:3 R
Types:Creature Goblin Warrior
PT:1/1
K:Haste
Oracle:Haste\\nWhenever Akki Lavarunner deals damage to an opponent, flip it.
ALTERNATE
Name:Tok-Tok, Volcano Born
ManaCost:no cost
Types:Legendary Creature Goblin Shaman
PT:2/2
AlternateMode:Flip
Oracle:Protection from red\\nIf a red source would deal damage to a player, it deals that much damage plus 1 to that player instead.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Flip(front, back) => {
                assert_eq!(front.name, "Akki Lavarunner");
                assert_eq!(back.name, "Tok-Tok, Volcano Born");
            }
            _ => panic!("Expected Flip layout, got {:?}", rules.layout),
        }
    }

    #[test]
    fn parse_mdfc_valki() {
        let input = "\
Name:Valki, God of Lies
ManaCost:1 B
Types:Legendary Creature God
PT:2/1
Oracle:When Valki enters the battlefield, each opponent reveals their hand.
ALTERNATE
Name:Tibalt, Cosmic Impostor
ManaCost:5 B R
Types:Legendary Planeswalker Tibalt
Loyalty:5
AlternateMode:Modal
Oracle:As Tibalt enters the battlefield, you get an emblem.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Modal(front, back) => {
                assert_eq!(front.name, "Valki, God of Lies");
                assert_eq!(back.name, "Tibalt, Cosmic Impostor");
                assert_eq!(back.loyalty.as_deref(), Some("5"));
            }
            _ => panic!("Expected Modal layout, got {:?}", rules.layout),
        }
    }

    #[test]
    fn parse_comments_and_unknown_keys() {
        let input = "\
# This is a comment
Name:Test Card
ManaCost:1
Types:Instant

UnknownKey:some value
FutureFeature:blah
Oracle:Test oracle text.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.name, "Test Card");
                assert_eq!(
                    face.mana_cost,
                    ManaCost::Cost {
                        shards: vec![],
                        generic: 1
                    }
                );
                assert_eq!(face.oracle_text.as_deref(), Some("Test oracle text."));
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_svar_double_colon() {
        let input = "\
Name:Svar Test
ManaCost:0
Types:Artifact
SVar:RemAIDeck:True
SVar:Picture:http://example.com/pic.jpg";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(
                    face.svars.get("RemAIDeck").map(|s| s.as_str()),
                    Some("True")
                );
                assert_eq!(
                    face.svars.get("Picture").map(|s| s.as_str()),
                    Some("http://example.com/pic.jpg")
                );
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_colors_override() {
        let input = "\
Name:Transguild Courier
ManaCost:4
Types:Artifact Creature Golem
PT:3/3
Colors:White,Blue,Black,Red,Green
Oracle:Transguild Courier is all colors.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                let colors = face.color_override.as_ref().unwrap();
                assert_eq!(colors.len(), 5);
                assert!(colors.contains(&ManaColor::White));
                assert!(colors.contains(&ManaColor::Blue));
                assert!(colors.contains(&ManaColor::Black));
                assert!(colors.contains(&ManaColor::Red));
                assert!(colors.contains(&ManaColor::Green));
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_star_pt() {
        let input = "\
Name:Tarmogoyf
ManaCost:1 G
Types:Creature Lhurgoyf
PT:*/*+1
Oracle:Tarmogoyf's power is equal to the number of card types among cards in all graveyards and its toughness is equal to that number plus 1.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.power.as_deref(), Some("*"));
                assert_eq!(face.toughness.as_deref(), Some("*+1"));
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_multiple_keywords() {
        let input = "\
Name:Questing Beast
ManaCost:2 G G
Types:Legendary Creature Beast
PT:4/4
K:Vigilance, Deathtouch, Haste
Oracle:Vigilance, deathtouch, haste";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.keywords, vec!["Vigilance", "Deathtouch", "Haste"]);
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_planeswalker_with_loyalty() {
        let input = "\
Name:Jace, the Mind Sculptor
ManaCost:2 U U
Types:Legendary Planeswalker Jace
Loyalty:3
A:AB$ Draw | Cost$ AddCounter<1/LOYALTY> | Defined$ You | NumCards$ 1
Oracle:+2: Look at the top card of target player's library.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.loyalty.as_deref(), Some("3"));
                assert_eq!(face.abilities.len(), 1);
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_defense() {
        let input = "\
Name:Invasion of Zendikar
ManaCost:3 G
Types:Battle Siege
Defense:3
Oracle:When Invasion of Zendikar enters the battlefield, search your library for up to two basic land cards.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.defense.as_deref(), Some("3"));
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_static_and_replacement() {
        let input = "\
Name:Test Static
ManaCost:1
Types:Enchantment
S:Mode$ Continuous | Affected$ Creature.YouCtrl | AddPower$ 1
R:Event$ Moved | ValidCard$ Card.Self | Destination$ Graveyard | ReplaceWith$ Exile
Oracle:Test card.";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.static_abilities.len(), 1);
                assert!(face.static_abilities[0].starts_with("Mode$ Continuous"));
                assert_eq!(face.replacements.len(), 1);
                assert!(face.replacements[0].starts_with("Event$ Moved"));
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_text_and_flavor_name() {
        let input = "\
Name:Godzilla Card
ManaCost:3 R R
Types:Creature Dinosaur
PT:7/6
Text:This creature is powerful.
FlavorName:Godzilla, King of the Monsters
Oracle:Trample, haste";

        let rules = parse_card_file(input).unwrap();
        match &rules.layout {
            CardLayout::Single(face) => {
                assert_eq!(
                    face.non_ability_text.as_deref(),
                    Some("This creature is powerful.")
                );
                assert_eq!(
                    face.flavor_name.as_deref(),
                    Some("Godzilla, King of the Monsters")
                );
            }
            _ => panic!("Expected Single layout"),
        }
    }

    #[test]
    fn parse_partner_with() {
        let input = "\
Name:Blaring Captain
ManaCost:3 B
Types:Creature Azra Warrior
PT:2/2
PartnerWith:Blaring Recruiter
Oracle:Partner with Blaring Recruiter";

        let rules = parse_card_file(input).unwrap();
        assert_eq!(rules.partner_with.as_deref(), Some("Blaring Recruiter"));
    }
}
