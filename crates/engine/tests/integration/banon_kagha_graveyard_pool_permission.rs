//! Banon, the Returners' Leader and Kagha, Shadow Archdruid — the
//! pool-qualified graveyard cast permission.
//!
//! Both print a once-per-turn graveyard cast permission whose pool is NOT the
//! whole graveyard but the subset that arrived there this turn, qualified by
//! where it arrived FROM:
//!
//!   Banon — "Pray — Once during each of your turns, you may cast a creature
//!   spell from among cards in your graveyard that were put there from anywhere
//!   other than the battlefield this turn."
//!   Kagha — "Once during each of your turns, you may play a land or cast a
//!   permanent spell from among cards in your graveyard that were put there
//!   from your library this turn."
//!
//! CR 604.2: both are static abilities creating a continuous effect.
//! CR 601.2a: the permission authorizes moving a matching card to the stack.
//! CR 400.7: "put there from <zone>" is a zone-change provenance predicate,
//! carried by `FilterProp::ZoneChangedThisTurn` on the permission's `affected`
//! filter rather than by a pool axis on the static mode.
//! A printed pool qualifier the parser cannot model must DECLINE the
//! whole permission — dropping it would offer the entire graveyard, which is
//! strictly more permissive than the printed instruction.
//! CR 701.17a: milling is specifically from the TOP of a library, so "milled
//! this turn" is not the same predicate as a library→graveyard zone change and
//! is deliberately NOT approximated by one (Raul, Trouble Shooter stays a gap).

use engine::game::casting::{
    graveyard_lands_playable_by_permission, spell_objects_available_to_cast,
};
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{FilterProp, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;
use engine::types::StaticDefinition;

const BANON_ORACLE: &str = "Pray — Once during each of your turns, you may cast a creature spell \
from among cards in your graveyard that were put there from anywhere other than the battlefield \
this turn.\nWhenever you attack, you may pay {1} and discard a card. If you do, draw a card.";

const KAGHA_ORACLE: &str = "Whenever Kagha attacks, it gains deathtouch until end of turn. Mill \
two cards.\nOnce during each of your turns, you may play a land or cast a permanent spell from \
among cards in your graveyard that were put there from your library this turn.";

/// Raul, Trouble Shooter — a DISCRIMINATING sibling. Shares Banon's anchor and
/// frequency but qualifies the pool by a keyword ACTION (CR 701.17a mill), not a
/// zone-change origin. It must stay unsupported; if it ever parses, the pool
/// predicate has been approximated rather than modeled.
const RAUL_ORACLE: &str = "Once during each of your turns, you may cast a spell from among cards \
in your graveyard that were milled this turn.\n{T}: Each player mills a card.";

/// Eye of Duskmantle — the second DISCRIMINATING sibling, and the one that
/// measures the qualifier slot is closed by DEFAULT rather than by a list of
/// recognised shapes.
///
/// Its pool qualifier is a possessive-perfect clause ("cards in your graveyard
/// **you've surveilled this turn**"), not a `that`-relative clause, and it
/// additionally prints an alternative cost. The first cut of the anchor
/// widening keyed "is a qualifier present?" on a leading `"that "` and so read
/// this line as unqualified: it emitted a `GraveyardCastPermission` over the
/// ENTIRE graveyard with the alternative cost dropped too. Every test in the
/// repo was green; only the card-by-card parse delta caught it.
const EYE_OF_DUSKMANTLE_ORACLE: &str = "Flying, lifelink\nYou may play lands and cast spells from \
among cards in your graveyard you've surveilled this turn. If you cast a spell this way, you pay \
life equal to its mana value rather than paying its mana cost.";

/// Karador — the unqualified baseline. Pins that widening the anchor did not
/// disturb the bare `from your graveyard` form.
const KARADOR_ORACLE: &str =
    "Once during each of your turns, you may cast a creature spell from your graveyard.";

fn parse(oracle: &str, name: &str) -> ParsedAbilities {
    parse_oracle_text(
        oracle,
        name,
        &[],
        &["Creature".to_string()],
        &["Human".to_string()],
    )
}

/// Positive reach guard: the permission must actually be a
/// `GraveyardCastPermission` static, not an `Unimplemented` gap that happens to
/// satisfy a weaker assertion. Returns its `affected` filter.
fn graveyard_permission(parsed: &ParsedAbilities) -> &StaticDefinition {
    parsed
        .statics
        .iter()
        .find(|def| matches!(def.mode, StaticMode::GraveyardCastPermission { .. }))
        .expect("a GraveyardCastPermission static must parse")
}

fn properties_of(filter: &TargetFilter) -> Vec<FilterProp> {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.clone(),
        TargetFilter::And { filters } => filters.iter().flat_map(properties_of).collect(),
        _ => Vec::new(),
    }
}

