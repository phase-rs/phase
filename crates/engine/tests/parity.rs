//! Parity integration tests: Forge-loaded vs JSON-loaded card comparison.
//!
//! Loads all 78 Standard cards via both `CardDatabase::load()` (Forge .txt parser)
//! and `CardDatabase::load_json()` (MTGJSON + ability JSON), then compares them
//! structurally. Only allowlisted divergences (basic land mana abilities, equipment
//! equip abilities) may differ between the two paths.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use engine::database::card_db::CardDatabase;
use engine::types::ability::{AbilityCost, AbilityKind, Effect};
use engine::types::card::{CardLayout, CardRules};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

fn load_manifest() -> Vec<String> {
    let path = data_dir().join("standard-cards.txt");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("Failed to read manifest at {}: {}", path.display(), e));
    content
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.trim().to_string())
        .collect()
}

/// Check if a card is a basic land (has a Basic supertype and Land core type).
fn is_basic_land(card: &CardRules) -> bool {
    let face = primary_face(card);
    face.card_type
        .supertypes
        .iter()
        .any(|s| format!("{:?}", s) == "Basic")
        && face
            .card_type
            .core_types
            .iter()
            .any(|c| format!("{:?}", c) == "Land")
}

/// Check if a card is an Equipment (has Equipment subtype).
fn is_equipment(card: &CardRules) -> bool {
    let face = primary_face(card);
    face.card_type.subtypes.iter().any(|s| s == "Equipment")
}

/// Get the primary face of a card.
fn primary_face(card: &CardRules) -> &engine::types::card::CardFace {
    match &card.layout {
        CardLayout::Single(f) => f,
        CardLayout::Split(a, _)
        | CardLayout::Flip(a, _)
        | CardLayout::Transform(a, _)
        | CardLayout::Meld(a, _)
        | CardLayout::Adventure(a, _)
        | CardLayout::Modal(a, _)
        | CardLayout::Omen(a, _) => a,
        CardLayout::Specialize(base, _) => base,
    }
}

/// Get all faces of a card layout as a vector.
fn layout_faces(layout: &CardLayout) -> Vec<&engine::types::card::CardFace> {
    match layout {
        CardLayout::Single(f) => vec![f],
        CardLayout::Split(a, b)
        | CardLayout::Flip(a, b)
        | CardLayout::Transform(a, b)
        | CardLayout::Meld(a, b)
        | CardLayout::Adventure(a, b)
        | CardLayout::Modal(a, b)
        | CardLayout::Omen(a, b) => vec![a, b],
        CardLayout::Specialize(base, variants) => {
            let mut faces = vec![base];
            faces.extend(variants);
            faces
        }
    }
}

/// Check if two mana cost JSON values are semantically equivalent.
/// NoCost and Cost{generic:0, shards:[]} both mean "no mana cost" (basic lands, tokens, etc.)
fn mana_costs_equivalent(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    let is_zero_cost = |v: &serde_json::Value| -> bool {
        if v == "NoCost" {
            return true;
        }
        if let Some(obj) = v.as_object() {
            if let Some(cost) = obj.get("Cost") {
                if let Some(cost_obj) = cost.as_object() {
                    let generic = cost_obj
                        .get("generic")
                        .and_then(|g| g.as_u64())
                        .unwrap_or(1);
                    let shards_empty = cost_obj
                        .get("shards")
                        .and_then(|s| s.as_array())
                        .map(|a| a.is_empty())
                        .unwrap_or(false);
                    return generic == 0 && shards_empty;
                }
            }
        }
        false
    };
    is_zero_cost(a) && is_zero_cost(b)
}

/// Extract the base keyword name (before the first colon).
/// E.g., "Ward:1" -> "ward", "Protection:Demon" -> "protection", "Flying" -> "flying".
fn keyword_base(kw: &str) -> String {
    kw.split(':').next().unwrap_or(kw).to_lowercase()
}

