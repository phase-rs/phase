use engine::parser::oracle::{keyword_display_name, parse_oracle_text};
use engine::types::keywords::Keyword;

fn parse(
    oracle_text: &str,
    card_name: &str,
    keywords: &[Keyword],
    types: &[&str],
    subtypes: &[&str],
) -> engine::parser::oracle::ParsedAbilities {
    let keyword_names: Vec<String> = keywords.iter().map(keyword_display_name).collect();
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle_text, card_name, &keyword_names, &types, &subtypes)
}

#[test]
fn snapshot_lightning_bolt() {
    let result = parse(
        "Lightning Bolt deals 3 damage to any target.",
        "Lightning Bolt",
        &[],
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_murder() {
    let result = parse("Destroy target creature.", "Murder", &[], &["Instant"], &[]);
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_counterspell() {
    let result = parse(
        "Counter target spell.",
        "Counterspell",
        &[],
        &["Instant"],
        &[],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_bonesplitter() {
    let result = parse(
        "Equipped creature gets +2/+0.\nEquip {1}",
        "Bonesplitter",
        &[],
        &["Artifact"],
        &["Equipment"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_questing_beast() {
    let result = parse(
        "Vigilance, deathtouch, haste\nQuesting Beast can't be blocked by creatures with power 2 or less.\nCombat damage that would be dealt by creatures you control can't be prevented.\nWhenever Questing Beast deals combat damage to a planeswalker, it deals that much damage to target planeswalker that player controls.",
        "Questing Beast",
        &[Keyword::Vigilance, Keyword::Deathtouch, Keyword::Haste],
        &["Creature"],
        &["Beast"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_baneslayer_angel() {
    let result = parse(
        "Flying, first strike, lifelink, protection from Demons and from Dragons",
        "Baneslayer Angel",
        &[Keyword::Flying, Keyword::FirstStrike, Keyword::Lifelink],
        &["Creature"],
        &["Angel"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_jace_the_mind_sculptor() {
    let result = parse(
        "+2: Look at the top card of target player's library. You may put that card on the bottom of that player's library.\n0: Draw three cards, then put two cards from your hand on top of your library in any order.\n\u{2212}1: Return target creature to its owner's hand.\n\u{2212}12: Exile all cards from target player's library, then that player shuffles their hand into their library.",
        "Jace, the Mind Sculptor",
        &[],
        &["Planeswalker"],
        &["Jace"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_forest() {
    let result = parse("({T}: Add {G}.)", "Forest", &[], &["Land"], &["Forest"]);
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_mox_pearl() {
    let result = parse("{T}: Add {W}.", "Mox Pearl", &[], &["Artifact"], &[]);
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_llanowar_elves() {
    let result = parse(
        "{T}: Add {G}.",
        "Llanowar Elves",
        &[],
        &["Creature"],
        &["Elf", "Druid"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_rancor() {
    let result = parse(
        "Enchant creature\nEnchanted creature gets +2/+0 and has trample.\nWhen Rancor is put into a graveyard from the battlefield, return Rancor to its owner's hand.",
        "Rancor",
        &[],
        &["Enchantment"],
        &["Aura"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_goblin_chainwhirler() {
    let result = parse(
        "First strike\nWhen Goblin Chainwhirler enters the battlefield, it deals 1 damage to each opponent and each creature and planeswalker they control.",
        "Goblin Chainwhirler",
        &[Keyword::FirstStrike],
        &["Creature"],
        &["Goblin", "Warrior"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn snapshot_wizard_class() {
    // CR 716: Class enchantment with all three level patterns:
    // Level 1 static, "When this Class becomes level 2" trigger, Level 3 continuous trigger
    let result = parse(
        "(Gain the next level as a sorcery to add its ability.)\nYou have no maximum hand size.\n{2}{U}: Level 2\nWhen this Class becomes level 2, draw two cards.\n{4}{U}: Level 3\nWhenever you draw a card, put a +1/+1 counter on target creature you control.",
        "Wizard Class",
        &[],
        &["Enchantment"],
        &["Class"],
    );
    insta::assert_json_snapshot!(result);
}

#[test]
fn class_structural_correctness() {
    // CR 716: Verify structural correctness of Class parsing
    let result = parse(
        "(Gain the next level as a sorcery to add its ability.)\nIf you would roll one or more dice, instead roll that many dice plus one and ignore the lowest roll.\n{1}{R}: Level 2\nWhenever you roll one or more dice, target creature you control gets +2/+0 and gains menace until end of turn.\n{2}{R}: Level 3\nCreatures you control have haste.",
        "Barbarian Class",
        &[],
        &["Enchantment"],
        &["Class"],
    );

    // 2 SetClassLevel activated abilities (Level 2 and Level 3)
    let set_class_levels: Vec<_> = result
        .abilities
        .iter()
        .filter(|a| {
            matches!(
                a.effect,
                engine::types::ability::Effect::SetClassLevel { .. }
            )
        })
        .collect();
    assert_eq!(
        set_class_levels.len(),
        2,
        "expected 2 SetClassLevel abilities"
    );

    // Level 2 ability has ClassLevelIs { level: 1 } restriction
    let level2 = &set_class_levels[0];
    assert!(
        level2.activation_restrictions.iter().any(|r| matches!(
            r,
            engine::types::ability::ActivationRestriction::ClassLevelIs { level: 1 }
        )),
        "Level 2 ability should require ClassLevelIs {{ level: 1 }}"
    );

    // Level 3 ability has ClassLevelIs { level: 2 } restriction
    let level3 = &set_class_levels[1];
    assert!(
        level3.activation_restrictions.iter().any(|r| matches!(
            r,
            engine::types::ability::ActivationRestriction::ClassLevelIs { level: 2 }
        )),
        "Level 3 ability should require ClassLevelIs {{ level: 2 }}"
    );
}
