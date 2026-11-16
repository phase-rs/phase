use std::path::PathBuf;

use engine::database::{BracketLists, CardDatabase};
use engine::game::bracket_estimate::CommanderBracketTier;
use engine::types::mana::ManaColor;
use phase_ai::config::AiDifficulty;
use phase_ai::deck_profile::DeckArchetype;
use phase_ai::pod_selection::{
    commander_color_identity, select_pod, AiDeckCandidate, BracketLabel, LabelProvenance,
    PodConstraint, PodSeatOccupant, PodSelectionError, PodSelectionRequest, SeatAttribute,
    TierEnforcement, TierSet, RELAXATION_ORDER,
};

const VERSION: &str = "2026-02-09-wotc";

fn face(name: &str, colors: &[ManaColor]) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "mana_cost": { "type": "NoCost" },
        "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
        "power": { "type": "Fixed", "value": 1 },
        "toughness": { "type": "Fixed", "value": 1 },
        "loyalty": null,
        "defense": null,
        "oracle_text": null,
        "non_ability_text": null,
        "flavor_name": null,
        "keywords": [],
        "abilities": [],
        "triggers": [],
        "static_abilities": [],
        "replacements": [],
        "color_override": null,
        "color_identity": colors,
        "scryfall_oracle_id": null
    })
}

