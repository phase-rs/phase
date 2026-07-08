//! Discriminating runtime regression for **Expensive Taste** (std impulse-exile
//! batch):
//!
//! > Exile the top two cards of target opponent's library face down. You may
//! > look at and play those cards for as long as they remain exiled.
//!
//! "You may look at and play those cards for as long as they remain exiled" is
//! an impulse-style `PlayFromExile` permission over the cards exiled by the
//! preceding `ExileTop` (the tracked set). It must grant the spell's controller
//! permission to play each exiled card. Before the fix the "look at and play
//! those cards" verb form (and the no-mana-conjunct "for as long as they remain
//! exiled" duration) was unrecognized, so the clause fell to
//! `Effect::Unimplemented` and no permission was granted.
//!
//! DISCRIMINATOR: after resolution, BOTH exiled opponent cards carry P0's
//! `PlayFromExile` permission. With the parse reverted no permission is attached
//! and the `.any(PlayFromExile { granted_to: P0 })` assertion flips to false.
//!
//! CR 406.6: the "look at" conjunct is a private-information grant the play
//! permission implies.
//! CR 400.7i: the controller may play the exiled cards.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{CastingPermission, Duration};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EXPENSIVE_TASTE: &str = "Exile the top two cards of target opponent's library face down. \
You may look at and play those cards for as long as they remain exiled.";

#[test]
fn expensive_taste_grants_play_permission_on_exiled_opponent_cards() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // A deeper P1 library card that must stay put (below the top-two window).
    // Added FIRST so the later top-pushes bury it beneath the exiled window.
    let deep = scenario.add_card_to_library_top(P1, "Opp Deep");
    // Top two cards of the OPPONENT's (P1) library — these are exiled. The last
    // `add_card_to_library_top` lands on top, so push the two that should be
    // exiled after `deep`.
    let exiled_cards: Vec<_> = ["Opp Top B", "Opp Top A"]
        .into_iter()
        .map(|name| scenario.add_card_to_library_top(P1, name))
        .collect();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Expensive Taste", false, EXPENSIVE_TASTE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let outcome = scenario.build().cast(spell).target_player(P1).resolve();

    for id in &exiled_cards {
        assert_eq!(
            outcome.zone_of(*id),
            Zone::Exile,
            "Expensive Taste must exile the top two opponent library cards"
        );
        let object = &outcome.state().objects[id];
        assert!(
            object.casting_permissions.iter().any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile {
                    granted_to: P0,
                    duration: Duration::Permanent,
                    ..
                }
            )),
            "exiled opponent card must carry P0's PlayFromExile permission, got {:?}",
            object.casting_permissions
        );
    }

    // NEGATIVE: a deeper opponent card stays in the library with no grant.
    assert_eq!(
        outcome.zone_of(deep),
        Zone::Library,
        "cards below the exiled window must remain in the opponent's library"
    );
    assert!(
        outcome.state().objects[&deep]
            .casting_permissions
            .is_empty(),
        "non-exiled cards must not receive any play permission"
    );
}
