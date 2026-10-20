//! Production actions for explicit resolution progress and its persisted owners.

use std::sync::Arc;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, CardPlayMode,
    CastFromZoneDriver, DieResultBranch, Effect, EffectKind, LibraryInstructionActor,
    LibraryPosition, ManaContribution, ManaProduction, PlayerFilter, PlayerScope, QuantityExpr,
    QuantityRef, SubAbilityLink, TargetFilter, TargetRef,
};
use engine::types::actions::{CastChoice, GameAction};
use engine::types::counter::CounterType;
use engine::types::events::{ClashResult, GameEvent};
use engine::types::game_state::{
    CastOfferKind, ManaChoice, ManaChoiceContext, ManaChoicePrompt, PersistedGameState,
    StackEntryKind, WaitingFor,
};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::ResolutionStateWire;
use engine::types::zones::Zone;

use super::cast_this_way_gate_8721::{accept_offer_and_pay, to_declare_attackers};
use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

// Verbatim MTGJSON Oracle text, read from AtomicCards.json before implementation.
const CLAIM_JUMPER: &str = "Vigilance\nWhen this creature enters, if an opponent controls more lands than you, you may search your library for a Plains card and put it onto the battlefield tapped. Then if an opponent controls more lands than you, repeat this process once. If you search your library this way, shuffle.";
const BARDS_COMPANY: &str = "You may cast this spell as though it had flash if you control a Human.\nOther creatures you control get +1/+1.\nWhenever this creature enters or attacks, recruit. (Draw a card, then discard a card. If you discarded a nonland card, create a 1/1 white Human Soldier creature token.)";
const OAKEN_BRAWLER: &str = "When this creature enters, clash with an opponent. If you win, put a +1/+1 counter on this creature. (Each clashing player reveals the top card of their library, then puts that card on their choice of the top or bottom. A player wins if their card had a greater mana value.)";
const GLIMMER: &str = "Scry 2, then draw two cards. You get {E}{E} (two energy counters).";
const GROVE: &str = "{T}: Add {C}.\n{T}: Add {R} or {G}. Each opponent gains 1 life.";
const TAINTED_REMEDY: &str =
    "If an opponent would gain life, that player loses that much life instead.";

fn def(effect: Effect) -> AbilityDefinition {
    AbilityDefinition::new(AbilityKind::Spell, effect)
}

fn gain(amount: i32) -> AbilityDefinition {
    def(Effect::GainLife {
        amount: QuantityExpr::Fixed { value: amount },
        player: TargetFilter::Controller,
    })
}

fn roll(count: i32, branch: Option<AbilityDefinition>) -> AbilityDefinition {
    def(Effect::RollDie {
        count: QuantityExpr::Fixed { value: count },
        sides: 1,
        modifier: None,
        results: branch
            .into_iter()
            .map(|effect| DieResultBranch {
                min: 1,
                max: 1,
                effect: Box::new(effect),
            })
            .collect(),
    })
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .life
}

fn pass_stack(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..24 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            || (runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty())
        {
            return events;
        }
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass resolving stack")
                .events,
        );
    }
    panic!(
        "stack did not reach its next decision: {:?}",
        runner.state().waiting_for
    );
}

fn assert_finished(runner: &GameRunner) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().resolution_stack.is_empty(),
        "{:?}",
        runner.state().resolution_stack
    );
}

fn restore(runner: &mut GameRunner) {
    runner.state_mut().capture_rng_word_pos();
    let json =
        serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone())).unwrap();
    assert_eq!(json["resolution_state_version"], 7);
    assert!(!json.to_string().contains("ResolutionProgress"));
    let restored = serde_json::from_value::<PersistedGameState>(json)
        .expect("live resolution decodes")
        .into_game_state()
        .expect("live resolution restores through the persistence boundary");
    *runner = GameRunner::from_state(restored);
}

#[test]
fn answered_mana_color_does_not_suspend_each_opponents_life_change() {
    for remedy in [false, true] {
        let mut s = GameScenario::new_n_player(4, 42);
        s.at_phase(Phase::PreCombatMain);
        let grove = s
            .add_land_from_oracle(P0, "Grove of the Burnwillows", GROVE)
            .id();
        if remedy {
            s.add_enchantment_from_oracle(P0, "Tainted Remedy", TAINTED_REMEDY);
        }
        let mut r = s.build();
        r.act(GameAction::ActivateAbility {
            source_id: grove,
            ability_index: 1,
        })
        .unwrap();
        assert!(matches!(
            &r.state().waiting_for,
            WaitingFor::ChooseManaColor {
                player: P0,
                choice: ManaChoicePrompt::SingleColor { options },
                context: ManaChoiceContext::ManaAbility(pending),
            } if pending.source_id == grove
                && options.contains(&ManaType::Red)
                && options.contains(&ManaType::Green)
        ));
        assert_eq!(life(&r, P1), 20, "production awaits the color answer");
        let events = r
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            })
            .unwrap()
            .events;
        assert_eq!(r.state().players[0].mana_pool.count_color(ManaType::Red), 1);
        assert!(r.state().objects[&grove].tapped);
        // CR 605.3b + CR 608.2c + CR 614.1a: the mana ability finishes its
        // instruction for every opponent, including each replaced life gain.
        let change = if remedy { -1 } else { 1 };
        assert_eq!(life(&r, P0), 20);
        for opponent in [P1, P2, PlayerId(3)] {
            assert_eq!(life(&r, opponent), 20 + change, "{opponent:?}");
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event,
                        GameEvent::LifeChanged { player_id, amount, .. }
                            if *player_id == opponent && *amount == change))
                    .count(),
                1,
                "each opponent's instruction completes exactly once"
            );
        }
        assert_finished(&r);
    }
}

