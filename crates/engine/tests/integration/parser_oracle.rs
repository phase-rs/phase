//! Tests relocated from `src/parser/oracle_tests.rs` (`parser::oracle`'s test module).

use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle::test_helpers::parse;
use engine::parser::oracle_replacement::{
    enters_with_fails_closed, parse_replacement_line, EntersWithGap,
};
use engine::types::ability::{
    AbilityCost, ChoiceType, ControllerRef, Effect, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;

/// Build a single-face `CardFace` from an oracle `text` through the real
/// synthesis path (`build_oracle_face`), so coverage checks recurse into
/// granted abilities and effect payloads exactly as production does.
#[cfg(test)]
fn oracle_face_for(
    name: &str,
    text: &str,
    types: &[&str],
    subtypes: &[&str],
) -> engine::types::card::CardFace {
    use engine::database::mtgjson::{AtomicCard, AtomicIdentifiers};
    let card = AtomicCard {
        name: name.to_string(),
        mana_cost: Some("{4}{B}".to_string()),
        colors: vec!["B".to_string()],
        color_identity: vec!["B".to_string()],
        power: Some("3".to_string()),
        toughness: Some("3".to_string()),
        loyalty: None,
        defense: None,
        text: Some(text.to_string()),
        layout: "normal".to_string(),
        type_line: Some(types.join(" ")),
        types: types.iter().map(|s| s.to_string()).collect(),
        subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
        supertypes: Vec::new(),
        keywords: None,
        side: None,
        face_name: None,
        mana_value: 5.0,
        legalities: Default::default(),
        leadership_skills: None,
        printings: Vec::new(),
        rulings: Vec::new(),
        is_game_changer: false,
        identifiers: AtomicIdentifiers {
            scryfall_oracle_id: Some(format!("{}-oracle", name.to_lowercase())),
            scryfall_id: Some(format!("{}-face", name.to_lowercase())),
        },
        foreign_data: Vec::new(),
        related_cards: engine::database::mtgjson::SetRelatedCards::default(),
    };
    engine::database::synthesis::build_oracle_face(&card, None)
}

/// CR 708.5: Found Footage's "You may look at face-down creatures your
/// opponents control any time" lowers to a `MayLookAtFaceDown` static whose
/// affected filter carries the FaceDown property and the opponent scope. The
/// whole card (look static + sacrifice ability) parses with zero
/// unimplemented parts. Runtime visibility discrimination lives in
/// `game::visibility::tests::found_footage_reveals_opponent_face_down`.
#[test]
fn found_footage_look_at_face_down_static_full_card_supported() {
    use engine::types::ability::FilterProp;
    let face = oracle_face_for(
            "Found Footage",
            "You may look at face-down creatures your opponents control any time.\n{2}, Sacrifice this artifact: Surveil 2, then draw a card.",
            &["Artifact"],
            &[],
        );
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "Found Footage must be fully supported, gaps: {gaps:?}"
    );
    // (test-only) Iterator::find over the parsed statics — not a parsing
    // dispatch; the parser-combinator gate's `.find(` heuristic flags the
    // string method, but this is a Vec<StaticDefinition> lookup.
    let look = face
        .static_abilities
        .iter()
        .find(|s| s.mode == StaticMode::MayLookAtFaceDown)
        .expect("MayLookAtFaceDown static must be present");
    let affected = look
        .affected
        .as_ref()
        .expect("look static must scope an affected filter");
    match affected {
        TargetFilter::Typed(t) => {
            assert!(
                t.properties
                    .iter()
                    .any(|p| matches!(p, FilterProp::FaceDown)),
                "affected filter must carry FaceDown, got {t:?}"
            );
            assert_eq!(
                t.controller,
                Some(ControllerRef::Opponent),
                "'your opponents control' must scope to Opponent"
            );
        }
        other => panic!("expected Typed affected filter, got {other:?}"),
    }
}

/// CR 602.2b + CR 601.2f + CR 102.1: Hylda's Crown of Winter parses with zero
/// unimplemented parts. The whole card is two activated abilities; the only
/// previously-failing fragment was the "This ability costs {1} less to
/// activate during your turn" cost-reduction clause, now extracted as
/// `cost_reduction` with `condition: IsYourTurn`. (Runtime under/over-the-gate
/// discrimination lives in `game::casting::tests::
/// hyldas_crown_cost_reduction_applies_only_during_your_turn`.)
#[test]
fn hyldas_crown_full_card_supported_with_during_your_turn_cost_reduction() {
    let face = oracle_face_for(
            "Hylda's Crown of Winter",
            "{1}, {T}: Tap target creature. This ability costs {1} less to activate during your turn.\n{3}, Sacrifice Hylda's Crown of Winter: Draw a card for each tapped creature your opponents control.",
            &["Legendary", "Artifact"],
            &[],
        );
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "Hylda's Crown must be fully supported, gaps: {gaps:?}"
    );
    let tap_ability = face
        .abilities
        .iter()
        .find(|a| a.cost_reduction.is_some())
        .expect("the tap ability must carry the extracted cost reduction");
    assert_eq!(
        tap_ability.cost_reduction.as_ref().unwrap().condition,
        Some(engine::types::ability::ParsedCondition::IsYourTurn),
        "cost reduction must gate on IsYourTurn"
    );
}

