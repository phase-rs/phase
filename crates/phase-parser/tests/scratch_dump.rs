//! Temporary scratch dump (removed before commit): print parsed ASTs for
//! cards affected by the same-name mass-exile detector.

use phase_parser::parser::oracle::parse_oracle_text;

fn dump(name: &str, text: &str, types: &[&str]) {
    let types: Vec<String> = types.iter().map(|s| s.to_string()).collect();
    let parsed = parse_oracle_text(text, name, &[], &types, &[]);
    for (i, trig) in parsed.triggers.iter().enumerate() {
        println!("== {name} trigger {i}");
        let mut cur = trig.execute.as_deref();
        while let Some(node) = cur {
            println!(
                "  node effect={:#?}\n  condition={:?} sub_link={:?} optional={:?}",
                node.effect, node.condition, node.sub_link, node.optional
            );
            cur = node.sub_ability.as_deref();
        }
    }
    for (i, ab) in parsed.abilities.iter().enumerate() {
        println!("== {name} ability {i}");
        let mut cur: Option<&phase_parser::types::ability::AbilityDefinition> = Some(ab);
        while let Some(node) = cur {
            println!(
                "  node effect={:#?}\n  condition={:?} sub_link={:?} optional={:?}",
                node.effect, node.condition, node.sub_link, node.optional
            );
            cur = node.sub_ability.as_deref();
        }
    }
}

#[test]
fn scratch_dump() {
    if std::env::var("SCRATCH_DUMP").is_err() {
        return;
    }
    dump(
        "Legion's End",
        "Exile target creature an opponent controls with mana value 2 or less and all other creatures that player controls with the same name as that creature. Then that player reveals their hand and exiles all cards with that name from their hand and graveyard.",
        &["Creature"],
    );
    dump(
        "Declaration in Stone",
        "Exile target creature and all other creatures its controller controls with the same name as that creature. That player investigates for each nontoken creature exiled this way.",
        &["Sorcery"],
    );
    dump(
        "Eradicate",
        "Exile target nonblack creature. Search its controller's graveyard, hand, and library for all cards with the same name as that creature and exile them. Then that player shuffles.",
        &["Sorcery"],
    );
    dump(
        "Detention Sphere",
        "When this enchantment enters, you may exile target nonland permanent not named Detention Sphere and all other permanents with the same name as that permanent.\nWhen this enchantment leaves the battlefield, return the exiled cards to the battlefield under their owner's control.",
        &["Enchantment"],
    );
}