#[test]
fn value_equal_successive_scry_prompts_keep_the_second_choice_before_the_tail() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["First", "Second", "Third"]);
    let scry = def(Effect::Scry {
        count: QuantityExpr::Fixed { value: 2 },
        target: TargetFilter::Controller,
    });
    let spell = s
        .add_spell_to_hand(P0, "Synthetic successive equal choices", true)
        .with_ability_definition(scry.clone().sub_ability(scry.sub_ability(gain(7))))
        .id();
    let mut r = s.build();
    r.cast(spell).commit();
    pass_stack(&mut r);
    let first_prompt = r.state().waiting_for.clone();
    let WaitingFor::ScryChoice { player: P0, cards } = &first_prompt else {
        panic!("first scry must offer its real choice: {first_prompt:?}");
    };
    assert_eq!(cards.len(), 2);
    r.act(GameAction::SelectCards {
        cards: cards.clone(),
    })
    .unwrap();
    // CR 608.2c + CR 608.2d: equal choices still belong to separate
    // instructions; the life-gain tail follows the second actual answer.
    assert_eq!(r.state().waiting_for, first_prompt);
    assert_eq!(life(&r, P0), 20);
    assert!(!r.state().resolution_stack.is_empty());
    r.act(GameAction::SelectCards {
        cards: vec![cards[1]],
    })
    .unwrap();
    assert_eq!(r.state().library_of(P0).front(), Some(&cards[1]));
    assert_eq!(r.state().library_of(P0).back(), Some(&cards[0]));
    assert_eq!(life(&r, P0), 27);
    assert_finished(&r);
}