/// CR 602.2b + CR 601.2f + CR 508.1a: Thaumaton Torpedo parses with zero
/// unimplemented parts. The card is a single activated ability; the only
/// previously-failing fragment was the "This ability costs {3} less to
/// activate if you attacked with a Spacecraft this turn" cost-reduction
/// clause, now extracted as a typed `QuantityComparison` over a filtered
/// `AttackedThisTurn` count (GE 1).
/// (Runtime discrimination — Spacecraft attacker required, opponent's
/// Spacecraft excluded — lives in `game::casting::tests::
/// thaumaton_torpedo_cost_reduction_requires_spacecraft_attacker`.)
#[test]
fn thaumaton_torpedo_full_card_supported_with_spacecraft_attacked_cost_reduction() {
    let face = oracle_face_for(
            "Thaumaton Torpedo",
            "{6}, {T}, Sacrifice this artifact: Destroy target nonland permanent. This ability costs {3} less to activate if you attacked with a Spacecraft this turn.",
            &["Artifact"],
            &[],
        );
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "Thaumaton Torpedo must be fully supported, gaps: {gaps:?}"
    );
    let ability = face
        .abilities
        .iter()
        .find(|a| a.cost_reduction.is_some())
        .expect("the destroy ability must carry the extracted cost reduction");
    assert!(
        matches!(
            ability.cost_reduction.as_ref().unwrap().condition,
            Some(
                engine::types::ability::ParsedCondition::QuantityComparison {
                    lhs: engine::types::ability::QuantityExpr::Ref {
                        qty: engine::types::ability::QuantityRef::AttackedThisTurn {
                            filter: Some(_),
                            ..
                        },
                    },
                    ..
                }
            )
        ),
        "cost reduction must gate on a filtered attacked-with condition, got {:?}",
        ability.cost_reduction
    );
}

/// CR 613.4b + CR 208.1 + CR 604.3: std BATCH 10 — Porcelain Gallery's static
/// "Creatures you control have base power and toughness each equal to the
/// number of creatures you control" must parse with zero coverage gaps. The
/// dynamic base-P/T set routes to layer-7b `SetPowerDynamic`/
/// `SetToughnessDynamic` with an `ObjectCount` value. (Runtime discrimination —
/// base P/T becomes and tracks the count — lives in
/// `tests/base_pt_dynamic_set_std_base_pt.rs`.)
#[test]
fn porcelain_gallery_full_card_supported_dynamic_base_pt() {
    let face = oracle_face_for(
            "Porcelain Gallery",
            "Creatures you control have base power and toughness each equal to the number of creatures you control.",
            &["Artifact"],
            &[],
        );
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "Porcelain Gallery must be fully supported, gaps: {gaps:?}"
    );
}

/// CR 613.4b + CR 208.1: std BATCH 10 — Pupu UFO's activated "{3}: Until end
/// of turn, this creature's base power becomes equal to the number of Towns
/// you control" must parse with zero coverage gaps. The power-only dynamic
/// base-set routes to a layer-7b `SetPowerDynamic(ObjectCount Towns)`; the
/// Flying keyword and the land-drop ability are already supported. (Runtime
/// discrimination lives in `tests/base_pt_dynamic_set_std_base_pt.rs`.)
#[test]
fn pupu_ufo_full_card_supported_dynamic_base_power() {
    // Build the face the way the card-data pipeline does, with MTGJSON's
    // printed `keywords: ["Flying"]` present so the standalone "Flying" line
    // is recognized as a keyword (not a stray ability) — exactly the input
    // production sees.
    use engine::database::mtgjson::{AtomicCard, AtomicIdentifiers};
    let card = AtomicCard {
            name: "Pupu UFO".to_string(),
            mana_cost: Some("{3}{G}".to_string()),
            colors: vec!["G".to_string()],
            color_identity: vec!["G".to_string()],
            power: Some("2".to_string()),
            toughness: Some("2".to_string()),
            loyalty: None,
            defense: None,
            text: Some(
                "Flying\n{T}: You may put a land card from your hand onto the battlefield.\n{3}: Until end of turn, this creature's base power becomes equal to the number of Towns you control.".to_string(),
            ),
            layout: "normal".to_string(),
            type_line: Some("Creature — Alien".to_string()),
            types: vec!["Creature".to_string()],
            subtypes: vec!["Alien".to_string()],
            supertypes: Vec::new(),
            keywords: Some(vec!["Flying".to_string()]),
            side: None,
            face_name: None,
            mana_value: 4.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: AtomicIdentifiers {
                scryfall_oracle_id: Some("pupu-ufo-oracle".to_string()),
                scryfall_id: Some("pupu-ufo-face".to_string()),
            },
            foreign_data: Vec::new(),
            related_cards: engine::database::mtgjson::SetRelatedCards::default(),
        };
    let face = engine::database::synthesis::build_oracle_face(&card, None);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "Pupu UFO must be fully supported, gaps: {gaps:?}"
    );
}

/// CR 613.4b + CR 208.1 + CR 702.177: std BATCH 10 — Sita Varma's Exhaust
/// ability "Put X +1/+1 counters … Then you may have the base power and
/// toughness of each other creature you control become equal to Sita Varma's
/// power until end of turn" must parse with zero coverage gaps. The "Then you
/// may have …" half is the inverted-genitive base-P/T set on the other
/// creatures you control, set to the source's power (`~`-normalized →
/// `Power{Source}`) via layer-7b `SetPowerDynamic`/`SetToughnessDynamic`.
#[test]
fn sita_varma_full_card_supported_inverted_genitive_base_pt() {
    use engine::database::mtgjson::{AtomicCard, AtomicIdentifiers};
    let card = AtomicCard {
            name: "Sita Varma, Masked Racer".to_string(),
            mana_cost: Some("{1}{G}{U}".to_string()),
            colors: vec!["G".to_string(), "U".to_string()],
            color_identity: vec!["G".to_string(), "U".to_string()],
            power: Some("3".to_string()),
            toughness: Some("3".to_string()),
            loyalty: None,
            defense: None,
            text: Some(
                "Exhaust \u{2014} {X}{G}{G}{U}: Put X +1/+1 counters on Sita Varma. Then you may have the base power and toughness of each other creature you control become equal to Sita Varma's power until end of turn. (Activate each exhaust ability only once.)".to_string(),
            ),
            layout: "normal".to_string(),
            type_line: Some("Legendary Creature — Human".to_string()),
            types: vec!["Creature".to_string()],
            subtypes: vec!["Human".to_string()],
            supertypes: vec!["Legendary".to_string()],
            keywords: Some(vec!["Exhaust".to_string()]),
            side: None,
            face_name: None,
            mana_value: 3.0,
            legalities: Default::default(),
            leadership_skills: None,
            printings: Vec::new(),
            rulings: Vec::new(),
            is_game_changer: false,
            identifiers: AtomicIdentifiers {
                scryfall_oracle_id: Some("sita-varma-oracle".to_string()),
                scryfall_id: Some("sita-varma-face".to_string()),
            },
            foreign_data: Vec::new(),
            related_cards: engine::database::mtgjson::SetRelatedCards::default(),
        };
    let face = engine::database::synthesis::build_oracle_face(&card, None);
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert!(
        gaps.is_empty(),
        "Sita Varma must be fully supported, gaps: {gaps:?}"
    );
}

