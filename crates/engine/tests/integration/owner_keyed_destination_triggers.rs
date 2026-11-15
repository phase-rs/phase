//! Real-card runtime tests for owner-keyed destination-zone triggers (CR 400.3, CR 108.2b, CR 110.1).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, Effect, FilterProp, OriginConstraint, QuantityExpr, TargetFilter, TargetRef,
    TriggerDefinition, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

// Verbatim Oracle text independently read from data/mtgjson/AtomicCards.json.
const BLOODCHIEF: &str = "At the beginning of each end step, if an opponent lost 2 or more life this turn, you may put a quest counter on this enchantment. (Damage causes loss of life.)\nWhenever a card is put into an opponent's graveyard from anywhere, if this enchantment has three or more quest counters on it, you may have that player lose 2 life. If you do, you gain 2 life.";
const MURDER: &str = "Destroy target creature.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";
const DISA: &str = "Whenever a Lhurgoyf permanent card is put into your graveyard from anywhere other than the battlefield, put it onto the battlefield.\nWhenever one or more creatures you control deal combat damage to a player, create a Tarmogoyf token.";
// Synthetic: C8 found no printed third-person union. Oglor supplies the union;
// Patron of the Nezumi supplies "that player" as the graveyard owner's anaphor.
const UNION_OWNER_OBSERVER: &str = "Whenever a creature card is put into a player's graveyard from the battlefield or a library, that player loses 1 life.";

fn trigger_instances(state: &GameState) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .count()
        + state
            .pending_trigger_order
            .as_ref()
            .map(|order| {
                order
                    .groups
                    .iter()
                    .map(|group| group.triggers.len())
                    .sum::<usize>()
            })
            .unwrap_or(0)
        + state.deferred_triggers.len()
}

fn give_priority(runner: &mut GameRunner, caster: PlayerId) {
    for _ in 0..runner.state().players.len() {
        if runner.state().priority_player == caster {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("give the spell's owner priority");
    }
    assert_eq!(runner.state().priority_player, caster);
}

fn cast_until_resolved(runner: &mut GameRunner, spell: ObjectId, targets: &[ObjectId]) {
    give_priority(runner, runner.state().objects[&spell].owner);
    runner.cast(spell).target_objects(targets).commit();
    finish_spell(runner, spell);
}

fn cast_player_until_resolved(runner: &mut GameRunner, spell: ObjectId, player: PlayerId) {
    give_priority(runner, runner.state().objects[&spell].owner);
    runner.cast(spell).target_player(player).commit();
    finish_spell(runner, spell);
}

fn finish_spell(runner: &mut GameRunner, spell: ObjectId) {
    for _ in 0..16 {
        if !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell)
        {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("spell did not resolve");
}

fn drain_to_optional_or_priority(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).rev().collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order simultaneous triggers");
            }
            WaitingFor::OptionalEffectChoice { .. } => return,
            other => panic!("unexpected resolution prompt: {other:?}"),
        }
    }
    panic!("resolution did not settle");
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

fn assert_settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty());
    assert_eq!(trigger_instances(runner.state()), 0);
    assert!(runner.state().pending_trigger.is_none());
    assert!(runner.state().resolution_stack.is_empty());
    assert!(runner.state().resolving_stack_entry.is_none());
    assert!(runner.state().pending_resolution_completion.is_none());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
}

/// V9(c/d): the live-source positive guards the removed-source reference reading.
#[test]
fn v9_bloodchief_live_and_removed_source_use_three_quest_counters() {
    for remove_source in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let ascension = s
            .add_enchantment_from_oracle(P0, "Bloodchief Ascension", BLOODCHIEF)
            .id();
        s.with_counter(ascension, CounterType::Generic("quest".into()), 3);
        let victim = s.add_creature(P1, "Reference creature", 2, 2).id();
        let murder = s
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let disenchant = s
            .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        let before = (life(&runner, P0), life(&runner, P1));
        cast_until_resolved(&mut runner, murder, &[victim]);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        assert_eq!(
            trigger_instances(runner.state()),
            1,
            "one Bloodchief trigger"
        );
        if remove_source {
            cast_until_resolved(&mut runner, disenchant, &[ascension]);
            assert_eq!(runner.state().objects[&ascension].zone, Zone::Graveyard);
            assert_eq!(
                trigger_instances(runner.state()),
                1,
                "source removal adds no opponent-graveyard event"
            );
        }
        drain_to_optional_or_priority(&mut runner);
        // CR 603.4: the condition is checked again; CR 608.2h: a departed source uses LKI.
        // CR 113.7a: destroying the source does not remove its queued ability.
        let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
            panic!(
                "V9 remove_source={remove_source}: expected optional drain, got {:?}; life={:?}",
                runner.state().waiting_for,
                (life(&runner, P0), life(&runner, P1))
            );
        };
        assert_eq!(player, P0);
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accept drain");
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(
            (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
            (2, -2),
            "remove_source={remove_source}"
        );
    }
}

/// V1s: head publication only; the independent pile-body defect is out of scope.
#[test]
fn v1s_triumph_owned_and_stolen_head_publication() {
    const TRIUMPH: &str = "Lifelink\nPraesidium Protectiva — When this creature is put into your graveyard from the battlefield, exile it and the top six cards of your library in a face-down pile. If you do, shuffle that pile and put it back on top of your library.\nMiracle {1}{W} (You may cast this card for its miracle cost when you draw it if it's the first card you drew this turn.)";
    for controller in [P0, P1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature(P0, "Triumph of Saint Katherine", 5, 5)
            .controlled_by(controller)
            .from_oracle_text_with_keywords(&["Lifelink", "Miracle"], TRIUMPH)
            .id();
        s.with_library_top(P0, &["A", "B", "C", "D", "E", "F", "G", "H"]);
        let murder = s
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        assert_eq!(
            (
                runner.state().objects[&source].owner,
                runner.state().objects[&source].controller
            ),
            (P0, controller)
        );
        cast_until_resolved(&mut runner, murder, &[source]);
        assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
        assert_eq!(
            trigger_instances(runner.state()),
            usize::from(controller == P0),
            "Triumph controller={controller:?}"
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        if controller == P1 {
            assert_settled(&runner);
        }
    }
}

/// V4k reference: the actual Brinefarer hand ability must conjure on an own draw.
#[test]
fn v4k_brinefarer_own_kithkin_draw_conjures_one_duplicate() {
    const BRINEFARER: &str = "Whenever a Kithkin card is put into your hand from your library, conjure a duplicate of that card into your hand.\nWhenever you attack, Kithkin creature cards in your hand perpetually get +1/+1.";
    for (drawer, kithkin) in [(P0, true), (P0, false), (P1, true)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P0, "Kithkin Brinefarer", 3, 3, BRINEFARER);
        let original = s.add_card_to_library_top(drawer, "Textless draw card");
        let draw = s
            .add_spell_to_hand_from_oracle(drawer, "Reach Through Mists", true, "Draw a card.")
            .id();
        let mut runner = s.build();
        if kithkin {
            let obj = runner.state_mut().objects.get_mut(&original).unwrap();
            obj.card_types.subtypes.push("Kithkin".into());
            obj.base_card_types = obj.card_types.clone();
        }
        assert_settled(&runner);
        cast_until_resolved(&mut runner, draw, &[]);
        assert_eq!(
            (
                runner.state().objects[&original].zone,
                runner.state().objects[&original].owner
            ),
            (Zone::Hand, drawer)
        );
        let expected = usize::from(drawer == P0 && kithkin);
        assert_eq!(
            trigger_instances(runner.state()),
            expected,
            "drawer={drawer:?}, Kithkin={kithkin}"
        );
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        let hand = &runner
            .state()
            .players
            .iter()
            .find(|p| p.id == drawer)
            .unwrap()
            .hand;
        assert!(hand.contains(&original));
        assert_eq!(
            hand.iter()
                .filter(|id| runner.state().objects[id].name == "Textless draw card")
                .count(),
            1 + expected
        );
        if drawer == P1 {
            assert!(runner
                .state()
                .players
                .iter()
                .find(|p| p.id == P0)
                .unwrap()
                .hand
                .is_empty());
        }
    }
}

/// V5: initial card qualification and sacrifice; the known residual count is not pinned.
#[test]
fn v5_energy_field_nontoken_positive_sacrifices_and_settles() {
    const ENERGY_FIELD: &str = "Prevent all damage that would be dealt to you by sources you don't control.\nWhen a card is put into your graveyard from anywhere, sacrifice this enchantment.";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let field = s
        .add_spell_to_hand_from_oracle(P0, "Energy Field", false, ENERGY_FIELD)
        .as_enchantment()
        .id();
    let victim = s.add_creature(P0, "Reference creature", 2, 2).id();
    let murder = s
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();
    runner.cast(field).resolve();
    assert_settled(&runner);
    assert_eq!(runner.state().objects[&field].zone, Zone::Battlefield);
    cast_until_resolved(&mut runner, murder, &[victim]);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(
        trigger_instances(runner.state()),
        1,
        "initial nontoken victim trigger"
    );
    for _ in 0..16 {
        if runner.state().objects[&field].zone == Zone::Graveyard {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("resolve Field's sacrifice trigger");
    }
    assert_eq!(runner.state().objects[&field].zone, Zone::Graveyard);
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
}

/// V5b positive: the typed-batched head must reach all three printed instructions.
#[test]
fn v5b_robot_domination_nontoken_death_draws_loses_life_and_adds_plan_counter() {
    const ROBOT: &str = "Whenever one or more creature cards are put into your graveyard from anywhere, you draw a card, lose 1 life, and put a plan counter on this enchantment.\nWhen the third plan counter is put on this enchantment, sacrifice it and create three 2/2 colorless Robot Villain artifact creature tokens.";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let robot = s
        .add_enchantment_from_oracle(P0, "Robot Domination", ROBOT)
        .id();
    let victim = s.add_creature(P0, "Reference creature", 2, 2).id();
    let drawn = s.add_card_to_library_top(P0, "Drawn reference card");
    let murder = s
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();
    assert_settled(&runner);
    let before = life(&runner, P0);
    assert!(runner.state().objects[&robot].counters.is_empty());
    cast_until_resolved(&mut runner, murder, &[victim]);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(
        trigger_instances(runner.state()),
        1,
        "one Robot Domination trigger"
    );
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    // CR 603.2c: the one qualifying event triggers one complete instruction chain.
    assert_eq!(life(&runner, P0) - before, -1);
    assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&robot]
            .counters
            .get(&CounterType::Generic("plan".into()))
            .copied()
            .unwrap_or(0),
        1
    );
}

/// V5v positive: "that many" is one for a single printed creature card.
#[test]
fn v5v_voracious_brood_nontoken_death_adds_one_counter() {
    const BROOD: &str = "This creature enters with a +1/+1 counter on it for each creature card in your graveyard.\nWhenever one or more creature cards are put into your graveyard from anywhere, put that many +1/+1 counters on this creature.";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let brood = s
        .add_creature_from_oracle(P0, "Voracious Brood", 1, 1, BROOD)
        .id();
    s.with_counter(brood, CounterType::Plus1Plus1, 1);
    let victim = s.add_creature(P0, "Reference creature", 2, 2).id();
    let murder = s
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();
    assert_settled(&runner);
    cast_until_resolved(&mut runner, murder, &[victim]);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
    assert_eq!(
        trigger_instances(runner.state()),
        1,
        "one Voracious Brood trigger"
    );
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    // CR 603.2c: one matching subject supplies one additional counter.
    assert_eq!(
        runner.state().objects[&brood].counters[&CounterType::Plus1Plus1],
        2
    );
}

