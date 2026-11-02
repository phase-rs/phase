//! Fold into Aether — "If that spell is countered this way, its controller may
//! put a creature card from their hand onto the battlefield" offered the
//! countered spell's controller the choice, but accepting it moved nothing:
//! the sub-ability inherited the Counter's object target (the countered
//! spell), so `change_zone::resolve` tried to move that spell out of a hand it
//! was never in and skipped the resolution-time hand scan.
//!
//! Oracle (verbatim, Scryfall):
//!   Counter target spell. If that spell is countered this way, its
//!   controller may put a creature card from their hand onto the battlefield.
//!
//! CR 608.2c: the second sentence is part of the same resolution and names
//! the countered spell's controller. CR 608.2h: that controller is read from
//! the spell's last known information after it left the stack. CR 608.2d: the
//! creature card is a choice that player makes while the effect is applied.
//!
//! Every test drives the real `apply()` pipeline (GameScenario +
//! GameRunner::cast + commit) one prompt at a time, so a missing "may"
//! decision or a leftover creature-card prompt fails the test instead of
//! being silently skipped.

use engine::game::scenario::{CastCommit, GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const FOLD_INTO_AETHER: &str = "Counter target spell. If that spell is countered this way, \
                                its controller may put a creature card from their hand onto \
                                the battlefield.";
const RHYTHM_OF_THE_WILD: &str = "Creature spells you control can't be countered.\nNontoken \
                                  creatures you control have riot. (They enter with your \
                                  choice of a +1/+1 counter or haste.)";

struct Board {
    runner: GameRunner,
    fold: ObjectId,
    /// P1's two creature cards in hand — two candidates force a real
    /// `EffectZoneChoice` instead of the single-candidate auto-pick.
    bears: [ObjectId; 2],
    /// A creature card in the caster's hand: never a candidate, because the
    /// choice belongs to the countered spell's controller.
    caster_creature: ObjectId,
}

fn setup(setup: impl FnOnce(&mut GameScenario)) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut cs =
        scenario.add_spell_to_hand_from_oracle(P0, "Fold into Aether", true, FOLD_INTO_AETHER);
    cs.with_mana_cost(ManaCost::Cost {
        generic: 0,
        shards: vec![ManaCostShard::Blue],
    });
    let fold = cs.id();
    scenario.add_basic_land(P0, ManaColor::Blue);
    let bears = [
        scenario
            .add_creature_to_hand(P1, "Grizzly Bears", 2, 2)
            .id(),
        scenario
            .add_creature_to_hand(P1, "Runeclaw Bear", 2, 2)
            .id(),
    ];
    let caster_creature = scenario.add_creature_to_hand(P0, "Hill Giant", 3, 3).id();
    setup(&mut scenario);
    Board {
        runner: scenario.build(),
        fold,
        bears,
        caster_creature,
    }
}