/// CR 601.2a + CR 609.4b + CR 601.3b: Azula, Cunning Usurper — the
/// cast-from-exile static line must lower to a supported
/// `StaticMode::ExileCastPermission` carrying the persistent Cast permission,
/// the any-type-mana spend concession, and the flash grant.
///
/// Azula is intentionally NOT full-card-0: the "Firebending 2" line is a
/// separate, out-of-scope keyword whose triggered mana ability remains an
/// honest `Effect::unknown` gap (handled by a different batch). This test
/// therefore pins the cast-from-exile line as the supported variant AND
/// asserts the only remaining gap is that Firebending ability — so it fails
/// both if the static line regresses to a gap and if a new unrelated gap
/// appears. (Runtime discrimination — any-type mana payable from a red-only
/// pool and instant-speed castability — lives in `game::casting::tests::
/// azula_exile_static_grants_any_type_mana_spend` and
/// `azula_exile_static_grants_flash_timing`.)
#[test]
fn azula_cunning_usurper_full_card_supported_with_exile_cast_permission() {
    let face = oracle_face_for(
            "Azula, Cunning Usurper",
            "Firebending 2 (Whenever this creature attacks, add {R}{R}. This mana lasts until end of combat.)\nWhen Azula enters, target opponent exiles a nontoken creature they control, then they exile a nonland card from their graveyard.\nDuring your turn, you may cast cards exiled with Azula and you may cast them as though they had flash. Mana of any type can be spent to cast those spells.",
            &["Legendary", "Creature"],
            &["Human"],
        );
    // The cast-from-exile static line must be fully supported. Locate it by
    // its typed parse category + handler label (not by matching Oracle text)
    // so the assertion stays robust to wording tweaks.
    let details = engine::game::coverage::build_parse_details_for_face(&face);
    let cast_static = details
        .iter()
        .find(|d| {
            matches!(d.category, engine::game::coverage::ParseCategory::Static)
                    // allow-noncombinator: matching the engine's own parse-detail handler label (not Oracle text), in a test.
                    && d.label.starts_with("ExileCastPermission")
        })
        .expect("Azula's cast-from-exile line must appear as an ExileCastPermission static");
    assert!(
        cast_static.supported,
        "the cast-from-exile static line must be supported, got {cast_static:?}"
    );
    // The only remaining coverage gap is the out-of-scope Firebending
    // ability — proving the cast-from-exile line no longer contributes a gap.
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert_eq!(
        gaps,
        vec!["Effect:unknown".to_string()],
        "the only remaining gap must be the out-of-scope Firebending ability"
    );
    let firebending = details
        .iter()
        .find(|d| !d.supported)
        .expect("Azula must have exactly the Firebending unsupported ability");
    assert_eq!(
        firebending.source_text.as_deref(),
        Some("Firebending 2"),
        "the single unsupported item must be the Firebending keyword line"
    );

    // Pin the structural variant the fix produces — guards against a silent
    // regression where the line stops parsing entirely.
    let static_def = face
        .static_abilities
        .iter()
        .find(|s| {
            matches!(
                s.mode,
                engine::types::statics::StaticMode::ExileCastPermission {
                    play_mode: engine::types::ability::CardPlayMode::Cast,
                    grants_flash: true,
                    mana_spend_permission: Some(
                        engine::types::ability::ManaSpendPermission::AnyTypeOrColor
                    ),
                    ..
                }
            )
        })
        .expect("Azula must emit a Cast permission with flash + any-mana");
    assert_eq!(
        static_def.affected,
        Some(engine::types::ability::TargetFilter::Any)
    );
}

