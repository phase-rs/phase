//! Issue #3245 — Abhorrent Oculus manifest dread must manifest the chosen card
//! to the battlefield and grave the other looked-at card.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

const ABHORRENT_OCULUS_ORACLE: &str =
    "Flying\nAt the beginning of each opponent's upkeep, manifest dread.";

const MANIFEST_DREAD_ORACLE: &str = "Manifest dread.";

fn enter_tapped_battlefield_replacement(description: &str) -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Battlefield)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: engine::types::ability::EffectScope::Single,
                state: engine::types::ability::TapStateChange::Tap,
            },
        ))
        .description(description.to_string())
}

fn advance_to_manifest_dread_choice(runner: &mut GameRunner) {
    for _ in 0..240 {
        match &runner.state().waiting_for {
            WaitingFor::ManifestDreadChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            _ => return,
        }
    }
}

#[test]
fn abhorrent_oculus_manifest_dread_manifests_choice_and_graves_other() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D", "Lib E"]);
    }
    let oculus = scenario
        .add_creature_from_oracle(P0, "Abhorrent Oculus", 5, 5, ABHORRENT_OCULUS_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Blue],
        })
        .id();
    scenario.add_card_to_library_top(P0, "Library Top");
    scenario.add_card_to_library_top(P0, "Second Top");

    let mut runner = scenario.build();
    let lib = runner.state().players[0].library.clone();
    let [top, second] = [lib[0], lib[1]];

    advance_to_manifest_dread_choice(&mut runner);

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ManifestDreadChoice { .. }
        ),
        "opponent-upkeep manifest dread must pause for choice, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards { cards: vec![top] })
        .expect("choose card to manifest");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Battlefield,
        "chosen card must manifest onto the battlefield"
    );
    assert!(
        runner.state().objects[&top].face_down,
        "manifested card must be face down on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&second].zone,
        Zone::Graveyard,
        "other looked-at card must go to the graveyard"
    );
    assert!(
        !runner.state().objects[&second].face_down,
        "graveyard card must be face up in the public zone"
    );
    assert_eq!(
        runner.state().objects[&top].entered_via_ability_source,
        Some(oculus),
        "manifest dread entry must be attributed to the resolving ability source"
    );
}

#[test]
fn manifest_dread_defers_graveyard_until_paused_manifest_entry_resolves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let other = scenario.add_card_to_library_top(P0, "Other Card");
    let manifest = scenario.add_card_to_library_top(P0, "Manifest Me");
    scenario
        .add_creature(P1, "Kismet", 0, 0)
        .as_enchantment()
        .with_replacement_definition(enter_tapped_battlefield_replacement(
            "Creatures enter the battlefield tapped.",
        ));
    scenario
        .add_creature(P1, "Frozen Aether", 0, 0)
        .as_enchantment()
        .with_replacement_definition(enter_tapped_battlefield_replacement(
            "Permanents enter the battlefield tapped.",
        ));
    let spell = scenario
        .add_spell_to_hand(P0, "Dread Test", false)
        .from_oracle_text(MANIFEST_DREAD_ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P0, vec![]);

    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    runner
        .act(GameAction::SelectCards {
            cards: vec![manifest],
        })
        .expect("choose card to manifest");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "manifest entry must pause on enter-tapped collision, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&other].zone,
        Zone::Library,
        "the non-manifested card must not be graved while manifest entry is paused"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("answer enter-tapped ordering");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&manifest].zone,
        Zone::Battlefield,
        "chosen card must finish manifesting onto the battlefield"
    );
    assert!(
        runner.state().objects[&manifest].face_down,
        "manifested card must be face down on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&other].zone,
        Zone::Graveyard,
        "other looked-at card must go to the graveyard after manifest completes"
    );
}
