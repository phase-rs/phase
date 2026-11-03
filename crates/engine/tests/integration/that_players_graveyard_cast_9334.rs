//! Issue #9334 — an untargeted free "you may cast a <type> spell from <a
//! graveyard> without paying its mana cost" names no card.
//!
//! `cast_from_zone::resolve` cast only the objects it was handed, and this
//! clause hands it none, so the grant did nothing: no graveyard card was
//! offered, neither the target opponent's (Sorcerous Squall), the caster's own
//! (Diviner of Mist, Counterpoint, Jarsyl) nor any graveyard's (Spectral
//! Arcanist). And Squall's filter named no owner, so "that player's graveyard"
//! would have matched any graveyard (CR 400.3).
//!
//! Fix: the card is chosen among the graveyard cards the filter admits and cast
//! while the ability resolves (CR 608.2g), through the same resolution pick a
//! free cast "from your hand" uses; Squall's "that player" is the opponent its
//! first clause targeted (CR 608.2c). With nothing castable the "you may" is
//! not offered (CR 608.2d), and an "If you do" instruction follows the cast.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{
    CastPaymentMode, CastingVariant, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SQUALL: &str = "Target opponent mills nine cards, then you may cast an instant or sorcery \
spell from that player's graveyard without paying its mana cost. If that spell would be put \
into a graveyard, exile it instead.";

const DIVINER_OF_MIST: &str = "Flying\nWhenever this creature attacks, mill four cards. You may \
cast an instant or sorcery spell from your graveyard with mana value 4 or less without paying \
its mana cost. If that spell would be put into your graveyard, exile it instead.";

const COUNTERPOINT: &str = "Counter target spell. You may cast a creature, instant, sorcery, or \
planeswalker spell from your graveyard with mana value less than or equal to that spell's mana \
value without paying its mana cost.";

/// Cast `spell` from P0's hand with no mana, answering a target prompt with
/// `target` and accepting the "you may", until the stack is empty or the
/// resolution pick opens. Returns the pick's offered cards, if it opened.
fn cast_and_reach_pick(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: Option<TargetRef>,
) -> Option<Vec<ObjectId>> {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");
    reach_pick(runner, target)
}

/// Answer prompts (a player or object target with `target`, the "you may" with
/// yes) until the stack is empty or the resolution pick opens. Returns the
/// pick's offered cards, if it opened.
fn reach_pick(runner: &mut GameRunner, target: Option<TargetRef>) -> Option<Vec<ObjectId>> {
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return None,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: target.clone(),
                    })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the cast");
            }
            WaitingFor::EffectZoneChoice {
                cards,
                zone: Zone::Graveyard,
                ..
            } => return Some(cards),
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the spell neither resolved nor opened the pick");
}

/// Pass priority until the stack is empty.
fn drain(runner: &mut GameRunner) {
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while draining: {other:?}"),
        }
    }
    panic!("the stack did not empty");
}

struct Squall {
    runner: GameRunner,
    theirs: ObjectId,
    mine: ObjectId,
    squall: ObjectId,
}

fn squall_board() -> Squall {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..12 {
        scenario.add_card_to_library_top(P1, &format!("Filler {i}"));
    }
    let theirs = scenario
        .add_spell_to_graveyard(P1, "Their Instant", true)
        .id();
    let mine = scenario.add_spell_to_graveyard(P0, "My Instant", true).id();
    let squall = scenario
        .add_spell_to_hand_from_oracle(P0, "Sorcerous Squall", false, SQUALL)
        .with_mana_cost(ManaCost::default())
        .id();
    Squall {
        runner: scenario.build(),
        theirs,
        mine,
        squall,
    }
}