/// CR 118.9: Valgavoth, Terror Eater — the cast-from-exile static line
/// ("During your turn, you may play cards exiled with ~. If you cast a spell
/// this way, pay life equal to its mana value rather than pay its mana
/// cost.") must be a fully-supported `ExileCastPermission` carrying an
/// ALTERNATIVE pay-life extra-cost (not a leftover Unimplemented gap).
#[test]
fn valgavoth_terror_eater_cast_from_exile_alt_cost_supported() {
    use engine::types::statics::{CastCostMode, StaticMode};
    let face = oracle_face_for(
            "Valgavoth, Terror Eater",
            "Flying, lifelink\nWard\u{2014}Sacrifice three nonland permanents.\nIf a card you didn't control would be put into an opponent's graveyard from anywhere, exile it instead.\nDuring your turn, you may play cards exiled with Valgavoth. If you cast a spell this way, pay life equal to its mana value rather than pay its mana cost.",
            &["Legendary", "Creature"],
            &["Demon"],
        );
    let details = engine::game::coverage::build_parse_details_for_face(&face);
    let cast_static = details
        .iter()
        .find(|d| {
            matches!(d.category, engine::game::coverage::ParseCategory::Static)
                    // allow-noncombinator: matching the engine's own parse-detail handler label (not Oracle text), in a test.
                    && d.label.starts_with("ExileCastPermission")
        })
        .expect("Valgavoth's cast-from-exile line must appear as an ExileCastPermission static");
    assert!(
        cast_static.supported,
        "the cast-from-exile static line must be supported, got {cast_static:?}"
    );

    // Pin the structural variant: a persistent Play permission carrying an
    // ALTERNATIVE pay-life extra-cost.
    let static_def = face
        .static_abilities
        .iter()
        .find(|s| matches!(s.mode, StaticMode::ExileCastPermission { .. }))
        .expect("Valgavoth must emit an ExileCastPermission static");
    let StaticMode::ExileCastPermission {
        play_mode,
        ref extra_cost,
        ..
    } = static_def.mode
    else {
        unreachable!("matched ExileCastPermission above");
    };
    assert_eq!(play_mode, engine::types::ability::CardPlayMode::Play);
    let extra = extra_cost
        .as_ref()
        .expect("Valgavoth must carry an alternative extra-cost");
    assert_eq!(extra.mode, CastCostMode::Alternative);
    assert!(
        matches!(
            extra.cost,
            engine::types::ability::AbilityCost::PayLife { .. }
        ),
        "Valgavoth's alternative cost must be PayLife, got {:?}",
        extra.cost
    );
}

/// CR 614.1a + CR 122.1a: End-to-end check that Vizier of Remedies
/// parses cleanly through `parse_oracle_text` (the canonical entry
/// point used by the card-data pipeline) and produces a single
/// AddCounter replacement gated to -1/-1 counters on creatures the
/// controller controls. The full card must be fully supported (zero
/// gaps) — this is what flips the runtime `supported: true` flag in
/// `card-data.json`.
#[test]
fn vizier_of_remedies_parses_to_single_counter_replacement() {
    use engine::game::coverage::{card_face_gaps, card_face_has_unimplemented_parts};
    use engine::types::ability::QuantityModification;
    use engine::types::card::CardFace;
    use engine::types::counter::{CounterMatch, CounterType};

    let oracle = "If one or more -1/-1 counters would be put on a creature you control, that many -1/-1 counters minus one are put on it instead.";
    let parsed = parse_oracle_text(
        oracle,
        "Vizier of Remedies",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Cleric".to_string()],
    );

    assert!(
        parsed.abilities.is_empty(),
        "no spell abilities expected, got {:?}",
        parsed.abilities
    );
    assert!(
        parsed.triggers.is_empty(),
        "no triggered abilities expected, got {:?}",
        parsed.triggers
    );
    assert_eq!(
        parsed.replacements.len(),
        1,
        "expected exactly one replacement, got {:?}",
        parsed.replacements
    );

    let repl = &parsed.replacements[0];
    assert_eq!(repl.event, ReplacementEvent::AddCounter);
    assert_eq!(
        repl.quantity_modification,
        Some(QuantityModification::Minus { value: 1 }),
        "Vizier subtracts 1 from the counter count (saturating at 0 — CR 122.1a)"
    );
    assert_eq!(
        repl.counter_match,
        Some(CounterMatch::OfType(CounterType::Minus1Minus1)),
        "Vizier must be gated to -1/-1 counters specifically"
    );
    assert!(matches!(
        repl.valid_card,
        Some(TargetFilter::Typed(TypedFilter {
            ref type_filters,
            controller: Some(ControllerRef::You),
            ..
        })) if type_filters == &vec![TypeFilter::Creature]
    ));

    // Coverage gate: build a CardFace from the parsed result and verify
    // the engine reports zero gaps (i.e. this is a fully-supported card).
    let face = CardFace {
        name: "Vizier of Remedies".to_string(),
        replacements: parsed.replacements.clone(),
        ..CardFace::default()
    };
    assert!(
        !card_face_has_unimplemented_parts(&face),
        "Vizier of Remedies must report no Unimplemented parts"
    );
    assert!(
        card_face_gaps(&face).is_empty(),
        "Vizier of Remedies must have zero coverage gaps, got: {:?}",
        card_face_gaps(&face)
    );
}

/// CR 607.2a: Realm Razer's LTB return carries an entry modifier ("return the
/// exiled cards to the battlefield TAPPED") the emitted `ExiledBySource` return
/// can't carry, so the pair is `ModifierUnsupported`: the LTB is not rewritten
/// and the unsupported return must be VISIBLE to
/// the coverage tooling (an `Effect::unimplemented("modifier_bearing_linked_return")`
/// marker on the LTB trigger) rather than the card showing as falsely
/// supported. Oracle text verbatim from Scryfall (2026-07). Caught in review of
/// #6055.
#[test]
fn mass_exile_ltb_return_with_entry_modifier_is_not_paired() {
    use engine::game::coverage::{card_face_gaps, card_face_has_unimplemented_parts};
    use engine::types::card::CardFace;

    let oracle = "When this creature enters, exile all lands.\n\
             When this creature leaves the battlefield, return the exiled cards to the \
             battlefield tapped under their owners' control.";
    let name = "Realm Razer";
    let result = parse(oracle, name, &[], &["Creature"], &[]);

    let ltb = result
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::LeavesBattlefield)
        .unwrap_or_else(|| panic!("{name} must have an LTB trigger"));
    let ltb_effect = &ltb
        .execute
        .as_deref()
        .unwrap_or_else(|| panic!("{name} LTB must have execute"))
        .effect;
    assert!(
        matches!(
            &**ltb_effect,
            Effect::ChangeZone {
                target: TargetFilter::TrackedSet { .. },
                ..
            }
        ),
        "{name}: the modifier-bearing LTB return must not be rewritten, got {ltb_effect:?}"
    );

    // Coverage-honesty gate (the maintainer's explicit ask, #6055): the
    // unsupported modifier-bearing return must be visible to the coverage
    // tooling, not silently green. Build a CardFace with BOTH triggers (the gap
    // marker lives on the LTB return trigger) and assert the gap surfaces.
    let face = CardFace {
        name: name.to_string(),
        triggers: result.triggers.clone(),
        ..CardFace::default()
    };
    assert!(
        card_face_has_unimplemented_parts(&face),
        "{name}: the modifier-bearing linked return must report an Unimplemented part so the \
         card is not falsely marked supported"
    );
    let gaps = card_face_gaps(&face);
    assert!(
        !gaps.is_empty(),
        "{name}: coverage must report a non-empty gap set for the unsupported return"
    );
    assert!(
        gaps.iter()
            .any(|g| g == "Effect:modifier_bearing_linked_return"),
        "{name}: coverage gaps must contain the modifier_bearing_linked_return marker, got: {gaps:?}"
    );
}