#[test]
fn answered_mana_color_finishes_each_instead_tail_before_repeating() {
    let override_clause = gain(99).condition(AbilityCondition::ConditionInstead {
        inner: Box::new(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        }),
    });
    let mut else_clause = override_clause.clone();
    else_clause.else_ability = Some(Box::new(gain(7)));
    let mut sibling_clause = override_clause;
    let mut tail = gain(7);
    tail.sub_link = SubAbilityLink::SequentialSibling;
    sibling_clause.sub_ability = Some(Box::new(tail));
    let contingent_clause = gain(7).condition(AbilityCondition::Or {
        conditions: vec![
            AbilityCondition::effect_performed(),
            AbilityCondition::IsYourTurn,
        ],
    });
    for (tail_kind, clause) in [
        ("instead else", else_clause),
        ("instead sibling", sibling_clause),
        ("contingent", contingent_clause),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let mut body = gain(1).sub_ability(clause);
        body.repeat_for = Some(QuantityExpr::Fixed { value: 3 });
        // Typed building-block fixture: a color-producing mana ability whose
        // repeated instruction has a not-swapped instead or contingent tail.
        // CR 605.1a: its costs and effects never move cards to/from a library.
        let root = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: QuantityExpr::Fixed { value: 1 },
                    color_options: vec![ManaColor::Red, ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
        .sub_ability(body);
        let source = s
            .add_land_from_oracle(P0, "Synthetic repeated instead mana source", "")
            .with_ability_definition(root)
            .id();
        let mut r = s.build();
        r.act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .unwrap();
        assert!(matches!(
            &r.state().waiting_for,
            WaitingFor::ChooseManaColor {
                player: P0,
                context: ManaChoiceContext::ManaAbility(pending),
                ..
            } if pending.source_id == source
        ));
        assert_eq!(life(&r, P0), 20);
        let events = r
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            })
            .unwrap()
            .events;
        // CR 608.2c: each completed parent runs its tail before the next
        // iteration; the ambient mana prompt never queues these instructions.
        assert_eq!(life(&r, P0), 44, "tail={tail_kind}");
        assert_eq!(
            events
                .iter()
                .filter_map(|event| match event {
                    GameEvent::LifeChanged {
                        player_id: P0,
                        amount,
                        ..
                    } => Some(*amount),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![1, 7, 1, 7, 1, 7]
        );
        assert_eq!(r.state().players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_finished(&r);
    }
}

#[test]
fn pending_parent_defers_false_conditional_else_and_sibling_before_repeating() {
    for sibling in [false, true] {
        let mut clause = gain(99).condition(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::IsYourTurn),
        });
        if sibling {
            let mut tail = gain(7);
            tail.sub_link = SubAbilityLink::SequentialSibling;
            clause.sub_ability = Some(Box::new(tail));
        } else {
            clause.else_ability = Some(Box::new(gain(7)));
        }
        let mut root = def(Effect::Scry {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        })
        .sub_ability(clause);
        root.repeat_for = Some(QuantityExpr::Fixed { value: 3 });
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.with_library_top(P0, &["First", "Second", "Third", "Fourth"]);
        let spell = s
            .add_spell_to_hand(P0, "Synthetic conditional scry repeat", true)
            .with_ability_definition(root)
            .id();
        let mut r = s.build();
        r.cast(spell).commit();
        pass_stack(&mut r);
        let mut expected_library = r.state().library_of(P0).clone();
        for completed in 0..3 {
            let WaitingFor::ScryChoice { player, cards } = &r.state().waiting_for else {
                panic!(
                    "sibling={sibling}, completed={completed}: {:?}",
                    r.state().waiting_for
                );
            };
            assert_eq!(*player, P0);
            assert_eq!(
                cards,
                &expected_library.iter().take(2).copied().collect::<Vec<_>>()
            );
            // CR 608.2c: even a false condition's else/sibling is a later
            // instruction; none may run before this scry has been answered.
            assert_eq!(life(&r, P0), 20 + completed * 7, "sibling={sibling}");
            let kept = cards[1];
            let bottomed = expected_library.pop_front().unwrap();
            expected_library.push_back(bottomed);
            let result = r
                .act(GameAction::SelectCards { cards: vec![kept] })
                .unwrap();
            assert_eq!(r.state().library_of(P0), &expected_library);
            assert_eq!(life(&r, P0), 20 + (completed + 1) * 7);
            assert_eq!(
                result
                    .events
                    .iter()
                    .filter_map(|event| match event {
                        GameEvent::LifeChanged {
                            player_id: P0,
                            amount,
                            ..
                        } => Some(*amount),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                [7]
            );
        }
        assert_eq!(life(&r, P0), 41);
        assert_finished(&r);
    }
}

#[test]
fn inline_paid_cast_trigger_keeps_its_offer_pending_through_nested_repeat_and_die() {
    // Verbatim current MTGJSON Oracle texts supply the paid-cast instructions.
    // The repeat/die process and TargetOnly -> ParentTarget binding below are
    // synthetic; the original rider and cast trigger keep their source.
    const ZEMO: &str = "Whenever Helmut Zemo attacks, you may cast target instant or sorcery card with mana value less than or equal to his power from your graveyard. If that spell would be put into your graveyard, exile it instead. If you cast a spell this way, put a +1/+1 counter on Helmut Zemo.";
    const OGRE: &str = "First strike\nWhenever this creature attacks, you may cast target instant or sorcery card from your graveyard by paying {R}{R} in addition to its other costs. If that spell would be put into a graveyard, exile it instead. When you cast that spell, this creature gets +X/+0 until end of turn, where X is that spell's mana value.";
    for (name, oracle, power) in [
        ("Helmut Zemo, Mastermind", ZEMO, 2),
        ("Ogre Battlecaster", OGRE, 3),
    ] {
        for accept in [false, true] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let source = s
                .add_creature_from_oracle(P0, name, power, power, oracle)
                .id();
            for _ in 0..4 {
                s.add_basic_land(P0, ManaColor::Red);
            }
            let card = s
                .add_spell_to_graveyard(P0, "Synthetic paid instant", true)
                .with_mana_cost(ManaCost::generic(1))
                .with_ability_definition(gain(1))
                .id();
            let mut r = s.build();
            let mut printed = *r.state().objects[&source].trigger_definitions[0]
                .definition
                .execute
                .clone()
                .expect("printed attack instructions");
            let target = printed
                .effect
                .target_filter()
                .expect("printed cast target")
                .clone();
            // The outer TargetOnly announces the printed target. This nested
            // instruction consumes that same card rather than declaring an
            // additional unannounced slot inside the die table.
            *printed
                .effect
                .target_filter_mut()
                .expect("printed cast target") = TargetFilter::ParentTarget;
            // The paid offer itself retains the may/decline decision. Avoid a
            // separate optional wrapper so the typed caller directly observes
            // the CastFromZone -> inline CreateDelayedTrigger return.
            printed.optional = false;
            printed.sub_link = SubAbilityLink::ContinuationStep;
            let mut repeated = gain(1).sub_ability(printed);
            repeated.repeat_for = Some(QuantityExpr::Fixed { value: 2 });
            let wrapper = def(Effect::TargetOnly { target })
                .sub_ability(roll(2, Some(repeated)).sub_ability(gain(7)));
            let object = r.state_mut().objects.get_mut(&source).unwrap();
            let mut triggers = object.base_trigger_definitions.as_ref().clone();
            triggers[0].execute = Some(Box::new(wrapper));
            object
                .install_trigger_base_definitions(Arc::new(triggers))
                .unwrap();
            to_declare_attackers(&mut r, P0);
            r.declare_attackers(&[(source, AttackTarget::Player(P1))])
                .unwrap();
            while matches!(
                r.state().waiting_for,
                WaitingFor::TriggerTargetSelection { .. }
            ) {
                r.choose_first_legal_target()
                    .expect("announce the graveyard target");
            }
            let entry = r
                .state()
                .stack
                .last()
                .expect("the attack produced a real stack entry");
            assert_eq!(entry.source_id, source);
            let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind else {
                panic!("the attack must produce its triggered ability");
            };
            assert!(matches!(ability.effect, Effect::TargetOnly { .. }));
            assert!(
                ability.targets.contains(&TargetRef::Object(card)),
                "announced targets: {:?}",
                ability.targets
            );
            let events = pass_stack(&mut r);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::DieRolled { .. }))
                    .count(),
                1,
                "the first survivor reaches its paid branch before the next die"
            );
            let mut offer_ids = Vec::new();
            for offer in 0..4 {
                let WaitingFor::CastOffer {
                    player: P0,
                    kind:
                        CastOfferKind::GraveyardPaidCast {
                            hit_card, cleanup, ..
                        },
                } = &r.state().waiting_for
                else {
                    panic!(
                        "{name}, accept={accept}, offer={offer}: {:?}",
                        r.state().waiting_for
                    );
                };
                assert_eq!(*hit_card, card);
                let offer_id = cleanup.offer_id.expect("producer-issued offer identity");
                assert!(!offer_ids.contains(&offer_id));
                offer_ids.push(offer_id);
                // CR 603.7a: registration precedes its possible cast event;
                // the receipt remains owned by this exact unanswered offer.
                assert_eq!(cleanup.delayed_trigger_receipts.len(), 1);
                assert_eq!(cleanup.delayed_trigger_receipts[0].offer_id, offer_id);
                assert_eq!(r.state().delayed_triggers.len(), 1);
                assert_eq!(
                    life(&r, P0),
                    21 + offer,
                    "{name}, accept={accept}: later work ran before the offer answer"
                );
                assert!(
                    r.state().resolution_stack.len() >= 2,
                    "both repeat and die remain unfinished"
                );
                // Accept the last offer so earlier iterations retain a legal
                // graveyard target; the all-decline sibling never casts it.
                if accept && offer == 3 {
                    accept_offer_and_pay(&mut r);
                    assert!(r.state().stack.iter().any(|entry| entry.id == card));
                    r.advance_until_stack_empty();
                } else {
                    r.act(GameAction::GraveyardPaidCastChoice {
                        choice: CastChoice::Decline,
                    })
                    .unwrap();
                }
            }
            // CR 608.2c + CR 608.2g: the enclosing later instruction runs once
            // after every offer settles; accepting also resolves the instant.
            assert_eq!(life(&r, P0), if accept { 32 } else { 31 });
            assert_eq!(
                r.state().objects[&card].zone,
                if accept { Zone::Exile } else { Zone::Graveyard }
            );
            assert!(r.state().objects[&card].casting_permissions.is_empty());
            assert!(r.state().delayed_triggers.is_empty());
            engine::game::layers::evaluate_layers(r.state_mut());
            assert_eq!(
                r.state().objects[&source].power,
                Some(power + i32::from(accept))
            );
            assert_finished(&r);
        }
    }
}

