//! M'Odo, the Gnarled Oracle — its printed Eminence ability through the
//! production activation and resolution path.
//!
//! "Eminence — {X}, Discard a card: Target player reveals cards from the top of
//! their library until they reveal a creature card with converted mana cost X or
//! less. Put that card onto the battlefield under your control, then that player
//! shuffles the rest into their library. Activate this ability only if M'Odo, the
//! Gnarled Oracle is on the battlefield or in the command zone."
//!
//! CR 113.6b: "An ability that states which zones it functions in functions only
//! from those zones." The restriction names the battlefield and the command zone,
//! so the ability is activatable from exactly those two zones.

use engine::ai_support::legal_actions;
use engine::game::casting::can_activate_ability_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::actions::GameAction;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Printed Oracle text of M'Odo, the Gnarled Oracle (MTGJSON AtomicCards / Scryfall).
const M_ODO_ORACLE: &str = "Eminence — {X}, Discard a card: Target player reveals cards from the top of their library until they reveal a creature card with converted mana cost X or less. Put that card onto the battlefield under your control, then that player shuffles the rest into their library. Activate this ability only if M'Odo, the Gnarled Oracle is on the battlefield or in the command zone.";

fn colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn shuffles_for(events: &[GameEvent], player: PlayerId) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == player
            )
        })
        .count()
}

fn m_odo_is_offered(runner: &GameRunner, m_odo: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(|action| {
        matches!(
            action,
            GameAction::ActivateAbility { source_id, .. } if *source_id == m_odo
        )
    })
}

/// CR 113.6b + CR 701.20a + CR 202.3 + CR 701.24c: Activated from the command
/// zone with X = 2 targeting the opponent. The discard is paid, the opponent's
/// reveal skips the land AND the mana-value-5 creature (above X), the
/// mana-value-2 creature enters under the activator's control, the other
/// revealed cards stay in the opponent's library, and only the opponent's
/// library is shuffled.
#[test]
fn m_odo_from_the_command_zone_takes_the_targets_first_creature_with_mana_value_at_most_x() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let m_odo = scenario
        .add_creature_from_oracle(P0, "M'Odo, the Gnarled Oracle", 0, 3, M_ODO_ORACLE)
        .id();
    scenario.with_commander(m_odo);
    let fodder = scenario.add_card_to_hand(P0, "Discard Fodder");
    scenario.with_mana_pool(P0, colorless(2));

    // P1's library, top first: land, mana value 5 creature, mana value 2
    // creature, deep card. `add_*_library_top` inserts at the top, so add in
    // reverse.
    let deep = scenario.add_card_to_library_top(P1, "P1 Deep Card");
    let bear = scenario
        .add_spell_to_library_top(P1, "P1 Bear", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let colossus = scenario
        .add_spell_to_library_top(P1, "P1 Colossus", false)
        .as_creature()
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let plains = scenario
        .add_spell_to_library_top(P1, "P1 Plains", false)
        .as_land()
        .id();
    let p0_library_card = scenario.add_card_to_library_top(P0, "P0 Library Card");

    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&m_odo].zone, Zone::Command);

    let outcome = runner
        .activate(m_odo, 0)
        .x(2)
        .target_player(P1)
        .pay_with(&[fodder])
        .resolve();
    let state = outcome.state();
    let events = outcome.events();

    // Cost: the discarded card is in its owner's graveyard; {X} was paid.
    assert_eq!(state.objects[&fodder].zone, Zone::Graveyard);
    assert!(state.players[P0.0 as usize].hand.is_empty());

    // Effect: the first creature card with mana value <= 2 enters under P0's
    // control; its owner is still P1.
    let bear_obj = &state.objects[&bear];
    assert_eq!(bear_obj.zone, Zone::Battlefield);
    assert_eq!(bear_obj.controller, P0);
    assert_eq!(bear_obj.owner, P1);

    // The land and the mana-value-5 creature were revealed but not taken; they
    // and the unrevealed deep card stay in P1's library.
    for id in [plains, colossus, deep] {
        assert_eq!(state.objects[&id].zone, Zone::Library, "{id:?}");
        assert!(state.players[P1.0 as usize].library.contains(&id));
    }
    assert_eq!(state.players[P1.0 as usize].library.len(), 3);

    // "then that player shuffles the rest into their library" — P1's library,
    // never the activator's.
    assert_eq!(shuffles_for(events, P1), 1, "{events:?}");
    assert_eq!(shuffles_for(events, P0), 0, "{events:?}");
    assert_eq!(state.players[P0.0 as usize].library.len(), 1);
    assert_eq!(state.objects[&p0_library_card].zone, Zone::Library);

    // Eminence stays in the command zone.
    assert_eq!(state.objects[&m_odo].zone, Zone::Command);
}

/// CR 113.6b: The printed restriction names the battlefield and the command zone,
/// so the ability is activatable (and offered) from exactly those zones and from
/// no other.
#[test]
fn m_odo_is_activatable_only_from_the_battlefield_or_the_command_zone() {
    let mut scenario = GameScenario::new_with_format(FormatConfig::commander(), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let m_odo = scenario
        .add_creature_from_oracle(P0, "M'Odo, the Gnarled Oracle", 0, 3, M_ODO_ORACLE)
        .id();
    // Pays "Discard a card"; X may be 0, so no mana is needed.
    scenario.add_card_to_hand(P0, "Discard Fodder");
    scenario.add_card_to_library_top(P1, "P1 Library Card");

    let mut runner = scenario.build();

    for zone in [
        Zone::Battlefield,
        Zone::Command,
        Zone::Hand,
        Zone::Graveyard,
        Zone::Exile,
    ] {
        if runner.state().objects[&m_odo].zone != zone {
            let mut events = Vec::new();
            move_to_zone(runner.state_mut(), m_odo, zone, &mut events);
        }
        assert_eq!(runner.state().objects[&m_odo].zone, zone);

        let permitted = matches!(zone, Zone::Battlefield | Zone::Command);
        assert_eq!(
            can_activate_ability_now(runner.state(), P0, m_odo, 0),
            permitted,
            "activation legality from {zone:?}"
        );
        assert_eq!(
            m_odo_is_offered(&runner, m_odo),
            permitted,
            "ActivateAbility offer from {zone:?}"
        );
    }
}