/// Positive-safety companion to `mass_exile_ltb_return_with_entry_modifier_is_not_paired`:
/// the already-merged unmodified class (Worldgorger Dragon) must still report
/// ZERO coverage gaps after the modifier-unsupported path was added — proving
/// the coverage-honesty fix did not regress the supported class. Oracle text
/// verbatim from `crates/engine/tests/fixtures/integration_cards.json.gz` (2026-07).
#[test]
fn mass_exile_ltb_return_unmodified_class_reports_no_gaps() {
    use engine::game::coverage::{card_face_gaps, card_face_has_unimplemented_parts};
    use engine::types::card::CardFace;

    let oracle = "Flying, trample\n\
             When this creature enters, exile all other permanents you control.\n\
             When this creature leaves the battlefield, return the exiled cards to the \
             battlefield under their owners' control.";
    let name = "Worldgorger Dragon";
    let result = parse(oracle, name, &[], &["Creature"], &[]);

    let face = CardFace {
        name: name.to_string(),
        triggers: result.triggers.clone(),
        ..CardFace::default()
    };
    assert!(
        !card_face_has_unimplemented_parts(&face),
        "{name}: the unmodified linked-return class must report no Unimplemented parts"
    );
    assert!(
        card_face_gaps(&face).is_empty(),
        "{name}: the unmodified linked-return class must report zero coverage gaps, got: {:?}",
        card_face_gaps(&face)
    );
}

/// The brace-matched BODY of the declaration `header` opens: the text between
/// the first `{` at or after `header`'s start and its matching `}`.
///
/// One rule serves both call shapes — an enum header that already ends in the
/// brace (`"pub enum Effect {"`) and a function header that does not
/// (`"fn render_effect_descriptions("`, whose signature carries no brace).
///
/// Test-only source lexing, in the `src/source_census.rs` family: it reads Rust
/// SOURCE TEXT for a census, never Oracle text for parsing dispatch. The comment
/// policy is not ours — callers pass text already through
/// `source_census::code_lines`, so a header quoted in a doc comment cannot be
/// what `find` lands on.
fn census_body<'a>(src: &'a str, header: &str) -> &'a str {
    let start = src
        .find(header)
        .unwrap_or_else(|| panic!("census: `{header}` not found in the scanned source"));
    let open = start
        + src[start..]
            .find('{')
            .unwrap_or_else(|| panic!("census: no opening brace after `{header}`"));
    let mut depth = 0usize;
    for (offset, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open + 1..open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("census: unbalanced braces after `{header}`");
}

