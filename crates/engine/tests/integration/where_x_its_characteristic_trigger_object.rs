//! #7851: in a triggered ability whose trigger condition names an object other
//! than the ability's source, "where X is its power/toughness" reads THAT object.
//!
//! CR 608.2k: the effect's "it"/"its" names the object the trigger condition
//! referred to — the dying enchanted/equipped creature, the attacking Cat — not
//! the Aura, Equipment or Arahbo that carries the ability.
//! CR 608.2h + CR 603.10a: a dies trigger looks back in time, so the dying
//! creature's power/toughness is its last known information on the battlefield
//! (every dies-trigger fixture adds a +1/+1 counter, which ceases to exist in
//! the graveyard, so a live read and the LKI read differ).
//! CR 107.1b: Arahbo's X feeds a signed "+X/+X"; a negative attacking Cat gives
//! X = 0 (Arahbo ruling: "If the attacking Cat's power is negative at that time,
//! X is 0.").
//! CR 118.3 + CR 119.4: Mask of Griselbrand's "pay X life" is X or nothing; a
//! player with less life than X cannot pay it, so the optional payment is never
//! offered.
//!
//! Every card under test is built from its verbatim Oracle text. Each drive
//! answers the engine's prompts with `GameAction`s, panics on any prompt it was
//! not told to expect, and ends at an empty stack with `Priority`.

use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const MASK_OF_GRISELBRAND: &str = "Equipped creature has flying and lifelink.\n\
Whenever equipped creature dies, you may pay X life, where X is its power. If you do, draw X cards.\n\
Equip {3}";
const DYING_WISH: &str = "Enchant creature you control\n\
When enchanted creature dies, target player loses X life and you gain X life, where X is its power.";
const FRUIT_OF_THE_FIRST_TREE: &str = "Enchant creature\n\
When enchanted creature dies, you gain X life and draw X cards, where X is its toughness.";
const MURDER_INVESTIGATION: &str = "Enchant creature you control\n\
When enchanted creature dies, create X 1/1 white Soldier creature tokens, where X is its power.";
const NECROSYNTHESIS: &str = "Enchant creature\n\
Enchanted creature has \"Whenever another creature dies, put a +1/+1 counter on this creature.\"\n\
When enchanted creature dies, look at the top X cards of your library, where X is its power. Put one of those cards into your hand and the rest on the bottom of your library in a random order.";
const ARAHBO: &str = "Eminence — At the beginning of combat on your turn, if Arahbo is in the command zone or on the battlefield, another target Cat you control gets +3/+3 until end of turn.\n\
Whenever another Cat you control attacks, you may pay {1}{G}{W}. If you do, it gains trample and gets +X/+X until end of turn, where X is its power.";

/// How a bounded drive answers the engine's prompts.
struct Answers {
    /// The answer to every optional-effect prompt; `None` means the prompt must
    /// never appear.
    optional: Option<bool>,
    /// Targets for trigger target slots, in order.
    targets: Vec<TargetRef>,
}

/// What a bounded drive observed.
#[derive(Default)]
struct Observed {
    optional_offers: usize,
    /// The cards each dig prompt offered.
    dig_offers: Vec<Vec<ObjectId>>,
}

/// Answer prompts until the stack is empty and the engine waits on `Priority`.
/// A dig prompt keeps the first offered card.
fn drive(runner: &mut GameRunner, answers: Answers) -> Observed {
    let mut observed = Observed::default();
    let mut targets = answers.targets.into_iter();
    for _ in 0..200 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let target = targets.next().expect("a target was declared for this slot");
                runner.act(GameAction::ChooseTarget {
                    target: Some(target),
                })
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                observed.optional_offers += 1;
                let accept = answers
                    .optional
                    .expect("CR 118.3 + CR 119.4: this optional payment must not be offered");
                runner.act(GameAction::DecideOptionalEffect { accept })
            }
            WaitingFor::ManaPayment { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DigChoice { cards, .. } => {
                observed.dig_offers.push(cards.clone());
                runner.act(GameAction::SelectCards {
                    cards: vec![cards[0]],
                })
            }
            WaitingFor::DigBottomOrder { cards, .. } => {
                runner.act(GameAction::SelectCards { cards })
            }
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return observed,
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            other => panic!("unexpected wait {other:?}"),
        };
        result.expect("the engine accepts the action");
    }
    panic!("the game did not settle");
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

fn has_trample(runner: &mut GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    runner.state().objects[&id].has_keyword(&Keyword::Trample)
}

