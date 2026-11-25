//! CR 601.2f + CR 205.3: Ancient Cellarspawn's "Each spell you cast that's a
//! Demon, Horror, or Nightmare costs {1} less to cast." through the production
//! cost route: legal-action discovery (`legal_actions`), the locked total of a
//! manual cast (`pending_cast.cost`, CR 601.2f), affordability and dispatch of
//! an automatic cast, and finalization (its own second ability reads the mana
//! actually spent). Spells other than the real cards are labelled synthetics.

use crate::support::shared_card_db;
use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{
    ManaColor::{self, Black, Blue, Red, White},
    ManaCost, ManaCostShard, ManaType, ManaUnit,
};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ANCIENT_CELLARSPAWN: &str = "Each spell you cast that's a Demon, Horror, or Nightmare costs {1} less to cast.\nWhenever you cast a spell, if the amount of mana spent to cast it was less than its mana value, target opponent loses life equal to the difference.";
const THALIA: &str = "First strike\nNoncreature spells cost {1} more to cast.";

fn cost(generic: u32, black: usize) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black; black],
        generic,
    }
}

fn pool(colorless: usize, black: usize) -> Vec<ManaUnit> {
    let unit = |t| ManaUnit::new(t, ObjectId(0), false, vec![]);
    std::iter::repeat_n(unit(ManaType::Colorless), colorless)
        .chain(std::iter::repeat_n(unit(ManaType::Black), black))
        .collect()
}

/// P0 controls Ancient Cellarspawn; `caster` holds the spell `add` creates and
/// the mana in `mana`, at the caster's main phase with priority.
fn board(
    caster: PlayerId,
    mana: Vec<ManaUnit>,
    add: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Ancient Cellarspawn", 3, 3, ANCIENT_CELLARSPAWN)
        .with_subtypes(vec!["Horror"]);
    let spell = add(&mut scenario);
    scenario.with_mana_pool(caster, mana);
    let mut runner = scenario.build();
    if caster != P0 {
        let state = runner.state_mut();
        state.active_player = caster;
        state.priority_player = caster;
        state.waiting_for = WaitingFor::Priority { player: caster };
    }
    (runner, spell)
}

fn creature_spell(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    subtypes: Vec<&str>,
    mana_cost: ManaCost,
) -> ObjectId {
    scenario
        .add_creature_to_hand(owner, name, 3, 3)
        .with_subtypes(subtypes)
        .with_mana_cost(mana_cost)
        .id()
}

fn offered(runner: &GameRunner, spell: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell),
    )
}

/// CR 601.2f: begin a manual cast and read the total it locked in.
fn locked(runner: &mut GameRunner, spell: ObjectId) -> ManaCost {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin the cast");
    runner
        .state()
        .pending_cast
        .as_ref()
        .map(|pending| pending.cost.clone())
        .expect("a manual cast parks at payment with its locked total")
}

/// Row 1: a Demon creature spell costing {2}{B} with exactly {1}{B}
/// available is offered, locks {1}{B}, and an automatic cast finalizes with 2
/// mana spent.
#[test]
fn a_listed_spell_is_offered_locks_the_reduced_cost_and_casts() {
    let (runner, demon) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon", vec!["Demon"], cost(2, 1))
    });
    assert!(offered(&runner, demon), "offered with only {{1}}{{B}}");
    let mut manual = runner;
    assert_eq!(locked(&mut manual, demon), cost(1, 1), "locked {{1}}{{B}}");

    let (mut runner, demon) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon", vec!["Demon"], cost(2, 1))
    });
    let card_id = runner.state().objects[&demon].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: demon,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the automatic cast is affordable");
    let spent = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                actual_mana_spent, ..
            } if entry.id == demon => Some(*actual_mana_spent),
            _ => None,
        });
    assert_eq!(spent, Some(2), "finalized with 2 mana spent");
}