const ZODIAC: &str = "When this creature is put into your graveyard from the battlefield, you may return it to your hand.";
const BROOD_COCKROACHES: &str = "When this creature is put into your graveyard from the battlefield, at the beginning of the next end step, you lose 1 life and return this card to your hand.";
const SPHINX: &str = "Flying\nWhen this creature is put into your graveyard from the battlefield, put it into your library third from the top.\nCascade (When you cast this spell, exile cards from the top of your library until you exile a nonland card that costs less. You may cast it without paying its mana cost. Put the exiled cards on the bottom in a random order.)";
const GUAN_YU: &str = "Horsemanship (This creature can't be blocked except by creatures with horsemanship.)\nWhen Guan Yu is put into your graveyard from the battlefield, you may shuffle Guan Yu into your library.";

fn accept_optional(runner: &mut GameRunner, expected_player: PlayerId) {
    let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
        panic!("expected optional effect: {:?}", runner.state().waiting_for);
    };
    assert_eq!(player, expected_player);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional effect");
    drain_to_optional_or_priority(runner);
}

/// V1 references: the owned positive bodies must complete, not just publish a trigger.
#[test]
fn v1_owned_self_bodies_complete() {
    for (name, text, keywords, power, toughness) in [
        ("Zodiac Dragon", ZODIAC, &[][..], 8, 8),
        ("Brood of Cockroaches", BROOD_COCKROACHES, &[][..], 1, 1),
        ("Enigma Sphinx", SPHINX, &["Flying", "Cascade"][..], 5, 4),
        (
            "Guan Yu, Sainted Warrior",
            GUAN_YU,
            &["Horsemanship"][..],
            3,
            5,
        ),
    ] {
        for controller in [P0, P1] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let source = s
                .add_creature(P0, name, power, toughness)
                .controlled_by(controller)
                .from_oracle_text_with_keywords(keywords, text)
                .id();
            let other = s
                .add_creature(
                    if name == "Guan Yu, Sainted Warrior" {
                        P1
                    } else {
                        P0
                    },
                    name,
                    power,
                    toughness,
                )
                .from_oracle_text_with_keywords(keywords, text)
                .id();
            s.with_library_top(P0, &["A", "B", "C", "D"]);
            let murder = s
                .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
                .id();
            let mut runner = s.build();
            assert_settled(&runner);
            assert_eq!(
                (
                    runner.state().objects[&source].owner,
                    runner.state().objects[&source].controller
                ),
                (P0, controller)
            );
            cast_until_resolved(&mut runner, murder, &[source]);
            assert_eq!(
                trigger_instances(runner.state()),
                usize::from(controller == P0),
                "{name}, controller={controller:?}"
            );
            assert_eq!(runner.state().objects[&other].zone, Zone::Battlefield);
            if controller == P1 {
                assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
                assert_settled(&runner);
                continue;
            }
            drain_to_optional_or_priority(&mut runner);
            if name == "Zodiac Dragon" || name == "Guan Yu, Sainted Warrior" {
                accept_optional(&mut runner, P0);
            }
            if name == "Brood of Cockroaches" {
                // CR 603.7a: the first trigger creates a delayed next-end-step ability.
                assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
                assert_eq!(runner.state().delayed_triggers.len(), 1);
                assert_settled(&runner);
                let before = life(&runner, P0);
                runner.advance_to_end_step();
                assert_eq!(runner.state().phase, Phase::DeclareAttackers);
                runner.declare_attackers(&[]).expect("declare no attackers");
                runner.advance_to_end_step();
                assert_eq!(runner.state().phase, Phase::End);
                drain_to_optional_or_priority(&mut runner);
                assert_eq!(life(&runner, P0) - before, -1);
            }
            assert_settled(&runner);
            let expected_zone = if name == "Enigma Sphinx" || name == "Guan Yu, Sainted Warrior" {
                Zone::Library
            } else {
                Zone::Hand
            };
            assert_eq!(
                runner.state().objects[&source].zone,
                expected_zone,
                "{name} completed its return"
            );
            if name == "Enigma Sphinx" {
                assert_eq!(
                    runner
                        .state()
                        .players
                        .iter()
                        .find(|p| p.id == P0)
                        .unwrap()
                        .library
                        .iter()
                        .nth(2),
                    Some(&source)
                );
            }
        }
    }
}

/// V2: a copied self ability is not a card-word observer.
#[test]
fn v2_copied_zodiac_token_triggers_but_cannot_return() {
    const COPY: &str = "Create a token that's a copy of target creature you control.\nFlashback {5}{U}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let original = s
        .add_creature_from_oracle(P0, "Zodiac Dragon", 8, 8, ZODIAC)
        .id();
    let copy = s
        .add_spell_to_hand_from_oracle(P0, "Cackling Counterpart", true, COPY)
        .id();
    let murder = s
        .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
        .id();
    let mut runner = s.build();
    runner.cast(copy).target_object(original).resolve();
    assert_settled(&runner);
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.name == "Zodiac Dragon")
        .map(|o| o.id)
        .collect();
    assert_eq!(tokens.len(), 1);
    let token = tokens[0];
    assert_eq!(
        (
            runner.state().objects[&token].owner,
            runner.state().objects[&token].controller
        ),
        (P0, P0)
    );
    cast_until_resolved(&mut runner, murder, &[token]);
    // CR 111.7: the token's matching ability triggers before it ceases to exist.
    assert_eq!(trigger_instances(runner.state()), 1);
    assert_eq!(runner.state().stack.last().unwrap().source_id, token);
    assert!(!runner.state().objects.contains_key(&token));
    drain_to_optional_or_priority(&mut runner);
    accept_optional(&mut runner, P0);
    assert_settled(&runner);
    // CR 111.8: a departed token cannot move again, even to its owner's hand.
    assert!(!runner.state().objects.contains_key(&token));
    assert_eq!(runner.state().objects[&original].zone, Zone::Battlefield);
    assert!(runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .iter()
        .all(|id| runner.state().objects[id].name != "Zodiac Dragon"));
}

const DUTIFUL: &str = "Whenever one or more cards are put into a library from anywhere, put a +1/+1 counter on this creature.\n{3}: Put target card from a graveyard on the bottom of its owner's library.";
const WAN_SHI_TONG: &str = "Flying\nWhen Wan Shi Tong enters, target nonland permanent's owner puts it into their library second from the top or on the bottom.\nWhenever one or more cards are put into a library from anywhere, create two 1/1 colorless Spirit creature tokens with \"This token can't block or be blocked by non-Spirit creatures.\"";

/// V6 reference: the two cards move in one batch; the caster, not the target, draws.
#[test]
fn v6_clear_the_mind_two_card_batch_resolves_each_observer_once() {
    for wan in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let observer = if wan {
            s.add_creature(P0, "Wan Shi Tong, All-Knowing", 4, 4)
                .from_oracle_text_with_keywords(&["Flying"], WAN_SHI_TONG)
                .id()
        } else {
            s.add_creature_from_oracle(P0, "Dutiful Knowledge Seeker", 2, 2, DUTIFUL)
                .id()
        };
        s.with_graveyard(P0, &["Grave A", "Grave B"]);
        s.with_library_top(P0, &["Existing"]);
        let drawn = s.add_card_to_library_top(P1, "Drawn");
        let spell = s
            .add_spell_to_hand_from_oracle(
                P1,
                "Clear the Mind",
                false,
                "Target player shuffles their graveyard into their library.\nDraw a card.",
            )
            .id();
        let mut runner = s.build();
        runner.state_mut().active_player = P1;
        runner.state_mut().priority_player = P1;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
        let moved: Vec<_> = runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .graveyard
            .iter()
            .copied()
            .collect();
        assert_eq!(moved.len(), 2);
        assert_settled(&runner);
        cast_player_until_resolved(&mut runner, spell, P0);
        for id in moved {
            assert_eq!(runner.state().objects[&id].zone, Zone::Library);
        }
        assert!(runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .graveyard
            .is_empty());
        assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
        // CR 603.2c: one or more cards in one batch produce one observer trigger.
        assert_eq!(trigger_instances(runner.state()), 1, "wan={wan}");
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        if wan {
            assert_eq!(
                runner
                    .state()
                    .objects
                    .values()
                    .filter(|o| o.is_token && o.name == "Spirit" && o.controller == P0)
                    .count(),
                2
            );
        } else {
            assert_eq!(
                runner.state().objects[&observer]
                    .counters
                    .get(&CounterType::Plus1Plus1),
                Some(&1)
            );
        }
    }
}

const OGLOR: &str = "At the beginning of your upkeep, look at the top two cards of your library, then put one of them into your graveyard.\nWhenever a creature card is put into your graveyard from your library or hand, it perpetually gains \"When this card leaves your graveyard, create a tapped 2/2 black Zombie creature token.\"";
const MILLSTONE: &str = "{2}, {T}: Target player mills two cards.";

fn activate_millstone_until_resolved(runner: &mut GameRunner, stone: ObjectId, target: PlayerId) {
    give_priority(runner, runner.state().objects[&stone].controller);
    runner
        .act(GameAction::ActivateAbility {
            source_id: stone,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Millstone");
    for _ in 0..8 {
        match runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(target)),
                    })
                    .expect("choose Millstone target");
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay Millstone");
            }
            WaitingFor::Priority { .. } => break,
            ref other => panic!("unexpected Millstone announcement: {other:?}"),
        }
    }
    assert!(runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == stone));
    finish_spell(runner, stone);
}

fn stage_library_creature(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.base_card_types = obj.card_types.clone();
}

/// V10o: complete the outer perpetual grant; its later moving-source trigger is excluded.
#[test]
fn v10o_oglor_library_union_installs_only_the_moved_creatures_grant() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let oglor = s
        .add_creature_from_oracle(P0, "Oglor, Devoted Assistant", 1, 1, OGLOR)
        .id();
    let stone = s.add_artifact_from_oracle(P0, "Millstone", MILLSTONE).id();
    let land = s.add_land_to_library_top(P0, "Textless land").id();
    let creature = s.add_card_to_library_top(P0, "Textless creature");
    let unrelated = s
        .add_creature_to_graveyard(P0, "Unrelated creature", 2, 2)
        .id();
    s.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, stone, false, vec![]); 2],
    );
    let mut runner = s.build();
    stage_library_creature(&mut runner, creature);
    assert_settled(&runner);
    activate_millstone_until_resolved(&mut runner, stone, P0);
    assert_eq!(runner.state().objects[&creature].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert_eq!(trigger_instances(runner.state()), 1);
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    let obj = &runner.state().objects[&creature];
    assert_eq!(obj.perpetual_mods.len(), 1);
    assert_eq!(obj.base_trigger_definitions.len(), 1);
    let granted = obj.base_trigger_definitions.first().unwrap();
    // CR 608.2c: the outer instruction installs the quoted ability on that creature.
    assert!(granted
        .zone_change_clauses
        .iter()
        .any(
            |clause| clause.origin == OriginConstraint::Equals(Zone::Graveyard)
                && clause.valid_card == Some(TargetFilter::SelfRef)
        ));
    assert!(
        matches!(granted.execute.as_ref().unwrap().effect.as_ref(), Effect::Token { name, tapped: true, .. } if name == "Zombie")
    );
    for id in [land, unrelated, oglor] {
        assert!(
            runner.state().objects[&id].perpetual_mods.is_empty(),
            "unrelated {id:?} received no grant"
        );
    }
}