fn add_library(scenario: &mut GameScenario, player: PlayerId, count: usize) -> Vec<ObjectId> {
    (0..count)
        .map(|i| scenario.add_card_to_library_top(player, &format!("Library {i}")))
        .collect()
}

/// A dies-trigger board: `host` (a creature with one +1/+1 counter) under
/// `host_owner`, an Aura from `aura_text` under P0 attached to it, a Bolt in
/// P0's hand and a five-card library for P0.
struct AuraBoard {
    runner: GameRunner,
    host: ObjectId,
    bolt: ObjectId,
    library: Vec<ObjectId>,
}

fn aura_board(
    aura_name: &str,
    aura_text: &str,
    host_owner: PlayerId,
    host_pt: (i32, i32),
) -> AuraBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario
        .add_creature(host_owner, "Host", host_pt.0, host_pt.1)
        .with_plus_counters(1)
        .id();
    // The subtype is set before the Oracle text is parsed, as card data does.
    let aura = scenario
        .add_enchantment_from_oracle(P0, aura_name, aura_text)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text(aura_text)
        .id();
    let library = add_library(&mut scenario, P0, 5);
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    attach_to(runner.state_mut(), aura, host);
    AuraBoard {
        runner,
        host,
        bolt,
        library,
    }
}

/// P0 casts Bolt on `host`; returns P0's hand size right after the cast.
fn bolt(runner: &mut GameRunner, bolt: ObjectId, host: ObjectId) -> usize {
    runner.cast(bolt).target_objects(&[host]).commit();
    hand_len(runner, P0)
}

/// A Mask of Griselbrand board: Mask equipped (through its Equip ability, paid
/// with three lands) to P0's 2/2 with one +1/+1 counter; P0 at `p0_life` with
/// a Bolt and a five-card library.
fn mask_board(p0_life: i32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, p0_life);
    let creature = scenario
        .add_creature(P0, "Equipped Bear", 2, 2)
        .with_plus_counters(1)
        .id();
    let mask = scenario
        .add_artifact_from_oracle(P0, "Mask of Griselbrand", MASK_OF_GRISELBRAND)
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(MASK_OF_GRISELBRAND)
        .id();
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    add_library(&mut scenario, P0, 5);
    let bolt = scenario.add_bolt_to_hand(P0);
    let mut runner = scenario.build();
    let equip = runner.state().objects[&mask]
        .abilities
        .iter()
        .position(|ability| matches!(*ability.effect, Effect::Attach { .. }))
        .expect("Mask of Griselbrand has its Equip ability");
    runner
        .activate(mask, equip)
        .target_object(creature)
        .resolve();
    assert_eq!(
        runner.state().objects[&creature].attachments,
        vec![mask],
        "reach guard: the Mask is equipped through its Equip ability"
    );
    (runner, creature, bolt)
}

/// R1 — CR 608.2k + CR 608.2h + CR 603.10a: Mask of Griselbrand pays and draws
/// the dying equipped creature's last-known power (3: a 2/2 with a +1/+1
/// counter), not the Equipment's (0). Base: pays 0 and draws 0.
#[test]
fn mask_of_griselbrand_pays_and_draws_equipped_creature_lki_power() {
    let (mut runner, creature, bolt_id) = mask_board(20);
    let hand_after_cast = bolt(&mut runner, bolt_id, creature);
    let observed = drive(
        &mut runner,
        Answers {
            optional: Some(true),
            targets: vec![],
        },
    );

    assert_eq!(observed.optional_offers, 1, "the payment is offered");
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "reach guard: the equipped creature died"
    );
    assert_eq!(
        runner.state().objects[&creature].power,
        Some(2),
        "LKI discriminator: in the graveyard the counter is gone"
    );
    assert_eq!(life(&runner, P0), 17, "pay X = 3 life");
    assert_eq!(hand_len(&runner, P0) - hand_after_cast, 3, "draw X = 3");
}

/// R1b — paired control for R1 (passes at base by design): declining the
/// payment pays nothing and draws nothing.
#[test]
fn mask_of_griselbrand_decline_pays_and_draws_nothing() {
    let (mut runner, creature, bolt_id) = mask_board(20);
    let hand_after_cast = bolt(&mut runner, bolt_id, creature);
    let observed = drive(
        &mut runner,
        Answers {
            optional: Some(false),
            targets: vec![],
        },
    );

    assert_eq!(observed.optional_offers, 1, "reach guard: offered");
    assert_eq!(life(&runner, P0), 20);
    assert_eq!(hand_len(&runner, P0), hand_after_cast);
}