/// CR 400.7 + CR 604.2: Banon's pool qualifier lowers to BOTH halves of the
/// printed predicate.
///
/// The `Not` alone would be satisfied vacuously by a card that has sat in the
/// graveyard since an earlier turn (it carries no battlefield→graveyard record
/// either), so the unconstrained-origin arrival property is load-bearing and is
/// asserted explicitly rather than by a `len()` count.
#[test]
fn banon_pool_qualifier_lowers_to_arrival_plus_origin_exclusion() {
    let parsed = parse(BANON_ORACLE, "Banon, the Returners' Leader");
    let def = graveyard_permission(&parsed);

    assert!(
        matches!(
            def.mode,
            StaticMode::GraveyardCastPermission {
                frequency: CastFrequency::OncePerTurn,
                play_mode: engine::types::ability::CardPlayMode::Cast,
                ..
            }
        ),
        "Banon prints a once-per-turn CAST permission, got {:?}",
        def.mode
    );

    let affected = def.affected.as_ref().expect("permission must scope a pool");
    let props = properties_of(affected);

    assert!(
        props.contains(&FilterProp::ZoneChangedThisTurn {
            from: None,
            to: Some(Zone::Graveyard),
        }),
        "the pool must require arrival in the graveyard THIS TURN — without it the \
         origin exclusion below is satisfied vacuously by every card already there; \
         got {props:?}"
    );
    assert!(
        props.contains(&FilterProp::Not {
            prop: Box::new(FilterProp::ZoneChangedThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Graveyard),
            }),
        }),
        "the pool must exclude a battlefield origin (\"anywhere other than the \
         battlefield\"); got {props:?}"
    );
}

/// CR 400.7: Kagha's affirmative sibling — same anchor, positive
/// origin — with the qualifier attached to EACH branch rather than ANDed onto
/// the union.
///
/// Kagha's pool phrase is the shared trailing complement of both verbs ("play a
/// land **or** cast a permanent spell from among cards in your graveyard that
/// were put there from your library this turn"), so it governs the land branch
/// and the spell branch alike. Asserting per branch, not over a flattened
/// property set, is what distinguishes that from ANDing both branches'
/// qualifiers onto the union — which would require a card satisfying either
/// printed alternative to satisfy both.
#[test]
fn kagha_pool_qualifier_scopes_each_branch_of_the_disjunction() {
    let parsed = parse(KAGHA_ORACLE, "Kagha, Shadow Archdruid");
    let def = graveyard_permission(&parsed);
    let affected = def.affected.as_ref().expect("permission must scope a pool");

    let expected = FilterProp::ZoneChangedThisTurn {
        from: Some(Zone::Library),
        to: Some(Zone::Graveyard),
    };
    let TargetFilter::Or { filters } = affected else {
        panic!("Kagha prints a land/spell disjunction, so its pool is a union; got {affected:?}");
    };
    assert_eq!(filters.len(), 2, "one branch per printed verb");
    for branch in filters {
        assert!(
            properties_of(branch).contains(&expected),
            "every branch must carry the pool qualifier; branch {branch:?} did not"
        );
    }
}

/// CR 400.7: an affirmative pool qualifier with NO time phrase is
/// refused rather than silently narrowed to a this-turn pool.
///
/// The shared `parse_zone_changed_this_turn_suffix` keeps `opt(" this turn")`
/// for its existing callers, but it always yields a `ZoneChangedThisTurn`
/// result — so the pool path requires the time phrase instead of inheriting a
/// predicate the card did not print.
#[test]
fn pool_qualifier_without_a_time_phrase_declines() {
    let line = "Once during each of your turns, you may cast a creature spell from among \
                cards in your graveyard that were put there from your library.";
    let parsed = parse(line, "Synthetic Timeless Pool");

    assert!(
        !parsed
            .statics
            .iter()
            .any(|def| matches!(def.mode, StaticMode::GraveyardCastPermission { .. })),
        "an unlimited pool must not be narrowed to \"this turn\"; got {:?}",
        parsed.statics
    );
    assert_refused_by_the_static_parser(&parsed, line);
}