/// V5v additive reference: one two-creature batch supplies exactly two counters.
#[test]
fn v5v_voracious_brood_two_milled_creatures_add_two_counters() {
    const BROOD: &str = "This creature enters with a +1/+1 counter on it for each creature card in your graveyard.\nWhenever one or more creature cards are put into your graveyard from anywhere, put that many +1/+1 counters on this creature.";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let brood = s
        .add_creature_from_oracle(P0, "Voracious Brood", 1, 1, BROOD)
        .id();
    let stone = s.add_artifact_from_oracle(P0, "Millstone", MILLSTONE).id();
    let a = s.add_card_to_library_top(P0, "Creature A");
    let b = s.add_card_to_library_top(P0, "Creature B");
    s.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, stone, false, vec![]); 2],
    );
    let mut runner = s.build();
    stage_library_creature(&mut runner, a);
    stage_library_creature(&mut runner, b);
    assert_settled(&runner);
    activate_millstone_until_resolved(&mut runner, stone, P0);
    assert_eq!(runner.state().objects[&a].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&b].zone, Zone::Graveyard);
    // CR 603.2c: one batched trigger retains the quantity of both matching subjects.
    assert_eq!(trigger_instances(runner.state()), 1);
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        runner.state().objects[&brood]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2)
    );
}

const PATRON: &str = "Rat offering (You may cast this spell any time you could cast an instant by sacrificing a Rat and paying the difference in mana costs between this and the sacrificed Rat. Mana cost includes color.)\nWhenever a permanent is put into an opponent's graveyard, that player loses 1 life.";
const ALARM: &str = "Create two 1/1 white Soldier creature tokens.";

/// V8: both co-departed observers see the complete mixed-ownership event.
#[test]
fn v8_co_departed_patrons_publish_four_plus_two_in_apnap_order() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s
        .add_creature_from_oracle(P0, "Patron of the Nezumi", 6, 6, PATRON)
        .id();
    let b = s
        .add_creature_from_oracle(P1, "Patron of the Nezumi", 6, 6, PATRON)
        .id();
    let x = s.add_creature(P0, "X", 2, 2).controlled_by(P1).id();
    let y = s.add_creature(P1, "Y", 2, 2).controlled_by(P0).id();
    let alarm = s
        .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, ALARM)
        .id();
    let wrath = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Wrath of God",
            false,
            "Destroy all creatures. They can't be regenerated.",
        )
        .id();
    let mut runner = s.build();
    cast_until_resolved(&mut runner, alarm, &[]);
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.is_token && o.name == "Soldier")
        .map(|o| o.id)
        .collect();
    assert_eq!(tokens.len(), 2);
    for (id, owner, controller) in [
        (a, P0, P0),
        (b, P1, P1),
        (x, P0, P1),
        (y, P1, P0),
        (tokens[0], P1, P1),
        (tokens[1], P1, P1),
    ] {
        assert_eq!(
            (
                runner.state().objects[&id].owner,
                runner.state().objects[&id].controller
            ),
            (owner, controller)
        );
    }
    let before = (life(&runner, P0), life(&runner, P1));
    cast_until_resolved(&mut runner, wrath, &[]);
    assert_eq!(
        trigger_instances(runner.state()),
        6,
        "co-departed observers retain all six triggers"
    );
    while let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).rev().collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("choose each APNAP group's order");
    }
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == a)
            .count(),
        4
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == b)
            .count(),
        2
    );
    assert_eq!(
        runner
            .state()
            .stack
            .iter()
            .map(|entry| entry.controller)
            .collect::<Vec<_>>(),
        vec![P0, P0, P0, P0, P1, P1]
    );
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    assert_eq!(
        (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
        (-2, -4)
    );
    for (id, owner) in [(a, P0), (b, P1), (x, P0), (y, P1)] {
        assert_eq!(
            (
                runner.state().objects[&id].zone,
                runner.state().objects[&id].owner
            ),
            (Zone::Graveyard, owner)
        );
    }
    for token in tokens {
        assert!(!runner.state().objects.contains_key(&token));
    }
}

const AETHERMAGE: &str =
    "Whenever a permanent is returned to your hand, you may pay {1}. If you do, draw a card.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

/// V3 reference: the existing pay/draw body completes on the unambiguous own bounce.
#[test]
fn v3_aethermage_own_bounce_payment_draws_exactly_one() {
    for (owner, controller, expected) in [(P0, P0, 1), (P0, P1, 1), (P1, P0, 0)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let observer = s
            .add_creature_from_oracle(P0, "Azorius Aethermage", 1, 1, AETHERMAGE)
            .id();
        let victim = s
            .add_creature(owner, "Bounced creature", 2, 2)
            .controlled_by(controller)
            .id();
        let bounce = s
            .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
            .id();
        let drawn = s.add_card_to_library_top(P0, "Drawn reference");
        s.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Colorless, observer, false, vec![])],
        );
        let mut runner = s.build();
        assert_settled(&runner);
        assert_eq!(
            (
                runner.state().objects[&victim].owner,
                runner.state().objects[&victim].controller
            ),
            (owner, controller)
        );
        cast_until_resolved(&mut runner, bounce, &[victim]);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Hand);
        assert_eq!(
            trigger_instances(runner.state()),
            expected,
            "owner={owner:?}, controller={controller:?}"
        );
        drain_to_optional_or_priority(&mut runner);
        if expected == 1 {
            accept_optional(&mut runner, P0);
        }
        assert_settled(&runner);
        assert_eq!(
            runner.state().objects[&drawn].zone,
            if expected == 1 {
                Zone::Hand
            } else {
                Zone::Library
            }
        );
        assert_eq!(
            runner
                .state()
                .players
                .iter()
                .find(|p| p.id == P0)
                .unwrap()
                .mana_pool
                .total(),
            usize::from(expected == 0)
        );
    }
}

const PRINCE: &str = "Whenever a permanent an opponent controls is put into a graveyard, put that card onto the battlefield under your control unless that opponent pays 3 life.";

/// V7p reference: the departure controller chooses; the owner is an independent player.
#[test]
fn v7p_prince_payment_and_decline_preserve_owner_and_chooser() {
    let p2 = PlayerId(2);
    for pay in [false, true] {
        let mut s = GameScenario::new_n_player(3, 7);
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P0, "Prince of Thralls", 7, 7, PRINCE);
        let victim = s
            .add_creature(p2, "Borrowed creature", 2, 2)
            .controlled_by(P1)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        assert_eq!(
            (
                runner.state().objects[&victim].owner,
                runner.state().objects[&victim].controller
            ),
            (p2, P1)
        );
        let before = life(&runner, P1);
        cast_until_resolved(&mut runner, murder, &[victim]);
        assert_eq!(trigger_instances(runner.state()), 1);
        for _ in 0..8 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            runner
                .act(GameAction::PassPriority)
                .expect("resolve Prince to payment");
        }
        let WaitingFor::UnlessPayment { player, .. } = runner.state().waiting_for else {
            panic!(
                "expected Prince payment, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(player, P1);
        runner
            .act(GameAction::PayUnlessCost { pay })
            .expect("answer Prince payment");
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        // CR 608.2c: the opponent who controlled it pays; returning never changes owner.
        assert_eq!(life(&runner, P1) - before, if pay { -3 } else { 0 });
        assert_eq!(runner.state().objects[&victim].owner, p2);
        assert_eq!(
            runner.state().objects[&victim].zone,
            if pay {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        if !pay {
            assert_eq!(runner.state().objects[&victim].controller, P0);
        }
    }
}

fn mill_nonbattlefield_permanents(prince: bool) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    if prince {
        s.add_creature_from_oracle(P0, "Prince of Thralls", 7, 7, PRINCE);
    } else {
        s.add_creature_from_oracle(P0, "Patron of the Nezumi", 6, 6, PATRON);
    }
    let stone = s.add_artifact_from_oracle(P0, "Millstone", MILLSTONE).id();
    let a = s.add_card_to_library_top(P1, "Milled A");
    let b = s.add_card_to_library_top(P1, "Milled B");
    s.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, stone, false, vec![]); 2],
    );
    let mut runner = s.build();
    stage_library_creature(&mut runner, a);
    stage_library_creature(&mut runner, b);
    assert_settled(&runner);
    let before = life(&runner, P1);
    activate_millstone_until_resolved(&mut runner, stone, P1);
    for id in [a, b] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
    }
    // CR 110.1: a permanent is on the battlefield, not a permanent card in a library.
    assert_eq!(
        trigger_instances(runner.state()),
        0,
        "prince={prince}: library cards are not departing permanents"
    );
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    assert_eq!(life(&runner, P1), before);
}

#[test]
fn v7_patron_ignores_milled_permanent_cards() {
    mill_nonbattlefield_permanents(false);
}

#[test]
fn v7p_prince_ignores_milled_permanent_cards() {
    mill_nonbattlefield_permanents(true);
}

const WARPED: &str =
    "Whenever a permanent is returned to a player's hand, that player discards a card.";

/// V4/V8h: owner binding survives capture, including when the observer is removed.
#[test]
fn v4_warped_devotion_owner_chooses_discard_with_and_without_source() {
    let p2 = PlayerId(2);
    for remove in [false, true] {
        let mut s = GameScenario::new_n_player(3, 7);
        s.at_phase(Phase::PreCombatMain);
        let observer = s
            .add_enchantment_from_oracle(P0, "Warped Devotion", WARPED)
            .id();
        let victim = s
            .add_creature(P1, "Bounced creature", 2, 2)
            .controlled_by(p2)
            .id();
        let chosen = s.add_card_to_hand(P1, "Chosen discard");
        s.add_card_to_hand(P1, "Keep");
        s.with_cards_in_hand(p2, &["Other A", "Other B"]);
        let bounce = s
            .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
            .id();
        let destroy = s
            .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        assert_eq!(
            (
                runner.state().objects[&victim].owner,
                runner.state().objects[&victim].controller
            ),
            (P1, p2)
        );
        let p2_hand = runner
            .state()
            .players
            .iter()
            .find(|p| p.id == p2)
            .unwrap()
            .hand
            .clone();
        cast_until_resolved(&mut runner, bounce, &[victim]);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Hand);
        assert_eq!(trigger_instances(runner.state()), 1);
        if remove {
            cast_until_resolved(&mut runner, destroy, &[observer]);
            assert_eq!(runner.state().objects[&observer].zone, Zone::Graveyard);
        }
        for _ in 0..16 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            runner
                .act(GameAction::PassPriority)
                .expect("resolve Devotion to discard");
        }
        let WaitingFor::DiscardChoice {
            player,
            cards,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "expected discard prompt, got {:?}",
                runner.state().waiting_for
            );
        };
        assert_eq!(
            *player, P1,
            "the hand's owner chooses, not its former controller"
        );
        assert_eq!(*count, 1);
        assert!(cards.contains(&chosen));
        runner
            .act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .expect("discard the selected nonreturned card");
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(runner.state().objects[&chosen].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Hand);
        assert_eq!(
            runner
                .state()
                .players
                .iter()
                .find(|p| p.id == p2)
                .unwrap()
                .hand,
            p2_hand
        );
    }
}

