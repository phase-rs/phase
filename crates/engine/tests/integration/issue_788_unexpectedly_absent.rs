//! Regression test for GitHub issue #788 — Unexpectedly Absent.
//!
//! "{X}{W}{W} Sorcery — Put target nonland permanent into its owner's library
//! just beneath the top X cards of that library."
//!
//! Reported behavior: the spell "doesn't let me select a target, it just
//! resolves and does nothing." Root cause: the parser had no handling for the
//! "just beneath the top X cards of that library" placement and emitted
//! `Effect::Unimplemented`, so the spell carried no target and no effect.
//!
//! The fix parses the clause to
//! `Effect::PutAtLibraryPosition { target: nonland permanent,
//! position: BeneathTop { depth: X } }`. "Beneath the top X cards" (CR 401.7)
//! places the permanent so that exactly X cards remain above it — the 0-based
//! library insertion index equals the announced `{X}`.
//!
//! Discriminator: with X = 2 and a four-card library, the targeted permanent
//! must land at library index 2 (two cards above, two below). A "do nothing"
//! regression leaves it on the battlefield; a naive top/bottom placement lands
//! it at index 0 or at the end.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Effect, LibraryPosition, QuantityExpr, QuantityRef, TargetFilter, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::identifiers::CardId;
use engine::types::zones::Zone;

const ORACLE: &str =
    "Put target nonland permanent into its owner's library just beneath the top X cards of that library.";

#[test]
fn unexpectedly_absent_parses_to_beneath_top_x_placement() {
    let parsed = parse_oracle_text(
        ORACLE,
        "Unexpectedly Absent",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .first()
        .expect("Unexpectedly Absent must parse to a spell ability");

    let Effect::PutAtLibraryPosition {
        target, position, ..
    } = &*ability.effect
    else {
        panic!(
            "expected PutAtLibraryPosition (not Unimplemented), got {:?}",
            ability.effect
        );
    };

    // The spell targets a nonland permanent — the missing target was the
    // user-visible symptom of #788.
    let TargetFilter::Typed(typed) = target else {
        panic!("expected a typed nonland-permanent target, got {target:?}");
    };
    assert!(
        typed.type_filters.len() == 2,
        "target must constrain to permanent + nonland, got {:?}",
        typed.type_filters
    );

    // The placement is "beneath the top X cards", with X = the spell's {X}.
    assert_eq!(
        *position,
        LibraryPosition::BeneathTop {
            depth: QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string()
                }
            }
        },
        "placement must be BeneathTop bound to the cast variable X"
    );
}

#[test]
fn unexpectedly_absent_places_target_beneath_top_x_cards() {
    let parsed = parse_oracle_text(
        ORACLE,
        "Unexpectedly Absent",
        &[],
        &["Sorcery".to_string()],
        &[],
    );
    let ability = parsed
        .abilities
        .into_iter()
        .next()
        .expect("Unexpectedly Absent must parse to a spell ability");

    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    // P0 casts the spell; P1 owns and controls the targeted permanent.
    let source = create_object(
        runner.state_mut(),
        CardId(900),
        P0,
        "Unexpectedly Absent".to_string(),
        Zone::Stack,
    );

    let victim = create_object(
        runner.state_mut(),
        CardId(901),
        P1,
        "Grizzly Bears".to_string(),
        Zone::Battlefield,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];

    // Four cards already in P1's library so index 2 is a true interior slot.
    let mut lib = Vec::new();
    for idx in 0..4 {
        let id = create_object(
            runner.state_mut(),
            CardId(910 + idx),
            P1,
            format!("Library Card {idx}"),
            Zone::Library,
        );
        lib.push(id);
    }
    let lib_len_before = runner.state().players[P1.0 as usize].library.len();
    assert_eq!(lib_len_before, 4, "P1 library should start with four cards");

    // Announce X = 2: the permanent goes beneath the top two cards.
    let mut resolved =
        build_resolved_from_def_with_targets(&ability, source, P0, vec![TargetRef::Object(victim)]);
    resolved.chosen_x = Some(2);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("Unexpectedly Absent resolution");

    let state = runner.state();
    let victim_obj = &state.objects[&victim];
    assert_eq!(
        victim_obj.zone,
        Zone::Library,
        "the targeted permanent must leave the battlefield for its owner's library"
    );
    assert_eq!(
        victim_obj.owner, P1,
        "the permanent goes into ITS OWNER's library"
    );

    let library = &state.players[P1.0 as usize].library;
    assert_eq!(
        library.len(),
        lib_len_before + 1,
        "library grows by exactly the placed permanent"
    );
    let pos = library
        .iter()
        .position(|id| *id == victim)
        .expect("victim must be in P1's library");
    assert_eq!(
        pos, 2,
        "beneath the top X=2 cards means index 2 (two cards above it); \
         index 0 would be a top placement and the end would be a bottom placement"
    );
}
