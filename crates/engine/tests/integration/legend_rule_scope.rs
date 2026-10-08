//! Legend-rule scope — the pre-M14 rule a custom format can opt back into.
//!
//! CR 704.5j is per-controller and gives the controller a choice. The form
//! modeled here — the "nullification rule" in force from Champions of Kamigawa
//! (2004) until M14 (2013) — grouped same-named legends across ALL controllers
//! and put every one of them into its owner's graveyard with no choice at all.
//!
//! Two things separate it from the modern rule, and both are asserted here
//! against a real state-based-action pass rather than against the helpers:
//!
//! 1. **Scope.** Two players each keeping their own copy is a legal board state
//!    today and was not before M14.
//! 2. **Resolution.** Modern pauses on `WaitingFor::ChooseLegend`; pre-M14 has
//!    nothing to choose.
//!
//! Every assertion is paired against the same board under a modern format, so a
//! failure to apply the legacy rule and a failure to set the board up are
//! distinguishable.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::custom_format::{test_rules_with_legacy, LegacyRuleSet, LegendRuleScope};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

const MIRROR: &str = "Mirrored Hero";

fn legacy_format() -> FormatConfig {
    FormatConfig::for_custom_rules(&test_rules_with_legacy(LegacyRuleSet {
        legend_rule_scope: LegendRuleScope::PreM14AnyController,
        ..LegacyRuleSet::default()
    }))
}

/// The paired control format: a custom format that declines the axis. Using a
/// custom format on both sides keeps the comparison to the axis itself rather
/// than to "custom vs built-in".
fn modern_custom_format() -> FormatConfig {
    FormatConfig::for_custom_rules(&test_rules_with_legacy(LegacyRuleSet::default()))
}

/// A board where `owners` each control one legendary permanent named `MIRROR`.
fn mirror_board(owners: &[PlayerId], format: FormatConfig) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ids: Vec<ObjectId> = owners
        .iter()
        .map(|player| {
            scenario
                .add_creature(*player, MIRROR, 2, 2)
                .as_legendary()
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    runner.state_mut().format_config = format;
    (runner, ids)
}

fn in_graveyard(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].zone == Zone::Graveyard
}

fn sba(runner: &mut GameRunner) {
    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
}

/// The functional difference the axis exists for: a cross-controller mirror.
#[test]
fn a_cross_controller_mirror_dies_under_the_legacy_scope() {
    let (mut runner, ids) = mirror_board(&[P0, P1], legacy_format());
    sba(&mut runner);

    assert!(
        ids.iter().all(|id| in_graveyard(&runner, *id)),
        "pre-M14: same-named legends group across controllers and ALL of them go \
         to their owners' graveyards"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the pre-M14 rule is choiceless — nothing may be asked of either player, \
         got {:?}",
        runner.state().waiting_for
    );
}

