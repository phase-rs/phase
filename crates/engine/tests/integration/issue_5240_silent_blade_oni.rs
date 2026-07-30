//! Regression for issue #5240: Silent-Blade Oni gives no prompt to choose or
//! cast a spell after dealing combat damage to a player.
//!
//! https://github.com/phase-rs/phase/issues/5240
//!
//! Oracle text (Scryfall-verified):
//!   Ninjutsu {4}{U}{B} ({4}{U}{B}, Return an unblocked attacker you control
//!   to hand: Put this card onto the battlefield from your hand tapped and
//!   attacking.)
//!   Whenever this creature deals combat damage to a player, look at that
//!   player's hand. You may cast a spell from among those cards without
//!   paying its mana cost.
//!
//! Root cause: the trigger parses correctly into `RevealHand { target:
//! TriggeringPlayer, reveal: false }` (the private "look at that player's
//! hand" clause) followed by a `CastFromZone` sub-ability for "cast a spell
//! from among those cards ... without paying its mana cost." But
//! `try_parse_cast_effect`'s "from among them"/"from among those cards" arm
//! unconditionally bound the cast target to `TargetFilter::ExiledBySource` —
//! correct for the exile-then-cast class (Improvisation Capstone, Etali) this
//! anaphor was originally built for, but wrong here: Silent-Blade Oni never
//! exiles anything, so at runtime `ExiledBySource` resolves against an empty
//! tracked-exile set and the cast permission silently offers nothing — no
//! prompt is ever raised, matching the reported bug exactly.
//!
//! The fix threads a same-chain "prior RevealHand producer" signal
//! (`ParseContext::chain_prior_hand_reveal_target`, mirroring the existing
//! `chain_has_prior_exile_producer` precedent for the singular "the exiled
//! card" anaphor) so the plural anaphor binds to the REVEALED PLAYER'S HAND
//! instead when no exile occurred in the chain. The runtime hand-cast
//! resolver (`open_private_zone_cast_selection` in
//! `game/effects/cast_from_zone.rs`) previously also hardcoded the ability's
//! OWN controller as the hand to scan; it now resolves the candidate hand via
//! the cast filter's own `ControllerRef` (here `TriggeringPlayer`, the
//! damaged player) through the single `controller_ref_player` authority.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{ControllerRef, Effect, FilterProp, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::rules::run_combat;

const SILENT_BLADE_ONI_ORACLE: &str = "Ninjutsu {4}{U}{B} ({4}{U}{B}, Return an unblocked \
attacker you control to hand: Put this card onto the battlefield from your hand tapped and \
attacking.)\nWhenever this creature deals combat damage to a player, look at that player's \
hand. You may cast a spell from among those cards without paying its mana cost.";

/// CR 603.2 + CR 701.20a + CR 118.9: the DamageDone trigger's execute chain
/// must be `RevealHand { TriggeringPlayer, reveal: false }` followed by a
/// `CastFromZone` sub-ability whose target is the DAMAGED PLAYER'S hand (a
/// `Typed` filter: `Card` + `controller: TriggeringPlayer` + `InZone(Hand)`),
/// not `TargetFilter::ExiledBySource`. This is the direct root-cause
/// assertion: before the fix, `target` here was `ExiledBySource`, which
/// resolves to an empty set because Silent-Blade Oni never exiles anything.
#[test]
fn silent_blade_oni_parses_hand_scoped_free_cast() {
    let parsed = parse_oracle_text(
        SILENT_BLADE_ONI_ORACLE,
        "Silent-Blade Oni",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Ninja".to_string()],
    );

    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::DamageDone)
        .expect("Silent-Blade Oni must parse a combat-damage-to-player trigger");
    assert_eq!(
        trigger.valid_target,
        Some(TargetFilter::Player),
        "trigger must fire on damage dealt to a player"
    );

    let execute = trigger
        .execute
        .as_ref()
        .expect("DamageDone trigger must have an execute chain");
    assert!(
        matches!(
            execute.effect.as_ref(),
            Effect::RevealHand {
                target: TargetFilter::TriggeringPlayer,
                reveal: false,
                ..
            }
        ),
        "expected a private look at the damaged player's hand, got {:?}",
        execute.effect
    );

    let cast_sub = execute
        .sub_ability
        .as_ref()
        .expect("RevealHand must be followed by the free-cast sub-ability");
    let Effect::CastFromZone {
        target,
        without_paying_mana_cost: true,
        ..
    } = cast_sub.effect.as_ref()
    else {
        panic!(
            "expected a without-paying CastFromZone sub-ability, got {:?}",
            cast_sub.effect
        );
    };
    let TargetFilter::Typed(tf) = target else {
        panic!(
            "expected the cast target to be a Typed hand filter, got {target:?} \
             (ExiledBySource here is the issue #5240 regression: nothing was ever exiled)"
        );
    };
    assert_eq!(tf.type_filters, vec![TypeFilter::Card]);
    assert_eq!(tf.controller, Some(ControllerRef::TriggeringPlayer));
    assert!(
        tf.properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }),
        "cast pool must be scoped to the Hand zone, got {:?}",
        tf.properties
    );
}