/// Compare two CardRules structurally, returning a list of mismatch descriptions.
/// Empty result means the cards match.
fn compare_card_rules(name: &str, forge: &CardRules, json: &CardRules) -> Vec<String> {
    let mut mismatches = Vec::new();

    let forge_faces = layout_faces(&forge.layout);
    let json_faces = layout_faces(&json.layout);

    // Compare layout variant (but not contents yet)
    let forge_layout_kind = std::mem::discriminant(&forge.layout);
    let json_layout_kind = std::mem::discriminant(&json.layout);
    if forge_layout_kind != json_layout_kind {
        mismatches.push(format!(
            "[{}] layout variant differs: forge={:?} json={:?}",
            name, forge_layout_kind, json_layout_kind
        ));
        return mismatches; // Can't compare faces if layout differs
    }

    if forge_faces.len() != json_faces.len() {
        mismatches.push(format!(
            "[{}] face count differs: forge={} json={}",
            name,
            forge_faces.len(),
            json_faces.len()
        ));
        return mismatches;
    }

    for (i, (ff, jf)) in forge_faces.iter().zip(json_faces.iter()).enumerate() {
        let prefix = if forge_faces.len() > 1 {
            format!("[{}:face{}]", name, i)
        } else {
            format!("[{}]", name)
        };

        // Name
        if ff.name != jf.name {
            mismatches.push(format!(
                "{} name differs: forge='{}' json='{}'",
                prefix, ff.name, jf.name
            ));
        }

        // Mana cost (compare via serde_json::to_value since ManaCost may not impl PartialEq)
        // NoCost and Cost{generic:0, shards:[]} are semantically equivalent (zero mana)
        let forge_mc = serde_json::to_value(&ff.mana_cost).unwrap();
        let json_mc = serde_json::to_value(&jf.mana_cost).unwrap();
        if forge_mc != json_mc && !mana_costs_equivalent(&forge_mc, &json_mc) {
            mismatches.push(format!(
                "{} mana_cost differs: forge={} json={}",
                prefix, forge_mc, json_mc
            ));
        }

        // Card type (compare via Debug since CardType may not impl PartialEq)
        let forge_ct = format!("{:?}", ff.card_type);
        let json_ct = format!("{:?}", jf.card_type);
        if forge_ct != json_ct {
            mismatches.push(format!(
                "{} card_type differs: forge={} json={}",
                prefix, forge_ct, json_ct
            ));
        }

        // Power, toughness, loyalty
        if ff.power != jf.power {
            mismatches.push(format!(
                "{} power differs: forge={:?} json={:?}",
                prefix, ff.power, jf.power
            ));
        }
        if ff.toughness != jf.toughness {
            mismatches.push(format!(
                "{} toughness differs: forge={:?} json={:?}",
                prefix, ff.toughness, jf.toughness
            ));
        }
        if ff.loyalty != jf.loyalty {
            mismatches.push(format!(
                "{} loyalty differs: forge={:?} json={:?}",
                prefix, ff.loyalty, jf.loyalty
            ));
        }

        // Keywords comparison:
        // 1. Forge preserves keyword parameters (e.g., "Ward:1", "Protection:Demon"),
        //    while MTGJSON strips them to bare names (e.g., "Ward", "Protection").
        //    Compare base keyword names only (before the first colon).
        // 2. MTGJSON includes action keywords (Scry, Mill, etc.) that Forge doesn't
        //    track as keywords. Allow extras on the JSON side.
        let forge_base_kw: HashSet<String> =
            ff.keywords.iter().map(|kw| keyword_base(kw)).collect();
        let json_base_kw: HashSet<String> = jf.keywords.iter().map(|kw| keyword_base(kw)).collect();
        // Every Forge keyword should exist in JSON (by base name)
        let missing_in_json: Vec<_> = forge_base_kw.difference(&json_base_kw).cloned().collect();
        if !missing_in_json.is_empty() {
            mismatches.push(format!(
                "{} keywords missing from json: {:?} (forge={:?} json={:?})",
                prefix, missing_in_json, ff.keywords, jf.keywords
            ));
        }
        // JSON may have extra keywords (Scry, Mill, etc.) -- that's acceptable.
        // Only report if Forge has keywords not in JSON.

        // Abilities count and per-ability comparison
        if ff.abilities.len() != jf.abilities.len() {
            mismatches.push(format!(
                "{} abilities count differs: forge={} json={}",
                prefix,
                ff.abilities.len(),
                jf.abilities.len()
            ));
        } else {
            for (ai, (fa, ja)) in ff.abilities.iter().zip(jf.abilities.iter()).enumerate() {
                let a_prefix = format!("{}:ability[{}]", prefix, ai);
                if fa.kind != ja.kind {
                    mismatches.push(format!(
                        "{} kind differs: forge={:?} json={:?}",
                        a_prefix, fa.kind, ja.kind
                    ));
                }
                let forge_effect = serde_json::to_value(&fa.effect).unwrap();
                let json_effect = serde_json::to_value(&ja.effect).unwrap();
                if forge_effect != json_effect {
                    mismatches.push(format!(
                        "{} effect differs: forge={} json={}",
                        a_prefix, forge_effect, json_effect
                    ));
                }
                let forge_cost = serde_json::to_value(&fa.cost).unwrap();
                let json_cost = serde_json::to_value(&ja.cost).unwrap();
                if forge_cost != json_cost {
                    mismatches.push(format!(
                        "{} cost differs: forge={} json={}",
                        a_prefix, forge_cost, json_cost
                    ));
                }
            }
        }

        // Triggers count and per-trigger comparison
        if ff.triggers.len() != jf.triggers.len() {
            mismatches.push(format!(
                "{} triggers count differs: forge={} json={}",
                prefix,
                ff.triggers.len(),
                jf.triggers.len()
            ));
        } else {
            for (ti, (ft, jt)) in ff.triggers.iter().zip(jf.triggers.iter()).enumerate() {
                let t_prefix = format!("{}:trigger[{}]", prefix, ti);
                let forge_mode = format!("{:?}", ft.mode);
                let json_mode = format!("{:?}", jt.mode);
                if forge_mode != json_mode {
                    mismatches.push(format!(
                        "{} mode differs: forge={} json={}",
                        t_prefix, forge_mode, json_mode
                    ));
                }
            }
        }

        // Statics count and per-static comparison
        if ff.static_abilities.len() != jf.static_abilities.len() {
            mismatches.push(format!(
                "{} statics count differs: forge={} json={}",
                prefix,
                ff.static_abilities.len(),
                jf.static_abilities.len()
            ));
        } else {
            for (si, (fs, js)) in ff
                .static_abilities
                .iter()
                .zip(jf.static_abilities.iter())
                .enumerate()
            {
                let s_prefix = format!("{}:static[{}]", prefix, si);
                let forge_mode = format!("{:?}", fs.mode);
                let json_mode = format!("{:?}", js.mode);
                if forge_mode != json_mode {
                    mismatches.push(format!(
                        "{} mode differs: forge={} json={}",
                        s_prefix, forge_mode, json_mode
                    ));
                }
            }
        }

        // Replacements count
        if ff.replacements.len() != jf.replacements.len() {
            mismatches.push(format!(
                "{} replacements count differs: forge={} json={}",
                prefix,
                ff.replacements.len(),
                jf.replacements.len()
            ));
        }

        // NOTE: We do NOT compare svars (JSON path doesn't populate these),
        // scryfall_oracle_id (only on JSON path), oracle_text, or non_ability_text
        // (only from MTGJSON).
    }

    mismatches
}