/// Row 2: a spell that isn't listed is not offered with {1}{B} and locks its
/// full {2}{B}; a dispatched automatic cast is rejected at affordability.
#[test]
fn an_unlisted_spell_is_not_discounted() {
    let (mut runner, bear) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Bear", vec!["Bear"], cost(2, 1))
    });
    assert!(!offered(&runner, bear), "not offered with {{1}}{{B}}");
    let card_id = runner.state().objects[&bear].card_id;
    assert!(
        runner
            .act(GameAction::CastSpell {
                object_id: bear,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err()
            || runner.state().objects[&bear].zone == Zone::Hand,
        "the dispatched cast is rejected"
    );
    let (mut manual, bear) = board(P0, pool(2, 1), |s| {
        creature_spell(s, P0, "Bear", vec!["Bear"], cost(2, 1))
    });
    assert_eq!(locked(&mut manual, bear), cost(2, 1));
}

/// Row 3: a Demon Horror (two listed subtypes) is discounted once.
#[test]
fn overlapping_listed_subtypes_discount_once() {
    let (mut runner, spell) = board(P0, pool(3, 1), |s| {
        creature_spell(s, P0, "Demon Horror", vec!["Demon", "Horror"], cost(3, 1))
    });
    assert_eq!(locked(&mut runner, spell), cost(2, 1));
}

/// Row 4: a non-creature spell with a listed subtype (a labelled synthetic
/// Horror instant) is discounted too: the static names spells, not creatures.
#[test]
fn a_noncreature_spell_with_a_listed_subtype_is_discounted() {
    let (mut runner, spell) = board(P0, pool(1, 1), |s| {
        s.add_spell_to_hand_from_oracle(P0, "Horror Rite", true, "Draw a card.")
            .with_subtypes(vec!["Horror"])
            .with_mana_cost(cost(1, 1))
            .id()
    });
    assert_eq!(locked(&mut runner, spell), cost(0, 1));
}

/// Row 5: "spell you cast" — an opponent's Demon is not discounted.
#[test]
fn an_opponents_listed_spell_is_not_discounted() {
    let (mut runner, demon) = board(P1, pool(2, 1), |s| {
        creature_spell(s, P1, "Their Demon", vec!["Demon"], cost(2, 1))
    });
    assert_eq!(locked(&mut runner, demon), cost(2, 1));
}

/// Row 6: CR 601.2f — a generic reduction reduces only generic mana; a Demon
/// costing {B}{B} still costs {B}{B}.
#[test]
fn a_generic_reduction_cannot_reduce_colored_mana() {
    let (mut runner, demon) = board(P0, pool(0, 2), |s| {
        creature_spell(s, P0, "Pure Demon", vec!["Demon"], cost(0, 2))
    });
    assert_eq!(locked(&mut runner, demon), cost(0, 2));
}

/// Row 7: CR 601.2f — increases and reductions both apply. Thalia's {1} tax
/// on the Horror instant and Cellarspawn's {1} discount cancel out.
#[test]
fn a_tax_and_the_discount_both_apply() {
    let (mut runner, spell) = board(P0, pool(1, 1), |s| {
        s.add_creature_from_oracle(P1, "Thalia, Guardian of Thraben", 2, 1, THALIA);
        s.add_spell_to_hand_from_oracle(P0, "Horror Rite", true, "Draw a card.")
            .with_subtypes(vec!["Horror"])
            .with_mana_cost(cost(1, 1))
            .id()
    });
    assert_eq!(locked(&mut runner, spell), cost(1, 1));
}

fn adventure_face() -> engine::game::game_object::BackFaceData {
    engine::game::game_object::BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Unlisted Adventure".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: {
            let mut card_type = CardType::default();
            card_type.core_types.push(CoreType::Instant);
            card_type.subtypes.push("Adventure".to_string());
            card_type
        },
        mana_cost: cost(1, 1),
        keywords: Vec::new(),
        abilities: vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Black],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        layout_kind: None,
        parse_warnings: vec![],
    }
}

/// Row 8: CR 601.2f + CR 715.3 — the cost is the face being cast. A labelled
/// synthetic adventurer whose creature face is a Demon ({2}{B}) and whose
/// Adventure face is not ({1}{B}): with only {B}, the Adventure can't be cast
/// (no discount); with {1}{B}, the Demon face can.
#[test]
fn only_the_face_being_cast_is_read() {
    let adventurer = |runner: &mut GameRunner, card: ObjectId| {
        runner.state_mut().objects.get_mut(&card).unwrap().back_face = Some(adventure_face());
    };
    let (mut runner, card) = board(P0, pool(0, 1), |s| {
        creature_spell(s, P0, "Demon Adventurer", vec!["Demon"], cost(2, 1))
    });
    adventurer(&mut runner, card);
    assert!(
        runner
            .cast(card)
            .adventure_face(false)
            .try_resolve()
            .is_err()
            || runner.state().objects[&card].zone == Zone::Hand,
        "the unlisted Adventure face is not discounted"
    );
    let (mut runner, card) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon Adventurer", vec!["Demon"], cost(2, 1))
    });
    adventurer(&mut runner, card);
    runner.cast(card).adventure_face(true).resolve();
    assert_eq!(runner.state().objects[&card].zone, Zone::Battlefield);
}