/// Positive reach guard for a DECLINE row: prove the line actually reached the
/// static dispatcher and was refused THERE, rather than passing its assertion
/// because routing dropped it somewhere upstream.
///
/// Without this, a regression in `oracle_classifier` that stopped routing these
/// lines to the static parser would leave every decline row below green while
/// `read_graveyard_pool_qualifier` was never reached at all — the guards
/// against silent drops would themselves be unguarded.
fn assert_refused_by_the_static_parser(parsed: &ParsedAbilities, line: &str) {
    // Select the residual BY LINE, not by position: a card can carry several
    // unsupported lines (Eye of Duskmantle's keyword line yields its own when
    // the keywords are not declared), and taking the first one would assert
    // against a residual that has nothing to do with the pool anchor.
    let name = parsed
        .abilities
        .iter()
        .find_map(|ability| match &*ability.effect {
            engine::types::ability::Effect::Unimplemented { name, description } => description
                .as_deref()
                .is_some_and(|text| text.contains(line))
                .then(|| name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "the pool-anchored line must survive as an Unimplemented residual naming \
                 it — if it vanished, the line was silently dropped rather than refused. \
                 Line: {line:?}"
            )
        });
    assert_eq!(
        name, "static_structure",
        "the line must reach the STATIC dispatcher and be refused there — another \
         gap name means routing changed and this row no longer measures the pool \
         guard at all"
    );
}

/// CR 701.17a: the silent-drop guard.
///
/// Raul's pool qualifier ("that were milled this turn") is printed but not
/// modeled. It must leave the permission UNPARSED. The failure this pins is not
/// hypothetical: the first cut of the anchor widening parsed Raul into a
/// `GraveyardCastPermission` with an EMPTY property set, i.e. a permission over
/// his entire graveyard, while every existing test stayed green.
#[test]
fn raul_unmodeled_pool_qualifier_declines_instead_of_offering_the_whole_graveyard() {
    let parsed = parse(RAUL_ORACLE, "Raul, Trouble Shooter");

    assert!(
        !parsed
            .statics
            .iter()
            .any(|def| matches!(def.mode, StaticMode::GraveyardCastPermission { .. })),
        "an unmodeled pool qualifier must DECLINE the permission, not emit one whose \
         pool is the whole graveyard; got {:?}",
        parsed.statics
    );
    assert_refused_by_the_static_parser(
        &parsed,
        "Once during each of your turns, you may cast a spell from among cards in your \
         graveyard that were milled this turn.",
    );
}

/// CR 118.9: the second silent-drop guard, on a qualifier shape the
/// first cut did not even recognise as a qualifier.
///
/// Eye of Duskmantle must stay unsupported. Two clauses would be dropped if it
/// parsed: the surveil-this-turn pool restriction AND the "pay life equal to
/// its mana value rather than paying its mana cost" alternative cost. Either
/// drop alone is strictly more permissive than the printed card; together they
/// grant the whole graveyard for free.
#[test]
fn eye_of_duskmantle_unrecognized_qualifier_shape_declines() {
    let parsed = parse(EYE_OF_DUSKMANTLE_ORACLE, "Eye of Duskmantle");

    assert!(
        !parsed
            .statics
            .iter()
            .any(|def| matches!(def.mode, StaticMode::GraveyardCastPermission { .. })),
        "a pool qualifier this parser does not model must DECLINE the permission — \
         emitting one here grants the entire graveyard AND drops the alternative \
         cost; got {:?}",
        parsed.statics
    );
    assert_refused_by_the_static_parser(
        &parsed,
        "You may play lands and cast spells from among cards in your graveyard you've \
         surveilled this turn.",
    );
}