#[test]
fn glimmer_nonidentity_scry_then_draw_and_energy_finish_once() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Glimmer of Genius", true, GLIMMER)
        .id();
    s.with_library_top(P0, &["First", "Second", "Third", "Fourth"]);
    let mut r = s.build();
    let library: Vec<_> = r.state().library_of(P0).iter().copied().collect();
    r.cast(spell).commit();
    pass_stack(&mut r);
    assert!(
        matches!(&r.state().waiting_for, WaitingFor::ScryChoice { player: P0, cards } if cards == &library[..2])
    );
    r.act(GameAction::SelectCards {
        cards: vec![library[1]],
    })
    .unwrap();
    // CR 608.2c: the selected second card stays on top, followed by the third;
    // the first is bottomed before the two draws and the energy instruction.
    assert_eq!(r.state().objects[&library[1]].zone, Zone::Hand);
    assert_eq!(r.state().objects[&library[2]].zone, Zone::Hand);
    assert_eq!(r.state().library_of(P0).back(), Some(&library[0]));
    assert_eq!(r.state().players[0].energy, 2);
    assert_finished(&r);
}

#[test]
fn recruit_single_drawn_card_hands_off_settled_land_and_nonland_results() {
    for land in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let company = s
            .add_creature_to_hand_from_oracle(P0, "Bard's Company", 3, 3, BARDS_COMPANY)
            .id();
        let drawn = if land {
            s.add_land_to_library_top(P0, "Plains").id()
        } else {
            s.add_spell_to_library_top(P0, "Drawn nonland", true).id()
        };
        let mut r = s.build();
        r.cast(company).commit();
        let events = pass_stack(&mut r);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Discarded { object_id, .. } if *object_id == drawn)));
        assert_eq!(r.state().objects[&drawn].zone, Zone::Graveyard);
        assert!(r.state().players[0].hand.is_empty());
        let tokens: Vec<_> = r
            .state()
            .objects
            .values()
            .filter(|o| o.zone == Zone::Battlefield && o.is_token)
            .collect();
        // CR 701.9a + CR 608.2c: the immediate reader uses this discarded card.
        assert_eq!(tokens.len(), usize::from(!land));
        if let Some(token) = tokens.first() {
            assert!(token.card_types.subtypes.iter().any(|s| s == "Human"));
            assert!(token.card_types.subtypes.iter().any(|s| s == "Soldier"));
        }
        assert_finished(&r);
    }
}

#[test]
fn claim_jumper_etb_finishes_search_before_bounded_repeat_and_restores() {
    for (opponent_lands, expected) in [(3, 2), (1, 1)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        for _ in 0..opponent_lands {
            s.add_basic_land(P1, engine::types::mana::ManaColor::Green);
        }
        let jumper = s
            .add_creature_to_hand_from_oracle(P0, "Claim Jumper", 3, 3, CLAIM_JUMPER)
            .id();
        let plains: Vec<_> = (0..3)
            .map(|_| {
                s.add_land_to_library_top(P0, "Plains")
                    .with_subtypes(vec!["Plains"])
                    .id()
            })
            .collect();
        let mut r = s.build();
        r.cast(jumper).commit();
        pass_stack(&mut r);
        for index in 0..expected {
            assert!(
                matches!(
                    r.state().waiting_for,
                    WaitingFor::OptionalEffectChoice { player: P0, .. }
                ),
                "expected optional iteration {index}, state={:?}, stack={:?}",
                r.state().waiting_for,
                r.state().stack
            );
            r.act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            let WaitingFor::SearchChoice { player, cards, .. } = &r.state().waiting_for else {
                panic!("{:?}", r.state().waiting_for)
            };
            assert_eq!(*player, P0);
            let chosen = *cards.first().expect("a Plains remains");
            assert!(plains.contains(&chosen));
            restore(&mut r);
            r.act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .unwrap();
            assert_eq!(r.state().objects[&chosen].zone, Zone::Battlefield);
            assert!(r.state().objects[&chosen].tapped);
            assert_eq!(
                plains
                    .iter()
                    .filter(|id| r.state().objects[id].zone == Zone::Battlefield)
                    .count(),
                index + 1
            );
        }
        // CR 608.2c: the once cap prevents a third search even with a deficit.
        assert_eq!(
            plains
                .iter()
                .filter(|id| r.state().objects[id].zone == Zone::Library)
                .count(),
            3 - expected
        );
        assert_finished(&r);
    }
}

