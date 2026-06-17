//! Runtime regression for PR #3461 (Cybership, WHO misparse cluster #19).
//!
//! Cybership: "Whenever this Vehicle deals combat damage to a player, put the
//! top two cards of that player's library onto the battlefield face down under
//! your control. They're 2/2 Cyberman artifact creatures."
//!
//! This drives the real pipeline end to end: `from_oracle_text` parses the
//! combat-damage trigger into `Effect::Manifest { target: TriggeringPlayer,
//! count: 2, profile: 2/2 Cyberman artifact, enters_under: You }` → Cybership
//! attacks and deals combat damage to P1 → the trigger fires and resolves →
//! the top two cards of P1's library enter the battlefield face down.
//!
//! The discriminator is the CR 110.2a controller redirect: the manifested
//! cards are P1's (the damaged player's library is the source), but they enter
//! under P0's — the Cybership controller's — control ("under your control"),
//! while ownership stays with P1. A resolver that collapsed `enters_under: You`
//! to the library owner would leave them under P1's control and fail
//! `controller == P0`. The profile assertions (2/2, Creature + Artifact,
//! Cyberman) guard the face-down characteristics.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 110.2a: an effect that puts an object onto the battlefield does so
//!     under the controlling player's control unless the effect states
//!     otherwise ("under your control" is the explicit override).
//!   - CR 701.40a / 701.40e: manifest face-down 2/2 mechanics, one at a time.
//!   - CR 708.2a: a face-down permanent uses the effect-specified profile.
//!   - CR 510.1c / CR 120.1: combat damage is dealt to the defending player.

use super::rules::{run_combat, GameRunner, GameScenario, Phase, PlayerId, P0, P1};
use engine::types::card_type::CoreType;

const CYBERSHIP: &str = "Whenever this Vehicle deals combat damage to a player, \
     put the top two cards of that player's library onto the battlefield face \
     down under your control. They're 2/2 Cyberman artifact creatures.";

/// Count of cards in `player`'s library.
fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .library
        .len()
}

/// CR 110.2a + CR 701.40a: when Cybership deals combat damage to P1, the top
/// two cards of P1's library are manifested as 2/2 Cyberman artifact creatures
/// under P0's control — the damaged player owns them, the Cybership controller
/// controls them.
#[test]
fn cybership_combat_damage_manifests_top_two_under_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Cybership is a Vehicle; crew it (treat it as a creature for the test) so
    // it can attack. The trigger keys on "this Vehicle" (the `~` self-ref), not
    // on crewed-ness, so it fires on combat damage either way.
    let cybership = scenario
        .add_creature(P0, "Cybership", 4, 5)
        .as_creature()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(CYBERSHIP)
        .id();

    // Seed the DEFENDING player's library. Only the top two should be
    // manifested; the deeper cards stay in the library.
    scenario.with_library_top(P1, &["Top0", "Top1", "Deep2", "Deep3"]);

    let mut runner = scenario.build();
    let p1_lib_before = library_len(&runner, P1);

    // Cybership attacks P1 and deals combat damage, then resolve the trigger.
    run_combat(&mut runner, vec![cybership], vec![]);
    runner.advance_until_stack_empty();

    // The top two of P1's library left it.
    assert_eq!(
        library_len(&runner, P1),
        p1_lib_before - 2,
        "CR 701.40a: exactly the top two cards of the damaged player's library \
         are manifested"
    );

    // Find the face-down permanents owned by P1 that entered the battlefield.
    let manifested: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == engine::types::zones::Zone::Battlefield && o.face_down && o.owner == P1
        })
        .collect();

    assert_eq!(
        manifested.len(),
        2,
        "two face-down permanents owned by P1 should be on the battlefield"
    );

    for obj in manifested {
        // CR 110.2a: the discriminator — entered under the Cybership
        // controller's control, NOT the library owner's.
        assert_eq!(
            obj.controller, P0,
            "CR 110.2a: manifested card must enter under P0 (Cybership \
             controller), not its owner P1"
        );
        assert_eq!(obj.owner, P1, "ownership stays with the library owner");

        // CR 708.2a: 2/2 Cyberman artifact creature face-down profile.
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
        assert!(obj.card_types.subtypes.contains(&"Cyberman".to_string()));
    }
}
