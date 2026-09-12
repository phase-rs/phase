//! CR 607.2a + CR 108.3 + CR 608.2g — Spell Queller.
//!
//! Verbatim Oracle text (Scryfall):
//!   Flash
//!   Flying
//!   When this creature enters, exile target spell with mana value 4 or less.
//!   When this creature leaves the battlefield, the exiled card's owner may cast
//!     that card without paying its mana cost.
//!
//! Ruling (2025-01-24): "If the player casts the exiled card, they do so as part
//! of the resolution of Spell Queller's last ability. The player can't wait to
//! cast it later in the turn."
//!
//! The leaves-the-battlefield ability asks the exiled card's OWNER (CR 108.3),
//! not Spell Queller's controller, and the cast happens while that ability
//! resolves (CR 608.2g). Declining leaves the card in exile with no standing
//! permission to cast it later.
//!
//! Fixture: P1 (active) casts a {2} sorcery; P0 responds with Spell Queller,
//! whose enters trigger exiles it; P1 then destroys Spell Queller with a removal
//! spell, so the real battlefield-exit lifecycle fires the linked ability.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SPELL_QUELLER_ORACLE: &str = "Flash\nFlying\n\
When this creature enters, exile target spell with mana value 4 or less.\n\
When this creature leaves the battlefield, the exiled card's owner may cast that card without paying its mana cost.";

/// The quelled card's effect names its own controller, so the life totals show
/// which player cast it.
const QUELLED_ORACLE: &str = "You gain 3 life.";

const DESTROY_TARGET_CREATURE: &str = "Destroy target creature.";

/// Mana P1 keeps floating after paying for the sorcery, so the decline test's
/// later cast attempt cannot fail for lack of mana.
const P1_SPARE_MANA: usize = 2;

struct QuellerOffer {
    runner: GameRunner,
    quelled: ObjectId,
}

/// Drive the fixture to the leaves-the-battlefield trigger's "may cast" offer,
/// asserting that the offer goes to the exiled card's owner (P1) and not to
/// Spell Queller's controller (P0).
fn offer_quelled_card_to_its_owner() -> QuellerOffer {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let quelled = scenario
        .add_spell_to_hand_from_oracle(P1, "Quelled Sorcery", false, QUELLED_ORACLE)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, DESTROY_TARGET_CREATURE)
        .id();
    let queller = scenario
        .add_creature_to_hand_from_oracle(P0, "Spell Queller", 2, 3, SPELL_QUELLER_ORACLE)
        .id();
    scenario.with_mana_pool(
        P1,
        (0..2 + P1_SPARE_MANA)
            .map(|_| ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    // P1 casts the sorcery and passes priority with it on the stack.
    runner.cast(quelled).commit();
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority with its sorcery on the stack");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "P0 must get priority to respond, got {:?}",
        runner.state().waiting_for
    );

    // CR 702.8a: flash lets P0 respond with Spell Queller; its enters trigger
    // exiles the sorcery before the sorcery can resolve.
    runner.cast(queller).target_object(quelled).resolve();
    assert_eq!(
        runner.state().objects[&quelled].zone,
        Zone::Exile,
        "Spell Queller's enters trigger must exile the sorcery"
    );

    // P1 destroys Spell Queller, which fires its leaves-the-battlefield trigger.
    runner.cast(removal).target_object(queller).commit();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(
                    runner.state().objects[&queller].zone,
                    Zone::Graveyard,
                    "the offer must come from the leaves-the-battlefield trigger"
                );
                // CR 108.3: the exiled card's owner is asked, not the
                // controller of Spell Queller's ability.
                assert_eq!(
                    player, P1,
                    "the exiled card's owner (P1) must be offered the cast"
                );
                return QuellerOffer { runner, quelled };
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority to resolve the stack must be accepted");
            }
            other => panic!(
                "Spell Queller's leaves-the-battlefield trigger must offer the owner a cast; \
                 reached {other:?} with stack {:?}",
                runner.state().stack
            ),
        }
    }
    panic!("the leaves-the-battlefield offer never surfaced");
}

/// CR 608.2g: declining the offered cast ends it. The card stays in exile, has
/// no casting permission, and a later cast attempt by its owner is rejected
/// even though P1 holds priority with enough mana to pay its cost.
///
/// DISCRIMINATING: a standing permission (the shape of the original
/// `ExileWithAltCost { duration: None }` grant) leaves `casting_permissions`
/// non-empty, lists the card in `legal_actions`, and lets the later `CastSpell`
/// succeed; that grant was also non-optional, so no offer surfaced at all.
/// Reverting the clause_shell "may" arm leaves the trigger `Unimplemented`, so
/// no offer is made and the drive in `offer_quelled_card_to_its_owner` panics.
#[test]
fn declining_the_owner_cast_leaves_no_standing_permission() {
    let QuellerOffer {
        mut runner,
        quelled,
    } = offer_quelled_card_to_its_owner();

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the offered cast must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&quelled].zone,
        Zone::Exile,
        "a declined card remains exiled"
    );
    assert!(
        runner.state().objects[&quelled]
            .casting_permissions
            .is_empty(),
        "declining must leave no standing permission, got {:?}",
        runner.state().objects[&quelled].casting_permissions
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "the owner must hold priority for the later cast attempt, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].mana_pool.total(),
        P1_SPARE_MANA,
        "the owner must still have mana to pay the card's cost"
    );
    assert!(
        !legal_actions(runner.state()).iter().any(|action| matches!(
            action,
            GameAction::CastSpell { object_id, .. } if *object_id == quelled
        )),
        "the declined card must not be castable later in the turn"
    );

    let card_id = runner.state().objects[&quelled].card_id;
    let later_cast = runner.act(GameAction::CastSpell {
        object_id: quelled,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        later_cast.is_err(),
        "a later cast of the declined card must be rejected"
    );
    assert_eq!(runner.state().objects[&quelled].zone, Zone::Exile);
}

/// CR 608.2g + CR 118.9: accepting casts the card during the trigger's
/// resolution, under its owner's control, without paying its mana cost.
///
/// DISCRIMINATING: reverting the `rewrite_player_scope_refs` CastFromZone
/// rebind leaves the cast targeting `ParentTarget`, which binds no card on a
/// trigger that declares no targets; the resolver takes its "nothing to cast"
/// exit, so the stack assertion fails and the card stays in exile. Resolving it
/// under P0 would move P0's life total instead of P1's.
#[test]
fn the_owner_casts_the_exiled_card_free_during_resolution() {
    let QuellerOffer {
        mut runner,
        quelled,
    } = offer_quelled_card_to_its_owner();
    let p0_life_before = runner.state().players[P0.0 as usize].life;
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the offered cast must be accepted");

    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.id == quelled && entry.controller == P1),
        "the owner must have cast the card onto the stack during resolution, got {:?}",
        runner.state().stack
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].mana_pool.total(),
        P1_SPARE_MANA,
        "the card is cast without paying its mana cost"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&quelled].zone,
        Zone::Graveyard,
        "the cast sorcery resolves to its owner's graveyard"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before + 3,
        "the owner controlled the spell"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, p0_life_before);
}
