//! One count position per verb, failing closed — driven through the real cast,
//! activation and trigger pipelines from verbatim Oracle text (card-data).
//!
//! Every count a migrated verb reads goes through `oracle_util::parse_count_slot`.
//! A quantity it reads resolves to the printed number; a quantity it cannot read
//! makes the clause an honest gap instead of a literal fallback (1, the leading
//! number, or the whole hand).
//!
//! The gaps are asserted on observable state, each paired with a reach marker
//! proving the spell, ability or trigger ran: an `Effect::Unimplemented` root
//! returns before the `unimplemented_oracle_ids` recorder, so that set is not a
//! usable signal here.

use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::PayCostKind;

const TORMENTED_THOUGHTS: &str = "As an additional cost to cast this spell, sacrifice a \
     creature.\nTarget player discards a number of cards equal to the sacrificed creature's \
     power.";

const TASTER_OF_WARES: &str = "When this creature enters, target opponent reveals X cards from \
     their hand, where X is the number of Goblins you control. You choose one of those cards. \
     That player exiles it. If an instant or sorcery card is exiled this way, you may cast it \
     for as long as you control this creature, and mana of any type can be spent to cast that \
     spell.";

const STABWHISKER: &str = "At the beginning of each opponent's upkeep, that player loses 1 life \
     for each card fewer than three in their hand.";

/// Synthetic sorcery (labelled): the token verb's for-each binder over a
/// quantity no parser reads.
const GOAT_FOR_EACH_UNREAD: &str = "Create a 0/1 white Goat creature token for each gorbleflux.";

const ESSENCE_SLIVER: &str = "Whenever a Sliver deals damage, its controller gains that much life.";

const SHILGENGAR: &str = "Flying\nSacrifice another creature: Create a Blood token. If you \
     sacrificed an Angel this way, create a number of Blood tokens equal to its toughness \
     instead.\n{W/B}{W/B}{W/B}, Sacrifice six Blood tokens: Return each creature card from your \
     graveyard to the battlefield with a finality counter on it. Those creatures are Vampires in \
     addition to their other types.";

const GOLDVEIN_HYDRA: &str = "Vigilance, trample, haste\nThis creature enters with X +1/+1 \
     counters on it.\nWhen this creature dies, create a number of tapped Treasure tokens equal \
     to its power.";

const DESTROY: &str = "Destroy target creature.";

const THREEFOLD_THUNDERHULK: &str = "This creature enters with three +1/+1 counters on \
     it.\nWhenever this creature enters or attacks, create a number of 1/1 colorless Gnome \
     artifact creature tokens equal to its power.\n{2}, Sacrifice another artifact: Put a +1/+1 \
     counter on this creature.";

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn graveyard_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].graveyard.len()
}

/// The tokens named `name` on the battlefield, with their tapped status.
fn tokens_named(runner: &GameRunner, name: &str) -> Vec<bool> {
    runner
        .state()
        .battlefield
        .iter()
        .map(|id| &runner.state().objects[id])
        .filter(|obj| obj.is_token && obj.name == name)
        .map(|obj| obj.tapped)
        .collect()
}

/// Cast Tormented Thoughts sacrificing `sacrifice` and targeting P1, answering
/// P1's discard prompt with the first cards offered. Returns whether the
/// sacrifice prompt fired.
fn cast_tormented_thoughts(runner: &mut GameRunner, spell: ObjectId, sacrifice: ObjectId) -> bool {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Tormented Thoughts must succeed");
    let mut sacrificed = false;
    for _ in 0..30 {
        match runner.state().waiting_for.clone() {
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![sacrifice],
                    })
                    .expect("sacrificing for the additional cost must succeed");
                sacrificed = true;
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("targeting P1 must succeed");
            }
            WaitingFor::DiscardChoice { count, cards, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: cards.into_iter().take(count).collect(),
                    })
                    .expect("P1's discard choice must be accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            other => panic!("unexpected waiting state during the cast: {other:?}"),
        }
    }
    sacrificed
}

/// Tormented Thoughts with P0 controlling a 3/1 and a 5/5 and P1 holding
/// `p1_hand` cards, cast sacrificing the 3/1.
fn tormented_thoughts(p1_hand: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Tormented Thoughts", false, TORMENTED_THOUGHTS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let sacrificed = scenario.add_creature(P0, "Three One", 3, 1).id();
    let bystander = scenario.add_creature(P0, "Five Five", 5, 5).id();
    for index in 0..p1_hand {
        scenario.add_card_to_hand(P1, &format!("P1 Card {index}"));
    }
    let mut runner = scenario.build();
    assert!(
        cast_tormented_thoughts(&mut runner, spell, sacrificed),
        "the sacrifice-for-cost prompt must fire"
    );
    (runner, sacrificed, bystander)
}