/// The qualifier slot's boundary is a FULL STOP, not any punctuation.
///
/// An earlier cut also treated `,` and `;` as "the pool phrase ends here", which
/// is a door that opens the over-permissive way: a qualifier introduced after a
/// comma would read as absent and emit a whole-graveyard permission. No printed
/// card comma-separates a restrictive qualifier from its head noun, so refusing
/// it costs nothing — this row exists so a future widening has to break a test
/// rather than a card.
#[test]
fn comma_separated_pool_qualifier_declines_rather_than_reading_as_absent() {
    let parsed = parse(
        "Once during each of your turns, you may cast a creature spell from among cards in \
         your graveyard, milled this turn.",
        "Synthetic Comma Qualifier",
    );

    assert!(
        !parsed
            .statics
            .iter()
            .any(|def| matches!(def.mode, StaticMode::GraveyardCastPermission { .. })),
        "a qualifier after a comma must still DECLINE — treating `,` as a clause end \
         emits a permission over the whole graveyard; got {:?}",
        parsed.statics
    );
    assert_refused_by_the_static_parser(
        &parsed,
        "Once during each of your turns, you may cast a creature spell from among cards \
         in your graveyard, milled this turn.",
    );
}

/// The unqualified baseline must be untouched by the anchor widening.
#[test]
fn karador_bare_graveyard_anchor_is_unchanged() {
    let parsed = parse(KARADOR_ORACLE, "Karador, Ghost Chieftain");
    let def = graveyard_permission(&parsed);
    let affected = def.affected.as_ref().expect("permission must scope a pool");
    assert!(
        properties_of(affected).is_empty(),
        "the bare anchor states no pool qualifier, so it must add no properties; got {:?}",
        properties_of(affected)
    );
}

/// CR 601.2a + CR 400.7: the RUNTIME row — the parsed properties are actually
/// evaluated against graveyard cards, and they discriminate.
///
/// Three cards sit in P0's graveyard under Banon's permission:
///   * `milled` — put there from the library THIS turn. Castable.
///   * `died` — put there from the battlefield THIS turn. NOT castable, because
///     Banon excludes a battlefield origin.
///   * `stale` — already there, no zone change this turn. NOT castable, because
///     Banon's pool is this-turn arrivals only.
///
/// The positive row is what makes the two negatives non-vacuous: without it a
/// permission that offered NOTHING at all would pass both exclusions.
#[test]
fn banon_offers_only_this_turn_non_battlefield_arrivals() {
    let mut scenario = GameScenario::new();
    let banon = scenario
        .add_creature_from_oracle(P0, "Banon, the Returners' Leader", 1, 3, BANON_ORACLE)
        .id();
    let milled = scenario.add_card_to_library_top(P0, "Milled Bear");
    let died = scenario.add_creature(P0, "Fallen Bear", 2, 2).id();
    let stale = scenario
        .add_creature_to_graveyard(P0, "Old Bear", 2, 2)
        .id();
    let mut runner = scenario.build();
    // `add_card_to_library_top` creates an untyped object; Banon's pool is
    // scoped to creature spells.
    if let Some(obj) = runner.state_mut().objects.get_mut(&milled) {
        obj.card_types.core_types = vec![CoreType::Creature];
    }

    // Reach guard: the permission actually exists and FUNCTIONS on the
    // battlefield source (CR 113.6 zone-of-function), which is what the runtime
    // rows below depend on.
    assert!(
        engine::game::functioning_abilities::active_static_definitions(
            runner.state(),
            &runner.state().objects[&banon],
        )
        .any(|def| matches!(def.mode, StaticMode::GraveyardCastPermission { .. })),
        "Banon must carry a functioning GraveyardCastPermission static; the runtime \
         rows below are vacuous without it"
    );

    // CR 400.7: drive the REAL moves that establish each card's provenance.
    // `stale` is deliberately left untouched, so it has no arrival this turn.
    let mut events = Vec::new();
    move_through(runner.state_mut(), milled, Zone::Graveyard, &mut events);
    move_through(runner.state_mut(), died, Zone::Graveyard, &mut events);
    assert_eq!(runner.state().objects[&milled].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&died].zone, Zone::Graveyard);

    let castable = spell_objects_available_to_cast(runner.state(), P0);

    assert!(
        castable.contains(&milled),
        "a card put into the graveyard from the LIBRARY this turn is in Banon's pool"
    );
    assert!(
        !castable.contains(&died),
        "a card put into the graveyard from the BATTLEFIELD this turn is excluded — \
         Banon prints \"anywhere other than the battlefield\""
    );
    assert!(
        !castable.contains(&stale),
        "a card that did not arrive this turn is excluded — Banon's pool is \
         this-turn arrivals only"
    );
}