/// Filter mismatches for allowed divergences.
/// Returns only the unexpected mismatches that should cause test failure.
fn filter_allowed_divergences(
    name: &str,
    mismatches: &[String],
    json_card: &CardRules,
) -> Vec<String> {
    if mismatches.is_empty() {
        return vec![];
    }

    let mut unexpected: Vec<String> = Vec::new();

    for mismatch in mismatches {
        // Basic land mana ability: JSON has 1 more ability (the synthesized mana ability)
        if is_basic_land(json_card) && mismatch.contains("abilities count differs") {
            // Allowed: JSON has exactly 1 more ability than Forge
            if mismatch.contains("forge=0 json=1") {
                continue; // This is the expected basic land divergence
            }
        }

        // Equipment equip ability: JSON has 1 more ability (the synthesized equip ability)
        if is_equipment(json_card)
            && mismatch.contains("abilities count differs")
            && mismatch.contains("json=")
        {
            // Check that the JSON side has exactly 1 more ability
            let forge_count: Option<usize> = extract_count(mismatch, "forge=");
            let json_count: Option<usize> = extract_count(mismatch, "json=");
            if let (Some(fc), Some(jc)) = (forge_count, json_count) {
                if jc == fc + 1 {
                    continue; // Expected equipment divergence
                }
            }
        }

        unexpected.push(format!("[{}] {}", name, mismatch));
    }

    unexpected
}