#[test]
fn claim_jumper_decline_and_fail_to_find_terminate_the_current_iteration() {
    for decline in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        for _ in 0..3 {
            s.add_basic_land(P1, engine::types::mana::ManaColor::Green);
        }
        let jumper = s
            .add_creature_to_hand_from_oracle(P0, "Claim Jumper", 3, 3, CLAIM_JUMPER)
            .id();
        s.add_land_to_library_top(P0, "Plains")
            .with_subtypes(vec!["Plains"]);
        let mut r = s.build();
        r.cast(jumper).commit();
        pass_stack(&mut r);
        for _ in 0..2 {
            assert!(
                matches!(
                    r.state().waiting_for,
                    WaitingFor::OptionalEffectChoice { .. }
                ),
                "decline={decline}, prompt={:?}, stack={:?}",
                r.state().waiting_for,
                r.state().stack
            );
            r.act(GameAction::DecideOptionalEffect { accept: !decline })
                .unwrap();
            if !decline {
                assert!(matches!(
                    r.state().waiting_for,
                    WaitingFor::SearchChoice { .. }
                ));
                r.act(GameAction::SelectCards { cards: vec![] }).unwrap();
            }
        }
        assert_finished(&r);
    }
}

#[test]
fn two_die_branches_finish_each_discard_draw_before_outer_aggregate_reader() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_cards_in_hand(P0, &["First discard", "Second discard"]);
    s.with_library_top(P0, &["A", "B", "C", "D", "E"]);
    let branch = def(Effect::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
        selection: engine::types::ability::CardSelectionMode::Chosen,
        unless_filter: None,
        filter: None,
    })
    .sub_ability(def(Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    }));
    let root = roll(2, Some(branch)).sub_ability(def(Effect::Draw {
        count: QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
        target: TargetFilter::Controller,
    }));
    let spell = s
        .add_spell_to_hand(P0, "Synthetic die instruction", true)
        .with_ability_definition(root)
        .id();
    let mut r = s.build();
    r.cast(spell).commit();
    let mut events = pass_stack(&mut r);
    for completed in 0..2 {
        let WaitingFor::DiscardChoice {
            player,
            cards,
            count,
            ..
        } = &r.state().waiting_for
        else {
            panic!("{:?}", r.state().waiting_for)
        };
        assert_eq!((*player, *count), (P0, 1));
        assert_eq!(
            cards.len(),
            2,
            "the next branch waits for the previous draw"
        );
        let chosen = cards[0];
        if completed == 1 {
            restore(&mut r);
        }
        events.extend(
            r.act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .unwrap()
            .events,
        );
        assert_eq!(r.state().objects[&chosen].zone, Zone::Graveyard);
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::DieRolled {
                    result: Some(1),
                    ..
                }
            ))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: engine::types::ability::EffectKind::RollDie,
                    ..
                }
            ))
            .count(),
        1
    );
    // CR 706.4: two branch draws plus the two-survivor aggregate draw.
    assert_eq!(r.state().players[0].hand.len(), 4);
    assert_eq!(r.state().library_of(P0).len(), 1);
    assert_finished(&r);
}

#[test]
fn no_table_dice_publish_survivor_aggregate_before_parent_reader() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["A", "B", "C"]);
    let root = roll(2, None).sub_ability(def(Effect::Draw {
        count: QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        },
        target: TargetFilter::Controller,
    }));
    let spell = s
        .add_spell_to_hand(P0, "Synthetic aggregate", true)
        .with_ability_definition(root)
        .id();
    let mut r = s.build();
    let outcome = r.cast(spell).resolve();
    assert_eq!(
        outcome
            .events()
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::DieRolled {
                    result: Some(1),
                    ..
                }
            ))
            .count(),
        2
    );
    outcome.assert_hand_drawn(P0, 2);
    assert_finished(&r);
}

#[test]
fn queued_clash_branch_settles_before_next_die_and_parent_tail() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    // Both empty libraries create queued work without a placement prompt.
    let branch = def(Effect::Clash).sub_ability(gain(2));
    let root = roll(2, Some(branch)).sub_ability(gain(7));
    let spell = s
        .add_spell_to_hand(P0, "Synthetic queued die branch", true)
        .with_ability_definition(root)
        .id();
    let mut r = s.build();
    let outcome = r.cast(spell).resolve();
    let events = outcome.events();
    let clash_positions: Vec<_> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            matches!(
                e,
                GameEvent::Clash {
                    result: ClashResult::Tied,
                    ..
                }
            )
            .then_some(i)
        })
        .collect();
    assert_eq!(clash_positions.len(), 2);
    assert!(events[clash_positions[0] + 1..clash_positions[1]]
        .iter()
        .any(|e| matches!(e, GameEvent::LifeChanged { amount: 2, .. })));
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged {
                    player_id: P0,
                    amount,
                    ..
                } => Some(*amount),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![2, 2, 7]
    );
    outcome.assert_life_delta(P0, 11);
    assert_finished(&r);
}

#[test]
fn queued_optional_decline_child_finishes_before_repeated_iteration() {
    queued_optional_repeat_order(None);
}

#[test]
fn queued_optional_decline_child_finishes_before_outermost_scoped_repeat() {
    queued_optional_repeat_order(Some(PlayerFilter::Controller));
}