/// CR 601.2a + CR 400.7: the runtime row for Kagha, whose `affected` takes the
/// OTHER branch of `inject_filter_props`.
///
/// Banon's filter is a bare `Typed`, so the props are pushed in place. Kagha's
/// disjunctive path builds an `Or[Land, Permanent]` first, so the props arrive
/// through the `And`-wrap arm instead. Same entry point, different internal
/// branch — without this row the composite-filter arm has AST coverage but no
/// production-pipeline coverage, and an `And`-wrap that silently failed to
/// constrain would look identical in the shape tests.
///
/// The positive row (`milled`, library origin) is what makes the negative
/// (`stale`, no arrival this turn) non-vacuous.
#[test]
fn kagha_composite_filter_pool_is_enforced_at_runtime() {
    let mut scenario = GameScenario::new();
    let kagha = scenario
        .add_creature_from_oracle(P0, "Kagha, Shadow Archdruid", 4, 4, KAGHA_ORACLE)
        .id();
    let milled = scenario.add_card_to_library_top(P0, "Milled Bear");
    let stale = scenario
        .add_creature_to_graveyard(P0, "Old Bear", 2, 2)
        .id();
    let mut runner = scenario.build();
    if let Some(obj) = runner.state_mut().objects.get_mut(&milled) {
        obj.card_types.core_types = vec![CoreType::Creature];
    }

    assert!(
        engine::game::functioning_abilities::active_static_definitions(
            runner.state(),
            &runner.state().objects[&kagha],
        )
        .any(|def| matches!(def.mode, StaticMode::GraveyardCastPermission { .. })),
        "Kagha must carry a functioning GraveyardCastPermission static"
    );

    let mut events = Vec::new();
    move_through(runner.state_mut(), milled, Zone::Graveyard, &mut events);
    assert_eq!(runner.state().objects[&milled].zone, Zone::Graveyard);

    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&milled),
        "a card put into the graveyard from the LIBRARY this turn is in Kagha's pool"
    );
    assert!(
        !castable.contains(&stale),
        "a card that did not arrive from the library this turn is excluded — the \
         And-wrapped pool props must constrain the Or[Land, Permanent] union"
    );
}

/// CR 400.7: the multi-hop row — the shared `ZoneChangedThisTurn` reading is
/// CURRENT-INCARNATION, not any-record — driven through the REAL zone-change
/// pipeline.
///
/// `bounced` starts on the battlefield, dies, is returned to hand, and is
/// discarded back into the graveyard, all in one turn, each hop performed by
/// `zones::move_to_zone` (which routes through `resolve_and_apply_zone_change`,
/// validating the source zone and bumping the incarnation). Its current
/// residency came from the HAND, so Banon must offer it. The engine keeps one
/// `ObjectId` across zone changes, so an any-record reading still sees the
/// stale battlefield→graveyard row and wrongly excludes it.
///
/// Driving the production mover rather than appending snapshots is the point:
/// it demonstrates that the move pipeline and `FilterProp::ZoneChangedThisTurn`
/// agree on which occurrence is final. A hand-built ledger could agree with the
/// filter while disagreeing with production.
///
/// `died` is the paired negative: same number of hops is irrelevant — what
/// matters is that its FINAL arrival is from the battlefield, so it stays
/// excluded. Without it, a reading that admitted everything would pass.
#[test]
fn banon_reads_the_current_incarnation_through_the_real_move_pipeline() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature_from_oracle(P0, "Banon, the Returners' Leader", 1, 3, BANON_ORACLE)
        .id();
    let bounced = scenario.add_creature(P0, "Recurring Bear", 2, 2).id();
    let died = scenario.add_creature(P0, "Fallen Bear", 2, 2).id();
    let mut runner = scenario.build();

    let mut events = Vec::new();
    // `bounced`: battlefield → graveyard → hand → graveyard. Final arrival: HAND.
    move_through(runner.state_mut(), bounced, Zone::Graveyard, &mut events);
    move_through(runner.state_mut(), bounced, Zone::Hand, &mut events);
    move_through(runner.state_mut(), bounced, Zone::Graveyard, &mut events);
    // `died`: battlefield → graveyard. Final arrival: BATTLEFIELD.
    move_through(runner.state_mut(), died, Zone::Graveyard, &mut events);

    // Reach guard 1: both really are in the graveyard, so the rows below are
    // about the READING and not about a move that silently failed.
    assert_eq!(runner.state().objects[&bounced].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&died].zone, Zone::Graveyard);
    // Reach guard 2: the stale battlefield→graveyard row for `bounced` is still
    // on the ledger, which is what makes this a current-incarnation test rather
    // than a test that the earlier hop was forgotten.
    assert!(
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .any(|r| r.object_id == bounced
                && r.from_zone == Some(Zone::Battlefield)
                && r.to_zone == Zone::Graveyard),
        "the stale battlefield hop must still be on the ledger for this row to mean anything"
    );

    let castable = spell_objects_available_to_cast(runner.state(), P0);
    assert!(
        castable.contains(&bounced),
        "the card's CURRENT graveyard residency came from the hand, so Banon offers \
         it — an any-record reading would see the earlier battlefield hop and \
         wrongly exclude it (CR 400.7)"
    );
    assert!(
        !castable.contains(&died),
        "a card whose FINAL arrival is from the battlefield stays excluded"
    );
}

