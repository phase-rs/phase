use engine::parser::oracle::parse_oracle_text;
use engine::types::keywords::Keyword;

fn parse(
    oracle_text: &str,
    card_name: &str,
    keywords: &[Keyword],
    types: &[&str],
    subtypes: &[&str],
) -> engine::parser::oracle::ParsedAbilities {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let subtypes: Vec<String> = subtypes.iter().map(|s| s.to_string()).collect();
    parse_oracle_text(oracle_text, card_name, keywords, &types, &subtypes)
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
