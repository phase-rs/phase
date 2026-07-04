//! Issue #3279 — Song of the Dryads must strip the enchanted permanent's
//! rules-text abilities when it becomes a basic land (CR 305.7).
//!
//! https://github.com/phase-rs/phase/issues/3279

use engine::game::effects::attach::attach_to;
use engine::game::mana_sources::activatable_land_mana_options;
use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

const OBUUN_ORACLE: &str = "At the beginning of combat on your turn, up to one target land you control becomes an X/X Elemental creature with trample and haste until end of turn, where X is Obuun's power. It's still a land.\nLandfall — Whenever a land you control enters, put a +1/+1 counter on target creature.";

const SONG_ORACLE: &str = "Enchant permanent\nEnchanted permanent is a colorless Forest land.";

#[test]
fn issue_3279_song_of_dryads_strips_enchanted_permanent_abilities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let obuun = scenario
        .add_creature_from_oracle(P0, "Obuun, Mul Daya Ancestor", 3, 3, OBUUN_ORACLE)
        .id();

    let song = scenario
        .add_creature(P0, "Song of the Dryads", 0, 0)
        .as_enchantment()
        .from_oracle_text(SONG_ORACLE)
        .id();

    let mut runner = scenario.build();

    {
        let obuun_obj = runner.state().objects.get(&obuun).expect("Obuun present");
        assert!(
            obuun_obj
                .base_trigger_definitions
                .iter()
                .any(|t| t.mode == TriggerMode::Phase && t.phase == Some(Phase::BeginCombat)),
            "Obuun must carry a begin-combat trigger before Song attaches"
        );
    }

    {
        let song_obj = runner
            .state_mut()
            .objects
            .get_mut(&song)
            .expect("Song present");
        if !song_obj.card_types.subtypes.iter().any(|s| s == "Aura") {
            song_obj.card_types.subtypes.push("Aura".to_string());
            song_obj.base_card_types = song_obj.card_types.clone();
        }
    }

    attach_to(runner.state_mut(), song, obuun);

    let obuun_obj = runner.state().objects.get(&obuun).expect("Obuun present");
    assert_eq!(
        obuun_obj.card_types.core_types,
        vec![CoreType::Land],
        "Song must replace the enchanted permanent's card types with Land"
    );
    assert!(
        obuun_obj.card_types.subtypes.iter().any(|s| s == "Forest"),
        "Song must grant the Forest basic land subtype, got {:?}",
        obuun_obj.card_types.subtypes
    );
    assert!(
        obuun_obj.trigger_definitions.is_empty(),
        "CR 305.7: rules-text triggers must be removed"
    );
    assert!(
        obuun_obj.static_definitions.is_empty(),
        "CR 305.7: rules-text statics must be removed"
    );
    assert!(
        obuun_obj.abilities.iter().all(|a| {
            matches!(
                a.effect.as_ref(),
                engine::types::ability::Effect::Mana { .. }
            )
        }),
        "only the intrinsic Forest mana ability may remain, got {:?}",
        obuun_obj.abilities.len()
    );

    let mana_options = activatable_land_mana_options(runner.state(), obuun, P0);
    assert!(
        mana_options.iter().any(|o| o.mana_type == ManaType::Green),
        "Forest land must tap for {{G}}, got {mana_options:?}"
    );

    runner.pass_both_players();
    assert_eq!(runner.state().phase, Phase::PostCombatMain);
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::OrderTriggers { .. }
        ),
        "Obuun's begin-combat trigger must not fire while enchanted by Song of the Dryads"
    );
}