#[test]
fn squall_casts_a_spell_from_the_target_opponents_graveyard_only() {
    let mut b = squall_board();
    let library_before = b.runner.state().players[1].library.len();
    let offered = cast_and_reach_pick(&mut b.runner, b.squall, Some(TargetRef::Player(P1)))
        .expect("the resolution pick opens");
    assert_eq!(
        b.runner.state().players[1].library.len(),
        library_before - 9,
        "the target opponent milled nine cards"
    );
    assert!(
        offered.contains(&b.theirs),
        "the opponent's instant is offered"
    );
    assert!(
        !offered.contains(&b.mine),
        "CR 400.3: the caster's own graveyard is not that player's graveyard"
    );

    b.runner
        .act(GameAction::SelectCards {
            cards: vec![b.theirs],
        })
        .expect("SelectCards accepted");
    assert_eq!(
        b.runner.state().objects[&b.theirs].zone,
        Zone::Stack,
        "CR 608.2g: the chosen card is cast while Squall resolves"
    );
    drain(&mut b.runner);
    assert_eq!(
        b.runner.state().objects[&b.theirs].zone,
        Zone::Exile,
        "CR 614.1a: the spell cast this way is exiled instead of going to a graveyard"
    );
    assert_eq!(b.runner.state().objects[&b.mine].zone, Zone::Graveyard);
}

#[test]
fn squall_declined_pick_casts_nothing() {
    let mut b = squall_board();
    let offered = cast_and_reach_pick(&mut b.runner, b.squall, Some(TargetRef::Player(P1)))
        .expect("the resolution pick opens");
    assert!(
        offered.contains(&b.theirs),
        "reach guard: the pick offered the card"
    );
    b.runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("an empty pick declines");
    drain(&mut b.runner);
    assert_eq!(
        b.runner.state().objects[&b.theirs].zone,
        Zone::Graveyard,
        "nothing was cast"
    );
}

#[test]
fn diviner_of_mist_picks_from_its_controllers_graveyard_under_the_mana_value_gate() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Own Filler {i}"));
    }
    let small = scenario
        .add_spell_to_graveyard(P0, "Small Instant", true)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let big = scenario
        .add_spell_to_graveyard(P0, "Big Instant", true)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let theirs = scenario
        .add_spell_to_graveyard(P1, "Their Instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let diviner = scenario
        .add_creature_from_oracle(P0, "Diviner of Mist", 0, 4, DIVINER_OF_MIST)
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(diviner, AttackTarget::Player(P1))])
        .expect("DeclareAttackers accepted");
    let offered = reach_pick(&mut runner, None).expect("the resolution pick opens");
    assert_eq!(
        runner.state().players[0].graveyard.len(),
        6,
        "the trigger milled four cards before the pick"
    );
    assert!(offered.contains(&small), "mana value 4 is offered");
    assert!(!offered.contains(&big), "mana value 5 is not");
    assert!(
        !offered.contains(&theirs),
        "\"your graveyard\" does not reach the opponent's"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![small] })
        .expect("SelectCards accepted");
    assert_eq!(runner.state().objects[&small].zone, Zone::Stack);
    drain(&mut runner);
    assert_eq!(runner.state().objects[&small].zone, Zone::Exile);
}

const SPECTRAL_ARCANIST: &str = "Flying\nWhen this creature enters, you may cast an \
instant or sorcery spell with mana value less than or equal to the number of Spirits you control \
from a graveyard without paying its mana cost. If that spell would be put into a graveyard, exile \
it instead.";

/// "From a graveyard" reaches every graveyard (CR 400.1): the pick offers both
/// players' cards under the Spirit-count ceiling.
#[test]
fn spectral_arcanist_picks_from_any_graveyard_under_the_spirit_count() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario
        .add_spell_to_graveyard(P0, "My Instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let theirs = scenario
        .add_spell_to_graveyard(P1, "Their Instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let too_big = scenario
        .add_spell_to_graveyard(P1, "Their Big Instant", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let arcanist = scenario
        .add_creature_to_hand_from_oracle(P0, "Spectral Arcanist", 1, 2, SPECTRAL_ARCANIST)
        .with_subtypes(vec!["Spirit", "Wizard"])
        .with_mana_cost(ManaCost::default())
        .id();
    let mut runner = scenario.build();

    let offered =
        cast_and_reach_pick(&mut runner, arcanist, None).expect("the resolution pick opens");
    assert!(offered.contains(&mine), "the caster's own graveyard");
    assert!(offered.contains(&theirs), "the opponent's graveyard");
    assert!(
        !offered.contains(&too_big),
        "one Spirit: mana value 2 is too big"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![theirs],
        })
        .expect("SelectCards accepted");
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Stack);
    drain(&mut runner);
    assert_eq!(runner.state().objects[&theirs].zone, Zone::Exile);
}