/// Put a spell of type `core` controlled by `controller` on the stack, the
/// shape `counter_rider_tail_8762.rs` uses for its countered spell.
fn put_spell_on_stack(runner: &mut GameRunner, controller: PlayerId, core: CoreType) -> ObjectId {
    let spell = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(701),
        controller,
        "Opposing Spell".to_string(),
        Zone::Stack,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .card_types
        .core_types = vec![core];
    runner.state_mut().stack.push_back(StackEntry {
        id: spell,
        source_id: spell,
        controller,
        kind: StackEntryKind::Spell {
            card_id: CardId(701),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    spell
}

/// Pass priority until Fold into Aether starts resolving and raises a prompt,
/// or finishes resolving and leaves the stack.
fn pass_until_fold_prompts_or_resolves(commit: &mut CastCommit<'_>, fold: ObjectId) {
    for _ in 0..4 {
        let fold_on_stack = commit.state().objects[&fold].zone == Zone::Stack;
        match commit.state().waiting_for {
            WaitingFor::Priority { .. } if fold_on_stack => {
                commit
                    .act(GameAction::PassPriority)
                    .expect("passing priority must be accepted");
            }
            _ => return,
        }
    }
    panic!(
        "Fold into Aether never resolved, got {:?}",
        commit.state().waiting_for
    );
}

/// CR 608.2d: the countered spell's controller must be asked the "may"
/// decision while Fold into Aether resolves — not skipped, not made mandatory.
fn require_optional_choice_for_p1(commit: &CastCommit<'_>) {
    let WaitingFor::OptionalEffectChoice { player, .. } = commit.state().waiting_for else {
        panic!(
            "the countered spell's controller must be offered the optional put, got {:?}",
            commit.state().waiting_for
        );
    };
    assert_eq!(player, P1, "the countered spell's controller decides");
}

/// CR 608.2n: Fold into Aether has finished resolving — it is in its owner's
/// graveyard and the game is back at a priority window with no optional or
/// creature-card choice left open.
fn assert_fold_finished(commit: &CastCommit<'_>, fold: ObjectId) {
    assert!(
        matches!(commit.state().waiting_for, WaitingFor::Priority { .. }),
        "Fold into Aether must finish resolving with no prompt left open, got {:?}",
        commit.state().waiting_for
    );
    assert_eq!(
        commit.state().objects[&fold].zone,
        Zone::Graveyard,
        "the resolved Fold into Aether goes to its owner's graveyard"
    );
    assert!(
        !commit.state().stack.iter().any(|entry| entry.id == fold),
        "Fold into Aether is no longer on the stack"
    );
}

#[test]
fn accepting_lets_the_countered_spells_controller_choose_a_creature_from_hand() {
    let Board {
        mut runner,
        fold,
        bears,
        caster_creature,
    } = setup(|_| {});
    let spell = put_spell_on_stack(&mut runner, P1, CoreType::Instant);

    let mut commit = runner.cast(fold).target_objects(&[spell]).commit();
    pass_until_fold_prompts_or_resolves(&mut commit, fold);
    require_optional_choice_for_p1(&commit);
    commit
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional put must be accepted");

    let WaitingFor::EffectZoneChoice { player, cards, .. } = &commit.state().waiting_for else {
        panic!(
            "accepting must open a creature-card choice, got {:?}",
            commit.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the countered spell's controller chooses");
    assert_eq!(cards.len(), 2, "both of P1's creature cards are candidates");
    commit
        .act(GameAction::SelectCards {
            cards: vec![bears[1]],
        })
        .expect("choosing an offered creature card must be accepted");

    assert_fold_finished(&commit, fold);
    let state = commit.state();
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Graveyard,
        "reach guard: the target spell must be countered (CR 701.6a)"
    );
    assert!(state.stack.is_empty(), "the countered spell left the stack");
    assert_eq!(
        state.objects[&bears[1]].zone,
        Zone::Battlefield,
        "CR 608.2d: the chosen creature card must enter the battlefield"
    );
    assert_eq!(
        state.objects[&bears[1]].controller, P1,
        "the creature enters under the countered spell's controller"
    );
    assert_eq!(
        state.objects[&bears[0]].zone,
        Zone::Hand,
        "only the one chosen creature card is put onto the battlefield"
    );
    assert_eq!(
        state.objects[&caster_creature].zone,
        Zone::Hand,
        "the caster's creature card stays in hand"
    );
}

/// CR 608.2d + CR 608.2h: the choice belongs to the countered spell's
/// controller and is made among the creature cards in THEIR hand only.
#[test]
fn the_countered_spells_controller_chooses_among_their_own_creature_cards() {
    let Board {
        mut runner,
        fold,
        bears,
        caster_creature,
    } = setup(|_| {});
    let spell = put_spell_on_stack(&mut runner, P1, CoreType::Instant);

    let mut commit = runner.cast(fold).target_objects(&[spell]).commit();
    pass_until_fold_prompts_or_resolves(&mut commit, fold);
    require_optional_choice_for_p1(&commit);
    commit
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional put must be accepted");

    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        zone,
        destination,
        ..
    } = &commit.state().waiting_for
    else {
        panic!(
            "accepting must open a card choice, got {:?}",
            commit.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the countered spell's controller chooses");
    assert_eq!(*zone, Zone::Hand);
    assert_eq!(*destination, Some(Zone::Battlefield));
    let mut offered = cards.clone();
    offered.sort();
    let mut expected = bears.to_vec();
    expected.sort();
    assert_eq!(
        offered, expected,
        "exactly P1's creature cards are offered — not the countered spell, \
         and not the caster's creature card {caster_creature:?}"
    );
}

#[test]
fn declining_leaves_every_creature_card_in_hand() {
    let Board {
        mut runner,
        fold,
        bears,
        caster_creature,
    } = setup(|_| {});
    let spell = put_spell_on_stack(&mut runner, P1, CoreType::Instant);

    let mut commit = runner.cast(fold).target_objects(&[spell]).commit();
    pass_until_fold_prompts_or_resolves(&mut commit, fold);
    require_optional_choice_for_p1(&commit);
    commit
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional put must be accepted");

    assert_fold_finished(&commit, fold);
    let state = commit.state();
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Graveyard,
        "reach guard: the target spell must still be countered on decline"
    );
    assert!(state.stack.is_empty(), "the countered spell left the stack");
    for card in bears.into_iter().chain([caster_creature]) {
        assert_eq!(
            state.objects[&card].zone,
            Zone::Hand,
            "CR 608.2d: declining puts no creature card onto the battlefield"
        );
    }
}

#[test]
fn an_uncounterable_spell_offers_no_creature_put() {
    let Board {
        mut runner,
        fold,
        bears,
        caster_creature,
    } = setup(|scenario| {
        scenario.add_enchantment_from_oracle(P1, "Rhythm of the Wild", RHYTHM_OF_THE_WILD);
    });
    let spell = put_spell_on_stack(&mut runner, P1, CoreType::Creature);

    let mut commit = runner.cast(fold).target_objects(&[spell]).commit();
    pass_until_fold_prompts_or_resolves(&mut commit, fold);

    // CR 608.2c: the spell was not countered this way, so Fold into Aether
    // finishes without offering the optional put or a creature-card choice.
    assert_fold_finished(&commit, fold);
    let state = commit.state();
    assert_eq!(
        state.objects[&spell].zone,
        Zone::Stack,
        "reach guard: the creature spell can't be countered (CR 101.2)"
    );
    assert_eq!(
        state.stack.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![spell],
        "only the uncountered opposing spell remains on the stack"
    );
    for card in bears.into_iter().chain([caster_creature]) {
        assert_eq!(
            state.objects[&card].zone,
            Zone::Hand,
            "nothing is put onto the battlefield"
        );
    }
}