const RIDERS: &str = "Flying\nWhen this creature enters, return two creatures you control to their owner's hand.\nWhenever this creature or another creature is returned to your hand from the battlefield, create a 1/1 white Soldier creature token.";

#[test]
fn v3s_stormfront_self_and_other_returns_use_hand_owner() {
    for (self_return, owner, controller, expected) in [
        (false, P0, P0, 1),
        (true, P0, P0, 1),
        (false, P0, P1, 1),
        (false, P1, P0, 0),
        (true, P0, P1, 0),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature(P0, "Stormfront Riders", 4, 3)
            .controlled_by(if self_return { controller } else { P0 })
            .from_oracle_text_with_keywords(&["Flying"], RIDERS)
            .id();
        let victim = if self_return {
            source
        } else {
            s.add_creature(owner, "Other creature", 2, 2)
                .controlled_by(controller)
                .id()
        };
        let bounce = s
            .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        assert_eq!(
            (
                runner.state().objects[&victim].owner,
                runner.state().objects[&victim].controller
            ),
            (owner, controller)
        );
        cast_until_resolved(&mut runner, bounce, &[victim]);
        assert_eq!(runner.state().objects[&victim].zone, Zone::Hand);
        assert_eq!(
            trigger_instances(runner.state()),
            expected,
            "self={self_return}, owner={owner:?}, controller={controller:?}"
        );
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|o| o.is_token && o.name == "Soldier")
                .count(),
            expected
        );
    }
}

/// V1s: the other newly found self-head consumer retains its complete return body.
#[test]
fn v1s_unquenchable_fury_owned_returns_and_stolen_stays() {
    const FURY: &str = "Enchant creature\nEnchanted creature has \"Whenever this creature attacks, it deals X damage to defending player, where X is the number of cards in their hand.\"\nWhen this Aura is put into your graveyard from the battlefield, return it to your hand.";
    for controller in [P0, P1] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let host = s.add_creature(P0, "Enchanted creature", 2, 2).id();
        let aura = s
            .add_enchantment_from_oracle(P0, "Unquenchable Fury", FURY)
            .with_subtypes(vec!["Aura"])
            .controlled_by(controller)
            .id();
        let destroy = s
            .add_spell_to_hand_from_oracle(P0, "Disenchant", true, DISENCHANT)
            .id();
        let mut runner = s.build();
        runner
            .state_mut()
            .objects
            .get_mut(&aura)
            .unwrap()
            .attached_to = Some(engine::game::game_object::AttachTarget::Object(host));
        engine::game::layers::evaluate_layers(runner.state_mut());
        assert_settled(&runner);
        assert_eq!(
            (
                runner.state().objects[&aura].owner,
                runner.state().objects[&aura].controller
            ),
            (P0, controller)
        );
        assert_eq!(
            runner.state().objects[&aura].attached_to,
            Some(engine::game::game_object::AttachTarget::Object(host))
        );
        cast_until_resolved(&mut runner, destroy, &[aura]);
        assert_eq!(
            trigger_instances(runner.state()),
            usize::from(controller == P0)
        );
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(
            runner.state().objects[&aura].zone,
            if controller == P0 {
                Zone::Hand
            } else {
                Zone::Graveyard
            }
        );
    }
}

const FIELD: &str = "Prevent all damage that would be dealt to you by sources you don't control.\nWhen a card is put into your graveyard from anywhere, sacrifice this enchantment.";
const QUEST: &str = "Whenever a card is put into your graveyard from anywhere, you may put a quest counter on this enchantment.\nRemove five quest counters from this enchantment and sacrifice it: Target player shuffles their graveyard into their library.";
const HAUNT: &str = "Flying, lifelink\nWhenever The Haunt of Hightower attacks, defending player discards a card.\nWhenever a card is put into an opponent's graveyard from anywhere, put a +1/+1 counter on The Haunt of Hightower.";
const MEMENTO: &str =
    "Whenever a creature card is put into an opponent's graveyard from anywhere, you gain 1 life.";

fn card_observer_victim_pairs(name: &str, text: &str, opponent: bool) {
    let qualifying_owner = if opponent { P1 } else { P0 };
    for (token, owner, controller, expected) in [
        (false, qualifying_owner, qualifying_owner, 1),
        (true, qualifying_owner, qualifying_owner, 0),
        (false, qualifying_owner, if opponent { P0 } else { P1 }, 1),
        (false, if opponent { P0 } else { P1 }, qualifying_owner, 0),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let observer = match name {
            "The Haunt of Hightower" => s
                .add_creature(P0, name, 3, 3)
                .from_oracle_text_with_keywords(&["Flying", "Lifelink"], text)
                .id(),
            "Profane Memento" => s.add_artifact_from_oracle(P0, name, text).id(),
            "Energy Field" | "Quest for Ancient Secrets" => s
                .add_spell_to_hand_from_oracle(P0, name, false, text)
                .as_enchantment()
                .id(),
            "Bloodchief Ascension" => s.add_enchantment_from_oracle(P0, name, text).id(),
            _ => unreachable!("only the named real-card observers use this fixture"),
        };
        if name == "Bloodchief Ascension" {
            s.with_counter(observer, CounterType::Generic("quest".into()), 3);
        }
        let printed = s
            .add_creature(owner, "Printed victim", 2, 2)
            .controlled_by(controller)
            .id();
        let alarm = s
            .add_spell_to_hand_from_oracle(owner, "Raise the Alarm", true, ALARM)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(if opponent { P0 } else { P1 }, "Murder", true, MURDER)
            .id();
        let mut runner = s.build();
        cast_until_resolved(&mut runner, alarm, &[]);
        drain_to_optional_or_priority(&mut runner);
        if name == "Bloodchief Ascension" && owner == P1 {
            let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
                panic!("Bloodchief setup must offer the spell-card trigger");
            };
            assert_eq!(player, P0);
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .expect("decline setup trigger");
            drain_to_optional_or_priority(&mut runner);
        }
        if !opponent {
            give_priority(&mut runner, P0);
            runner.cast(observer).resolve();
        }
        assert_settled(&runner);
        assert_eq!(runner.state().objects[&observer].zone, Zone::Battlefield);
        let victim = if token {
            let tokens: Vec<_> = runner
                .state()
                .objects
                .values()
                .filter(|o| o.is_token && o.name == "Soldier" && o.owner == owner)
                .map(|o| o.id)
                .collect();
            assert_eq!(tokens.len(), 2);
            tokens[0]
        } else {
            printed
        };
        assert_eq!(
            (
                runner.state().objects[&victim].owner,
                runner.state().objects[&victim].controller
            ),
            (owner, controller)
        );
        let before = (life(&runner, P0), life(&runner, P1));
        let counter_type = if name == "Quest for Ancient Secrets" {
            CounterType::Generic("quest".into())
        } else {
            CounterType::Plus1Plus1
        };
        let counters = runner.state().objects[&observer]
            .counters
            .get(&counter_type)
            .copied()
            .unwrap_or(0);
        cast_until_resolved(&mut runner, murder, &[victim]);
        if token {
            assert!(!runner.state().objects.contains_key(&victim));
        } else {
            assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        }
        assert_eq!(
            trigger_instances(runner.state()),
            expected,
            "{name}: token={token}, owner={owner:?}, controller={controller:?}"
        );
        drain_to_optional_or_priority(&mut runner);
        if expected == 1 && matches!(name, "Quest for Ancient Secrets" | "Bloodchief Ascension") {
            accept_optional(&mut runner, P0);
        }
        assert_settled(&runner);
        match name {
            "Energy Field" => assert_eq!(
                runner.state().objects[&observer].zone,
                if expected == 1 {
                    Zone::Graveyard
                } else {
                    Zone::Battlefield
                }
            ),
            "Quest for Ancient Secrets" | "The Haunt of Hightower" => assert_eq!(
                runner.state().objects[&observer]
                    .counters
                    .get(&counter_type)
                    .copied()
                    .unwrap_or(0)
                    - counters,
                expected as u32
            ),
            "Bloodchief Ascension" => assert_eq!(
                (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
                if expected == 1 { (2, -2) } else { (0, 0) }
            ),
            "Profane Memento" => assert_eq!(
                (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
                (expected as i32, 0)
            ),
            _ => unreachable!(),
        }
    }
}

#[test]
fn v5_energy_field_card_token_and_ownership_pairs() {
    card_observer_victim_pairs("Energy Field", FIELD, false);
}
#[test]
fn v5_quest_card_token_and_ownership_pairs() {
    card_observer_victim_pairs("Quest for Ancient Secrets", QUEST, false);
}
#[test]
fn v5_bloodchief_card_token_and_ownership_pairs() {
    card_observer_victim_pairs("Bloodchief Ascension", BLOODCHIEF, true);
}
#[test]
fn v5_haunt_card_token_and_ownership_pairs() {
    card_observer_victim_pairs("The Haunt of Hightower", HAUNT, true);
}
#[test]
fn v5p_memento_card_token_and_ownership_pairs() {
    card_observer_victim_pairs("Profane Memento", MEMENTO, true);
}

/// V10c: controller-position anaphors must not be rewritten to destination ownership.
#[test]
fn v10c_departure_controller_recipients_remain_independent_of_owner() {
    const PAIN: &str = "Menace\nWhenever a player casts their first spell each turn, they create a Treasure token.\nWhenever an artifact an opponent controls is put into a graveyard from the battlefield, this creature deals 1 damage to that player.";
    const WURM: &str = "When this creature enters, creatures your opponents control get -2/-2 until end of turn.\nWhenever a creature an opponent controls dies, that player loses 2 life.";
    for pain in [false, true] {
        for (owner, controller, expected) in [(P0, P1, 1), (P1, P0, 0)] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            if pain {
                s.add_creature(P0, "Pain Distributor", 2, 3)
                    .from_oracle_text_with_keywords(&["Menace"], PAIN);
            } else {
                s.add_creature_from_oracle(P0, "Massacre Wurm", 6, 5, WURM);
            }
            let victim = s
                .add_creature(owner, "Artifact creature", 2, 2)
                .as_artifact_creature()
                .controlled_by(controller)
                .id();
            let murder = s
                .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
                .id();
            let mut runner = s.build();
            assert_settled(&runner);
            assert_eq!(
                (
                    runner.state().objects[&victim].owner,
                    runner.state().objects[&victim].controller
                ),
                (owner, controller)
            );
            let before = (life(&runner, P0), life(&runner, P1));
            cast_until_resolved(&mut runner, murder, &[victim]);
            assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
            assert_eq!(trigger_instances(runner.state()), expected);
            drain_to_optional_or_priority(&mut runner);
            assert_settled(&runner);
            assert_eq!(
                (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
                (0, -(expected as i32) * if pain { 1 } else { 2 })
            );
        }
    }
}

/// V10k: the shared grammar preserves each disjunct's origin and card noun.
#[test]
fn v10k_syr_konrad_three_routes_exclude_lands_and_own_death() {
    const KONRAD: &str = "Whenever another creature dies, or a creature card is put into a graveyard from anywhere other than the battlefield, or a creature card leaves your graveyard, Syr Konrad deals 1 damage to each opponent.\n{1}{B}: Each player mills a card. (They each put the top card of their library into their graveyard.)";
    for route in 0..5 {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature_from_oracle(P0, "Syr Konrad, the Grim", 5, 4, KONRAD)
            .id();
        let victim = s.add_creature(P1, "Dying creature", 2, 2).id();
        let returning = s
            .add_creature_to_graveyard(P0, "Returning creature", 2, 2)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let raise = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Raise Dead",
                false,
                "Return target creature card from your graveyard to your hand.",
            )
            .id();
        let stone = s.add_artifact_from_oracle(P0, "Millstone", MILLSTONE).id();
        let land = s.add_land_to_library_top(P1, "Land A").id();
        let top = if route == 3 {
            s.add_land_to_library_top(P1, "Land B").id()
        } else {
            s.add_card_to_library_top(P1, "Milled creature")
        };
        s.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Colorless, stone, false, vec![]); 2],
        );
        let mut runner = s.build();
        if route != 3 {
            stage_library_creature(&mut runner, top);
        }
        assert_settled(&runner);
        let before = life(&runner, P1);
        match route {
            0 => cast_until_resolved(&mut runner, murder, &[victim]),
            1 | 3 => {
                activate_millstone_until_resolved(&mut runner, stone, P1);
                assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
                assert_eq!(runner.state().objects[&top].zone, Zone::Graveyard);
            }
            2 => {
                cast_until_resolved(&mut runner, raise, &[returning]);
                assert_eq!(runner.state().objects[&returning].zone, Zone::Hand);
            }
            4 => cast_until_resolved(&mut runner, murder, &[source]),
            _ => unreachable!(),
        }
        let expected = usize::from(route < 3);
        assert_eq!(
            trigger_instances(runner.state()),
            expected,
            "Konrad route {route}"
        );
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(life(&runner, P1) - before, -(expected as i32));
    }
}