fn queued_optional_repeat_order(scope: Option<PlayerFilter>) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let impossible = def(Effect::CastFromZone {
        target: TargetFilter::ParentTarget,
        without_paying_mana_cost: true,
        mode: CardPlayMode::Cast,
        cast_transformed: false,
        alt_ability_cost: None,
        constraint: None,
        duration: None,
        driver: CastFromZoneDriver::DuringResolution,
        mana_spend_permission: None,
        additional_cost: None,
        cast_cost_modifier: None,
    })
    .optional()
    .sub_ability({
        let mut clash = def(Effect::Clash).sub_ability(def(Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal {
                    player: PlayerScope::Controller,
                },
            },
            player: TargetFilter::Controller,
        }));
        clash.sub_link = SubAbilityLink::SequentialSibling;
        clash
    });
    // The empty producer supplies real missing-referent provenance. Merely
    // omitting a cast target still permits an optional offer in this class.
    let empty_producer = def(Effect::ExileTop {
        player: TargetFilter::Controller,
        count: QuantityExpr::Fixed { value: 1 },
        position: LibraryPosition::Top,
        face_down: false,
        actor: LibraryInstructionActor::Controller,
    })
    .sub_ability(impossible);
    let mut root = gain(1).sub_ability(empty_producer);
    root.repeat_for = Some(QuantityExpr::Fixed { value: 3 });
    root.player_scope = scope;
    let spell = s
        .add_spell_to_hand(P0, "Synthetic optional repeat", true)
        .with_ability_definition(root)
        .id();
    let mut r = s.build();
    assert!(r.state().library_of(P0).is_empty() && r.state().library_of(P1).is_empty());
    r.cast(spell).commit();
    let events = pass_stack(&mut r);
    assert!(
        !matches!(
            r.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "the missing referent must auto-decline without an optional prompt"
    );
    assert_finished(&r);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::ExileTop,
                    ..
                }
            ))
            .count(),
        3,
        "each empty producer is attempted without an optional prompt"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::Clash { .. }))
            .count(),
        3
    );
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged {
                    player_id: P0,
                    amount,
                    ..
                } => Some(*amount),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![1, 21, 1, 43, 1, 87]
    );
    assert_eq!(life(&r, P0) - 20, 154);
}

#[test]
fn clash_nonactive_controller_defers_both_bottoms_and_preserves_result_on_restore() {
    for won in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let brawler = s
            .add_creature_to_hand_from_oracle(P1, "Oaken Brawler", 3, 3, OAKEN_BRAWLER)
            .with_keyword(Keyword::Flash)
            .id();
        let p0_bottom = s.add_land_to_library_top(P0, "P0 bottom").id();
        let p0_top = s
            .add_spell_to_library_top(P0, "P0 top", true)
            .with_mana_cost(ManaCost::generic(if won { 0 } else { 6 }))
            .id();
        let p1_bottom = s.add_land_to_library_top(P1, "P1 bottom").id();
        let p1_top = s
            .add_spell_to_library_top(P1, "High mana value", true)
            .with_mana_cost(ManaCost::generic(5))
            .id();
        let mut r = s.build();
        r.act(GameAction::PassPriority).unwrap();
        r.cast(brawler).commit();
        let events = pass_stack(&mut r);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::Clash {
                controller: P1,
                opponent: P0,
                result,
                ..
            } if *result == if won { ClashResult::Won } else { ClashResult::Lost }
        )));
        assert!(
            matches!(r.state().waiting_for, WaitingFor::ClashCardPlacement { player: P0, card, .. } if card == p0_top)
        );
        r.act(GameAction::ChooseTopOrBottom { top: false }).unwrap();
        assert_eq!(
            r.state().library_of(P0).iter().copied().collect::<Vec<_>>(),
            [p0_top, p0_bottom]
        );
        assert_eq!(
            r.state().library_of(P1).iter().copied().collect::<Vec<_>>(),
            [p1_top, p1_bottom]
        );
        assert!(
            matches!(&r.state().waiting_for, WaitingFor::ClashCardPlacement { player: P1, card, bottom_cards, .. } if *card == p1_top && bottom_cards == &[p0_top])
        );
        restore(&mut r);
        assert!(engine::ai_support::legal_actions(r.state())
            .iter()
            .any(|action| matches!(action, GameAction::ChooseTopOrBottom { top: false })));
        r.act(GameAction::ChooseTopOrBottom { top: false }).unwrap();
        assert_eq!(
            r.state().library_of(P0).iter().copied().collect::<Vec<_>>(),
            [p0_bottom, p0_top]
        );
        assert_eq!(
            r.state().library_of(P1).iter().copied().collect::<Vec<_>>(),
            [p1_bottom, p1_top]
        );
        // CR 701.30d: the revealed values, not the new tops, determine the win rider.
        assert_eq!(
            r.state().objects[&brawler]
                .counters
                .get(&CounterType::Plus1Plus1),
            if won { Some(&1) } else { None }
        );
        assert_finished(&r);
    }
}

#[test]
fn multiplayer_clash_restores_opponent_choice_and_ignores_nonparticipant() {
    let mut s = GameScenario::new_n_player(3, 42);
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand(P0, "Synthetic multiplayer clash", true)
        .with_ability_definition(def(Effect::Clash).sub_ability(gain(3)))
        .id();
    s.with_library_top(P0, &["Mine"]);
    s.with_library_top(P1, &["Not chosen"]);
    s.with_library_top(P2, &["Chosen"]);
    let mut r = s.build();
    let third = r.state().library_of(P1).clone();
    r.cast(spell).commit();
    pass_stack(&mut r);
    assert!(
        matches!(&r.state().waiting_for, WaitingFor::ClashChooseOpponent { player: P0, candidates, .. } if candidates == &[P1, P2])
    );
    restore(&mut r);
    let result = r
        .act(GameAction::ChooseClashOpponent { opponent: P2 })
        .unwrap();
    assert!(result.events.iter().any(|e| matches!(
        e,
        GameEvent::Clash {
            controller: P0,
            opponent: P2,
            ..
        }
    )));
    for player in [P0, P2] {
        assert!(
            matches!(r.state().waiting_for, WaitingFor::ClashCardPlacement { player: actor, .. } if actor == player)
        );
        r.act(GameAction::ChooseTopOrBottom { top: false }).unwrap();
    }
    assert_eq!(r.state().library_of(P1), &third);
    assert_eq!(life(&r, P0), 23);
    assert_finished(&r);
}