/// Extract a count value from a mismatch string like "forge=2 json=3".
fn extract_count(s: &str, prefix: &str) -> Option<usize> {
    s.find(prefix).and_then(|start| {
        let after = &s[start + prefix.len()..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        num_str.parse().ok()
    })
}

// ---------------------------------------------------------------------------
// Positive assertions for implicit abilities
// ---------------------------------------------------------------------------

/// Assert that JSON-loaded basic lands have synthesized mana abilities.
fn assert_basic_lands_have_mana_ability(db: &CardDatabase, manifest: &[String]) {
    let basic_land_names: HashSet<&str> = ["Plains", "Island", "Swamp", "Mountain", "Forest"]
        .iter()
        .copied()
        .collect();

    for name in manifest {
        if !basic_land_names.contains(name.as_str()) {
            continue;
        }

        let card = db
            .get_by_name(name)
            .unwrap_or_else(|| panic!("JSON DB missing basic land: {}", name));
        let face = primary_face(card);

        let has_mana_ability = face.abilities.iter().any(|a| {
            a.kind == AbilityKind::Activated
                && matches!(&a.effect, Effect::Mana { .. })
                && a.cost == Some(AbilityCost::Tap)
        });

        assert!(
            has_mana_ability,
            "JSON-loaded {} should have a synthesized mana ability, but has {} abilities: {:?}",
            name,
            face.abilities.len(),
            face.abilities
                .iter()
                .map(|a| format!("{:?}", a.kind))
                .collect::<Vec<_>>()
        );
    }
}

/// Assert that JSON-loaded equipment cards have synthesized equip abilities.
fn assert_equipment_has_equip_ability(db: &CardDatabase, manifest: &[String]) {
    for name in manifest {
        let card = match db.get_by_name(name) {
            Some(c) => c,
            None => continue, // Not all cards may be in the DB
        };

        if !is_equipment(card) {
            continue;
        }

        let face = primary_face(card);
        let has_equip = face.abilities.iter().any(|a| {
            a.kind == AbilityKind::Activated && matches!(&a.effect, Effect::Attach { .. })
        });

        assert!(
            has_equip,
            "JSON-loaded equipment {} should have a synthesized equip ability, but has {} abilities: {:?}",
            name,
            face.abilities.len(),
            face.abilities
                .iter()
                .map(|a| format!("{:?}/{:?}", a.kind, a.effect))
                .collect::<Vec<_>>()
        );
    }
}

// ---------------------------------------------------------------------------
// Main parity test
// ---------------------------------------------------------------------------

#[test]
fn parity_all_standard_cards() {
    let manifest = load_manifest();
    assert_eq!(
        manifest.len(),
        78,
        "Manifest should contain 78 standard cards"
    );

    let data = data_dir();

    // Load both databases
    let forge_db = CardDatabase::load(&data.join("standard-cards"))
        .expect("Forge CardDatabase::load should succeed");
    let json_db = CardDatabase::load_json(
        &data.join("mtgjson/test_fixture.json"),
        &data.join("abilities"),
    )
    .expect("JSON CardDatabase::load_json should succeed");

    let mut all_failures: Vec<String> = Vec::new();

    for name in &manifest {
        let forge_card = forge_db.get_by_name(name).unwrap_or_else(|| {
            panic!(
                "Card '{}' missing from Forge DB (loaded {} cards, {} errors)",
                name,
                forge_db.card_count(),
                forge_db.errors().len()
            )
        });

        let json_card = json_db.get_by_name(name).unwrap_or_else(|| {
            panic!(
                "Card '{}' missing from JSON DB (loaded {} cards, {} errors). Errors: {:?}",
                name,
                json_db.card_count(),
                json_db.errors().len(),
                json_db
                    .errors()
                    .iter()
                    .filter(|(_, msg)| !msg.contains("No MTGJSON match"))
                    .take(5)
                    .collect::<Vec<_>>()
            )
        });

        let mismatches = compare_card_rules(name, forge_card, json_card);
        let unexpected = filter_allowed_divergences(name, &mismatches, json_card);
        all_failures.extend(unexpected);
    }

    // Run positive assertions for implicit abilities
    assert_basic_lands_have_mana_ability(&json_db, &manifest);
    assert_equipment_has_equip_ability(&json_db, &manifest);

    // Report all failures at end
    if !all_failures.is_empty() {
        let msg = format!(
            "Parity check found {} unexpected mismatch(es):\n{}",
            all_failures.len(),
            all_failures.join("\n")
        );
        panic!("{}", msg);
    }
}

// ---------------------------------------------------------------------------
// Individual spot-check tests
// ---------------------------------------------------------------------------

#[test]
fn parity_lightning_bolt() {
    let data = data_dir();
    let forge_db = CardDatabase::load(&data.join("standard-cards")).unwrap();
    let json_db = CardDatabase::load_json(
        &data.join("mtgjson/test_fixture.json"),
        &data.join("abilities"),
    )
    .unwrap();

    let forge = forge_db
        .get_by_name("Lightning Bolt")
        .expect("Forge DB should have Lightning Bolt");
    let json = json_db
        .get_by_name("Lightning Bolt")
        .expect("JSON DB should have Lightning Bolt");

    // Both should be Single layout
    let forge_face = match &forge.layout {
        CardLayout::Single(f) => f,
        other => panic!(
            "Expected Single layout, got {:?}",
            std::mem::discriminant(other)
        ),
    };
    let json_face = match &json.layout {
        CardLayout::Single(f) => f,
        other => panic!(
            "Expected Single layout, got {:?}",
            std::mem::discriminant(other)
        ),
    };

    // Both should have exactly 1 Spell ability with DealDamage effect
    assert_eq!(
        forge_face.abilities.len(),
        1,
        "Forge Lightning Bolt should have 1 ability"
    );
    assert_eq!(
        json_face.abilities.len(),
        1,
        "JSON Lightning Bolt should have 1 ability"
    );
    assert_eq!(forge_face.abilities[0].kind, AbilityKind::Spell);
    assert_eq!(json_face.abilities[0].kind, AbilityKind::Spell);
    assert!(
        matches!(&forge_face.abilities[0].effect, Effect::DealDamage { .. }),
        "Forge Lightning Bolt should have DealDamage effect"
    );
    assert!(
        matches!(&json_face.abilities[0].effect, Effect::DealDamage { .. }),
        "JSON Lightning Bolt should have DealDamage effect"
    );

    // Structural comparison should find zero mismatches
    let mismatches = compare_card_rules("Lightning Bolt", forge, json);
    assert!(
        mismatches.is_empty(),
        "Lightning Bolt should have zero mismatches: {:?}",
        mismatches
    );
}

#[test]
fn parity_jace_the_mind_sculptor() {
    let data = data_dir();
    let forge_db = CardDatabase::load(&data.join("standard-cards")).unwrap();
    let json_db = CardDatabase::load_json(
        &data.join("mtgjson/test_fixture.json"),
        &data.join("abilities"),
    )
    .unwrap();

    // Jace is NOT in the standard-cards directory, but IS in the MTGJSON fixture
    // and ability files. For parity, we need him in both. If Forge DB doesn't have
    // him, skip the parity check but verify the JSON-loaded version is correct.
    let json_card = json_db
        .get_by_name("Jace, the Mind Sculptor")
        .expect("JSON DB should have Jace, the Mind Sculptor");

    let json_face = match &json_card.layout {
        CardLayout::Single(f) => f,
        other => panic!(
            "Expected Single layout for Jace, got {:?}",
            std::mem::discriminant(other)
        ),
    };

    // Jace should have 4 loyalty abilities with costs +2, 0, -1, -12
    assert_eq!(
        json_face.abilities.len(),
        4,
        "Jace should have 4 loyalty abilities"
    );

    let loyalty_costs: Vec<i32> = json_face
        .abilities
        .iter()
        .filter_map(|a| match &a.cost {
            Some(AbilityCost::Loyalty { amount }) => Some(*amount),
            _ => None,
        })
        .collect();

    assert!(loyalty_costs.contains(&2), "Jace should have +2 ability");
    assert!(loyalty_costs.contains(&0), "Jace should have 0 ability");
    assert!(loyalty_costs.contains(&-1), "Jace should have -1 ability");
    assert!(loyalty_costs.contains(&-12), "Jace should have -12 ability");

    // If Forge DB also has Jace, do parity comparison
    if let Some(forge_card) = forge_db.get_by_name("Jace, the Mind Sculptor") {
        let mismatches = compare_card_rules("Jace, the Mind Sculptor", forge_card, json_card);
        assert!(
            mismatches.is_empty(),
            "Jace should have zero mismatches: {:?}",
            mismatches
        );
    }
}

#[test]
fn parity_lovestruck_beast() {
    let data = data_dir();
    let forge_db = CardDatabase::load(&data.join("standard-cards")).unwrap();
    let json_db = CardDatabase::load_json(
        &data.join("mtgjson/test_fixture.json"),
        &data.join("abilities"),
    )
    .unwrap();

    let forge = forge_db
        .get_by_name("Lovestruck Beast")
        .expect("Forge DB should have Lovestruck Beast");
    let json = json_db
        .get_by_name("Lovestruck Beast")
        .expect("JSON DB should have Lovestruck Beast");

    // Both should be Adventure layout
    match &forge.layout {
        CardLayout::Adventure(a, b) => {
            assert_eq!(a.name, "Lovestruck Beast");
            assert_eq!(b.name, "Heart's Desire");
        }
        other => panic!(
            "Expected Adventure layout for Forge Lovestruck Beast, got {:?}",
            std::mem::discriminant(other)
        ),
    }

    match &json.layout {
        CardLayout::Adventure(a, b) => {
            assert_eq!(a.name, "Lovestruck Beast");
            assert_eq!(b.name, "Heart's Desire");
        }
        other => panic!(
            "Expected Adventure layout for JSON Lovestruck Beast, got {:?}",
            std::mem::discriminant(other)
        ),
    }

    // Parity comparison
    let mismatches = compare_card_rules("Lovestruck Beast", forge, json);
    let unexpected = filter_allowed_divergences("Lovestruck Beast", &mismatches, json);
    assert!(
        unexpected.is_empty(),
        "Lovestruck Beast should have zero unexpected mismatches: {:?}",
        unexpected
    );
}