/// R1c — CR 118.3 + CR 119.4 (Mask ruling: "You must pay exactly X life or
/// nothing."): at 2 life, X = 3 cannot be paid, so the optional payment is
/// never offered and nothing is drawn. Base: X = 0 is affordable and offered.
#[test]
fn mask_of_griselbrand_unaffordable_x_life_is_not_offered() {
    let (mut runner, creature, bolt_id) = mask_board(2);
    let hand_after_cast = bolt(&mut runner, bolt_id, creature);
    let observed = drive(
        &mut runner,
        Answers {
            optional: None,
            targets: vec![],
        },
    );

    assert_eq!(observed.optional_offers, 0);
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Graveyard,
        "reach guard: the equipped creature died"
    );
    assert_eq!(life(&runner, P0), 2);
    assert_eq!(hand_len(&runner, P0), hand_after_cast);
}

/// R2 — CR 608.2k + CR 608.2h: Dying Wish's X is the enchanted creature's
/// last-known power (3). The target player (P1) loses 3, P0 gains 3.
#[test]
fn dying_wish_drains_enchanted_creature_lki_power() {
    let AuraBoard {
        mut runner,
        host,
        bolt: bolt_id,
        ..
    } = aura_board("Dying Wish", DYING_WISH, P0, (2, 2));
    bolt(&mut runner, bolt_id, host);
    drive(
        &mut runner,
        Answers {
            optional: None,
            targets: vec![TargetRef::Player(P1)],
        },
    );

    assert_eq!(runner.state().objects[&host].zone, Zone::Graveyard);
    assert_eq!(life(&runner, P1), 17, "target player loses X = 3");
    assert_eq!(life(&runner, P0), 23, "you gain X = 3");
}

/// R3 — CR 608.2k + CR 608.2h: Fruit of the First Tree reads the enchanted
/// creature's last-known TOUGHNESS (a 1/2 with a counter: 3). A power misread
/// gives 2; a live graveyard read gives 2.
#[test]
fn fruit_of_the_first_tree_reads_enchanted_creature_lki_toughness() {
    let AuraBoard {
        mut runner,
        host,
        bolt: bolt_id,
        ..
    } = aura_board(
        "Fruit of the First Tree",
        FRUIT_OF_THE_FIRST_TREE,
        P0,
        (1, 2),
    );
    let hand_after_cast = bolt(&mut runner, bolt_id, host);
    drive(
        &mut runner,
        Answers {
            optional: None,
            targets: vec![],
        },
    );

    assert_eq!(runner.state().objects[&host].zone, Zone::Graveyard);
    assert_eq!(life(&runner, P0), 23, "gain X = 3");
    assert_eq!(hand_len(&runner, P0) - hand_after_cast, 3, "draw X = 3");
}

/// R3b — Fruit ruling: "Fruit of the First Tree can enchant any creature, but
/// Fruit of the First Tree's controller will gain life and draw cards." P0's
/// Fruit on P1's creature: P0 gains 3 and draws 3; P1 is unchanged.
#[test]
fn fruit_of_the_first_tree_on_opponents_creature_rewards_aura_controller() {
    let AuraBoard {
        mut runner,
        host,
        bolt: bolt_id,
        ..
    } = aura_board(
        "Fruit of the First Tree",
        FRUIT_OF_THE_FIRST_TREE,
        P1,
        (1, 2),
    );
    let p1_hand = hand_len(&runner, P1);
    let hand_after_cast = bolt(&mut runner, bolt_id, host);
    drive(
        &mut runner,
        Answers {
            optional: None,
            targets: vec![],
        },
    );

    assert_eq!(runner.state().objects[&host].zone, Zone::Graveyard);
    assert_eq!(life(&runner, P0), 23);
    assert_eq!(hand_len(&runner, P0) - hand_after_cast, 3);
    assert_eq!(life(&runner, P1), 20);
    assert_eq!(hand_len(&runner, P1), p1_hand);
}

/// R4 — CR 608.2k + CR 608.2h (Murder Investigation ruling: "use the power of
/// the enchanted creature as it last existed on the battlefield"): exactly 3
/// Soldier tokens.
#[test]
fn murder_investigation_creates_tokens_for_enchanted_creature_lki_power() {
    let AuraBoard {
        mut runner,
        host,
        bolt: bolt_id,
        ..
    } = aura_board("Murder Investigation", MURDER_INVESTIGATION, P0, (2, 2));
    bolt(&mut runner, bolt_id, host);
    drive(
        &mut runner,
        Answers {
            optional: None,
            targets: vec![],
        },
    );

    let state = runner.state();
    assert_eq!(state.objects[&host].zone, Zone::Graveyard, "reach guard");
    let soldiers = state
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &state.objects[*id];
            obj.is_token
                && obj.controller == P0
                && obj.card_types.subtypes.iter().any(|s| s == "Soldier")
        })
        .count();
    assert_eq!(soldiers, 3);
}