#[test]
fn historical_v5_clash_finishes_only_its_remaining_placement() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let spell = s
        .add_spell_to_hand(P0, "Synthetic old Clash", true)
        .with_ability_definition(def(Effect::Clash).sub_ability(gain(3)))
        .id();
    s.with_library_top(P0, &["A", "B"]);
    s.with_library_top(P1, &["C", "D"]);
    let mut r = s.build();
    r.cast(spell).commit();
    pass_stack(&mut r);
    let old_top = *r.state().library_of(P0).front().unwrap();
    let old_bottom = *r.state().library_of(P0).back().unwrap();
    r.act(GameAction::ChooseTopOrBottom { top: false }).unwrap();
    let mut historical_events = Vec::new();
    engine::game::zones::move_to_library_position(
        r.state_mut(),
        old_top,
        false,
        &mut historical_events,
    );
    assert_eq!(
        r.state().library_of(P0).iter().copied().collect::<Vec<_>>(),
        [old_bottom, old_top]
    );
    let mut json =
        serde_json::to_value(ResolutionStateWire::from_game_state(r.state().clone())).unwrap();
    json["resolution_state_version"] = 5.into();
    json["waiting_for"]["data"]
        .as_object_mut()
        .unwrap()
        .remove("bottom_cards");
    let wire: ResolutionStateWire =
        serde_json::from_value(json).expect("historical placement defaults absent choices");
    r = GameRunner::from_state(wire.into_game_state());
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ClashCardPlacement { player: P1, .. }
    ));
    r.act(GameAction::ChooseTopOrBottom { top: false }).unwrap();
    assert_eq!(
        r.state().library_of(P0).iter().copied().collect::<Vec<_>>(),
        [old_bottom, old_top],
        "the v5 choice already happened and is never replayed"
    );
    assert_eq!(life(&r, P0), 23);
    assert_finished(&r);
}

#[test]
fn die_keep_restores_the_selected_ignore_set_before_interactive_branches() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_enchantment_from_oracle(P0, "Barbarian Class", "If you would roll one or more dice, instead roll that many dice plus one and ignore the lowest roll.");
    s.with_library_top(P0, &["A", "B", "C", "D"]);
    s.with_cards_in_hand(P0, &["One", "Two"]);
    let branch = def(Effect::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
        selection: engine::types::ability::CardSelectionMode::Chosen,
        unless_filter: None,
        filter: None,
    })
    .sub_ability(def(Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    }));
    let spell = s
        .add_spell_to_hand(P0, "Synthetic keep resume", true)
        .with_ability_definition(roll(2, Some(branch)).sub_ability(gain(7)))
        .id();
    let mut r = s.build();
    r.cast(spell).commit();
    let mut events = pass_stack(&mut r);
    assert!(
        matches!(&r.state().waiting_for, WaitingFor::DieKeepChoice { player: P0, results, ignorable_indices, ignore_count: 1 } if results == &[1,1,1] && ignorable_indices == &[0,1,2])
    );
    restore(&mut r);
    assert!(engine::ai_support::legal_actions(r.state()).iter().any(|action| matches!(action, GameAction::SelectDieRolls { ignore_indices } if ignore_indices == &[1])));
    let saved = r.state().waiting_for.clone();
    assert!(r
        .act(GameAction::SelectDieRolls {
            ignore_indices: vec![3]
        })
        .is_err());
    assert_eq!(r.state().waiting_for, saved);
    events.extend(
        r.act(GameAction::SelectDieRolls {
            ignore_indices: vec![1],
        })
        .unwrap()
        .events,
    );
    for iteration in 0..2 {
        let WaitingFor::DiscardChoice { cards, .. } = &r.state().waiting_for else {
            panic!(
                "branch={iteration}, prompt={:?}, frames={:?}, events={events:?}",
                r.state().waiting_for,
                r.state().resolution_stack
            )
        };
        let chosen = cards[0];
        restore(&mut r);
        events.extend(
            r.act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .unwrap()
            .events,
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::DieRollIgnored { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::DieRolled { .. }))
            .count(),
        2
    );
    assert_eq!(life(&r, P0), 27);
    assert_finished(&r);
}

#[test]
fn no_op_die_shapes_still_run_the_parent_tail_once() {
    for (count, band) in [(0, None), (1, Some((2, 3))), (1, None)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let mut root = roll(count, None);
        if let (Effect::RollDie { results, .. }, Some((min, max))) = (&mut *root.effect, band) {
            results.push(DieResultBranch {
                min,
                max,
                effect: Box::new(gain(20)),
            });
        }
        let spell = s
            .add_spell_to_hand(P0, "Synthetic no-op roll", true)
            .with_ability_definition(root.sub_ability(gain(3)))
            .id();
        let mut r = s.build();
        let outcome = r.cast(spell).resolve();
        assert!(outcome.events().iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: engine::types::ability::EffectKind::RollDie,
                ..
            }
        )));
        outcome.assert_life_delta(P0, 3);
        assert_finished(&r);
    }
}

#[test]
fn clash_win_loss_tie_empty_and_single_library_run_rider_and_tail_once() {
    for (mine, theirs, expected) in [
        (Some(5), Some(1), 25),
        (Some(1), Some(5), 23),
        (Some(1), Some(1), 23),
        (None, None, 23),
        (Some(1), None, 25),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let mut tail = gain(3);
        tail.sub_link = SubAbilityLink::SequentialSibling;
        let mut rider = gain(2).sub_ability(tail);
        rider.condition = Some(engine::types::ability::AbilityCondition::effect_performed());
        let spell = s
            .add_spell_to_hand(P0, "Synthetic Clash outcomes", true)
            .with_ability_definition(def(Effect::Clash).sub_ability(rider))
            .id();
        if let Some(mv) = mine {
            s.add_spell_to_library_top(P0, "Mine", true)
                .with_mana_cost(ManaCost::generic(mv));
        }
        if let Some(mv) = theirs {
            s.add_spell_to_library_top(P1, "Theirs", true)
                .with_mana_cost(ManaCost::generic(mv));
        }
        let mut r = s.build();
        r.cast(spell).commit();
        let events = pass_stack(&mut r);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::Clash { .. }))
                .count(),
            1
        );
        for _ in 0..usize::from(mine.is_some()) + usize::from(theirs.is_some()) {
            assert!(matches!(
                r.state().waiting_for,
                WaitingFor::ClashCardPlacement { .. }
            ));
            r.act(GameAction::ChooseTopOrBottom { top: true }).unwrap();
        }
        assert_eq!(life(&r, P0), expected);
        assert_finished(&r);
    }
}