/// CR 701.9a + CR 608.2k: Tormented Thoughts makes its target discard as many
/// cards as the sacrificed creature's power — not one card, and not the power
/// of another creature its caster controls.
#[test]
fn tormented_thoughts_discards_the_sacrificed_creatures_power() {
    let (runner, sacrificed, bystander) = tormented_thoughts(5);
    // Reach guard: the cost was paid and the spell's discard ran.
    assert_eq!(zone(&runner, sacrificed), Zone::Graveyard);
    assert!(graveyard_len(&runner, P1) >= 1, "P1 discarded something");

    assert_eq!(hand_len(&runner, P1), 2, "5 cards − the 3/1's power");
    assert_eq!(graveyard_len(&runner, P1), 3);
    assert_eq!(zone(&runner, bystander), Zone::Battlefield);

    // A hand smaller than the count discards all of it.
    let (runner, _, _) = tormented_thoughts(2);
    assert_eq!(hand_len(&runner, P1), 0);
    assert_eq!(graveyard_len(&runner, P1), 2);
}

/// Taster of Wares on the battlefield-bound stack with one other Goblin and one
/// non-Goblin under P0, P1 holding `p1_hand` cards and P0 holding two.
/// Returns the runner at the ETB trigger's reveal prompt and P1's hand.
fn taster_at_reveal(p1_hand: usize) -> (GameRunner, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let taster = scenario
        .add_creature_to_hand_from_oracle(P0, "Taster of Wares", 3, 2, TASTER_OF_WARES)
        .with_subtypes(vec!["Goblin", "Warlock"])
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario
        .add_creature(P0, "Other Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"]);
    scenario
        .add_creature(P0, "Not a Goblin", 2, 2)
        .with_subtypes(vec!["Elf"]);
    let p1_cards: Vec<ObjectId> = (0..p1_hand)
        .map(|index| scenario.add_card_to_hand(P1, &format!("P1 Card {index}")))
        .collect();
    let p0_cards: Vec<ObjectId> = (0..2)
        .map(|index| scenario.add_card_to_hand(P0, &format!("P0 Card {index}")))
        .collect();
    let mut runner = scenario.build();
    let outcome = runner.cast(taster).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::RevealChoice { .. }),
        "Taster's ETB must reach its reveal prompt: {:?}",
        outcome.final_waiting_for()
    );
    (runner, p1_cards, p0_cards)
}