/// V11: a library-origin positive guards the battlefield-token negative.
#[test]
fn v11_devourer_mill_pumps_but_token_death_does_not() {
    const DEVOURER: &str = "Whenever one or more cards are put into your graveyard from your library, this creature gets +1/+1 until end of turn and can't be blocked this turn.\n{1}{U}{B}: Mill a card.";
    for mill in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature_from_oracle(P0, "Devourer of Memory", 2, 1, DEVOURER)
            .id();
        let library = s.add_card_to_library_top(P0, "Milled card");
        let alarm = s
            .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, ALARM)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .id();
        s.with_mana_pool(
            P0,
            [ManaType::Colorless, ManaType::Blue, ManaType::Black]
                .into_iter()
                .map(|color| ManaUnit::new(color, source, false, vec![]))
                .collect(),
        );
        let mut runner = s.build();
        cast_until_resolved(&mut runner, alarm, &[]);
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        if mill {
            give_priority(&mut runner, P0);
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("activate printed mill");
            for _ in 0..16 {
                if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                    && !runner.state().stack.iter().any(|e| {
                        e.source_id == source
                            && matches!(e.kind, StackEntryKind::ActivatedAbility { .. })
                    })
                {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pay and resolve mill activation");
            }
            assert_eq!(runner.state().objects[&library].zone, Zone::Graveyard);
        } else {
            let token = runner
                .state()
                .objects
                .values()
                .find(|o| o.is_token && o.name == "Soldier")
                .unwrap()
                .id;
            cast_until_resolved(&mut runner, murder, &[token]);
            assert!(!runner.state().objects.contains_key(&token));
        }
        assert_eq!(trigger_instances(runner.state()), usize::from(mill));
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        let obj = &runner.state().objects[&source];
        assert_eq!(
            (obj.power, obj.toughness),
            if mill {
                (Some(3), Some(2))
            } else {
                (Some(2), Some(1))
            }
        );
    }
}

/// S1 SHAPE: no printed qualified-library consumer exists. The nearest printed
/// unqualified Dutiful head remains executable through the public parser.
#[test]
fn s1_qualified_library_heads_stay_unsupported() {
    for (name, oracle) in [
        ("Dutiful Knowledge Seeker", DUTIFUL),
        ("Patron of the Nezumi", PATRON),
        ("Oglor, Devoted Assistant", OGLOR),
    ] {
        let parsed = parse_oracle_text(oracle, name, &[], &[], &[]);
        assert!(
            parsed.triggers.iter().any(|t| matches!(
                t.mode,
                TriggerMode::ChangesZone | TriggerMode::ChangesZoneAll
            )),
            "{name}: {parsed:?}"
        );
    }
    for oracle in [
        "Whenever one or more cards are put into your library from anywhere, put a +1/+1 counter on this creature.",
        "Whenever one or more cards are put into an opponent's library from anywhere, put a +1/+1 counter on this creature.",
    ] {
        let parsed = parse_oracle_text(oracle, "Unsupported nearest printed head", &[], &[], &[]);
        assert_eq!(parsed.triggers.len(), 1, "{oracle}: {parsed:?}");
        assert!(matches!(parsed.triggers[0].mode, TriggerMode::Unknown(_)), "{oracle}: {parsed:?}");
    }
}

/// V5b/V5v: trigger count is the discriminator; a ceased token can produce a
/// zero "that many" quantity even when incorrectly admitted by a typed head.
fn typed_batched_card_heads_reject_tokens_and_resolve_cards(robot: bool) {
    const ROBOT: &str = "Whenever one or more creature cards are put into your graveyard from anywhere, you draw a card, lose 1 life, and put a plan counter on this enchantment.\nWhen the third plan counter is put on this enchantment, sacrifice it and create three 2/2 colorless Robot Villain artifact creature tokens.";
    const BROOD: &str = "This creature enters with a +1/+1 counter on it for each creature card in your graveyard.\nWhenever one or more creature cards are put into your graveyard from anywhere, put that many +1/+1 counters on this creature.";
    for token in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let observer = if robot {
            s.add_enchantment_from_oracle(P0, "Robot Domination", ROBOT)
                .id()
        } else {
            s.add_creature_from_oracle(P0, "Voracious Brood", 1, 1, BROOD)
                .id()
        };
        let victim = s.add_creature(P0, "Nontoken victim", 2, 2).id();
        let alarm = s
            .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, ALARM)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .id();
        let drawn = s.add_card_to_library_top(P0, "Robot draw");
        let mut runner = s.build();
        cast_until_resolved(&mut runner, alarm, &[]);
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        let victim = if token {
            runner
                .state()
                .objects
                .values()
                .find(|o| o.is_token && o.name == "Soldier")
                .unwrap()
                .id
        } else {
            victim
        };
        let before_life = life(&runner, P0);
        let before_hand = runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .unwrap()
            .hand
            .clone();
        let counter = if robot {
            CounterType::Generic("plan".into())
        } else {
            CounterType::Plus1Plus1
        };
        let before_counter = runner.state().objects[&observer]
            .counters
            .get(&counter)
            .copied()
            .unwrap_or(0);
        cast_until_resolved(&mut runner, murder, &[victim]);
        // CR 108.2b + CR 603.2c: card nouns exclude the ceased token.
        assert_eq!(
            trigger_instances(runner.state()),
            usize::from(!token),
            "robot={robot}, token={token}"
        );
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(
            runner.state().objects[&observer]
                .counters
                .get(&counter)
                .copied()
                .unwrap_or(0)
                - before_counter,
            u32::from(!token)
        );
        assert_eq!(
            life(&runner, P0) - before_life,
            if robot && !token { -1 } else { 0 }
        );
        if robot && !token {
            assert_eq!(runner.state().objects[&drawn].zone, Zone::Hand);
        } else {
            assert_eq!(
                runner
                    .state()
                    .players
                    .iter()
                    .find(|p| p.id == P0)
                    .unwrap()
                    .hand,
                before_hand
            );
        }
    }
}

#[test]
fn v5b_typed_batched_card_head_rejects_tokens_and_resolves_cards() {
    typed_batched_card_heads_reject_tokens_and_resolve_cards(true);
}

#[test]
fn v5v_typed_batched_card_head_rejects_tokens_and_resolves_cards() {
    typed_batched_card_heads_reject_tokens_and_resolve_cards(false);
}

/// V5d: the delayed observer consumes the original record after token purge.
#[test]
fn v5d_duskmantle_delayed_card_head_rejects_token_death() {
    const GUILDMAGE: &str = "{1}{U}{B}: Whenever a card is put into an opponent's graveyard from anywhere this turn, that player loses 1 life.\n{2}{U}{B}: Target player mills two cards.";
    for token in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature_from_oracle(P0, "Duskmantle Guildmage", 2, 2, GUILDMAGE)
            .id();
        let printed = s.add_creature(P1, "Printed victim", 2, 2).id();
        let alarm = s
            .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, ALARM)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        s.with_mana_pool(
            P0,
            [ManaType::Colorless, ManaType::Blue, ManaType::Black]
                .into_iter()
                .map(|color| ManaUnit::new(color, source, false, vec![]))
                .collect(),
        );
        let mut runner = s.build();
        cast_until_resolved(&mut runner, alarm, &[]);
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        let ability = runner.state().objects[&source]
            .abilities
            .iter()
            .position(|a| matches!(*a.effect, Effect::CreateDelayedTrigger { .. }))
            .unwrap();
        give_priority(&mut runner, P0);
        runner.activate(source, ability).resolve();
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        assert_settled(&runner);
        let victim = if token {
            runner
                .state()
                .objects
                .values()
                .find(|o| o.is_token && o.name == "Soldier")
                .unwrap()
                .id
        } else {
            printed
        };
        let before = (life(&runner, P0), life(&runner, P1));
        cast_until_resolved(&mut runner, murder, &[victim]);
        assert_eq!(trigger_instances(runner.state()), usize::from(!token));
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(
            (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
            (0, if token { 0 } else { -1 })
        );
    }
}

/// V6: Oust rewards departure control independently of the library card noun.
#[test]
fn v6_library_observers_oust_token_and_card_pairs() {
    for wan in [false, true] {
        for token in [false, true] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let observer = if wan {
                s.add_creature(P0, "Wan Shi Tong, All-Knowing", 4, 4)
                    .from_oracle_text_with_keywords(&["Flying"], WAN_SHI_TONG)
                    .id()
            } else {
                s.add_creature_from_oracle(P0, "Dutiful Knowledge Seeker", 2, 2, DUTIFUL)
                    .id()
            };
            let printed = s.add_creature(P1, "Ousted creature", 2, 2).id();
            let alarm = s
                .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, ALARM)
                .id();
            let oust = s.add_spell_to_hand_from_oracle(P0, "Oust", false,
                "Put target creature into its owner's library second from the top. Its controller gains 3 life.").id();
            s.with_library_top(P1, &["Top", "Bottom"]);
            let mut runner = s.build();
            cast_until_resolved(&mut runner, alarm, &[]);
            drain_to_optional_or_priority(&mut runner);
            assert_settled(&runner);
            let victim = if token {
                runner
                    .state()
                    .objects
                    .values()
                    .find(|o| o.is_token && o.name == "Soldier")
                    .unwrap()
                    .id
            } else {
                printed
            };
            let before = life(&runner, P1);
            cast_until_resolved(&mut runner, oust, &[victim]);
            assert_eq!(life(&runner, P1) - before, 3);
            assert_eq!(trigger_instances(runner.state()), usize::from(!token));
            if token {
                assert!(!runner.state().objects.contains_key(&victim));
            } else {
                let player = runner.state().players.iter().find(|p| p.id == P1).unwrap();
                assert_eq!(player.library.iter().nth(1), Some(&victim));
            }
            drain_to_optional_or_priority(&mut runner);
            assert_settled(&runner);
            if wan {
                assert_eq!(
                    runner
                        .state()
                        .objects
                        .values()
                        .filter(|o| o.is_token && o.name == "Spirit")
                        .count(),
                    if token { 0 } else { 2 }
                );
            } else {
                assert_eq!(
                    runner.state().objects[&observer]
                        .counters
                        .get(&CounterType::Plus1Plus1)
                        .copied()
                        .unwrap_or(0),
                    u32::from(!token)
                );
            }
        }
    }
}

