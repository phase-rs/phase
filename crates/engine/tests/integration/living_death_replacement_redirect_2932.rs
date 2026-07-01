//! Living Death (#2932) — REPLACEMENT REDIRECT. The maintainer's KEY [HIGH]
//! case: a destination-changing replacement (CR 614.1 / CR 614.6) must NOT
//! defeat a "this way" consumer, because the relationship is about the ACTION
//! performed, not the final zone.
//!
//! Setup: Rest in Peace ("If a card would be put into a graveyard from anywhere,
//! exile it instead." — modeled exactly as the parser builds it, a `Moved`
//! replacement scoped to `destination_zone(Graveyard)` whose execute is a self
//! ChangeZone to Exile) is on the battlefield. A spell then sacrifices a
//! creature and gains life equal to "the number of creatures sacrificed this
//! way."
//!
//! CR 701.21a: a sacrifice would put the permanent into the graveyard, but the
//! Rest in Peace replacement (CR 614.6) sends it to EXILE instead. The permanent
//! was still SACRIFICED this way. Under the old zone-only model the consumer is
//! bound to `landed_in: Some(Graveyard)` and the member's recorded landing zone
//! is Exile, so the count is 0 (the bug). Binding to the `Sacrificed` cause —
//! stamped from the `Effect::Sacrifice` producer, independent of the redirected
//! zone — counts the sacrificed creature: life gain MUST equal 1.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const ORACLE: &str = "Sacrifice all creatures you control, then you gain life equal to the number of creatures sacrificed this way.";

/// CR 614.6: "If a card would be put into a graveyard from anywhere, exile it
/// instead." (Rest in Peace / Leyline of the Void class.) Built exactly as the
/// parser does: a `Moved` replacement scoped to `destination_zone(Graveyard)`
/// whose execute is a self `ChangeZone` to Exile. `valid_card` left unset =
/// global.
fn graveyard_exile_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
            },
        ))
        .description(
            "If a card would be put into a graveyard from anywhere, exile it instead.".to_string(),
        )
}

/// CR 608.2c + CR 614.6: a sacrifice redirected to Exile by Rest in Peace is
/// still "sacrificed this way" — the cause-bound consumer counts it.
#[test]
fn living_death_sacrificed_this_way_counts_replacement_redirected_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Rest in Peace supplies the global graveyard->exile Moved replacement.
    scenario
        .add_creature(P0, "Rest in Peace", 0, 0)
        .as_enchantment()
        .with_replacement_definition(graveyard_exile_replacement());

    // One battlefield creature → SACRIFICED this way. Its sacrifice would put it
    // in the graveyard, but the replacement redirects it to EXILE.
    let victim = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // Non-creature source so the spell source never matches the creature filter.
    let source = scenario.add_basic_land(P0, ManaColor::Black);

    let mut runner = scenario.build();
    let starting_life = runner.state().players[0].life;

    let def = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("sacrifice lifegain variant must resolve");

    let state = runner.state();
    // CR 614.6: the sacrificed creature was redirected to Exile, NOT the
    // graveyard. This is the precondition that breaks the zone-only model.
    assert_eq!(
        state
            .objects
            .get(&victim)
            .expect("victim still exists")
            .zone,
        Zone::Exile,
        "Rest in Peace must redirect the sacrificed creature to Exile"
    );
    assert!(
        state.players[0].graveyard.is_empty(),
        "no sacrificed creature reached the graveyard — it was exiled by the redirect"
    );

    // CR 608.2c: despite landing in Exile, the creature was SACRIFICED this way,
    // so the cause-bound life-gain still counts it. The zone-only model counts 0.
    assert_eq!(
        state.players[0].life,
        starting_life + 1,
        "life gain must count the sacrifice (cause = Sacrificed) even though the \
         replacement redirected the member to Exile instead of the graveyard"
    );
}