/// R5 — CR 608.2k + CR 608.2h (Necrosynthesis ruling: "Use the enchanted
/// creature's power as it last existed on the battlefield"): the dig looks at
/// exactly the top 3 cards; one goes to hand, the other two to the bottom.
/// Base: X = 0, no dig prompt.
#[test]
fn necrosynthesis_looks_at_enchanted_creature_lki_power_cards() {
    let AuraBoard {
        mut runner,
        host,
        bolt: bolt_id,
        library,
    } = aura_board("Necrosynthesis", NECROSYNTHESIS, P0, (2, 2));
    bolt(&mut runner, bolt_id, host);
    let observed = drive(
        &mut runner,
        Answers {
            optional: None,
            targets: vec![],
        },
    );

    // `add_card_to_library_top` puts each card on top: the last three added
    // are the top three.
    let mut top_three = library[2..].to_vec();
    top_three.sort();
    assert_eq!(observed.dig_offers.len(), 1, "one dig prompt");
    let mut offered = observed.dig_offers[0].clone();
    offered.sort();
    assert_eq!(offered, top_three, "the top X = 3 cards are looked at");

    let kept = observed.dig_offers[0][0];
    let state = runner.state();
    assert_eq!(state.objects[&kept].zone, Zone::Hand);
    let rest: Vec<ObjectId> = top_three.iter().copied().filter(|&c| c != kept).collect();
    let deck: Vec<ObjectId> = state.players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    assert_eq!(deck.len(), 4);
    let mut bottom = deck[2..].to_vec();
    bottom.sort();
    assert_eq!(bottom, rest, "the rest go to the bottom");
}

/// An Arahbo board: Arahbo (5/5 Cat Avatar) and `cats` vanilla Cats under P0,
/// with lands for `payments` {1}{G}{W} payments (a Forest, a Plains and a
/// second Forest per payment, so every payment is colour-feasible).
fn arahbo_board(cats: &[(i32, i32, u32)], payments: usize) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Arahbo, Roar of the World", 5, 5)
        .with_subtypes(vec!["Cat", "Avatar"])
        .from_oracle_text(ARAHBO);
    let cat_ids = cats
        .iter()
        .enumerate()
        .map(|(i, &(power, toughness, minus))| {
            scenario
                .add_creature(P0, &format!("Cat {i}"), power, toughness)
                .with_subtypes(vec!["Cat"])
                .with_minus_counters(minus)
                .id()
        })
        .collect();
    for _ in 0..payments {
        scenario.add_basic_land(P0, ManaColor::Green);
        scenario.add_basic_land(P0, ManaColor::White);
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    (scenario.build(), cat_ids)
}

/// Advance to the point where attackers are declared. `advance_to_combat`
/// stops when the engine waits on anything but `Priority`, so an Eminence
/// target prompt is answered with `eminence_target`, the Eminence trigger is
/// resolved by passing priority, and the advance resumes.
fn advance_to_declare_attackers(runner: &mut GameRunner, eminence_target: ObjectId) {
    for _ in 0..16 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::TriggerTargetSelection { .. } => runner.act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(eminence_target)),
            }),
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority)
            }
            // CR 507.2: the empty beginning-of-combat priority window; the
            // runner's `declare_attackers` passes it before declaring.
            WaitingFor::Priority { .. } if runner.state().phase == Phase::BeginCombat => return,
            WaitingFor::Priority { .. } => {
                runner.advance_to_combat();
                continue;
            }
            other => panic!("unexpected wait before attackers: {other:?}"),
        };
        result.expect("the engine accepts the action");
    }
    panic!(
        "declare attackers was not reached: {:?} in {:?}",
        runner.state().waiting_for,
        runner.state().phase
    );
}

fn attack(runner: &mut GameRunner, attackers: &[ObjectId]) {
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("the attack is accepted");
}

/// Pass the rest of the turn's priority windows up to the end step.
fn advance_to_end_step(runner: &mut GameRunner) {
    for _ in 0..64 {
        if runner.state().phase == Phase::End {
            return;
        }
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            other => panic!("unexpected wait before the end step: {other:?}"),
        };
        result.unwrap_or_else(|e| {
            panic!(
                "pass rejected at {:?}/{:?}: {e:?}",
                runner.state().phase,
                runner.state().waiting_for
            )
        });
    }
    panic!("the end step was not reached");
}