fn synthetic_db() -> CardDatabase {
    let raw = serde_json::json!({
        "heliod, sun-crowned": face("Heliod, Sun-Crowned", &[ManaColor::White]),
        "inalla, archmage ritualist": face("Inalla, Archmage Ritualist", &[ManaColor::Blue, ManaColor::Black, ManaColor::Red]),
        "winota, joiner of forces": face("Winota, Joiner of Forces", &[ManaColor::White, ManaColor::Red]),
        "white commander": face("White Commander", &[ManaColor::White]),
        "grixis commander": face("Grixis Commander", &[ManaColor::Blue, ManaColor::Black, ManaColor::Red]),
        "boros commander": face("Boros Commander", &[ManaColor::White, ManaColor::Red]),
        "commander a": face("Commander A", &[ManaColor::White]),
        "commander b": face("Commander B", &[ManaColor::Blue]),
        "commander c": face("Commander C", &[ManaColor::Black]),
        "commander d": face("Commander D", &[ManaColor::Red])
    });
    let lists = BracketLists::from_json_str(&format!(r#"{{"version":"{VERSION}"}}"#))
        .expect("bracket list fixture parses");
    CardDatabase::from_json_str(&raw.to_string())
        .expect("card fixture parses")
        .with_bracket_lists(lists)
}

fn label(tier: CommanderBracketTier, provenance: LabelProvenance) -> Option<BracketLabel> {
    Some(BracketLabel {
        tier,
        provenance,
        data_version: VERSION.to_string(),
    })
}

fn candidate(id: &str, commander: &str, label: Option<BracketLabel>) -> AiDeckCandidate {
    AiDeckCandidate {
        id: id.to_string(),
        commander: vec![commander.to_string()],
        label,
        coverage_pct: Some(100),
        archetype: Some(DeckArchetype::Aggro),
    }
}

fn request(
    seats: u8,
    allowed: Vec<CommanderBracketTier>,
    prefer: Option<CommanderBracketTier>,
    enforcement: TierEnforcement,
    constraints: Vec<PodConstraint>,
    seed: u64,
) -> PodSelectionRequest {
    PodSelectionRequest::new(
        TierSet::new(allowed),
        prefer,
        enforcement,
        seats,
        constraints,
        90,
        None,
        seed,
    )
}

fn cedh_candidates() -> Vec<AiDeckCandidate> {
    [
        ("BundledCedh_HeliodBallista_Demo", "Heliod, Sun-Crowned"),
        (
            "BundledCedh_InallaThoracle_Demo",
            "Inalla, Archmage Ritualist",
        ),
        (
            "BundledCedh_WinotaKikiFelidar_Demo",
            "Winota, Joiner of Forces",
        ),
    ]
    .into_iter()
    .map(|(id, commander)| {
        candidate(
            id,
            commander,
            label(CommanderBracketTier::Cedh, LabelProvenance::Declared),
        )
    })
    .collect()
}

fn cedh_request(seats: u8) -> PodSelectionRequest {
    request(
        seats,
        vec![CommanderBracketTier::Cedh],
        Some(CommanderBracketTier::Cedh),
        TierEnforcement::HardGate,
        vec![
            PodConstraint::Distinct(SeatAttribute::Deck),
            PodConstraint::Distinct(SeatAttribute::Commander),
            PodConstraint::Distinct(SeatAttribute::ColorIdentity),
            PodConstraint::BracketDistance,
            PodConstraint::LabelProvenance,
            PodConstraint::CoverageFloor,
        ],
        11,
    )
}

#[test]
fn commander_color_identity_unions_partners_and_reports_unknown_names() {
    let db = synthetic_db();
    let commanders = vec![
        "White Commander".to_string(),
        "Grixis Commander".to_string(),
    ];
    assert_eq!(
        commander_color_identity(&db, &commanders, "partner-deck").unwrap(),
        vec![
            ManaColor::White,
            ManaColor::Blue,
            ManaColor::Black,
            ManaColor::Red,
        ]
    );

    assert_eq!(
        commander_color_identity(&db, &["Missing Commander".to_string()], "partner-deck"),
        Err(PodSelectionError::UnknownCommander {
            candidate_id: "partner-deck".to_string(),
            name: "Missing Commander".to_string(),
        })
    );
}

#[test]
fn provenance_outranks_tier_distance() {
    let db = synthetic_db();
    let candidates = vec![
        candidate(
            "declared",
            "Commander A",
            label(CommanderBracketTier::Upgraded, LabelProvenance::Declared),
        ),
        candidate(
            "estimated",
            "Commander B",
            label(CommanderBracketTier::Optimized, LabelProvenance::Estimated),
        ),
    ];
    let assignment = select_pod(
        &candidates,
        &request(
            1,
            vec![
                CommanderBracketTier::Upgraded,
                CommanderBracketTier::Optimized,
            ],
            Some(CommanderBracketTier::Optimized),
            TierEnforcement::Advisory,
            vec![PodConstraint::Distinct(SeatAttribute::Deck)],
            1,
        ),
        &db,
    )
    .unwrap();
    assert_eq!(assignment.seats[0].candidate_id, "declared");
}

#[test]
fn three_shipped_cedh_demos_fill_a_cedh_pod() {
    let data_root = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data"));
    let export = data_root.join("card-data.json");
    if !export.exists() {
        eprintln!(
            "skipping full-export assertion: {} is absent",
            export.display()
        );
        return;
    }
    let lists_path = data_root.join("bracket_lists.json");
    if !lists_path.exists() {
        eprintln!(
            "skipping full-export assertion: {} is absent",
            lists_path.display()
        );
        return;
    }
    let lists = BracketLists::from_json_path(&lists_path).expect("load bracket lists");
    let db = CardDatabase::from_export(&export)
        .expect("load card export")
        .with_bracket_lists(lists);
    let candidates = cedh_candidates();
    let assignment = select_pod(&candidates, &cedh_request(3), &db).unwrap();
    assert_eq!(assignment.seats.len(), 3);
    assert!(assignment.relaxations.is_empty());
    let ids: std::collections::BTreeSet<_> = assignment
        .seats
        .iter()
        .map(|seat| seat.candidate_id.as_str())
        .collect();
    assert_eq!(ids.len(), 3);
}

#[test]
fn synthetic_cedh_demos_fill_a_cedh_pod() {
    let assignment = select_pod(&cedh_candidates(), &cedh_request(3), &synthetic_db()).unwrap();
    assert_eq!(assignment.seats.len(), 3);
    assert!(assignment.relaxations.is_empty());
    assert!(assignment
        .seats
        .iter()
        .all(|seat| seat.difficulty == AiDifficulty::CEDH));
}

#[test]
fn cedh_hard_gate_refuses_instead_of_relaxing() {
    let mut candidates = cedh_candidates().into_iter().take(2).collect::<Vec<_>>();
    candidates.push(candidate(
        "core-only",
        "Commander D",
        label(CommanderBracketTier::Core, LabelProvenance::Declared),
    ));
    assert_eq!(
        select_pod(&candidates, &cedh_request(3), &synthetic_db()),
        Err(PodSelectionError::HardGateUnsatisfiable {
            requested: 3,
            available: 2,
        })
    );
}

#[test]
fn advisory_records_every_relaxation_in_order() {
    let db = synthetic_db();
    let candidates = vec![
        candidate(
            "a",
            "White Commander",
            label(CommanderBracketTier::Core, LabelProvenance::Declared),
        ),
        candidate(
            "b",
            "White Commander",
            label(CommanderBracketTier::Upgraded, LabelProvenance::Estimated),
        ),
        candidate(
            "c",
            "White Commander",
            label(CommanderBracketTier::Optimized, LabelProvenance::Estimated),
        ),
    ];
    let mut req = request(
        3,
        vec![CommanderBracketTier::Core],
        Some(CommanderBracketTier::Core),
        TierEnforcement::Advisory,
        vec![
            PodConstraint::Distinct(SeatAttribute::Deck),
            PodConstraint::Distinct(SeatAttribute::Commander),
            PodConstraint::Distinct(SeatAttribute::ColorIdentity),
            PodConstraint::BracketDistance,
            PodConstraint::LabelProvenance,
            PodConstraint::Archetype,
        ],
        2,
    );
    req.archetype = Some(DeckArchetype::Control);
    let assignment = select_pod(&candidates, &req, &db).unwrap();
    assert_eq!(assignment.seats.len(), 3);
    assert_eq!(assignment.relaxations.len(), RELAXATION_ORDER.len());
    assert_eq!(
        assignment
            .relaxations
            .iter()
            .map(|relaxation| relaxation.relaxed)
            .collect::<Vec<_>>(),
        RELAXATION_ORDER
    );
    assert_eq!(
        assignment
            .relaxations
            .iter()
            .map(|relaxation| relaxation.seat_index)
            .collect::<Vec<_>>(),
        vec![0, 0, 0, 1, 1]
    );
}

#[test]
fn relaxation_order_is_pinned() {
    assert_eq!(
        RELAXATION_ORDER,
        [
            PodConstraint::LabelProvenance,
            PodConstraint::BracketDistance,
            PodConstraint::Archetype,
            PodConstraint::Distinct(SeatAttribute::ColorIdentity),
            PodConstraint::Distinct(SeatAttribute::Commander),
        ]
    );
    assert!(!RELAXATION_ORDER.contains(&PodConstraint::Distinct(SeatAttribute::Deck)));
    assert!(!RELAXATION_ORDER.contains(&PodConstraint::CoverageFloor));
}

#[test]
fn same_seed_same_pod() {
    let db = synthetic_db();
    let candidates: Vec<_> = (0..200)
        .map(|index| {
            candidate(
                &format!("deck-{index}"),
                "Commander A",
                label(CommanderBracketTier::Core, LabelProvenance::Declared),
            )
        })
        .collect();
    let make_request = |seed| {
        request(
            3,
            vec![CommanderBracketTier::Core],
            None,
            TierEnforcement::Advisory,
            vec![PodConstraint::Distinct(SeatAttribute::Deck)],
            seed,
        )
    };
    let first = select_pod(&candidates, &make_request(7), &db).unwrap();
    let second = select_pod(&candidates, &make_request(7), &db).unwrap();
    assert_eq!(first, second);

    let variants: std::collections::BTreeSet<_> = (8..16)
        .map(|seed| {
            select_pod(&candidates, &make_request(seed), &db)
                .unwrap()
                .seats
                .into_iter()
                .map(|seat| seat.candidate_id)
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(variants.len() > 1, "different seeds must be able to differ");
}

#[test]
fn color_identity_comes_from_the_database() {
    let db = synthetic_db();
    let candidates = vec![
        candidate(
            "white",
            "White Commander",
            label(CommanderBracketTier::Core, LabelProvenance::Declared),
        ),
        candidate(
            "grixis",
            "Grixis Commander",
            label(CommanderBracketTier::Core, LabelProvenance::Declared),
        ),
        candidate(
            "boros",
            "Boros Commander",
            label(CommanderBracketTier::Core, LabelProvenance::Declared),
        ),
    ];
    let assignment = select_pod(
        &candidates,
        &request(
            3,
            vec![CommanderBracketTier::Core],
            None,
            TierEnforcement::Advisory,
            vec![
                PodConstraint::Distinct(SeatAttribute::Deck),
                PodConstraint::Distinct(SeatAttribute::ColorIdentity),
            ],
            4,
        ),
        &db,
    )
    .unwrap();
    let identities: std::collections::BTreeSet<_> = assignment
        .seats
        .iter()
        .map(|seat| seat.color_identity.clone())
        .collect();
    assert_eq!(identities.len(), 3);
    assert!(identities.contains(&vec![ManaColor::White]));
    assert!(identities.contains(&vec![ManaColor::Blue, ManaColor::Black, ManaColor::Red]));
    assert!(identities.contains(&vec![ManaColor::White, ManaColor::Red]));

    let unknown = vec![candidate(
        "broken",
        "Missing Commander",
        label(CommanderBracketTier::Core, LabelProvenance::Declared),
    )];
    assert_eq!(
        select_pod(
            &unknown,
            &request(1, vec![], None, TierEnforcement::Advisory, vec![], 0),
            &db
        ),
        Err(PodSelectionError::UnknownCommander {
            candidate_id: "broken".to_string(),
            name: "Missing Commander".to_string(),
        })
    );
}

#[test]
fn distinct_deck_is_never_relaxed() {
    let candidates = vec![
        candidate("a", "Commander A", None),
        candidate("b", "Commander B", None),
    ];
    assert_eq!(
        select_pod(
            &candidates,
            &request(
                5,
                vec![],
                None,
                TierEnforcement::Advisory,
                vec![PodConstraint::Distinct(SeatAttribute::Deck)],
                0,
            ),
            &synthetic_db(),
        ),
        Err(PodSelectionError::InsufficientCandidates {
            requested: 5,
            available: 2,
        })
    );
}

#[test]
fn unknown_coverage_passes_the_floor() {
    let mut unknown = candidate(
        "unknown",
        "Commander A",
        label(CommanderBracketTier::Core, LabelProvenance::Declared),
    );
    unknown.coverage_pct = None;
    let mut measured_below_floor = candidate(
        "measured-below-floor",
        "Commander B",
        label(CommanderBracketTier::Core, LabelProvenance::Declared),
    );
    measured_below_floor.coverage_pct = Some(50);

    let assignment = select_pod(
        &[measured_below_floor, unknown],
        &request(
            1,
            vec![CommanderBracketTier::Core],
            None,
            TierEnforcement::Advisory,
            vec![
                PodConstraint::Distinct(SeatAttribute::Deck),
                PodConstraint::CoverageFloor,
            ],
            0,
        ),
        &synthetic_db(),
    )
    .unwrap();

    assert_eq!(assignment.seats[0].candidate_id, "unknown");
}

#[test]
fn stale_declared_label_is_demoted_to_estimated() {
    let db = synthetic_db();
    let mut stale = candidate(
        "stale",
        "Commander A",
        label(CommanderBracketTier::Core, LabelProvenance::Declared),
    );
    stale.label.as_mut().unwrap().data_version = "old".to_string();
    let fresh = candidate(
        "fresh",
        "Commander B",
        label(CommanderBracketTier::Optimized, LabelProvenance::Estimated),
    );
    let assignment = select_pod(
        &[stale, fresh],
        &request(
            1,
            vec![CommanderBracketTier::Core, CommanderBracketTier::Optimized],
            Some(CommanderBracketTier::Optimized),
            TierEnforcement::Advisory,
            vec![PodConstraint::Distinct(SeatAttribute::Deck)],
            0,
        ),
        &db,
    )
    .unwrap();
    assert_eq!(assignment.seats[0].candidate_id, "fresh");
    assert_eq!(
        assignment.seats[0].provenance,
        Some(LabelProvenance::Estimated)
    );
}

#[test]
fn occupied_seats_block_collisions() {
    let candidates = vec![
        candidate("occupied-deck", "Commander B", None),
        candidate("commander-collision", "White Commander", None),
        candidate(
            "identity-collision",
            "Commander A",
            label(CommanderBracketTier::Core, LabelProvenance::Declared),
        ),
        candidate("safe-c", "Commander C", None),
        candidate("safe-d", "Commander D", None),
    ];
    let req = request(
        2,
        vec![],
        None,
        TierEnforcement::Advisory,
        vec![
            PodConstraint::Distinct(SeatAttribute::Deck),
            PodConstraint::Distinct(SeatAttribute::Commander),
            PodConstraint::Distinct(SeatAttribute::ColorIdentity),
        ],
        9,
    )
    .with_occupied(vec![PodSeatOccupant {
        deck_id: "occupied-deck".to_string(),
        commander: vec!["White Commander".to_string()],
    }]);
    let assignment = select_pod(&candidates, &req, &synthetic_db()).unwrap();
    let ids: std::collections::BTreeSet<_> = assignment
        .seats
        .iter()
        .map(|seat| seat.candidate_id.as_str())
        .collect();
    assert_eq!(ids, std::collections::BTreeSet::from(["safe-c", "safe-d"]));
}

#[test]
fn tier_set_canonicalises_on_deserialise() {
    let tiers: TierSet = serde_json::from_str(r#"["cedh","cedh","core"]"#).unwrap();
    assert_eq!(
        tiers.as_slice(),
        [CommanderBracketTier::Core, CommanderBracketTier::Cedh]
    );
    assert_eq!(serde_json::to_string(&tiers).unwrap(), r#"["core","cedh"]"#);
}

#[test]
fn pod_constraint_wire_shape_is_pinned() {
    assert_eq!(
        serde_json::to_value(PodConstraint::Distinct(SeatAttribute::ColorIdentity)).unwrap(),
        serde_json::json!({"distinct": "color_identity"})
    );
    assert_eq!(
        serde_json::to_value(PodConstraint::CoverageFloor).unwrap(),
        serde_json::json!("coverage_floor")
    );
}