/// Known-gap pin, not a correctness claim: Counterpoint's ceiling is the
/// countered spell's mana value (CR 608.2h), but its "that spell's mana value"
/// reads 0 here, so only a mana value 0 card is offered. The pick itself
/// opens, from the caster's graveyard only.
#[test]
fn counterpoint_pick_opens_but_its_ceiling_reads_zero() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let three = scenario
        .add_spell_to_graveyard(P0, "Three Instant", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let zero = scenario
        .add_spell_to_graveyard(P0, "Zero Instant", true)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let theirs = scenario
        .add_spell_to_graveyard(P1, "Their Zero Instant", true)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let counterpoint = scenario
        .add_spell_to_hand_from_oracle(P0, "Counterpoint", true, COUNTERPOINT)
        .with_mana_cost(ManaCost::default())
        .id();
    let mut runner = scenario.build();
    let foe = engine::game::zones::create_object(
        runner.state_mut(),
        CardId(77),
        P1,
        "Opponent Spell".to_string(),
        Zone::Stack,
    );
    runner.state_mut().objects.get_mut(&foe).unwrap().mana_cost = ManaCost::generic(3);
    runner.state_mut().stack.push_back(StackEntry {
        id: foe,
        source_id: foe,
        controller: P1,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    let offered = cast_and_reach_pick(&mut runner, counterpoint, Some(TargetRef::Object(foe)))
        .expect("the resolution pick opens");
    assert_eq!(
        runner.state().objects[&foe].zone,
        Zone::Graveyard,
        "countered"
    );
    assert_eq!(
        offered,
        vec![zero],
        "known gap: the ceiling reads 0, not 3 ({three:?})"
    );
    assert!(!offered.contains(&theirs), "\"your graveyard\" only");
}

const JARSYL: &str = "Starting intensity 1\nAt the beginning of combat on your turn, you may \
cast a spell with mana value equal to Jarsyl, Dark Age Scion's intensity from your graveyard \
without paying its mana cost. If you do, Jarsyl intensifies by 1.";

/// What the beginning-of-combat trigger asked: whether the "you may" was
/// offered, and the pick's cards if it opened.
struct BeginCombat {
    asked: bool,
    offered: Option<Vec<ObjectId>>,
    /// Jarsyl's intensity and the stack right after the pick was answered.
    after_pick: Option<(u32, Vec<ObjectId>)>,
}

/// Move from the precombat main phase through Jarsyl's trigger, answering its
/// "you may" with `accept` and its pick with `pick`.
fn run_jarsyl_trigger(
    runner: &mut GameRunner,
    jarsyl: ObjectId,
    accept: bool,
    pick: Vec<ObjectId>,
) -> BeginCombat {
    let mut seen = BeginCombat {
        asked: false,
        offered: None,
        after_pick: None,
    };
    for _ in 0..30 {
        let state = runner.state();
        let left_main = state.phase != Phase::PreCombatMain;
        match state.waiting_for.clone() {
            WaitingFor::Priority { .. } if left_main && state.stack.is_empty() => return seen,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                seen.asked = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("DecideOptionalEffect accepted");
            }
            WaitingFor::EffectZoneChoice {
                cards,
                zone: Zone::Graveyard,
                ..
            } => {
                seen.offered = Some(cards);
                runner
                    .act(GameAction::SelectCards {
                        cards: pick.clone(),
                    })
                    .expect("SelectCards accepted");
                let state = runner.state();
                seen.after_pick = Some((
                    state.objects[&jarsyl].intensity,
                    state.stack.iter().map(|entry| entry.id).collect(),
                ));
            }
            WaitingFor::DeclareAttackers { .. } => return seen,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    panic!("the trigger did not finish");
}

fn jarsyl_board(graveyard_mana_values: &[u32]) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let cards = graveyard_mana_values
        .iter()
        .enumerate()
        .map(|(i, mv)| {
            scenario
                .add_spell_to_graveyard(P0, &format!("Instant {i}"), true)
                .with_mana_cost(ManaCost::generic(*mv))
                .id()
        })
        .collect();
    let jarsyl = scenario
        .add_creature_from_oracle(P0, "Jarsyl, Dark Age Scion", 3, 3, JARSYL)
        .id();
    scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::default());
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&jarsyl)
        .unwrap()
        .intensity = 1;
    (runner, jarsyl, cards)
}