/// R6 — CR 608.2k + CR 608.2h (Arahbo ruling: "The value of X is determined
/// only as Arahbo's last ability resolves."): X is the attacking Cat's power
/// (6 after Eminence), not Arahbo's (5): a 3/2 Cat becomes 6/5, then 12/11 with
/// trample, and stays so for the turn. Base: 11/10.
#[test]
fn arahbo_pumps_attacking_cat_by_its_own_power() {
    let (mut runner, cats) = arahbo_board(&[(3, 2, 0)], 1);
    let cat = cats[0];
    advance_to_declare_attackers(&mut runner, cat);
    assert_eq!(
        pt(&mut runner, cat),
        (6, 5),
        "reach guard: Eminence resolved"
    );

    attack(&mut runner, &[cat]);
    let observed = drive(
        &mut runner,
        Answers {
            optional: Some(true),
            targets: vec![],
        },
    );

    assert_eq!(observed.optional_offers, 1);
    assert!(
        has_trample(&mut runner, cat),
        "reach guard: the payment was made"
    );
    assert_eq!(pt(&mut runner, cat), (12, 11));
    advance_to_end_step(&mut runner);
    assert_eq!(
        pt(&mut runner, cat),
        (12, 11),
        "the bonus lasts until end of turn"
    );
}

/// R6b — Arahbo ruling: "If multiple Cats attack, Arahbo's last ability
/// triggers that many times." Each resolution reads its own attacking Cat:
/// A (3/2, Eminence target, 6/5) becomes 12/11 and B (1/1) becomes 2/2.
/// Base: A 11/10, B 6/6.
#[test]
fn arahbo_pumps_each_attacking_cat_by_its_own_power() {
    let (mut runner, cats) = arahbo_board(&[(3, 2, 0), (1, 1, 0)], 2);
    let (a, b) = (cats[0], cats[1]);
    advance_to_declare_attackers(&mut runner, a);
    assert_eq!(pt(&mut runner, a), (6, 5), "reach guard: Eminence on A");
    assert_eq!(pt(&mut runner, b), (1, 1));

    attack(&mut runner, &[a, b]);
    let observed = drive(
        &mut runner,
        Answers {
            optional: Some(true),
            targets: vec![],
        },
    );

    assert_eq!(observed.optional_offers, 2, "one trigger per attacking Cat");
    assert_eq!(pt(&mut runner, a), (12, 11));
    assert_eq!(pt(&mut runner, b), (2, 2));
    assert!(has_trample(&mut runner, a));
    assert!(has_trample(&mut runner, b));
}

/// R6c — paired control for R6 (passes at base by design): declining the
/// payment leaves the Cat at its Eminence size, without trample.
#[test]
fn arahbo_decline_leaves_cat_unpumped() {
    let (mut runner, cats) = arahbo_board(&[(3, 2, 0)], 1);
    let cat = cats[0];
    advance_to_declare_attackers(&mut runner, cat);
    attack(&mut runner, &[cat]);
    let observed = drive(
        &mut runner,
        Answers {
            optional: Some(false),
            targets: vec![],
        },
    );

    assert_eq!(observed.optional_offers, 1, "reach guard: offered");
    assert_eq!(pt(&mut runner, cat), (6, 5));
    assert!(!has_trample(&mut runner, cat));
}

/// R6d — CR 107.1b (Arahbo ruling: "If the attacking Cat's power is negative
/// at that time, X is 0."): a 1/9 Cat with five -1/-1 counters is -4/4, -1/7
/// after Eminence; paying gives X = 0, so it stays -1/7 and gains trample.
/// Unclamped X = -1 would give -2/6; base (Arahbo's power, 5) gives 4/12.
#[test]
fn arahbo_negative_power_cat_gets_x_zero() {
    let (mut runner, cats) = arahbo_board(&[(1, 9, 5)], 1);
    let cat = cats[0];
    advance_to_declare_attackers(&mut runner, cat);
    assert_eq!(
        pt(&mut runner, cat),
        (-1, 7),
        "reach guard: Eminence resolved"
    );

    attack(&mut runner, &[cat]);
    let observed = drive(
        &mut runner,
        Answers {
            optional: Some(true),
            targets: vec![],
        },
    );

    assert_eq!(observed.optional_offers, 1);
    assert!(
        has_trample(&mut runner, cat),
        "reach guard: the payment was made"
    );
    assert_eq!(pt(&mut runner, cat), (-1, 7));
}