/// The variant (or field) names declared at depth 0 of a `census_body` slice.
///
/// Splits on depth-0 commas, drops `#[..]` attributes, and takes the leading
/// identifier of each part. Struct-like, tuple-like, and unit variants all yield
/// exactly their name.
fn census_variant_names(body: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in body.chars() {
        match ch {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            _ => {}
        }
        if ch == ',' && depth == 0 {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    parts.push(current);

    let mut names = Vec::new();
    for part in parts {
        // Drop attributes so `#[serde(default)] Foo` yields `Foo`, not `serde`.
        let mut without_attrs = String::new();
        let mut rest = part.as_str();
        while let Some(hash) = rest.find("#[") {
            without_attrs.push_str(&rest[..hash]);
            let mut depth = 0i32;
            let mut end = None;
            for (offset, ch) in rest[hash..].char_indices() {
                match ch {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(hash + offset + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(e) => rest = &rest[e..],
                None => {
                    rest = "";
                    break;
                }
            }
        }
        without_attrs.push_str(rest);

        let trimmed = without_attrs.trim_start();
        let ident: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !ident.is_empty() && trimmed.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            names.push(ident);
        }
    }
    names
}

/// CR 201.5a — STANDING CENSUS for the display-render net's carrier set.
///
/// `render_effect_descriptions` (`parser/oracle.rs`) ends in `_ => {}` on
/// purpose: a wildcard-free match would duplicate `types::ability_visit`'s
/// ~206-name leaf list, the duplication CLAUDE.md forbids. This census restores
/// the decision-forcing property the wildcard gives up, in the shape
/// `src/source_census.rs` documents ("a producer added or deleted without
/// adjudication is a counted event"), and routes through that module's
/// comment-stripping authority rather than bringing its own policy.
///
/// MEASURED: adding an `Effect` variant with an `AbilityDefinition` payload
/// breaks 20 wildcard-free matches elsewhere in the crate and NONE of them is
/// the render net; this census is what fails.
///
/// THREE PINS, for the three ways the descend set can silently rot:
///
/// 1. `EFFECT_VARIANT_PIN` — a new `Effect` VARIANT.
/// 2. `PAYLOAD_ENUM_PINS` — a new variant on an INTERMEDIATE PAYLOAD ENUM. Each
///    of these five enums has exactly ONE description-reaching variant today, so
///    the net's arms destructure that one variant and every sibling falls
///    through the outer `_ => {}` — silently, since neither the `Effect`-variant
///    pin nor the planted-marker fixture can see it. `types::ability_visit` has
///    the same shape for `VoteSubject`/`CounterSourceRider` and only DISCLOSES
///    it in prose; pinning is chosen here instead because the cost is one table
///    row per enum rather than a leaf-name list, and because prose disclosure is
///    exactly what let four carriers escape a careful manual enumeration.
///    `DelayedTriggerCondition` is deliberately ABSENT from the table:
///    `render_delayed_condition_descriptions`'s match is wildcard-free, so a new
///    variant there is a COMPILE ERROR — strictly stronger than a pin.
/// 3. `RENDER_DESCEND` — the net's own arm set, scanned out of its body, so
///    deleting an arm reds independently of any behavioral fixture.
///
/// `ModalChoice` is deliberately ABSENT from all three. It is a STRUCT reached by
/// FIELD (`ParsedAbilities::modal`, `AbilityDefinition::modal`), not an `Effect`
/// variant and not a payload ENUM, so no mechanism here can see it — a new field
/// on it is drift class (c). `parser::oracle::render_modal_descriptions`
/// destructures it EXHAUSTIVELY instead, making that addition a COMPILE ERROR,
/// which is strictly stronger than a pin — the same trade
/// `render_delayed_condition_descriptions`' wildcard-free match makes.
///
/// Regenerate the descend set (the derivation rule is FIELD TYPES followed
/// transitively into named types, with `AbilityCost` blocked as a separate
/// axis — unblocked it makes 184 of 232 variants carriers via
/// `TargetFilter -> TypedFilter -> FilterProp -> Keyword -> AbilityCost`) with
/// the closure script in the plan's carrier-enumeration section.
///
/// Revert-to-red (a): add any `Effect` variant -> the count pin fails, naming
/// the classification decision. (b): add a variant to any of the five payload
/// enums -> that row fails, naming the outer-wildcard hole. (c): delete any arm
/// from `render_effect_descriptions` -> the descend-set equality fails, naming
/// it.
#[test]
fn render_net_effect_carrier_census() {
    const EFFECT_VARIANT_PIN: usize = 238;
    /// `(enum header, pinned variant count, the ONE variant the net destructures)`.
    const PAYLOAD_ENUM_PINS: &[(&str, usize, &str)] = &[
        ("pub enum CastingPermission {", 8, "ExileWithAltCost"),
        ("pub enum ExiledSpellRider {", 2, "ReturnTo"),
        ("pub enum CounterSourceRider {", 2, "LosesAbilities"),
        ("pub enum VoteSubject {", 2, "Objects"),
    ];
    const RENDER_DESCEND: &[&str] = &[
        "AddPendingEntersModifications",
        "AddTargetReplacement",
        "BecomeCopy",
        "ChooseOneOf",
        "CopySpell",
        "CopyTokenOf",
        "Counter",
        "CreateBoon",
        "CreateDelayedTrigger",
        "CreateDrawReplacement",
        "CreateEmblem",
        "CreatePlaneswalkReplacement",
        "EachPlayerCopyChosen",
        "ExileResolvingSpellInsteadOfGraveyard",
        "ExileTopCardContest",
        "FlipCoins",
        "GenericEffect",
        "GrantCastingPermission",
        "Mana",
        "ReturnAsAura",
        "RevealFromHand",
        "RollDie",
        "SeparateIntoPiles",
        "Token",
        "Unimplemented",
        "Vote",
    ];

    let ability_src =
        crate::source_census::code_lines(include_str!("../../../phase-ast/src/types/ability.rs"));
    let variants = census_variant_names(census_body(&ability_src, "pub enum Effect {"));
    assert!(
        variants.len() > 200 && variants.iter().any(|v| v == "GenericEffect"),
        "reach-guard: the scan found `pub enum Effect`'s body (n={}), not an \
         empty or wrong region",
        variants.len()
    );
    assert_eq!(
        variants.len(),
        EFFECT_VARIANT_PIN,
        "`Effect` gained or lost a variant. Decide whether any of its FIELD TYPES \
         transitively reaches a `description` string; if so add a descend arm to \
         `render_effect_descriptions` (parser/oracle.rs) and list it in \
         RENDER_DESCEND, then bump this pin."
    );

    let mana_src =
        crate::source_census::code_lines(include_str!("../../../phase-ast/src/types/mana.rs"));
    for (header, pin, destructured) in PAYLOAD_ENUM_PINS.iter().copied().chain([(
        "pub enum ManaSpellGrant {",
        4usize,
        "TriggerOnSpend",
    )]) {
        let src = if header.contains("ManaSpellGrant") {
            &mana_src
        } else {
            &ability_src
        };
        let payload = census_variant_names(census_body(src, header));
        assert!(
            payload.iter().any(|v| v == destructured),
            "reach-guard: the scan found `{header}`'s body, which must contain the \
             one variant the render net destructures ({destructured}); got {payload:?}"
        );
        assert_eq!(
            payload.len(),
            pin,
            "`{header}` gained or lost a variant. `render_effect_descriptions` \
             destructures only `{destructured}` from this enum, and every sibling \
             falls through its outer `_ => {{}}` UNSEEN — neither the `Effect` \
             variant pin nor the planted-marker fixture catches that. Decide \
             whether the new variant reaches a `description`; if so give it its \
             own arm, then bump this pin."
        );
    }

    let oracle_src = crate::source_census::code_lines(include_str!(
        "../../../phase-parser/src/parser/oracle.rs"
    ));
    let net = census_body(&oracle_src, "fn render_effect_descriptions(");
    let mut arms: Vec<String> = Vec::new();
    for (i, _) in net.match_indices("Effect::") {
        let name: String = net[i + "Effect::".len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && !arms.contains(&name) {
            arms.push(name);
        }
    }
    arms.sort();
    assert!(
        !arms.is_empty(),
        "reach-guard: the arm scan found the net's body"
    );
    let mut pinned: Vec<String> = RENDER_DESCEND.iter().map(|s| s.to_string()).collect();
    pinned.sort();
    assert_eq!(arms, pinned, "render_effect_descriptions' descend arms");
    for name in RENDER_DESCEND {
        assert!(
            variants.iter().any(|v| v == name),
            "{name} is pinned as a descend arm but is not an `Effect` variant"
        );
    }
}

/// CR 118.12 + CR 118.12a: an "unless [a player] pays [cost]" modifier riding a clause that
/// becomes a `ChooseOneOf` BRANCH must survive the branch lift.
/// `oracle_effect::ability_definition_from_clause` used to copy `ParsedEffectClause`'s fields
/// one at a time and silently omitted `unless_pay`, so the modifier the clause parser had
/// already recognised was dropped between clause and definition without a diagnostic.
///
/// **The fixture is SYNTHETIC and deliberately so.** Reaching the branch lift with a
/// modifier-bearing clause needs a clause that carries its OWN "unless ... pays" INSIDE the
/// default half of a "[default] unless that player [A] or [B]" three-branch choice — i.e. two
/// "unless"es in one clause. A census of every distinct oracle text in
/// `data/mtgjson/AtomicCards.json` (sha256 6b3d6262…22d5, 67184 text lines over all printings,
/// not just the first per name) finds 20 lines matching `try_parse_unless_three_branch_choice`'s
/// own text gate — its " unless that player " / " unless they " needle, a following " or ", and
/// its rejection of a second " or " — and ZERO of those 20 have a second "unless" in the default
/// or either alternative half. That line set is a SUPERSET of what actually routes to the
/// splitter (a matching line can still fail to parse into three branches), which is the safe
/// direction for a zero-result claim: nothing routes there that the superset omits. The other
/// half of the argument is that `unless_pay` is only ever set by paths requiring the literal
/// "unless " in the clause text (`parse_unless_payment`, `try_parse_unless_player_have_deal_damage`,
/// and the `strip_unless_entered_suffix` deferral). The drop was therefore latent, not live, on
/// today's corpus; this test pins the seam so it stays closed for the next clause shape routed
/// through a branch site. The sibling counter-choice branch site cannot reach it at all:
/// `parse_effect_clause` strips the unless-payment suffix into the PARENT clause before the
/// counter-choice recognizer runs, so its synthesized branch text never contains "unless".
#[test]
fn unless_pay_survives_the_choose_one_of_branch_lift() {
    let parsed = parse_oracle_text(
        "Counter target spell unless its controller pays {2} unless that player sacrifices a creature or discards a card.",
        "Synthetic Branch Unless Pay",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("the three-branch unless choice parses to one ability");
    let Effect::ChooseOneOf { branches, .. } = ability.effect.as_ref() else {
        panic!(
            "expected a ChooseOneOf three-branch choice, got {:?}",
            ability.effect
        );
    };
    assert_eq!(
        branches.len(),
        3,
        "default consequence plus two avoidance options"
    );

    // Reach guard: branch 0 really is the default consequence (the half that carried the
    // inner "unless its controller pays {2}"), not an avoidance option. Without this the
    // modifier assertion below could pass against the wrong branch.
    assert!(
        matches!(branches[0].effect.as_ref(), Effect::Counter { .. }),
        "branch 0 must be the default consequence, got {:?}",
        branches[0].effect
    );

    // THE discriminating assertion: reverting `def.unless_pay = unless_pay` in
    // `ability_definition_from_clause` makes this `None`. Measured both ways.
    let modifier = branches[0]
        .unless_pay
        .as_ref()
        .expect("the default branch must carry its own CR 118.12a unless-payment modifier");
    assert!(
        matches!(
            &modifier.cost,
            AbilityCost::Mana { cost } if *cost == ManaCost::generic(2)
        ),
        "the {{2}} the default half named must survive the lift, got {:?}",
        modifier.cost
    );
    // CR 118.12a: the payer is the countered spell's controller, bound through the branch's
    // own parent target slot rather than rescanned at resolution.
    assert_eq!(modifier.payer, TargetFilter::ParentTargetController);

    // Siblings: the avoidance options are costs the player may take INSTEAD, so they carry no
    // unless-payment of their own. This pins the lift to the clause that actually had one and
    // would catch a fix that sprayed the parent's modifier onto every branch.
    assert!(
        branches[1].unless_pay.is_none() && branches[2].unless_pay.is_none(),
        "avoidance branches must not acquire an unless-payment: {:?} / {:?}",
        branches[1].unless_pay,
        branches[2].unless_pay
    );

    // Non-vacuity: a threaded field that no runtime carrier reads would be inert. This is the
    // exact hand-off `game::effects::choose_one_of::resolve_branch` performs on the chosen
    // branch, and `resolve_chain_body`'s CR 118.12 intercept reads `ResolvedAbility.unless_pay`.
    let resolved = engine::game::ability_utils::build_resolved_from_def(
        &branches[0],
        engine::types::identifiers::ObjectId(1),
        engine::types::player::PlayerId(0),
    );
    assert!(
        resolved.unless_pay.is_some(),
        "the branch definition's modifier must reach the ResolvedAbility the runtime reads"
    );
}

/// CR 614.1c + CR 608.2c: Power Level Analyzer's as-enters choice line carries
/// a source enters-with sentence (and an "instead" override) beside the choice.
/// One replacement cannot carry them all, so the whole line is one
/// `enters_with_clause` gap — never a `Choose` replacement that drops the
/// counters (issue #9993). Oracle text verbatim (Scryfall / AtomicCards).
#[test]
fn as_enters_choose_line_with_source_enters_with_sentence_is_one_enters_with_clause_gap() {
    use engine::game::coverage::{card_face_gaps, card_face_has_unimplemented_parts};
    use engine::types::card::CardFace;

    const NAME: &str = "Power Level Analyzer";
    const ORACLE: &str = "As Power Level Analyzer enters the battlefield, you and target opponent each secretly choose a number between 1 and 10, then reveal. Power Level Analyzer enters with a number of +1/+1 counters equal to the distance between the two numbers. If both players chose the same number, put seven +1/+1 counters on Power Level Analyzer instead.";
    const NORMALIZED: &str = "As ~ enters the battlefield, you and target opponent each secretly choose a number between 1 and 10, then reveal. ~ enters with a number of +1/+1 counters equal to the distance between the two numbers. If both players chose the same number, put seven +1/+1 counters on ~ instead.";
    const FIRST_SENTENCE: &str = "As Power Level Analyzer enters the battlefield, you and target opponent each secretly choose a number between 1 and 10, then reveal.";

    let parsed = parse(ORACLE, NAME, &[], &["Artifact", "Creature"], &["Construct"]);
    // (1) Negative, revert-failing: no replacement carries part of the line.
    assert!(parsed.replacements.is_empty(), "{:?}", parsed.replacements);
    // (2) Positive: the whole line is exactly one enters_with_clause gap.
    assert_eq!(parsed.abilities.len(), 1, "{:?}", parsed.abilities);
    assert!(
        matches!(
            &*parsed.abilities[0].effect,
            Effect::Unimplemented { name, description: Some(text) }
                if name == "enters_with_clause" && text == NORMALIZED
        ),
        "the whole line is the gap, got {:?}",
        parsed.abilities
    );
    assert!(parsed.triggers.is_empty(), "{:?}", parsed.triggers);
    assert!(parsed.statics.is_empty(), "{:?}", parsed.statics);
    // (3) Coverage honesty: the card face reports the AST gap.
    let face = CardFace {
        name: NAME.to_string(),
        abilities: parsed.abilities.clone(),
        triggers: parsed.triggers.clone(),
        static_abilities: parsed.statics.clone(),
        replacements: parsed.replacements.clone(),
        ..CardFace::default()
    };
    assert!(card_face_has_unimplemented_parts(&face));
    assert!(
        card_face_gaps(&face)
            .iter()
            .any(|g| g == "Effect:enters_with_clause"),
        "{:?}",
        card_face_gaps(&face)
    );
    // Reach guards for (1): the gap authority owns the line, and the
    // as-enters choose arm is live — it still claims the choice sentence alone,
    // so (1) is not satisfied by a broken arm.
    assert_eq!(
        enters_with_fails_closed(ORACLE, NAME),
        Some(EntersWithGap::Clause)
    );
    let choice = parse_replacement_line(FIRST_SENTENCE, NAME)
        .expect("the choice sentence alone is still an as-enters choose replacement");
    assert_eq!(choice.event, ReplacementEvent::Moved);
    assert!(
        matches!(
            &*choice.execute.as_ref().expect("execute").effect,
            Effect::Choose {
                choice_type: ChoiceType::NumberRange {
                    min: 1,
                    max: Some(10),
                    ..
                },
                persist: true,
                ..
            }
        ),
        "{:?}",
        choice.execute
    );
}

/// CR 603.2 + CR 603.10 + CR 603.3a + CR 613.1b: Risky Move, verbatim, parses
/// to the right shape: the second ability ("When you gain control of this
/// enchantment from another player, ...") is the gain-side
/// `ChangesController`, and the first is the upkeep `GiveControl` of the
/// enchantment to the scoped (active) player. The card nevertheless stays
/// honestly unsupported: layer-2 control changes (Confiscate, Control Magic)
/// emit no `ControllerChanged` (phase-rs/phase#9903), so `card_face_gaps`
/// reports exactly the gain-side trigger until that emitter lands.
#[test]
fn risky_move_gain_side_control_trigger_parses_but_card_stays_unsupported() {
    use engine::types::triggers::{ControlChangeSide, TriggerMode};
    let face = oracle_face_for(
        "Risky Move",
        "At the beginning of each player's upkeep, that player gains control of this enchantment.\nWhen you gain control of this enchantment from another player, choose a creature you control and an opponent. Flip a coin. If you lose the flip, that opponent gains control of that creature.",
        &["Enchantment"],
        &[],
    );
    let gaps = engine::game::coverage::card_face_gaps(&face);
    assert_eq!(
        gaps,
        vec![String::from("Trigger:ChangesController { side: Gains }")]
    );
    assert_eq!(face.triggers.len(), 2, "{:#?}", face.triggers);
    let upkeep = &face.triggers[0];
    assert_eq!(upkeep.mode, TriggerMode::Phase);
    assert_eq!(upkeep.phase, Some(Phase::Upkeep));
    assert_eq!(
        upkeep
            .execute
            .as_ref()
            .expect("upkeep body")
            .effect
            .as_ref(),
        &Effect::GiveControl {
            target: TargetFilter::SelfRef,
            recipient: TargetFilter::ScopedPlayer,
        }
    );
    assert_eq!(
        face.triggers[1].mode,
        TriggerMode::ChangesController {
            side: ControlChangeSide::Gains
        }
    );
}