/// CR 701.20a + CR 608.2h: Taster of Wares makes the target opponent reveal
/// exactly X cards (X = Goblins its controller controls, Taster included), and
/// the exiled card is one of them.
#[test]
fn taster_of_wares_reveals_exactly_x_and_exiles_one_of_them() {
    let (mut runner, p1_cards, p0_cards) = taster_at_reveal(5);
    let WaitingFor::RevealChoice { player, cards, .. } = runner.state().waiting_for.clone() else {
        unreachable!("taster_at_reveal checked the prompt");
    };
    // Reach guard: P0 chooses among the revealed cards.
    assert_eq!(player, P0);
    assert_eq!(
        cards.len(),
        2,
        "X = Taster + one other Goblin; the Elf does not count"
    );
    assert!(
        cards.iter().all(|card| p1_cards.contains(card)),
        "{cards:?}"
    );
    assert!(cards.iter().all(|card| !p0_cards.contains(card)));

    let chosen = cards[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("choosing a revealed card must be accepted");
    // "You choose one of those cards": the choice is among the revealed cards.
    let WaitingFor::ChooseFromZoneChoice {
        player,
        cards: offered,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the choose prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        offered.iter().all(|card| cards.contains(card)),
        "{offered:?} ⊄ {cards:?}"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("choosing one of those cards must be accepted");
    runner.advance_until_stack_empty();
    assert_eq!(zone(&runner, chosen), Zone::Exile);
    assert_eq!(hand_len(&runner, P1), 4);

    // X greater than the hand reveals the whole (one-card) hand.
    let (runner, p1_cards, _) = taster_at_reveal(1);
    let WaitingFor::RevealChoice { cards, .. } = &runner.state().waiting_for else {
        unreachable!("taster_at_reveal checked the prompt");
    };
    assert_eq!(*cards, p1_cards);
}

/// CR 119.3: Stabwhisker's "1 life for each card fewer than three in their
/// hand" is a count the parser cannot read, so the upkeep trigger resolves as a
/// gap instead of a literal one-life loss.
#[test]
fn stabwhisker_unparsed_per_card_loss_fails_closed() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let stabwhisker = scenario
        .add_creature_from_oracle(P0, "Stabwhisker the Odious", 3, 3, STABWHISKER)
        .id();
    scenario.add_card_to_hand(P1, "Only Card");
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_upkeep();

    // Reach guard: the opponent's-upkeep trigger is on the stack.
    let stack = &runner.state().stack;
    assert_eq!(stack.len(), 1, "{stack:?}");
    assert_eq!(stack[0].source_id, stabwhisker);
    for _ in 0..10 {
        if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert!(runner.state().stack.is_empty(), "the trigger resolved");
    assert_eq!(runner.life(P1), 20);
}

/// CR 111.1: a token count whose for-each quantity no parser reads creates no
/// token (synthetic sorcery; base created one Goat).
#[test]
fn token_for_each_unparsed_creates_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Goat Gorbleflux", false, GOAT_FOR_EACH_UNREAD)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let before = runner.battlefield_count(P0);
    let _ = runner.cast(spell).resolve();
    // Reach guard: the spell resolved.
    assert_eq!(zone(&runner, spell), Zone::Graveyard);
    assert_eq!(runner.battlefield_count(P0), before);
    assert!(tokens_named(&runner, "Goat").is_empty());
}

/// CR 510.2 + CR 119.3: Essence Sliver's controller gains the damage its Sliver
/// dealt ("that much life"), not one life.
#[test]
fn essence_sliver_controller_gains_the_damage_dealt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let sliver = scenario
        .add_creature_from_oracle(P0, "Essence Sliver", 3, 3, ESSENCE_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(sliver, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("attacking with Essence Sliver must succeed");
    for _ in 0..30 {
        if runner.act(GameAction::PassPriority).is_err() || runner.state().phase == Phase::End {
            break;
        }
    }
    // Reach guard: the damage event the trigger reads happened.
    assert_eq!(runner.life(P1), 17);
    assert_eq!(runner.life(P0), 23);
}

/// Shilgengar with P0 controlling `fodder`, activated sacrificing it.
fn shilgengar_bloods(fodder_subtype: &str, toughness: i32) -> Vec<bool> {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shilgengar = scenario
        .add_creature(P0, "Shilgengar, Sire of Famine", 6, 6)
        .from_oracle_text_with_keywords(&["Flying"], SHILGENGAR)
        .id();
    let fodder = scenario
        .add_creature(P0, "Fodder", 2, toughness)
        .with_subtypes(vec![fodder_subtype])
        .id();
    let mut runner = scenario.build();
    runner.activate(shilgengar, 0).pay_with(&[fodder]).resolve();
    runner.advance_until_stack_empty();
    // Reach guard: the sacrifice cost was paid.
    assert_eq!(zone(&runner, fodder), Zone::Graveyard);
    tokens_named(&runner, "Blood")
}

/// CR 614.1a + CR 608.2k: sacrificing an Angel replaces the one Blood with as
/// many Bloods as the sacrificed Angel's toughness.
#[test]
fn shilgengar_angel_instead_creates_toughness_bloods() {
    assert_eq!(shilgengar_bloods("Angel", 4).len(), 4);
    assert_eq!(shilgengar_bloods("Human", 4).len(), 1);
}

/// CR 603.10a: Goldvein Hydra's dies trigger reads its last-known power and
/// creates that many tapped Treasures.
#[test]
fn goldvein_hydra_dies_creates_power_treasures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = scenario
        .add_creature(P0, "Goldvein Hydra", 5, 5)
        .from_oracle_text_with_keywords(&["Vigilance", "Trample", "Haste"], GOLDVEIN_HYDRA)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P0, "Removal Probe", true, DESTROY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(destroy).target_object(hydra).resolve();
    runner.advance_until_stack_empty();
    // Reach guard: the Hydra died.
    assert_eq!(zone(&runner, hydra), Zone::Graveyard);
    let treasures = tokens_named(&runner, "Treasure");
    assert_eq!(treasures.len(), 5);
    assert!(treasures.iter().all(|tapped| *tapped));
}

/// CR 608.2k + CR 113.7a: Threefold Thunderhulk's "its power" is the
/// Thunderhulk's own power. Attacking beside another creature, the attack event
/// names both attackers, and the Thunderhulk still creates as many Gnomes as its
/// power.
#[test]
fn threefold_thunderhulk_attacking_beside_another_creature_creates_power_gnomes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let hulk = scenario
        .add_creature_from_oracle(P0, "Threefold Thunderhulk", 0, 0, THREEFOLD_THUNDERHULK)
        .id();
    scenario.with_counter(hulk, CounterType::Plus1Plus1, 4);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (hulk, AttackTarget::Player(P1)),
                (bear, AttackTarget::Player(P1)),
            ],
            bands: vec![],
        })
        .expect("attacking with the Thunderhulk and the Bear must succeed");
    // Reach guard: the attack trigger is on the stack and the Thunderhulk's
    // power is 4.
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().objects[&hulk].power, Some(4));
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(tokens_named(&runner, "Gnome").len(), 4);
}

/// CR 614.1c + CR 608.2k: the Thunderhulk enters with three +1/+1 counters, and
/// its enters trigger creates three Gnomes.
#[test]
fn threefold_thunderhulk_enters_creates_power_gnomes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hulk = scenario
        .add_creature_to_hand_from_oracle(P0, "Threefold Thunderhulk", 0, 0, THREEFOLD_THUNDERHULK)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let _ = runner.cast(hulk).resolve();
    runner.advance_until_stack_empty();
    // Reach guard: the Thunderhulk is on the battlefield with its three counters.
    assert_eq!(zone(&runner, hulk), Zone::Battlefield);
    assert_eq!(runner.state().objects[&hulk].power, Some(3));
    assert_eq!(tokens_named(&runner, "Gnome").len(), 3);
}