/// Row 9: CR 601.2f — once the total is locked in, it doesn't change: the
/// Cellarspawn leaving during payment (moved directly, labelled) leaves the
/// locked {1}{B}, and the payment completes with it.
#[test]
fn the_locked_cost_survives_the_source_leaving_during_payment() {
    let (mut runner, demon) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon", vec!["Demon"], cost(2, 1))
    });
    assert_eq!(locked(&mut runner, demon), cost(1, 1));
    let cellarspawn = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Ancient Cellarspawn")
        .expect("Cellarspawn");
    {
        let state = runner.state_mut();
        engine::game::zones::move_to_zone(state, cellarspawn, Zone::Graveyard, &mut Vec::new());
    }
    assert_eq!(
        runner.state().pending_cast.as_ref().map(|p| p.cost.clone()),
        Some(cost(1, 1)),
        "still locked"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner.act(GameAction::PassPriority).expect("pay");
    assert_eq!(
        runner.state().objects[&demon].zone,
        Zone::Stack,
        "finalized"
    );
}

const P2: PlayerId = PlayerId(2);

fn three_player_board() -> GameScenario {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Ancient Cellarspawn", 3, 3, ANCIENT_CELLARSPAWN)
        .with_subtypes(vec!["Horror"]);
    scenario
}

fn choose_second_opponent(runner: &mut GameRunner) {
    let WaitingFor::TriggerTargetSelection {
        player: P0,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Cellarspawn's target announcement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    assert_eq!(target_slots[0].legal_targets.len(), 2);
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Player(P1)));
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Player(P2)));
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Player(P2)],
        })
        .expect("announce the second opponent");
}

#[test]
fn the_second_ability_hits_the_announced_opponent_on_discounted_casts_only() {
    let mut scenario = three_player_board();
    let db = shared_card_db().expect("printed card fixture");
    let demon = scenario.add_real_card(P0, "Doom Whisperer", Zone::Hand, db);
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, db);
    for _ in 0..2 {
        scenario.add_basic_land(P0, ManaColor::Black);
        scenario.add_basic_land(P0, ManaColor::White);
    }
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&demon].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: demon,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the discounted Demon");
    assert_eq!(runner.state().objects[&demon].mana_spent_to_cast_amount, 4);
    choose_second_opponent(&mut runner);
    assert_eq!(runner.state().stack.len(), 2);
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&demon].zone, Zone::Battlefield);
    assert_eq!(
        (runner.life(P0), runner.life(P1), runner.life(P2)),
        (20, 20, 19)
    );

    drop(runner.cast(bear).commit());
    assert_eq!(runner.state().objects[&bear].mana_spent_to_cast_amount, 2);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "full price produces no trigger"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().objects[&bear].zone, Zone::Battlefield);
    assert_eq!(
        (runner.life(P0), runner.life(P1), runner.life(P2)),
        (20, 20, 19)
    );
}