/// V7/V7a: origin, destination owner, and subject control are independent.
#[test]
fn v7_v7a_patron_and_marvel_origin_and_owner_controller_pairs() {
    const MARVEL: &str = "Whenever a permanent you control is put into a graveyard, you get {E} (an energy counter).\n{T}, Pay six {E}: Look at the top six cards of your library. You may cast a spell from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
    let p2 = PlayerId(2);
    for marvel in [false, true] {
        for route in 0..5 {
            let mut s = GameScenario::new_n_player(3, 7);
            s.at_phase(Phase::PreCombatMain);
            if marvel {
                s.add_artifact_from_oracle(P0, "Aetherworks Marvel", MARVEL);
            } else {
                s.add_creature_from_oracle(P0, "Patron of the Nezumi", 6, 6, PATRON);
            }
            let holder = if marvel { P0 } else { P1 };
            let (owner, controller) = if route == 3 {
                (P0, P1)
            } else {
                (P1, if marvel { P0 } else { p2 })
            };
            let victim = s
                .add_creature(owner, "Split permanent", 2, 2)
                .controlled_by(controller)
                .id();
            let alarm = s
                .add_spell_to_hand_from_oracle(holder, "Raise the Alarm", true, ALARM)
                .id();
            let murder = s
                .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
                .id();
            let discard = s
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Mind Rot",
                    false,
                    "Target player discards two cards.",
                )
                .id();
            let stone = s.add_artifact_from_oracle(P0, "Millstone", MILLSTONE).id();
            let a = s.add_card_to_library_top(holder, "Milled A");
            let b = s.add_card_to_library_top(holder, "Milled B");
            let held_a = s.add_card_to_hand(holder, "Held A");
            let held_b = s.add_card_to_hand(holder, "Held B");
            s.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Colorless, stone, false, vec![]); 2],
            );
            let mut runner = s.build();
            for id in [a, b, held_a, held_b] {
                stage_library_creature(&mut runner, id);
            }
            cast_until_resolved(&mut runner, alarm, &[]);
            drain_to_optional_or_priority(&mut runner);
            assert_settled(&runner);
            let before = life(&runner, P1);
            let other_lives = (life(&runner, P0), life(&runner, p2));
            match route {
                0 => activate_millstone_until_resolved(&mut runner, stone, holder),
                1 => {
                    give_priority(&mut runner, P0);
                    runner
                        .cast(discard)
                        .target_player(holder)
                        .discard(&[held_a, held_b])
                        .commit();
                    finish_spell(&mut runner, discard);
                    if holder == P0 {
                        let WaitingFor::DiscardChoice {
                            player,
                            count,
                            cards,
                            ..
                        } = &runner.state().waiting_for
                        else {
                            panic!("Marvel fixture must choose two of its three held cards");
                        };
                        assert_eq!((*player, *count), (P0, 2));
                        assert!(cards.contains(&held_a) && cards.contains(&held_b));
                        runner
                            .act(GameAction::SelectCards {
                                cards: vec![held_b, held_a],
                            })
                            .unwrap();
                    }
                    assert_eq!(runner.state().objects[&held_a].zone, Zone::Graveyard);
                    assert_eq!(runner.state().objects[&held_b].zone, Zone::Graveyard);
                }
                2 | 3 => {
                    assert_eq!(
                        (
                            runner.state().objects[&victim].owner,
                            runner.state().objects[&victim].controller
                        ),
                        (owner, controller)
                    );
                    cast_until_resolved(&mut runner, murder, &[victim]);
                }
                4 => {
                    let token = runner
                        .state()
                        .objects
                        .values()
                        .find(|o| o.is_token && o.name == "Soldier")
                        .unwrap()
                        .id;
                    cast_until_resolved(&mut runner, murder, &[token]);
                }
                _ => unreachable!(),
            }
            let expected = usize::from(route == 2 || route == 4);
            assert_eq!(
                trigger_instances(runner.state()),
                expected,
                "marvel={marvel}, route={route}"
            );
            drain_to_optional_or_priority(&mut runner);
            assert_settled(&runner);
            if marvel {
                assert_eq!(
                    runner
                        .state()
                        .players
                        .iter()
                        .find(|p| p.id == P0)
                        .unwrap()
                        .energy,
                    expected as u32
                );
            } else {
                assert_eq!(life(&runner, P1) - before, -(expected as i32));
            }
            assert_eq!((life(&runner, P0), life(&runner, p2)), other_lives);
        }
    }
}

/// V9(a/b/c): false at publication, false on resolution, and declined optional.
#[test]
fn v9_bloodchief_condition_rechecks_and_optional_decline() {
    for (initial, remove_counters, accept) in [
        (2, false, true),
        (3, true, true),
        (3, false, false),
        (3, false, true),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let observer = s
            .add_enchantment_from_oracle(P0, "Bloodchief Ascension", BLOODCHIEF)
            .id();
        s.with_counter(observer, CounterType::Generic("quest".into()), initial);
        let hexmage = s
            .add_creature(P0, "Vampire Hexmage", 2, 1)
            .from_oracle_text_with_keywords(
                &["First strike"],
                "First strike\nSacrifice this creature: Remove all counters from target permanent.",
            )
            .id();
        let victim = s.add_creature(P1, "Dying card", 2, 2).id();
        let murder = s
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        let before = (life(&runner, P0), life(&runner, P1));
        cast_until_resolved(&mut runner, murder, &[victim]);
        assert_eq!(trigger_instances(runner.state()), usize::from(initial == 3));
        if remove_counters {
            give_priority(&mut runner, P0);
            runner
                .activate(hexmage, 0)
                .target_object(observer)
                .stop_at_optional()
                .resolve();
            assert_eq!(runner.state().objects[&hexmage].zone, Zone::Graveyard);
            assert!(runner.state().objects[&observer].counters.is_empty());
        }
        drain_to_optional_or_priority(&mut runner);
        if initial == 3 && !remove_counters {
            let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
                panic!("Bloodchief must offer the qualified optional instruction");
            };
            assert_eq!(player, P0);
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap();
            drain_to_optional_or_priority(&mut runner);
        }
        // CR 603.4: intervening-if must hold at publication and resolution.
        assert_settled(&runner);
        assert_eq!(
            (life(&runner, P0) - before.0, life(&runner, P1) - before.1),
            if initial == 3 && !remove_counters && accept {
                (2, -2)
            } else {
                (0, 0)
            }
        );
    }
}

/// V10o: the same union grants only the discarded creature, never a battlefield
/// death. The library positive above independently reaches the other union arm.
#[test]
fn v10o_oglor_hand_union_and_battlefield_negative() {
    for discard in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let observer = s
            .add_creature_from_oracle(P0, "Oglor, Devoted Assistant", 1, 1, OGLOR)
            .id();
        let held = s.add_card_to_hand(P0, "Held creature");
        let land = s.add_card_to_hand(P0, "Held land");
        let unrelated = s.add_creature_to_graveyard(P0, "Unrelated", 2, 2).id();
        let battlefield = s.add_creature(P0, "Dying creature", 2, 2).id();
        let rot = s
            .add_spell_to_hand_from_oracle(
                P1,
                "Mind Rot",
                false,
                "Target player discards two cards.",
            )
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .id();
        let mut runner = s.build();
        // Fixture begins in P1's main phase so its actual Mind Rot is legal.
        runner.state_mut().active_player = P1;
        runner.state_mut().priority_player = P1;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
        stage_library_creature(&mut runner, held);
        let held_land = runner.state_mut().objects.get_mut(&land).unwrap();
        held_land.card_types.core_types = vec![CoreType::Land];
        held_land.base_card_types = held_land.card_types.clone();
        assert_settled(&runner);
        if discard {
            give_priority(&mut runner, P1);
            runner
                .cast(rot)
                .target_player(P0)
                .discard(&[held, land])
                .commit();
            finish_spell(&mut runner, rot);
        } else {
            cast_until_resolved(&mut runner, murder, &[battlefield]);
        }
        assert_eq!(trigger_instances(runner.state()), usize::from(discard));
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        let recipient = if discard { held } else { battlefield };
        assert_eq!(
            runner.state().objects[&recipient].perpetual_mods.len(),
            usize::from(discard)
        );
        assert_eq!(
            runner.state().objects[&recipient]
                .base_trigger_definitions
                .len(),
            usize::from(discard)
        );
        for id in [land, observer, unrelated] {
            assert!(runner.state().objects[&id].perpetual_mods.is_empty());
        }
    }
}

/// V4: a returned token leaves no discardable card in an otherwise empty hand.
#[test]
fn v4_warped_devotion_empty_hand_token_return_settles() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.add_enchantment_from_oracle(P0, "Warped Devotion", WARPED);
    let alarm = s
        .add_spell_to_hand_from_oracle(P1, "Raise the Alarm", true, ALARM)
        .id();
    let bounce = s
        .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
        .id();
    let mut runner = s.build();
    cast_until_resolved(&mut runner, alarm, &[]);
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    assert!(runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .hand
        .is_empty());
    let token = runner
        .state()
        .objects
        .values()
        .find(|o| o.is_token && o.name == "Soldier")
        .unwrap()
        .id;
    cast_until_resolved(&mut runner, bounce, &[token]);
    assert_eq!(trigger_instances(runner.state()), 1);
    assert!(!runner.state().objects.contains_key(&token));
    drain_to_optional_or_priority(&mut runner);
    assert_settled(&runner);
    assert!(runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .hand
        .is_empty());
    // The printed-card chooser/selected-discard positive is V4 above.
}

/// V1: optional declines and a response removing the first graveyard incarnation.
#[test]
fn v1_self_optional_declines_and_missing_zodiac_do_not_return() {
    for (name, text, keywords, exile) in [
        ("Zodiac Dragon", ZODIAC, &[][..], false),
        (
            "Guan Yu, Sainted Warrior",
            GUAN_YU,
            &["Horsemanship"][..],
            false,
        ),
        ("Zodiac Dragon", ZODIAC, &[][..], true),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature(P0, name, 8, 8)
            .from_oracle_text_with_keywords(keywords, text)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .id();
        let crypt = s
            .add_artifact_from_oracle(
                P0,
                "Tormod's Crypt",
                "{T}, Sacrifice this artifact: Exile target player's graveyard.",
            )
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        cast_until_resolved(&mut runner, murder, &[source]);
        assert_eq!(trigger_instances(runner.state()), 1);
        if exile {
            give_priority(&mut runner, P0);
            runner
                .activate(crypt, 0)
                .target_player(P0)
                .stop_at_optional()
                .resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Exile);
        } else {
            drain_to_optional_or_priority(&mut runner);
        }
        let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
            panic!("self-return must offer its optional choice");
        };
        assert_eq!(player, P0);
        runner
            .act(GameAction::DecideOptionalEffect { accept: exile })
            .unwrap();
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        // CR 603.6 + CR 400.7e: the queued return cannot fetch it from exile.
        assert_eq!(
            runner.state().objects[&source].zone,
            if exile { Zone::Exile } else { Zone::Graveyard }
        );
    }
    // Owned acceptance and exact-source/non-source identity are exercised by V1.
}