/// CR 305.1 + CR 601.2a: Kagha's `Play` half at the land-play action boundary.
///
/// Kagha's permission is `play_mode: Play`, so a qualifying LAND must be offered
/// by `graveyard_lands_playable_by_permission` — a different authority from the
/// `spell_objects_available_to_cast` path every other runtime row here uses —
/// and must be accepted by `GameAction::PlayLand`. Without this row the newly
/// parsed `Play` branch could be absent or miswired while every other assertion
/// stayed green.
///
/// CR 305.1 also requires the land NOT to appear on the cast path.
///
/// `from_battlefield` is the paired negative: a land whose graveyard arrival was
/// from the battlefield fails Kagha's printed library-origin pool.
#[test]
fn kagha_play_half_authorizes_a_library_origin_land_at_the_play_action() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature_from_oracle(P0, "Kagha, Shadow Archdruid", 4, 4, KAGHA_ORACLE)
        .id();
    let milled_land = scenario.add_card_to_library_top(P0, "Milled Forest");
    let from_battlefield = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    // `add_card_to_library_top` creates an untyped object; the land-play path
    // requires the Land core type.
    if let Some(obj) = runner.state_mut().objects.get_mut(&milled_land) {
        obj.card_types.core_types = vec![CoreType::Land];
    }

    let mut events = Vec::new();
    move_through(
        runner.state_mut(),
        milled_land,
        Zone::Graveyard,
        &mut events,
    );
    move_through(
        runner.state_mut(),
        from_battlefield,
        Zone::Graveyard,
        &mut events,
    );

    runner.state_mut().phase = Phase::PreCombatMain;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;

    let playable = graveyard_lands_playable_by_permission(runner.state(), P0);
    assert!(
        playable.iter().any(|(id, _)| *id == milled_land),
        "a land put into the graveyard from the LIBRARY this turn is in Kagha's \
         Play pool; got {playable:?}"
    );
    assert!(
        !playable.iter().any(|(id, _)| *id == from_battlefield),
        "a land that reached the graveyard from the BATTLEFIELD fails Kagha's \
         printed library-origin pool"
    );
    // CR 305.1: a land is played, never cast.
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&milled_land),
        "a land must not surface on the spell-cast path (CR 305.1)"
    );

    // Drive the real action, not just the authority query.
    let card_id = runner.state().objects[&milled_land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: milled_land,
            card_id,
        })
        .expect("Kagha's Play half must authorize the land-play special action");
    assert_eq!(
        runner.state().objects[&milled_land].zone,
        Zone::Battlefield,
        "playing the land via Kagha's permission must move it to the battlefield"
    );
}

/// CR 400.7: perform one real zone change through the production mover, so the
/// occurrence bookkeeping and the zone-change ledger are written exactly as they
/// are in a game. Deliberately NOT a hand-built `record_zone_change`: a
/// manufactured ledger can agree with the filter while disagreeing with the
/// pipeline that actually produces it.
fn move_through(
    state: &mut engine::types::game_state::GameState,
    object_id: ObjectId,
    to: Zone,
    events: &mut Vec<engine::types::events::GameEvent>,
) {
    engine::game::zones::move_to_zone(state, object_id, to, events);
}