#[test]
fn lethal_damage_waits_for_clash_choices_and_the_healing_tail_before_sbas() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_library_top(P0, &["A", "B"]);
    s.with_library_top(P1, &["C", "D"]);
    let root = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::SelfRef,
            damage_source: None,
            excess: None,
        },
    )
    .sub_ability(def(Effect::Clash).sub_ability(def(Effect::RemoveAllDamage {
        target: TargetFilter::SelfRef,
    })));
    let creature = s
        .add_creature(P0, "Synthetic resilient source", 2, 2)
        .with_ability_definition(root)
        .id();
    let mut r = s.build();
    r.activate(creature, 0).resolve();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ClashCardPlacement { .. }
    ));
    assert_eq!(r.state().objects[&creature].damage_marked, 2);
    assert_eq!(r.state().objects[&creature].zone, Zone::Battlefield);
    for _ in 0..2 {
        r.act(GameAction::ChooseTopOrBottom { top: false }).unwrap();
    }
    // CR 704.4 + CR 117.2e: choices do not expose priority or run SBAs midway.
    assert_eq!(r.state().objects[&creature].zone, Zone::Battlefield);
    assert_eq!(r.state().objects[&creature].damage_marked, 0);
    assert_finished(&r);
}

#[test]
fn nested_die_child_stack_finishes_before_each_outer_survivor() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_cards_in_hand(P0, &["One", "Two"]);
    s.with_library_top(P0, &["A", "B", "C"]);
    let discard = def(Effect::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
        selection: engine::types::ability::CardSelectionMode::Chosen,
        unless_filter: None,
        filter: None,
    })
    .sub_ability(def(Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    }));
    let inner = roll(1, Some(discard)).sub_ability(gain(1));
    let spell = s
        .add_spell_to_hand(P0, "Synthetic nested dice", true)
        .with_ability_definition(roll(2, Some(inner)).sub_ability(gain(7)))
        .id();
    let mut r = s.build();
    r.cast(spell).commit();
    let mut events = pass_stack(&mut r);
    for completed in 0..2 {
        let WaitingFor::DiscardChoice { cards, .. } = &r.state().waiting_for else {
            panic!("{:?}", r.state().waiting_for)
        };
        assert_eq!(
            life(&r, P0),
            20 + completed,
            "inner tail finishes before the next outer survivor"
        );
        assert_eq!(cards.len(), 2);
        assert!(
            r.state().resolution_stack.len() >= 4,
            "nested child owns more than the top frame"
        );
        let chosen = cards[0];
        restore(&mut r);
        events.extend(
            r.act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .unwrap()
            .events,
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::DieRolled { .. }))
            .count(),
        4
    );
    assert_eq!(life(&r, P0), 29);
    assert_eq!(r.state().players[0].hand.len(), 2);
    assert_finished(&r);
}

#[test]
fn domain_result_survives_typed_progress_for_its_immediate_consumer_and_tail() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let completion = def(Effect::CompletePlayerAction {
        parent_kind: engine::types::ability::EffectKind::Forage,
        action: engine::types::events::PlayerActionKind::Forage,
        required_result: engine::types::ability::EffectResolutionResult {
            cause: engine::types::ability::ThisWayCause::Sacrificed,
            count: 1,
        },
    });
    let root = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 0,
        },
    )
    .sub_ability(completion.sub_ability(gain(3)));
    let victim = s
        .add_creature(P0, "Sacrificed member", 2, 2)
        .with_ability_definition(root)
        .id();
    let mut r = s.build();
    let outcome = r.activate(victim, 0).resolve();
    assert_eq!(
        outcome
            .events()
            .iter()
            .filter(|e| matches!(
                e,
                GameEvent::PlayerPerformedAction {
                    action: engine::types::events::PlayerActionKind::Forage,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(r.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(life(&r, P0), 23);
    assert_finished(&r);
}

#[test]
fn random_target_helper_preserves_queued_child_at_root_and_inline_handoffs() {
    for inline in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let mut selected = def(Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Player,
        })
        .condition(engine::types::ability::AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Fixed { value: 1 },
            comparator: engine::types::ability::Comparator::EQ,
            rhs: QuantityExpr::Fixed { value: 1 },
        })
        .sub_ability(def(Effect::Clash).sub_ability(gain(3)));
        selected.target_selection_mode = engine::types::ability::TargetSelectionMode::Random;
        let branch = if inline {
            gain(1).sub_ability(selected)
        } else {
            selected
        };
        let spell = s
            .add_spell_to_hand(P0, "Synthetic helper handoff", true)
            .with_ability_definition(roll(2, Some(branch)).sub_ability(gain(7)))
            .id();
        let mut r = s.build();
        let before: i32 = r.state().players.iter().map(|p| p.life).sum();
        let outcome = r.cast(spell).resolve();
        let events = outcome.events();
        let dice: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| matches!(e, GameEvent::DieRolled { .. }).then_some(i))
            .collect();
        assert_eq!(dice.len(), 2);
        assert!(
            events[dice[0] + 1..dice[1]]
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { amount: 3, .. })),
            "auto-selected helper's queued tail finishes before the second die"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, GameEvent::Clash { .. }))
                .count(),
            2
        );
        let after: i32 = r.state().players.iter().map(|p| p.life).sum();
        assert_eq!(after - before, if inline { 19 } else { 17 });
        assert_finished(&r);
    }
}