/// V3: captured ability control survives a real control-changing response;
/// decline and inability to pay leave no suspended draw.
#[test]
fn v3_aethermage_decline_no_mana_and_captured_controller() {
    const AGGRESSION: &str = "({R/P} can be paid with either {R} or 2 life.)\nGain control of target creature an opponent controls until end of turn. Untap that creature. It gains haste until end of turn.";
    for (mana, accept, steal) in [
        (true, true, true),
        (true, false, false),
        (false, true, false),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature_from_oracle(P0, "Azorius Aethermage", 1, 1, AETHERMAGE)
            .id();
        let victim = s.add_creature(P0, "Returned creature", 2, 2).id();
        let bounce = s
            .add_spell_to_hand_from_oracle(P0, "Unsummon", true, UNSUMMON)
            .id();
        let theft = s
            .add_spell_to_hand_from_oracle(P1, "Act of Aggression", true, AGGRESSION)
            .id();
        let drawn = s.add_card_to_library_top(P0, "Drawn card");
        if mana {
            s.with_mana_pool(
                P0,
                vec![ManaUnit::new(ManaType::Colorless, source, false, vec![])],
            );
        }
        let mut runner = s.build();
        assert_settled(&runner);
        cast_until_resolved(&mut runner, bounce, &[victim]);
        assert_eq!(trigger_instances(runner.state()), 1);
        if steal {
            cast_until_resolved(&mut runner, theft, &[source]);
            assert_eq!(runner.state().objects[&source].controller, P1);
            assert_eq!(trigger_instances(runner.state()), 1);
        }
        drain_to_optional_or_priority(&mut runner);
        let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
            panic!("Aethermage must offer its optional cost");
        };
        assert_eq!(player, P0);
        assert_eq!(
            runner
                .state()
                .players
                .iter()
                .find(|p| p.id == P0)
                .unwrap()
                .mana_pool
                .total(),
            usize::from(mana)
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap();
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        // CR 118.3 + CR 118.12: attempted acceptance cannot pay absent mana,
        // so "If you do" must not draw or leave a suspended continuation.
        assert_eq!(
            runner.state().objects[&drawn].zone,
            if accept && mana {
                Zone::Hand
            } else {
                Zone::Library
            }
        );
        assert_eq!(
            runner
                .state()
                .players
                .iter()
                .find(|p| p.id == P0)
                .unwrap()
                .mana_pool
                .total(),
            usize::from(mana && !accept)
        );
    }
}

/// V7p: destination owner never replaces Prince's printed opponent-controller
/// restriction. The qualifying sibling completes the actual unless choice.
#[test]
fn v7p_prince_rejects_own_controlled_opponent_owned_death() {
    for qualifies in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P0, "Prince of Thralls", 7, 7, PRINCE);
        let controller = if qualifies { P1 } else { P0 };
        let victim = s
            .add_creature(P1, "Independent owner", 2, 2)
            .controlled_by(controller)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        assert_eq!(
            (
                runner.state().objects[&victim].owner,
                runner.state().objects[&victim].controller
            ),
            (P1, controller)
        );
        cast_until_resolved(&mut runner, murder, &[victim]);
        assert_eq!(trigger_instances(runner.state()), usize::from(qualifies));
        if qualifies {
            for _ in 0..8 {
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            let WaitingFor::UnlessPayment { player, .. } = runner.state().waiting_for else {
                panic!("Prince payment required");
            };
            assert_eq!(player, P1);
            runner
                .act(GameAction::PayUnlessCost { pay: false })
                .unwrap();
        }
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(
            runner.state().objects[&victim].zone,
            if qualifies {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        assert_eq!(runner.state().objects[&victim].owner, P1);
    }
}

/// Optional Quest counter placement may be declined without a continuation.
#[test]
fn v5_quest_counter_accept_and_decline() {
    for accept in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let observer = s
            .add_enchantment_from_oracle(P0, "Quest for Ancient Secrets", QUEST)
            .id();
        let victim = s.add_creature(P0, "Qualifying card", 2, 2).id();
        let murder = s
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        cast_until_resolved(&mut runner, murder, &[victim]);
        assert_eq!(trigger_instances(runner.state()), 1);
        drain_to_optional_or_priority(&mut runner);
        let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
            panic!("Quest must offer a counter");
        };
        assert_eq!(player, P0);
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap();
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(
            runner.state().objects[&observer]
                .counters
                .get(&CounterType::Generic("quest".into()))
                .copied()
                .unwrap_or(0),
            u32::from(accept)
        );
    }
}

/// V1: leaving and returning to the same graveyard cannot substitute a later
/// incarnation for the original event's arrival object.
#[test]
fn v1_zodiac_cannot_return_a_later_graveyard_incarnation() {
    for replace_incarnation in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let source = s
            .add_creature_from_oracle(P0, "Zodiac Dragon", 8, 8, ZODIAC)
            .id();
        let murder = s
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .id();
        let reanimate = s.add_spell_to_hand_from_oracle(P0, "Shallow Grave", true,
            "Return the top creature card of your graveyard to the battlefield. That creature gains haste until end of turn. Exile it at the beginning of the next end step.").id();
        let second_murder = s
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .id();
        let mut runner = s.build();
        assert_settled(&runner);
        cast_until_resolved(&mut runner, murder, &[source]);
        assert_eq!(trigger_instances(runner.state()), 1);
        let arrival = runner.state().objects[&source].incarnation;
        if replace_incarnation {
            cast_until_resolved(&mut runner, reanimate, &[]);
            assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert_ne!(runner.state().objects[&source].incarnation, arrival);
            assert_eq!(trigger_instances(runner.state()), 1);
            cast_until_resolved(&mut runner, second_murder, &[source]);
            assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
            assert_ne!(runner.state().objects[&source].incarnation, arrival);
            assert_eq!(trigger_instances(runner.state()), 2);
            drain_to_optional_or_priority(&mut runner);
            let WaitingFor::OptionalEffectChoice { player, .. } = runner.state().waiting_for else {
                panic!("new incarnation's trigger must offer its own return");
            };
            assert_eq!(player, P0);
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap();
        }
        drain_to_optional_or_priority(&mut runner);
        accept_optional(&mut runner, P0);
        assert_settled(&runner);
        // CR 400.7 + CR 400.7e + CR 603.6: the first arrival can be found,
        // but a later incarnation in that same destination cannot be returned.
        assert_eq!(
            runner.state().objects[&source].zone,
            if replace_incarnation {
                Zone::Graveyard
            } else {
                Zone::Hand
            }
        );
    }
}

fn assert_typed_subject(
    subject: &Option<TargetFilter>,
    types: &[TypeFilter],
    owner: Option<ControllerRef>,
) {
    let Some(TargetFilter::Typed(typed)) = subject else {
        panic!("expected a typed subject, got {subject:?}");
    };
    for ty in types {
        assert!(
            typed.type_filters.iter().any(|actual| match (actual, ty) {
                (TypeFilter::Subtype(actual), TypeFilter::Subtype(expected)) => {
                    actual.eq_ignore_ascii_case(expected)
                }
                _ => actual == ty,
            }),
            "{ty:?}: {typed:?}"
        );
    }
    assert_eq!(typed.controller, None, "no destination-derived controller");
    let owners: Vec<_> = typed
        .properties
        .iter()
        .filter_map(|prop| match prop {
            FilterProp::Owned { controller } => Some(controller.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(owners, owner.into_iter().collect::<Vec<_>>());
}

fn assert_owner_loss_body(trigger: &TriggerDefinition) {
    let execute = trigger.execute.as_deref().expect("executable trigger body");
    assert_eq!(
        execute.effect.as_ref(),
        &Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: Some(TargetFilter::ParentTargetOwner),
        }
    );
}

/// S2 SHAPE (synthetic): Oglor's union and Patron's owner anaphor compose.
/// CR 400.3 + CR 608.2c: accepted complete heads bind the destination owner.
#[test]
fn s2_shared_union_heads_preserve_acceptance_and_owner_binding() {
    for (oracle, owner, batched) in [
        (
            "Whenever a creature card is put into an opponent's graveyard from their library or hand, that player loses 1 life.",
            Some(ControllerRef::Opponent),
            false,
        ),
        (
            "Whenever a creature card is put into a player's graveyard from a library or hand, that player loses 1 life.",
            None,
            false,
        ),
        (
            "Whenever one or more creature cards are put into an opponent's graveyard from an opponent's library or hand, that player loses 1 life.",
            Some(ControllerRef::Opponent),
            true,
        ),
    ] {
        let parsed = parse_oracle_text(oracle, "Synthetic union observer", &[], &[], &[]);
        assert_eq!(parsed.triggers.len(), 1, "{parsed:?}");
        let trigger = &parsed.triggers[0];
        assert_eq!(
            trigger.mode,
            if batched { TriggerMode::ChangesZoneAll } else { TriggerMode::ChangesZone }
        );
        assert_eq!(trigger.batched, batched);
        assert_eq!(trigger.origin, None);
        assert_eq!(trigger.origin_zones, vec![Zone::Library, Zone::Hand]);
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert_typed_subject(&trigger.valid_card, &[TypeFilter::Creature, TypeFilter::Card], owner);
        assert_owner_loss_body(trigger);
    }
    for oracle in [
        "Whenever a creature card is put into your graveyard from an opponent's library or hand, that player loses 1 life.",
        "Whenever a creature card is put into an opponent's graveyard from your library or their hand, that player loses 1 life.",
        "Whenever a creature card is put into a player's graveyard from their library or hand, that player loses 1 life.",
        "Whenever a creature card is put into an opponent's graveyard from their library or hand with a leftover tail, that player loses 1 life.",
    ] {
        let parsed = parse_oracle_text(oracle, "Synthetic inconsistent union", &[], &[], &[]);
        assert_eq!(parsed.triggers.len(), 1, "{parsed:?}");
        assert!(matches!(parsed.triggers[0].mode, TriggerMode::Unknown(_)), "{parsed:?}");
    }
    for oracle in [
        "Whenever a creature card is put into your graveyard from your library or hand, that player loses 1 life.",
        "Whenever a creature card is put into a graveyard from a library or hand, that player loses 1 life.",
    ] {
        let parsed = parse_oracle_text(oracle, "Synthetic no third-person antecedent", &[], &[], &[]);
        let trigger = &parsed.triggers[0];
        assert_eq!(trigger.mode, TriggerMode::ChangesZone);
        let Effect::LoseLife { target, .. } = trigger.execute.as_ref().unwrap().effect.as_ref() else {
            panic!("expected executable life loss: {trigger:?}");
        };
        assert_ne!(target.as_ref(), Some(&TargetFilter::ParentTargetOwner));
    }
}

/// S2r SHAPE is deliberately separate from the public-action recipient witness.
#[test]
fn s2r_union_owner_observer_shape() {
    let parsed = parse_oracle_text(
        UNION_OWNER_OBSERVER,
        "Synthetic union owner observer",
        &[],
        &[],
        &[],
    );
    assert_eq!(parsed.triggers.len(), 1);
    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::ChangesZone);
    assert_eq!(trigger.origin, None);
    assert_eq!(trigger.origin_zones, vec![Zone::Battlefield, Zone::Library]);
    assert_eq!(trigger.destination, Some(Zone::Graveyard));
    assert_typed_subject(
        &trigger.valid_card,
        &[TypeFilter::Creature, TypeFilter::Card],
        None,
    );
    assert_owner_loss_body(trigger);
}

/// S2r synthetic runtime: nearest printed Oglor supplies the union and Patron
/// supplies "that player loses 1 life"; no printed exact third-person union exists.
#[test]
fn s2r_union_owner_binding_uses_destination_owner_not_departure_controller() {
    let p2 = PlayerId(2);
    for origin in [Zone::Battlefield, Zone::Library, Zone::Hand] {
        let mut s = GameScenario::new_n_player(3, 7);
        s.at_phase(Phase::PreCombatMain);
        let observer = s
            .add_enchantment_from_oracle(P0, "Synthetic union owner observer", UNION_OWNER_OBSERVER)
            .id();
        let victim = match origin {
            Zone::Battlefield => s
                .add_creature(P1, "Borrowed creature", 2, 2)
                .controlled_by(p2)
                .id(),
            Zone::Library => s.add_card_to_library_top(P1, "Milled creature"),
            Zone::Hand => s.add_card_to_hand(P1, "Discarded creature"),
            _ => unreachable!(),
        };
        let companion = match origin {
            Zone::Library => Some(s.add_land_to_library_top(P1, "Milled land").id()),
            Zone::Hand => Some(s.add_card_to_hand(P1, "Discarded land")),
            _ => None,
        };
        let murder = s
            .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
            .id();
        let rot = s
            .add_spell_to_hand_from_oracle(
                P0,
                "Mind Rot",
                false,
                "Target player discards two cards.",
            )
            .id();
        let stone = s.add_artifact_from_oracle(P0, "Millstone", MILLSTONE).id();
        s.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Colorless, stone, false, vec![]); 2],
        );
        let mut runner = s.build();
        if origin != Zone::Battlefield {
            stage_library_creature(&mut runner, victim);
        }
        if origin == Zone::Hand {
            let land = runner
                .state_mut()
                .objects
                .get_mut(&companion.unwrap())
                .unwrap();
            land.card_types.core_types = vec![CoreType::Land];
            land.base_card_types = land.card_types.clone();
        }
        assert_settled(&runner);
        assert_eq!(runner.state().objects[&observer].controller, P0);
        let before = (life(&runner, P0), life(&runner, P1), life(&runner, p2));
        match origin {
            Zone::Battlefield => {
                assert_eq!(
                    (
                        runner.state().objects[&victim].owner,
                        runner.state().objects[&victim].controller
                    ),
                    (P1, p2)
                );
                cast_until_resolved(&mut runner, murder, &[victim]);
            }
            Zone::Library => activate_millstone_until_resolved(&mut runner, stone, P1),
            Zone::Hand => cast_player_until_resolved(&mut runner, rot, P1),
            _ => unreachable!(),
        }
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&victim].owner, P1);
        if let Some(companion) = companion {
            assert_eq!(runner.state().objects[&companion].zone, Zone::Graveyard);
        }
        let qualifies = origin != Zone::Hand;
        assert_eq!(
            trigger_instances(runner.state()),
            usize::from(qualifies),
            "{origin:?}"
        );
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        // CR 400.3 + CR 608.2c: "that player" is P1, not event controller P2.
        assert_eq!(
            (
                life(&runner, P0) - before.0,
                life(&runner, P1) - before.1,
                life(&runner, p2) - before.2
            ),
            (0, if qualifies { -1 } else { 0 }, 0),
            "{origin:?}"
        );
    }
}