/// The control, on the identical board: today each player keeps their own copy,
/// and the SBA does not fire at all.
#[test]
fn a_cross_controller_mirror_survives_under_the_modern_scope() {
    let (mut runner, ids) = mirror_board(&[P0, P1], modern_custom_format());
    sba(&mut runner);

    assert!(
        ids.iter().all(|id| !in_graveyard(&runner, *id)),
        "CR 704.5j is per-controller: two players may each keep a copy"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Same-controller is where the two rules overlap in *scope* but still differ
/// in resolution — and the difference is visible: modern keeps one, pre-M14
/// keeps none.
#[test]
fn one_controller_with_two_copies_keeps_none_under_the_legacy_scope() {
    let (mut runner, ids) = mirror_board(&[P0, P0], legacy_format());
    sba(&mut runner);

    assert!(
        ids.iter().all(|id| in_graveyard(&runner, *id)),
        "pre-M14: every member of the group dies, not all but one"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // The modern control on the same board: a choice is raised and nothing has
    // moved yet, which is the behavior the legacy branch must NOT reach.
    let (mut modern, modern_ids) = mirror_board(&[P0, P0], modern_custom_format());
    sba(&mut modern);
    assert!(
        matches!(modern.state().waiting_for, WaitingFor::ChooseLegend { .. }),
        "CR 704.5j: the controller chooses, got {:?}",
        modern.state().waiting_for
    );
    assert!(modern_ids.iter().all(|id| !in_graveyard(&modern, *id)));
}

/// A lone legend is untouched by either scope. Without this, a legacy branch
/// that put every legendary permanent into the graveyard would pass the tests
/// above.
#[test]
fn a_lone_legend_is_untouched_by_either_scope() {
    for (label, format) in [
        ("legacy", legacy_format()),
        ("modern", modern_custom_format()),
    ] {
        let (mut runner, ids) = mirror_board(&[P0], format);
        sba(&mut runner);
        assert!(
            !in_graveyard(&runner, ids[0]),
            "{label}: the rule applies only to a group of two or more"
        );
    }
}

/// Two DIFFERENT legendary names never group, under either scope — the rule is
/// keyed on name, and a branch that grouped by supertype alone would pass every
/// assertion above.
#[test]
fn different_names_never_group_under_either_scope() {
    for (label, format) in [
        ("legacy", legacy_format()),
        ("modern", modern_custom_format()),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario.add_creature(P0, MIRROR, 2, 2).as_legendary().id();
        let second = scenario
            .add_creature(P1, "Distinct Hero", 2, 2)
            .as_legendary()
            .id();
        let mut runner = scenario.build();
        runner.state_mut().format_config = format;
        sba(&mut runner);

        assert!(
            !in_graveyard(&runner, first) && !in_graveyard(&runner, second),
            "{label}: differently-named legends are not the same group"
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
}

/// A non-legendary permanent sharing the name is not in the group either. The
/// legacy path must filter on the Legendary supertype exactly as the modern
/// path does.
#[test]
fn a_nonlegendary_with_the_same_name_is_not_in_the_group() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let legend = scenario.add_creature(P0, MIRROR, 2, 2).as_legendary().id();
    let mundane = scenario.add_creature(P1, MIRROR, 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().format_config = legacy_format();
    sba(&mut runner);

    assert!(
        !in_graveyard(&runner, legend) && !in_graveyard(&runner, mundane),
        "one legendary permanent and one ordinary one of the same name is not a \
         group of two legends"
    );
}

/// CR 616.1 + CR 704.3: **the pass must stop when a pre-M14 legend move parks a
/// replacement-ordering choice.**
///
/// `move_to_graveyard_via_pipeline` requires its caller to bail when it reports
/// a pause; the modern legend rule never moves anything, so before this scope
/// existed there was no guard between `check_legend_rule` and the Aura check
/// that follows it. An unattached Aura is the witness: if the pass ran on, it
/// would be swept into the graveyard (CR 704.5m) while the legend's own move is
/// still unsettled.
#[test]
fn a_paused_legend_move_stops_the_rest_of_the_sba_pass() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // CR 616.1: two replacements that both apply to the SAME battlefield ->
    // graveyard move, so the controller must order them and the pipeline parks
    // a choice. `Moved` + `destination_zone(Graveyard)` is the Rest-in-Peace
    // shape the engine already uses for "if it would be put into a graveyard,
    // do this instead"; the two differ in where they send it, so neither
    // subsumes the other.
    let redirect = |label: &str, destination: Zone| {
        ReplacementDefinition::new(ReplacementEvent::Moved)
            .destination_zone(Zone::Graveyard)
            // Creatures only. Left unscoped these would also intercept the
            // Aura's own graveyard move, and the Aura is the witness — it would
            // then survive whether or not the pass stopped, and this test would
            // prove nothing. (It did exactly that on the first attempt.)
            .valid_card(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
            .description(label.to_string())
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChangeZone {
                    destination,
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
                    enters_modified_if: None,
                },
            ))
    };
    scenario
        .add_creature(P0, "Graveyard Redirects", 1, 1)
        .with_replacement_definition(redirect("Exile instead", Zone::Exile))
        .with_replacement_definition(redirect("To hand instead", Zone::Hand));

    let first = scenario.add_creature(P0, MIRROR, 2, 2).as_legendary().id();
    let second = scenario.add_creature(P1, MIRROR, 2, 2).as_legendary().id();

    // An Aura attached to nothing: CR 704.5m would put it into its owner's
    // graveyard on this same pass, if the pass were allowed to continue.
    let orphan_aura = scenario
        .add_enchantment_from_oracle(P0, "Orphaned Aura", "")
        .with_subtypes(vec!["Aura"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().format_config = legacy_format();
    sba(&mut runner);

    assert!(
        runner.state().pending_replacement.is_some()
            || matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ),
        "the legend move must park a CR 616.1 ordering choice, or this test is \
         not exercising the pause at all; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !in_graveyard(&runner, orphan_aura),
        "CR 704.3: no later SBA may run while the legend move is unsettled — \
         the unattached Aura must still be on the battlefield"
    );
    // CONTROL: the witness must be live. Without the replacements — so nothing
    // pauses — the very same board sweeps the Aura on this same pass, which is
    // what makes its survival above evidence of anything.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature(P0, MIRROR, 2, 2).as_legendary();
        scenario.add_creature(P1, MIRROR, 2, 2).as_legendary();
        let aura = scenario
            .add_enchantment_from_oracle(P0, "Orphaned Aura", "")
            .with_subtypes(vec!["Aura"])
            .id();
        let mut unpaused = scenario.build();
        unpaused.state_mut().format_config = legacy_format();
        sba(&mut unpaused);
        assert!(
            in_graveyard(&unpaused, aura),
            "CR 704.5m: an unattached Aura must be swept when the pass is NOT \
             paused, or the assertion above proves nothing"
        );
    }

    // And the paused move really has not completed.
    assert!(
        !in_graveyard(&runner, first) || !in_graveyard(&runner, second),
        "the paused move has not completed"
    );
}