/// CR 608.2g: the pick offers exactly Jarsyl's intensity as the mana value
/// (CR 608.2h: the trigger reads its live source). Casting the picked card is
/// its "If you do" (CR 118.12), so Jarsyl intensifies while the trigger still
/// resolves — before the cast spell, the only object on the stack, resolves.
#[test]
fn jarsyl_casts_its_intensitys_mana_value_and_intensifies() {
    let (mut runner, jarsyl, cards) = jarsyl_board(&[1, 0, 2]);
    let seen = run_jarsyl_trigger(&mut runner, jarsyl, true, vec![cards[0]]);
    assert!(seen.asked, "the \"you may\" is offered");
    assert_eq!(seen.offered, Some(vec![cards[0]]), "mana value 1 only");
    assert_eq!(
        seen.after_pick,
        Some((2, vec![cards[0]])),
        "intensified as the trigger finishes; the cast spell waits on the stack"
    );
    assert_eq!(
        runner.state().objects[&cards[0]].zone,
        Zone::Graveyard,
        "cast and resolved"
    );
    assert_eq!(
        runner.state().objects[&jarsyl].intensity,
        2,
        "If you do: intensify"
    );
}

/// CR 608.2g: a picked spell that pauses its own cast for a target still
/// lets the trigger finish first: Jarsyl intensifies once, before the cast
/// spell resolves.
#[test]
fn jarsyl_intensifies_once_when_the_cast_spell_needs_a_target() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();
    let jarsyl = scenario
        .add_creature_from_oracle(P0, "Jarsyl, Dark Age Scion", 3, 3, JARSYL)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&jarsyl)
        .unwrap()
        .intensity = 1;
    let life_before = runner.state().players[1].life;

    let mut intensity_before_bolt_resolved = None;
    for _ in 0..40 {
        let state = runner.state();
        match state.waiting_for.clone() {
            WaitingFor::Priority { .. }
                if state.phase != Phase::PreCombatMain && state.stack.is_empty() =>
            {
                break
            }
            WaitingFor::Priority { .. } => {
                if state.stack.iter().any(|entry| entry.id == bolt) {
                    intensity_before_bolt_resolved = Some(state.objects[&jarsyl].intensity);
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the cast");
            }
            WaitingFor::EffectZoneChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![bolt] })
                    .expect("SelectCards accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(P1)),
                    })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::DeclareAttackers { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(
        intensity_before_bolt_resolved,
        Some(2),
        "intensified before the cast spell resolved"
    );
    assert_eq!(
        life_before - runner.state().players[1].life,
        3,
        "Bolt resolved"
    );
    assert_eq!(runner.state().objects[&jarsyl].intensity, 2, "exactly once");
}