fn assert_exclusion_subject(trigger: &TriggerDefinition, mode: TriggerMode) {
    assert_eq!(trigger.mode, mode);
    assert_eq!(trigger.batched, mode == TriggerMode::ChangesZoneAll);
    assert_eq!(trigger.valid_card, None);
    assert_eq!(trigger.zone_change_clauses.len(), 1);
    let clause = &trigger.zone_change_clauses[0];
    assert_eq!(
        clause.origin,
        OriginConstraint::NotEquals(Zone::Battlefield)
    );
    assert_eq!(clause.destination, Some(Zone::Graveyard));
    assert_typed_subject(
        &clause.valid_card,
        &[
            TypeFilter::Permanent,
            TypeFilter::Subtype("lhurgoyf".into()),
            TypeFilter::Card,
        ],
        Some(ControllerRef::You),
    );
}

/// V12 SHAPE: inspect Disa's first ability, not its unrelated combat-token body.
#[test]
fn v12_disa_singular_exclusion_preserves_complete_subject() {
    let parsed = parse_oracle_text(DISA, "Disa the Restless", &[], &[], &[]);
    assert_eq!(parsed.triggers.len(), 2, "complete Oracle text: {parsed:?}");
    let trigger = &parsed.triggers[0];
    assert_exclusion_subject(trigger, TriggerMode::ChangesZone);
    assert!(
        matches!(
            trigger.execute.as_ref().unwrap().effect.as_ref(),
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                target: TargetFilter::ParentTarget,
                ..
            }
        ),
        "{trigger:?}"
    );
}

/// SHAPE: synthetic strict failures; nearest printed texts are Syr Konrad's Squire
/// (disjunctive self exclusion), Narcomoeba (self library-to-graveyard), and
/// Stormfront Riders (mixed self/other battlefield-to-hand).
/// CR 603.2 + CR 603.6c: matching destination events must trigger; "from anywhere"
/// is not a leaves-the-battlefield trigger. Do not advertise a dead self head.
#[test]
fn v12_synthetic_self_exclusion_fails_closed_without_rejecting_disa() {
    let control = parse_oracle_text(DISA, "Disa the Restless", &[], &[], &[]);
    assert_eq!(control.triggers.len(), 2);
    assert_exclusion_subject(&control.triggers[0], TriggerMode::ChangesZone);

    let riders = parse_oracle_text(RIDERS, "Stormfront Riders", &["Flying".into()], &[], &[]);
    assert_eq!(riders.triggers.len(), 2, "{riders:?}");
    let returned = &riders.triggers[1];
    assert_eq!(returned.mode, TriggerMode::ChangesZone, "{returned:?}");
    assert_eq!(returned.origin, Some(Zone::Battlefield));
    assert_eq!(returned.destination, Some(Zone::Hand));
    assert!(returned.execute.is_some(), "{returned:?}");
    assert!(
        matches!(returned.valid_card, Some(TargetFilter::Or { .. })),
        "the supported control must retain its mixed subject: {returned:?}"
    );

    for text in [
        "When this card is put into your graveyard from anywhere other than the battlefield, you gain 2 life.",
        "When this card or another creature card is put into your graveyard from anywhere other than the battlefield, you gain 2 life.",
    ] {
        let parsed = parse_oracle_text(
            text,
            "Synthetic self exclusion observer",
            &[],
            &[],
            &[],
        );
        assert_eq!(parsed.triggers.len(), 1, "{text}: {parsed:?}");
        assert!(
            matches!(parsed.triggers[0].mode, TriggerMode::Unknown(_)),
            "unsupported self source admission must fail closed: {text}: {parsed:?}"
        );
    }
}

/// SHAPE (synthetic): Narcomoeba supplies self graveyard arrival and Stormfront
/// Riders supplies the mixed subject. Oglor supplies the inclusion union.
/// CR 603.2: preserve base acceptance for mixed-self inclusion heads.
#[test]
fn v12_synthetic_mixed_inclusions_preserve_base_acceptance() {
    // These mixed-self origins were accepted at base. Their self branch's
    // runtime admission is pre-existing and deliberately outside this fix.
    for origin in ["anywhere", "your library or hand"] {
        let oracle = format!("When this card or another creature card is put into your graveyard from {origin}, you gain 2 life.");
        let parsed =
            parse_oracle_text(&oracle, "Synthetic mixed inclusion observer", &[], &[], &[]);
        assert_eq!(parsed.triggers.len(), 1, "{oracle}: {parsed:?}");
        let trigger = &parsed.triggers[0];
        assert_eq!(
            trigger.mode,
            TriggerMode::ChangesZone,
            "{oracle}: {trigger:?}"
        );
        assert_eq!(trigger.destination, Some(Zone::Graveyard));
        assert!(trigger.execute.is_some(), "{oracle}: {trigger:?}");
    }
}

/// V12: full-Oracle Disa returns only the owned Lhurgoyf card that was milled.
/// CR 400.7e + CR 110.2a: find that arrival object and put it under P0's control.
#[test]
fn v12_disa_mill_returns_only_owned_lhurgoyf_and_excludes_battlefield() {
    for (owner, lhurgoyf, origin) in [
        (P0, true, Zone::Library),
        (P1, true, Zone::Library),
        (P0, false, Zone::Library),
        (P0, true, Zone::Battlefield),
    ] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        s.add_creature_from_oracle(P0, "Disa the Restless", 5, 6, DISA);
        let unrelated = s
            .add_creature_to_graveyard(P0, "Unrelated Lhurgoyf", 2, 2)
            .with_subtypes(vec!["Lhurgoyf"])
            .id();
        let victim = if origin == Zone::Battlefield {
            s.add_creature(owner, "Moved Lhurgoyf", 2, 2)
                .with_subtypes(vec!["Lhurgoyf"])
                .id()
        } else {
            s.add_card_to_library_top(owner, "Moved creature")
        };
        let land = s.add_land_to_library_top(owner, "Milled land").id();
        let stone = s.add_artifact_from_oracle(P0, "Millstone", MILLSTONE).id();
        let murder = s
            .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
            .id();
        s.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Colorless, stone, false, vec![]); 2],
        );
        let mut runner = s.build();
        if origin == Zone::Library {
            stage_library_creature(&mut runner, victim);
            let object = runner.state_mut().objects.get_mut(&victim).unwrap();
            object.power = Some(2);
            object.toughness = Some(2);
            object.base_power = Some(2);
            object.base_toughness = Some(2);
            if lhurgoyf {
                let object = runner.state_mut().objects.get_mut(&victim).unwrap();
                object.card_types.subtypes.push("Lhurgoyf".into());
                object.base_card_types = object.card_types.clone();
            }
        }
        assert_settled(&runner);
        if origin == Zone::Library {
            activate_millstone_until_resolved(&mut runner, stone, owner);
            assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
        } else {
            cast_until_resolved(&mut runner, murder, &[victim]);
        }
        assert_eq!(runner.state().objects[&victim].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&victim].owner, owner);
        let qualifies = owner == P0 && lhurgoyf && origin == Zone::Library;
        // CR 603.2 + CR 603.6c: exactly the non-battlefield owned subject qualifies.
        assert_eq!(
            trigger_instances(runner.state()),
            usize::from(qualifies),
            "{owner:?}/{lhurgoyf}/{origin:?}"
        );
        drain_to_optional_or_priority(&mut runner);
        assert_settled(&runner);
        assert_eq!(
            runner.state().objects[&victim].zone,
            if qualifies {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            }
        );
        if qualifies {
            assert_eq!(runner.state().objects[&victim].controller, P0);
        }
        assert_eq!(runner.state().objects[&unrelated].zone, Zone::Graveyard);
        if origin == Zone::Library {
            assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
        }
    }
}