// CR 202.3e + CR 608.2a + CR 608.2h: The pending trigger uses the first
// cast's payment and stack mana value after it is countered and even after
// that same card has been cast again with different facts.
#[test]
fn countered_x_cast_keeps_its_original_difference_after_a_same_id_recast() {
    let db = shared_card_db().expect("printed card fixture");
    for (recast, legacy_event) in [(true, true), (false, false), (false, true)] {
        let mut scenario = three_player_board();
        scenario.add_real_card(P0, "Leyline of Anticipation", Zone::Battlefield, db);
        if recast {
            // The extra cast-history tax makes the recast fail Cellarspawn's
            // comparison, so reading the newer occurrence changes the outcome.
            scenario.add_real_card(P0, "Damping Sphere", Zone::Battlefield, db);
        }
        let spell = scenario.add_real_card(P0, "Orcus, Prince of Undeath", Zone::Hand, db);
        let counterspell = scenario.add_real_card(P1, "Counterspell", Zone::Hand, db);
        let recast_counterspell = scenario.add_real_card(P1, "Counterspell", Zone::Hand, db);
        let recover = scenario.add_real_card(P0, "Grim Harvest", Zone::Hand, db);
        let mut lands_for = |player, colors: &[ManaColor]| {
            colors
                .iter()
                .map(|&color| scenario.add_basic_land(player, color))
                .collect::<Vec<_>>()
        };
        let first_mana = lands_for(P0, &[Black, Red, White, White, White, White]);
        let counter_mana = lands_for(P1, &[Blue, Blue]);
        let recast_counter_mana = lands_for(P1, &[Blue, Blue, Blue]);
        let recover_mana = lands_for(P0, &[Black, White, White]);
        let recast_mana = lands_for(P0, &[Black, Red, White, White, White]);
        let tap_lands = |runner: &mut GameRunner, lands: &[ObjectId]| {
            for &source_id in lands {
                runner
                    .act(GameAction::ActivateAbility {
                        source_id,
                        ability_index: 0,
                        payment_mode: CastPaymentMode::Auto,
                    })
                    .expect("activate a basic land's mana ability");
                assert!(runner.state().objects[&source_id].tapped);
            }
        };
        let counter = |runner: &mut GameRunner, card, lands: &[ObjectId]| {
            runner
                .act(GameAction::PassPriority)
                .expect("pass to the Counterspell player");
            tap_lands(runner, lands);
            drop(runner.cast(card).target_object(spell).commit());
            runner.resolve_top();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        };
        let mut runner = scenario.build();
        tap_lands(&mut runner, &first_mana);
        let original_incarnation = {
            let cast = runner.cast(spell).x(3).target_player(P2).commit();
            assert_eq!(cast.state().stack.len(), 2);
            assert_eq!(cast.state().objects[&spell].spell_mana_value(), 7);
            assert_eq!(cast.state().objects[&spell].mana_spent_to_cast_amount, 6);
            cast.state().objects[&spell].incarnation
        };

        let original = runner
            .state_mut()
            .stack
            .back_mut()
            .expect("original trigger");
        let original_trigger = original.id;
        if legacy_event {
            // Model a saved trigger from before SpellCast carried the payment
            // and incarnation. Its ability still pins the first cast.
            let pin = original
                .ability()
                .unwrap()
                .context
                .triggering_spell
                .unwrap();
            assert_eq!(
                (pin.object_id, pin.incarnation),
                (spell, original_incarnation)
            );
            let StackEntryKind::TriggeredAbility {
                trigger_event:
                    Some(GameEvent::SpellCast {
                        cast_mana_spent,
                        cast_incarnation,
                        ..
                    }),
                ..
            } = &mut original.kind
            else {
                panic!("expected the original cast trigger");
            };
            assert_eq!(cast_mana_spent.take(), Some(6));
            assert_eq!(cast_incarnation.take(), Some(original_incarnation));
        }

        counter(&mut runner, counterspell, &counter_mana);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack[0].id, original_trigger);

        if recast {
            tap_lands(&mut runner, &recover_mana);
            drop(runner.cast(recover).target_object(spell).commit());
            runner.resolve_top();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
            tap_lands(&mut runner, &recast_mana);
            drop(runner.cast(spell).x(0).commit());
            let orcus = &runner.state().objects[&spell];
            assert_eq!(orcus.zone, Zone::Stack);
            assert_ne!(orcus.incarnation, original_incarnation);
            assert_eq!(orcus.spell_mana_value(), 4);
            assert_eq!(orcus.mana_spent_to_cast_amount, 5);
            assert_eq!(
                runner.state().stack.len(),
                2,
                "the taxed recast creates no trigger"
            );
            assert_eq!(runner.state().stack[0].id, original_trigger);

            let recast_incarnation = orcus.incarnation;
            counter(&mut runner, recast_counterspell, &recast_counter_mana);
            let departed = &runner.state().departed_stack_spells[&spell];
            assert!(departed.contains_key(&original_incarnation));
            assert!(departed.contains_key(&recast_incarnation));
            assert_eq!(runner.state().stack.len(), 1);
            assert_eq!(runner.state().stack[0].id, original_trigger);
        }

        assert_eq!(runner.life(P2), 20, "the original trigger is still pending");
        runner.advance_until_stack_empty();
        assert!(runner.state().stack.is_empty());
        assert_eq!(
            (runner.life(P0), runner.life(P1), runner.life(P2)),
            (20, 20, 19)
        );
    }
}