/// CR 733.1 + CR 118.12: a picked spell whose cast is cancelled at its target
/// step was not cast, so "If you do" does not intensify; the same cast
/// completed intensifies (above).
#[test]
fn jarsyl_cancelled_cast_does_not_intensify() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let bolt = scenario
        .add_spell_to_graveyard(P0, "Lightning Bolt", true)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("Lightning Bolt deals 3 damage to any target.")
        .id();
    let jarsyl = scenario
        .add_creature_from_oracle(P0, "Jarsyl, Dark Age Scion", 3, 3, JARSYL)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&jarsyl)
        .unwrap()
        .intensity = 1;

    let mut cancelled = false;
    for _ in 0..40 {
        let state = runner.state();
        match state.waiting_for.clone() {
            WaitingFor::Priority { .. }
                if state.phase != Phase::PreCombatMain && state.stack.is_empty() =>
            {
                break
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the cast");
            }
            WaitingFor::EffectZoneChoice { .. } => {
                runner
                    .act(GameAction::SelectCards { cards: vec![bolt] })
                    .expect("SelectCards accepted");
            }
            WaitingFor::TargetSelection { .. } => {
                cancelled = true;
                runner
                    .act(GameAction::CancelCast)
                    .expect("CancelCast accepted");
            }
            WaitingFor::DeclareAttackers { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(cancelled, "reach guard: the cast reached its target step");
    assert_eq!(runner.state().objects[&bolt].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&jarsyl].intensity, 1);
}

/// CR 608.2h: Jarsyl removed in response to its trigger is read through its
/// last known information, intensity 1: the pick offers mana value 1, not 0.
#[test]
fn jarsyl_removed_in_response_reads_its_last_known_intensity() {
    jarsyl_removed_in_response(&[0, 1], None);
}

/// CR 608.2h: the last known information is the value as Jarsyl left, not as
/// its trigger fired: intensified to 2 after the trigger and then removed, the
/// pick offers mana value 2, not 1.
#[test]
fn jarsyl_intensified_then_removed_reads_its_intensity_as_it_left() {
    jarsyl_removed_in_response(&[1, 2], Some(2));
}

/// Jarsyl's trigger goes on the stack; optionally Jarsyl's intensity is raised
/// to `raise_to`; then Murder destroys Jarsyl in response. The pick must offer
/// only `cards[1]`.
fn jarsyl_removed_in_response(mana_values: &[u32], raise_to: Option<u32>) {
    let (mut runner, jarsyl, cards) = jarsyl_board(mana_values);
    let murder = *runner.state().players[0]
        .hand
        .iter()
        .find(|id| runner.state().objects[*id].name == "Murder")
        .expect("Murder in hand");
    let mut murdered = false;
    let mut offered = None;
    for _ in 0..40 {
        let state = runner.state();
        match state.waiting_for.clone() {
            WaitingFor::Priority { .. }
                if state.phase != Phase::PreCombatMain && state.stack.is_empty() =>
            {
                break
            }
            WaitingFor::Priority { player } => {
                if !murdered && player == P0 && !state.stack.is_empty() {
                    murdered = true;
                    if let Some(intensity) = raise_to {
                        runner
                            .state_mut()
                            .objects
                            .get_mut(&jarsyl)
                            .unwrap()
                            .intensity = intensity;
                    }
                    let card_id = runner.state().objects[&murder].card_id;
                    runner
                        .act(GameAction::CastSpell {
                            object_id: murder,
                            card_id,
                            targets: vec![],
                            payment_mode: CastPaymentMode::Auto,
                        })
                        .expect("Murder cast in response");
                } else {
                    runner
                        .act(GameAction::PassPriority)
                        .expect("PassPriority accepted");
                }
            }
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(jarsyl)),
                    })
                    .expect("Murder targets Jarsyl");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the cast");
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                offered = Some(cards);
                runner
                    .act(GameAction::SelectCards { cards: vec![] })
                    .expect("SelectCards accepted");
            }
            WaitingFor::DeclareAttackers { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert!(
        murdered,
        "reach guard: Murder was cast with the trigger on the stack"
    );
    assert_eq!(
        runner.state().objects[&jarsyl].zone,
        Zone::Graveyard,
        "Jarsyl died"
    );
    assert_eq!(
        offered,
        Some(vec![cards[1]]),
        "only the mana value Jarsyl had as it left"
    );
}

/// CR 118.12 + CR 608.2c: an empty pick casts nothing, so "If you do" does not
/// intensify.
#[test]
fn jarsyl_declined_pick_does_not_intensify() {
    let (mut runner, jarsyl, cards) = jarsyl_board(&[1]);
    let seen = run_jarsyl_trigger(&mut runner, jarsyl, true, vec![]);
    assert_eq!(
        seen.offered,
        Some(vec![cards[0]]),
        "reach guard: the pick opened"
    );
    assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&jarsyl].intensity, 1);
}

/// CR 608.2d: with no castable card the "you may" is impossible, so it is not
/// offered and "If you do" does not intensify; the same board with a mana
/// value 1 card offers it (above).
#[test]
fn jarsyl_with_nothing_castable_is_not_asked_and_does_not_intensify() {
    let (mut runner, jarsyl, _) = jarsyl_board(&[0, 2]);
    let seen = run_jarsyl_trigger(&mut runner, jarsyl, true, vec![]);
    assert!(!seen.asked, "an impossible \"you may\" is not offered");
    assert_eq!(seen.offered, None);
    assert_eq!(runner.state().objects[&jarsyl].intensity, 1);
}

const SWORD_OF_ONCE_AND_FUTURE: &str = "Equipped creature gets +2/+2 and has protection from \
blue and from black.\nWhenever equipped creature deals combat damage to a player, surveil 2. Then \
you may cast an instant or sorcery spell with mana value 2 or less from your graveyard without \
paying its mana cost. If that spell would be put into your graveyard, exile it instead.\nEquip {2}";