/// Full end-to-end discriminator: Silent-Blade Oni connects with combat
/// damage, the controller accepts the "you may cast" prompt, and the
/// resulting `EffectZoneChoice` offers cards from the DEFENDING player's hand
/// (not an empty exile set). Selecting one casts it onto the stack for the
/// Oni's controller without paying its mana cost.
#[test]
fn silent_blade_oni_offers_free_cast_from_damaged_players_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let oni = scenario
        .add_creature(P0, "Silent-Blade Oni", 3, 2)
        .with_subtypes(vec!["Human", "Ninja"])
        .from_oracle_text(SILENT_BLADE_ONI_ORACLE)
        .id();

    // P1 (the player who will take combat damage) has a nonland card in hand
    // that P0 should be offered to cast for free.
    let opp_spell = scenario.add_creature_to_hand(P1, "Opp Creature", 2, 2).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    run_combat(&mut runner, vec![oni], vec![]);

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "unblocked Oni must deal 3 combat damage to P1"
    );

    // Drain priority passes until the "you may cast" prompt surfaces. The
    // private RevealHand look is non-interactive and resolves silently; the
    // optional CastFromZone sub-ability is what raises a prompt.
    let mut reached_optional = false;
    for _ in 0..40 {
        match runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                reached_optional = true;
                break;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority should succeed while draining the stack");
            }
            ref other => panic!("unexpected waiting state while draining: {other:?}"),
        }
    }
    assert!(
        reached_optional,
        "expected the 'you may cast a spell' prompt to appear; waiting_for={:?}",
        runner.state().waiting_for
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: p, .. } if p == P0
        ),
        "the Oni's controller (P0), not the damaged player, decides whether to cast"
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept the may-cast sub-ability");

    let waiting = runner.state().waiting_for.clone();
    let WaitingFor::EffectZoneChoice {
        cards,
        zone,
        player,
        ..
    } = waiting
    else {
        panic!(
            "expected EffectZoneChoice after accepting the free-cast offer — before the fix this \
             prompt never appeared because the cast target resolved against an empty \
             ExiledBySource set; got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        zone,
        Zone::Hand,
        "the free-cast pool must be drawn from a hand, not exile"
    );
    assert_eq!(player, P0, "P0 (the Oni's controller) makes the choice");
    assert!(
        cards.contains(&opp_spell),
        "P1's hand card must be offered as a free-cast candidate; eligible={cards:?}"
    );

    let p0_mana_before = runner.state().players[0].mana_pool.total();

    runner
        .act(GameAction::SelectCards {
            cards: vec![opp_spell],
        })
        .expect("select the opponent's card to cast for free");

    assert_eq!(
        runner.state().objects[&opp_spell].zone,
        Zone::Stack,
        "the selected card must be cast onto the stack"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        p0_mana_before,
        "the cast must be free — no mana spent"
    );
}