/// The pick follows a surveil prompt in the same trigger: it opens after the
/// surveil is answered and offers the caster's graveyard under the mana value
/// gate.
#[test]
fn sword_of_once_and_future_picks_after_its_surveil() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Own Filler {i}"));
    }
    let two = scenario
        .add_spell_to_graveyard(P0, "Two Instant", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let three = scenario
        .add_spell_to_graveyard(P0, "Three Instant", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let theirs = scenario
        .add_spell_to_graveyard(P1, "Their Instant", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let bearer = scenario.add_creature(P0, "Bearer", 2, 2).id();
    let sword = scenario
        .add_artifact_from_oracle(P0, "Sword of Once and Future", SWORD_OF_ONCE_AND_FUTURE)
        .with_subtypes(vec!["Equipment"])
        .id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&sword).unwrap().attached_to = Some(bearer.into());
        state
            .objects
            .get_mut(&bearer)
            .unwrap()
            .attachments
            .push(sword);
    }
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bearer, AttackTarget::Player(P1))])
        .expect("DeclareAttackers accepted");

    let mut surveilled = false;
    let mut offered = None;
    for _ in 0..40 {
        let state = runner.state();
        match state.waiting_for.clone() {
            WaitingFor::Priority { .. }
                if state.stack.is_empty()
                    && state.phase != Phase::DeclareAttackers
                    && state.phase != Phase::DeclareBlockers
                    && state.phase != Phase::CombatDamage =>
            {
                break
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: Vec::new(),
                    })
                    .expect("no blocks");
            }
            WaitingFor::SurveilChoice { cards, .. } => {
                surveilled = true;
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("surveil answered");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the cast");
            }
            WaitingFor::EffectZoneChoice {
                cards,
                zone: Zone::Graveyard,
                ..
            } => {
                assert!(surveilled, "the surveil comes first");
                offered = Some(cards);
                runner
                    .act(GameAction::SelectCards { cards: vec![two] })
                    .expect("SelectCards accepted");
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    let offered = offered.expect("the resolution pick opens");
    assert!(offered.contains(&two), "mana value 2 is offered");
    assert!(!offered.contains(&three), "mana value 3 is not");
    assert!(!offered.contains(&theirs), "\"your graveyard\" only");
    assert_eq!(runner.state().objects[&two].zone, Zone::Exile);
}

const DILUVIAN_PRIMORDIAL: &str = "Flying\nWhen this creature enters, for each opponent, you may \
cast up to one target instant or sorcery card from that player's graveyard without paying its mana \
cost. If a spell cast this way would be put into a graveyard, exile it instead.";

/// CR 115.1: Diluvian Primordial's "up to one target … card" is chosen on the
/// stack, not at resolution. With the target chosen its cast offer opens; with
/// none chosen nothing is offered — the untargeted graveyard pick does not
/// stand in for a declined target. (The chain already skips a cast with no
/// chosen target before it reaches the pick; the pick's own guard is pinned by
/// `graveyard_pick_opens_only_for_the_untargeted_shape`.)
#[test]
fn diluvian_primordial_with_no_target_offers_no_graveyard_pick() {
    for choose_target in [true, false] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let theirs = scenario
            .add_spell_to_graveyard(P1, "Their Instant", true)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let diluvian = scenario
            .add_creature_to_hand_from_oracle(P0, "Diluvian Primordial", 5, 5, DILUVIAN_PRIMORDIAL)
            .with_mana_cost(ManaCost::default())
            .id();
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&diluvian].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: diluvian,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("CastSpell accepted");
        let mut offered = false;
        for _ in 0..30 {
            match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                WaitingFor::Priority { .. } => {
                    runner
                        .act(GameAction::PassPriority)
                        .expect("PassPriority accepted");
                }
                WaitingFor::TriggerTargetSelection { .. } => {
                    runner
                        .act(GameAction::ChooseTarget {
                            target: choose_target.then_some(TargetRef::Object(theirs)),
                        })
                        .expect("ChooseTarget accepted");
                }
                WaitingFor::CastOffer { .. } | WaitingFor::EffectZoneChoice { .. } => {
                    offered = true;
                    break;
                }
                other => panic!("unexpected prompt: {other:?}"),
            }
        }
        assert_eq!(offered, choose_target, "target chosen: {choose_target}");
        assert_eq!(runner.state().objects[&theirs].zone, Zone::Graveyard);
    }
}
