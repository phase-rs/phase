//! Look-back block history bound to an earlier instruction's target, and the
//! referent an instruction leaves for the next one when it emits no event naming it.
//!
//! Every card is built from its verbatim Oracle text and driven through production
//! actions (`cast`, `activate`, `DeclareAttackers`, `DeclareBlockers`,
//! `PassPriority`, `OrderTriggers`, `ChooseTarget`, `DecideOptionalEffect`,
//! `RetargetSpell`). Creatures are textless unless a row names a card. A row that
//! crosses a turn gives both players a library, so no draw ends the game.
//!
//! - Gaze of the Gorgon, Venomous Breath, Glyph of Doom: "At this turn's next end of
//!   combat, destroy all creatures that [blocked or were|were] blocked by it/that
//!   creature this turn." (CR 509.1g, CR 603.7b, CR 608.2h).
//! - Ride Down: "Creatures that were blocked by that creature this combat gain
//!   trample until end of turn." (CR 509.1h, CR 511.3, CR 611.2c).
//! - Heal the Scars and its class: "You gain life equal to that creature's
//!   toughness." after an instruction that leaves no event naming its target
//!   (CR 608.2c).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, CombatRelationSubject, Effect, EffectKind, FilterProp, TargetFilter,
    TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use std::sync::Arc;

const GAZE: &str = "({B/G} can be paid with either {B} or {G}.)\nRegenerate target creature. At this turn's next end of combat, destroy all creatures that blocked or were blocked by it this turn.";
const BREATH: &str = "Choose target creature. At this turn's next end of combat, destroy all creatures that blocked or were blocked by it this turn.";
const GLYPH: &str = "Choose target Wall creature. At this turn's next end of combat, destroy all creatures that were blocked by that creature this turn.";
const RIDE: &str = "Destroy target blocking creature. Creatures that were blocked by that creature this combat gain trample until end of turn.";
const TRITON: &str = "Up to two target creatures each get +0/+3 until end of turn. Untap those creatures. At this turn's next end of combat, tap each creature that was blocked by one of those creatures this turn and it doesn't untap during its controller's next untap step.";
const FERRETS: &str = "Whenever this creature attacks, it gets +0/+2 until end of turn.\nAt end of combat, tap all creatures that blocked this creature this turn. They don't untap during their controller's next untap step.";
const HEAL: &str = "Regenerate target creature. You gain life equal to that creature's toughness.";
const MURDER: &str = "Destroy target creature.";
const EPHEMERATE: &str =
    "Exile target creature you control, then return it to the battlefield under its owner's control.";
const REDIRECT: &str = "You may choose new targets for target spell.";
const MIRROR_MATCH: &str = "Cast this spell only during the declare blockers step.\nFor each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.";
const FULL_THROTTLE: &str = "After this main phase, there are two additional combat phases.\nAt the beginning of each combat this turn, untap all creatures that attacked this turn.";
const PALACE_GUARD: &str = "This creature can block any number of creatures.";

const SHELTERING_WORD: &str = "Target creature you control gains hexproof until end of turn. You gain life equal to that creature's toughness. (A creature with hexproof can't be the target of spells or abilities your opponents control.)";
const THE_PROVIDER: &str = "Exile The Provider: Put two +1/+1 counters on target creature you control. You gain life equal to that creature's toughness.";
const BOROS_FURY_SHIELD: &str = "Prevent all combat damage that would be dealt by target attacking or blocking creature this turn. If {R} was spent to cast this spell, Boros Fury-Shield deals damage to that creature's controller equal to the creature's power.";
const IMPS_MISCHIEF: &str = "Change the target of target spell with a single target. You lose life equal to that spell's mana value.";
const DEVOUR_IN_SHADOW: &str = "Destroy target creature. It can't be regenerated. You lose life equal to that creature's toughness.";
const FEED_THE_SWARM: &str = "Destroy target creature or enchantment an opponent controls. You lose life equal to that permanent's mana value.";
const SEVER_SOUL: &str = "Destroy target nonblack creature. It can't be regenerated. You gain life equal to its toughness.";
const PHTHISIS: &str = "Destroy target creature. Its controller loses life equal to its power plus its toughness.\nSuspend 5\u{2014}{1}{B} (Rather than cast this card from your hand, you may pay {1}{B} and exile it with five time counters on it. At the beginning of your upkeep, remove a time counter. When the last is removed, you may cast it without paying its mana cost.)";
const DIVINE_OFFERING: &str = "Destroy target artifact. You gain life equal to its mana value.";
const DEATHS_CARESS: &str =
    "Destroy target creature. If that creature was a Human, you gain life equal to its toughness.";
const OVERWHELMING_INTELLECT: &str =
    "Counter target creature spell. Draw cards equal to that spell's mana value.";
const ESSENCE_BACKLASH: &str = "Counter target creature spell. Essence Backlash deals damage equal to that spell's power to its controller.";
const UNCOUNTERABLE: &str = "This spell can't be countered.";
const JUDGE_UNWORTHY: &str = "Choose target attacking or blocking creature. Scry 3, then reveal the top card of your library. Judge Unworthy deals damage equal to that card's mana value to that creature.";
const CRUSH_UNDERFOOT: &str =
    "Choose a Giant creature you control. It deals damage equal to its power to target creature.";
const ORIMS_THUNDER: &str = "Kicker {R} (You may pay an additional {R} as you cast this spell.)\nDestroy target artifact or enchantment. If this spell was kicked, it deals damage equal to that permanent's mana value to target creature.";
const BRIGHTMARE: &str = "When this creature enters, tap up to one target creature. You gain life equal to that creature's power.";
const LAGONNA_BAND_STORYTELLER: &str = "When this creature enters, you may put target enchantment card from your graveyard on top of your library. If you do, you gain life equal to its mana value.";

// ── Driving helpers ─────────────────────────────────────────────────────

/// Answer every prompt with the plain production default until `done` holds:
/// pass priority, order triggers as offered, declare no attackers and no
/// blockers. Returns how many `OrderTriggers` prompts were answered.
fn pass_until(runner: &mut GameRunner, label: &str, done: impl Fn(&GameState) -> bool) -> usize {
    let mut prompts = 0;
    for _ in 0..300 {
        if done(runner.state()) {
            return prompts;
        }
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                prompts += 1;
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            other => panic!("{label}: unexpected prompt {other:?}"),
        }
    }
    panic!("{label}: never reached; phase {:?}", runner.state().phase);
}

/// CR 117.3d: with an empty stack, the priority holder passes and the other
/// player receives priority in the same step.
fn pass_priority_to(runner: &mut GameRunner, player: PlayerId) {
    assert!(runner.state().stack.is_empty(), "pass with an empty stack");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: holder } if holder != player),
        "the other player holds priority: {:?}",
        runner.state().waiting_for
    );
    runner.act(GameAction::PassPriority).expect("pass priority");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player: holder } if holder == player),
        "priority passes: {:?}",
        runner.state().waiting_for
    );
}

fn attack(runner: &mut GameRunner, attackers: &[ObjectId]) {
    let attacks: Vec<_> = attackers
        .iter()
        .map(|id| (*id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("declare attackers");
}

fn to_block_prompt(runner: &mut GameRunner) {
    pass_until(runner, "blockers", |s| {
        matches!(s.waiting_for, WaitingFor::DeclareBlockers { .. })
    });
}

/// Declare `blocks` and stop at P0's priority in the declare blockers step.
fn block(runner: &mut GameRunner, blocks: &[(ObjectId, ObjectId)]) {
    runner.declare_blockers(blocks).expect("declare blockers");
    pass_until(runner, "declare blockers priority", |s| {
        s.phase == Phase::DeclareBlockers
            && matches!(s.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
}

fn attack_block(runner: &mut GameRunner, attackers: &[ObjectId], blocks: &[(ObjectId, ObjectId)]) {
    runner.advance_to_combat();
    attack(runner, attackers);
    to_block_prompt(runner);
    block(runner, blocks);
}

fn drain(runner: &mut GameRunner) {
    pass_until(runner, "drain", |s| {
        s.stack.is_empty() && matches!(s.waiting_for, WaitingFor::Priority { .. })
    });
}

/// Run through the end of combat step into the postcombat main phase with an
/// empty stack. Returns how many `OrderTriggers` prompts were answered.
fn past_end_of_combat(runner: &mut GameRunner) -> usize {
    let prompts = pass_until(runner, "end of combat", |s| s.phase == Phase::EndCombat);
    prompts
        + pass_until(runner, "postcombat main", |s| {
            s.phase == Phase::PostCombatMain && s.stack.is_empty()
        })
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn tramples(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Trample)
}

fn tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.life(player)
}

fn free_spell(scenario: &mut GameScenario, player: PlayerId, name: &str, text: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, true, text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

fn colorless(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); count]
}

/// The default board: P0's attackers A and A2, P1's blockers B and B2, two copies
/// of the card under test, and a Murder, a redirect and P1's Ephemerate.
struct Board {
    runner: GameRunner,
    a: ObjectId,
    a2: ObjectId,
    b: ObjectId,
    b2: ObjectId,
    spell: ObjectId,
    spell2: ObjectId,
    murder: ObjectId,
    redirect: ObjectId,
    ephemerate: ObjectId,
}

fn board(text: &str, attacker_power: i32) -> Board {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", attacker_power, 5).id();
    let a2 = s.add_creature(P0, "A2", attacker_power, 5).id();
    let b = s.add_creature(P1, "B", 1, 6).id();
    let b2 = s.add_creature(P1, "B2", 1, 6).id();
    let spell = free_spell(&mut s, P0, "Spell", text);
    let spell2 = free_spell(&mut s, P0, "Spell Two", text);
    let murder = free_spell(&mut s, P0, "Murder", MURDER);
    let redirect = free_spell(&mut s, P0, "Redirect", REDIRECT);
    let ephemerate = free_spell(&mut s, P1, "Ephemerate", EPHEMERATE);
    s.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    s.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    Board {
        runner: s.build(),
        a,
        a2,
        b,
        b2,
        spell,
        spell2,
        murder,
        redirect,
        ephemerate,
    }
}

/// Default board in combat: B blocks A and B2 blocks A2, P0 holding priority in
/// the declare blockers step.
fn blocked_board(text: &str, attacker_power: i32) -> Board {
    let mut x = board(text, attacker_power);
    attack_block(&mut x.runner, &[x.a, x.a2], &[(x.b, x.a), (x.b2, x.a2)]);
    x
}

/// Walk the combats a Full Throttle turn adds: in the first, `attacker` attacks
/// and `blocker` blocks it; stop at the second combat's attack declaration.
fn to_combat_two(runner: &mut GameRunner, attacker: ObjectId, blocker: ObjectId) {
    let mut declarations = 0;
    for _ in 0..400 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                declarations += 1;
                if declarations == 2 {
                    return;
                }
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![(attacker, AttackTarget::Player(P1))],
                        bands: vec![],
                    })
                    .expect("combat one attack");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![(blocker, attacker)],
                    })
                    .expect("combat one block");
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("no second combat");
}

/// A Full Throttle turn: A, B and B2, with a Venomous Breath or Ride Down in hand.
fn full_throttle_board(text: &str) -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 1, 5).id();
    let b = s.add_creature(P1, "B", 1, 6).id();
    let b2 = s.add_creature(P1, "B2", 1, 6).id();
    let throttle = s
        .add_spell_to_hand_from_oracle(P0, "Full Throttle", false, FULL_THROTTLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let spell = free_spell(&mut s, P0, "Spell", text);
    let mut runner = s.build();
    runner.cast(throttle).resolve();
    runner.advance_until_stack_empty();
    (runner, a, b, b2, spell)
}

/// The token Mirror Match put onto the battlefield blocking `attacker`.
fn mirror_token(runner: &GameRunner, attacker: ObjectId) -> ObjectId {
    *runner
        .state()
        .combat
        .as_ref()
        .expect("in combat")
        .blocker_assignments
        .get(&attacker)
        .and_then(|blockers| blockers.last())
        .expect("Mirror Match's token blocks the attacker")
}

/// Pass priority until a retarget prompt opens, accepting a "you may".
fn drive_to_retarget(runner: &mut GameRunner) {
    for _ in 0..12 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept");
            }
            WaitingFor::CopyRetarget { .. } | WaitingFor::RetargetChoice { .. } => return,
            ref other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("no retarget prompt");
}

/// Resolve the stack, choosing each trigger target from `prefer` and answering a
/// "you may" with `accept`. Returns whether an optional choice was answered.
fn settle(runner: &mut GameRunner, prefer: &[ObjectId], accept: bool) -> bool {
    let mut answered_optional = false;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let legal = &target_slots[selection.current_slot].legal_targets;
                let pick = prefer
                    .iter()
                    .find(|id| legal.contains(&TargetRef::Object(**id)));
                runner
                    .act(GameAction::ChooseTarget {
                        target: pick.map(|id| TargetRef::Object(*id)),
                    })
                    .expect("choose target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                answered_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional choice");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::Priority { .. } => return answered_optional,
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the stack never settled");
}

/// Pass priority until the stack is empty, answering a scry by keeping every card on top.
/// Returns the events those actions emitted.
fn resolve_keeping_scry_on_top(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return events,
            WaitingFor::Priority { .. } => {
                let result = runner.act(GameAction::PassPriority).expect("pass priority");
                events.extend(result.events);
            }
            WaitingFor::ScryChoice { cards, .. } => {
                let result = runner
                    .act(GameAction::SelectCards { cards })
                    .expect("scry everything to the top");
                events.extend(result.events);
            }
            other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the stack never settled");
}

// ── V1, V3, V18: either direction, and the slot witness ────────────────

/// CR 509.1g + CR 603.7b: Gaze of the Gorgon and Venomous Breath destroy every
/// creature that blocked or was blocked by the target, in either direction, and
/// nothing else. The delayed ability reaches the stack with no target prompt: its
/// "it" is the target announced with the spell, frozen at creation.
#[test]
fn gaze_and_breath_read_either_direction() {
    for card in [GAZE, BREATH] {
        // Target the attacker A: its blocker B dies.
        let mut x = blocked_board(card, 1);
        x.runner.cast(x.spell).target_objects(&[x.a]).resolve();
        drain(&mut x.runner);
        assert_eq!(
            x.runner.state().delayed_triggers.len(),
            1,
            "one delayed ability"
        );
        assert_eq!(
            x.runner.state().delayed_triggers[0].ability.targets.len(),
            1,
            "reach: the announced target is frozen at creation"
        );
        pass_until(&mut x.runner, "end of combat", |s| {
            s.phase == Phase::EndCombat
        });
        assert_eq!(
            x.runner.state().stack.len(),
            1,
            "the delayed ability triggered"
        );
        assert!(
            matches!(x.runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the delayed ability asks for no target: {:?}",
            x.runner.state().waiting_for
        );
        past_end_of_combat(&mut x.runner);
        assert_eq!(zone(&x.runner, x.b), Zone::Graveyard, "A's blocker");
        assert_eq!(zone(&x.runner, x.b2), Zone::Battlefield, "no widening");
        assert_eq!(zone(&x.runner, x.a), Zone::Battlefield);
        assert_eq!(zone(&x.runner, x.a2), Zone::Battlefield);
        assert!(x.runner.state().delayed_triggers.is_empty());

        // Target the blocker B: the attacker it blocked dies.
        let mut x = blocked_board(card, 1);
        x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
        drain(&mut x.runner);
        past_end_of_combat(&mut x.runner);
        assert_eq!(
            zone(&x.runner, x.a),
            Zone::Graveyard,
            "the creature B blocked"
        );
        assert_eq!(zone(&x.runner, x.a2), Zone::Battlefield, "no widening");
        assert_eq!(zone(&x.runner, x.b), Zone::Battlefield);
    }
}

// ── V2: a block after resolution counts ────────────────────────────────

/// CR 608.2h: the delayed ability reads the history when it resolves, so a block
/// declared after Venomous Breath resolved still counts (Gaze's ruling: "at any
/// point during the turn").
#[test]
fn breath_cast_before_blocks_reads_the_later_block() {
    let mut x = board(BREATH, 1);
    x.runner.advance_to_combat();
    attack(&mut x.runner, &[x.a, x.a2]);
    pass_until(&mut x.runner, "declare attackers priority", |s| {
        s.phase == Phase::DeclareAttackers
            && matches!(s.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    x.runner.cast(x.spell).target_objects(&[x.a]).resolve();
    drain(&mut x.runner);
    assert_eq!(
        x.runner.state().delayed_triggers.len(),
        1,
        "reach: the delayed ability exists before blocks"
    );
    to_block_prompt(&mut x.runner);
    block(&mut x.runner, &[(x.b, x.a), (x.b2, x.a2)]);
    past_end_of_combat(&mut x.runner);
    assert_eq!(zone(&x.runner, x.b), Zone::Graveyard);
    assert_eq!(zone(&x.runner, x.b2), Zone::Battlefield);
}

// ── V4: the target leaves before the delayed ability resolves ──────────

/// CR 608.2i: the delayed ability reads the announced creature's history even
/// after it left. Venomous Breath's target is destroyed; Gaze's target is
/// destroyed too, but its shield replaces that (CR 701.19a: tapped and removed
/// from combat). Either way its blocker dies.
#[test]
fn delayed_read_survives_the_target_leaving() {
    for card in [GAZE, BREATH] {
        let mut x = blocked_board(card, 1);
        x.runner.cast(x.spell).target_objects(&[x.a]).resolve();
        drain(&mut x.runner);
        x.runner.cast(x.murder).target_objects(&[x.a]).resolve();
        drain(&mut x.runner);
        if card == GAZE {
            assert_eq!(zone(&x.runner, x.a), Zone::Battlefield, "regenerated");
            assert!(tapped(&x.runner, x.a), "CR 701.19a: tapped");
            let combat = x.runner.state().combat.as_ref().expect("in combat");
            assert!(
                !combat.attackers.iter().any(|at| at.object_id == x.a),
                "CR 701.19a: removed from combat"
            );
        } else {
            assert_eq!(
                zone(&x.runner, x.a),
                Zone::Graveyard,
                "reach: the target left"
            );
        }
        assert!(
            !x.runner.state().delayed_triggers[0]
                .ability
                .target_incarnations
                .is_empty(),
            "reach: the target is pinned"
        );
        past_end_of_combat(&mut x.runner);
        assert_eq!(zone(&x.runner, x.b), Zone::Graveyard);
        assert_eq!(zone(&x.runner, x.b2), Zone::Battlefield);
    }
}

// ── V5, V6, V7: incarnations ───────────────────────────────────────────

/// CR 400.7: "it" is the announced incarnation. P1 blinks its own targeted
/// blocker B after it blocked: the attacker the announced B blocked still dies.
#[test]
fn breath_reads_the_announced_incarnation() {
    let mut x = blocked_board(BREATH, 1);
    x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
    drain(&mut x.runner);
    let before = x.runner.state().objects[&x.b].incarnation;
    pass_priority_to(&mut x.runner, P1);
    x.runner.cast(x.ephemerate).target_objects(&[x.b]).resolve();
    drain(&mut x.runner);
    assert_eq!(zone(&x.runner, x.b), Zone::Battlefield, "reach");
    assert_ne!(
        x.runner.state().objects[&x.b].incarnation,
        before,
        "reach: B is a new object"
    );
    past_end_of_combat(&mut x.runner);
    assert_eq!(zone(&x.runner, x.a), Zone::Graveyard);
    assert_eq!(zone(&x.runner, x.a2), Zone::Battlefield);
}

/// CR 400.7: a returned object is a different creature. Venomous Breath on B in
/// the main phase, P1 blinks B, and the new B blocks A: A survives. The twin with
/// no blink kills A, so the one id carries two incarnations.
#[test]
fn breath_ignores_blocks_by_a_returned_target() {
    for blink in [false, true] {
        let mut x = board(BREATH, 1);
        x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
        drain(&mut x.runner);
        if blink {
            pass_priority_to(&mut x.runner, P1);
            x.runner.cast(x.ephemerate).target_objects(&[x.b]).resolve();
            drain(&mut x.runner);
        }
        attack_block(&mut x.runner, &[x.a, x.a2], &[(x.b, x.a), (x.b2, x.a2)]);
        past_end_of_combat(&mut x.runner);
        let expected = if blink {
            Zone::Battlefield
        } else {
            Zone::Graveyard
        };
        assert_eq!(zone(&x.runner, x.a), expected, "blink = {blink}");
        assert_eq!(zone(&x.runner, x.a2), Zone::Battlefield);
    }
}

/// A meaning-preserving wrapper around one filter property.
type PropWrap = fn(FilterProp) -> FilterProp;

/// Rewrap the delayed payload's history relation on `spell`, keeping its meaning:
/// `AnyOf [relation]` and `Not (Not relation)` select what the bare relation does.
/// Returns how many relations were wrapped.
fn wrap_history_relation(runner: &mut GameRunner, spell: ObjectId, wrap: PropWrap) -> usize {
    fn in_definition(def: &mut AbilityDefinition, wrap: PropWrap) -> usize {
        let here = match &mut *def.effect {
            Effect::CreateDelayedTrigger { effect, .. } => in_definition(effect, wrap),
            Effect::DestroyAll {
                target: TargetFilter::Typed(typed),
                ..
            } => typed
                .properties
                .iter_mut()
                .filter(|prop| {
                    matches!(
                        prop,
                        FilterProp::CombatRelation {
                            subject: CombatRelationSubject::AnaphoricTarget,
                            ..
                        }
                    )
                })
                .map(|prop| *prop = wrap(prop.clone()))
                .count(),
            _ => 0,
        };
        here + def
            .sub_ability
            .as_deref_mut()
            .map_or(0, |sub| in_definition(sub, wrap))
    }
    let object = runner
        .state_mut()
        .objects
        .get_mut(&spell)
        .expect("spell in hand");
    let mut wrapped = 0;
    for abilities in [&mut object.abilities, &mut object.base_abilities] {
        for def in Arc::make_mut(abilities).iter_mut() {
            wrapped += in_definition(def, wrap);
        }
    }
    wrapped
}

/// Whether the payload's population carries a relation inside `AnyOf` or `Not`.
fn payload_has_wrapped_relation(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::DestroyAll { target: TargetFilter::Typed(typed), .. }
            if typed.properties.iter().any(|prop| matches!(
                prop,
                FilterProp::AnyOf { .. } | FilterProp::Not { .. }
            ))
    )
}

/// Synthetic: no printed card puts its history relation inside `AnyOf` or `Not`;
/// Gaze of the Gorgon and Venomous Breath are the nearest printed shapes, so the
/// fixture is Venomous Breath with its relation rewrapped to the same meaning.
/// CR 603.7c + CR 608.2i: the delayed ability captures the announced B AND pins its
/// incarnation, through the wrapper. P1 blinks B, so the B that blocks A is a new
/// object that the ability does not read: A survives. Unpinned, the read falls back
/// to the live B and destroys A.
#[test]
fn a_wrapped_history_relation_keeps_its_target_pin() {
    let wraps: [(&str, PropWrap); 2] = [
        ("AnyOf", |prop| FilterProp::AnyOf { props: vec![prop] }),
        ("Not Not", |prop| FilterProp::Not {
            prop: Box::new(FilterProp::Not {
                prop: Box::new(prop),
            }),
        }),
    ];
    for (label, wrap) in wraps {
        // Control, no blink: the same wrapped payload reads B's block, so A, which B
        // blocked, dies and A2, which B2 blocked, stays.
        let mut x = board(BREATH, 1);
        assert_eq!(
            wrap_history_relation(&mut x.runner, x.spell, wrap),
            2,
            "{label} control: reach: the relation is wrapped in both ability lists"
        );
        x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
        drain(&mut x.runner);
        let delayed = &x.runner.state().delayed_triggers;
        assert_eq!(delayed.len(), 1, "{label} control: one delayed ability");
        assert!(
            payload_has_wrapped_relation(&delayed[0].ability.effect),
            "{label} control: reach: the payload keeps the wrapper: {:?}",
            delayed[0].ability.effect
        );
        attack_block(&mut x.runner, &[x.a, x.a2], &[(x.b, x.a), (x.b2, x.a2)]);
        past_end_of_combat(&mut x.runner);
        assert!(
            x.runner.state().delayed_triggers.is_empty(),
            "{label} control: reach: the delayed ability resolved"
        );
        assert_eq!(
            zone(&x.runner, x.a),
            Zone::Graveyard,
            "{label} control: B blocked A"
        );
        assert_eq!(
            zone(&x.runner, x.a2),
            Zone::Battlefield,
            "{label} control: B did not block A2"
        );

        let mut x = board(BREATH, 1);
        assert_eq!(
            wrap_history_relation(&mut x.runner, x.spell, wrap),
            2,
            "{label}: reach: the relation is wrapped in both ability lists"
        );
        let announced = x.runner.state().objects[&x.b].incarnation;
        x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
        drain(&mut x.runner);
        let delayed = &x.runner.state().delayed_triggers;
        assert_eq!(delayed.len(), 1, "{label}: one delayed ability");
        assert!(
            payload_has_wrapped_relation(&delayed[0].ability.effect),
            "{label}: reach: the payload keeps the wrapper: {:?}",
            delayed[0].ability.effect
        );
        assert_eq!(
            delayed[0].ability.targets,
            vec![TargetRef::Object(x.b)],
            "{label}: the announced target is captured"
        );
        assert_eq!(
            delayed[0].ability.target_incarnations,
            vec![ObjectIncarnationRef::of(x.b, announced)],
            "{label}: and pinned to its announced incarnation"
        );

        pass_priority_to(&mut x.runner, P1);
        x.runner.cast(x.ephemerate).target_objects(&[x.b]).resolve();
        drain(&mut x.runner);
        assert_eq!(zone(&x.runner, x.b), Zone::Battlefield, "{label}: reach");
        assert_ne!(
            x.runner.state().objects[&x.b].incarnation,
            announced,
            "{label}: reach: B is a new object"
        );
        assert_eq!(
            x.runner.state().delayed_triggers[0]
                .ability
                .target_incarnations,
            vec![ObjectIncarnationRef::of(x.b, announced)],
            "{label}: the pin survives the zone change"
        );

        attack_block(&mut x.runner, &[x.a, x.a2], &[(x.b, x.a), (x.b2, x.a2)]);
        pass_until(&mut x.runner, "end of combat", |s| {
            s.phase == Phase::EndCombat
        });
        assert_eq!(
            x.runner.state().stack.len(),
            1,
            "{label}: reach: the delayed ability triggered"
        );
        past_end_of_combat(&mut x.runner);
        assert!(
            x.runner.state().delayed_triggers.is_empty(),
            "{label}: reach: the delayed ability resolved"
        );
        assert_eq!(
            zone(&x.runner, x.a),
            Zone::Battlefield,
            "{label}: the returned B's block is not read"
        );
        assert_eq!(zone(&x.runner, x.a2), Zone::Battlefield, "{label}");
    }
}

/// CR 400.7: a candidate that re-entered is a new object. B and B2 block A,
/// Venomous Breath on A, P1 blinks B: B stays, B2 dies.
#[test]
fn breath_spares_a_blocker_that_re_entered() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 1, 5).id();
    let b = s.add_creature(P1, "B", 1, 6).id();
    let b2 = s.add_creature(P1, "B2", 1, 6).id();
    let spell = free_spell(&mut s, P0, "Venomous Breath", BREATH);
    let ephemerate = free_spell(&mut s, P1, "Ephemerate", EPHEMERATE);
    let mut runner = s.build();
    attack_block(&mut runner, &[a], &[(b, a), (b2, a)]);
    runner.cast(spell).target_objects(&[a]).resolve();
    drain(&mut runner);
    pass_priority_to(&mut runner, P1);
    runner.cast(ephemerate).target_objects(&[b]).resolve();
    drain(&mut runner);
    past_end_of_combat(&mut runner);
    assert_eq!(zone(&runner, b), Zone::Battlefield, "the returned blocker");
    assert_eq!(zone(&runner, b2), Zone::Graveyard);
}

// ── V8: Gaze's shield ──────────────────────────────────────────────────

/// CR 701.19a: Gaze of the Gorgon's shield works with its destruction. A 1/1
/// blocked by a 3/3 survives combat damage tapped, and its blocker dies at end of
/// combat. Twin: a Gaze from each side, on A and on B, so each shield replaces the
/// other Gaze's destruction and both stay.
#[test]
fn gaze_shield_replaces_destruction() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 1, 1).id();
    let b = s.add_creature(P1, "B", 3, 3).id();
    let gaze = free_spell(&mut s, P0, "Gaze of the Gorgon", GAZE);
    let mut runner = s.build();
    attack_block(&mut runner, &[a], &[(b, a)]);
    runner.cast(gaze).target_objects(&[a]).resolve();
    drain(&mut runner);
    past_end_of_combat(&mut runner);
    assert_eq!(zone(&runner, a), Zone::Battlefield, "regenerated");
    assert!(tapped(&runner, a));
    assert_eq!(zone(&runner, b), Zone::Graveyard, "its blocker still dies");

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 1, 5).id();
    let b = s.add_creature(P1, "B", 1, 6).id();
    let gaze = free_spell(&mut s, P0, "Gaze of the Gorgon", GAZE);
    let gaze2 = free_spell(&mut s, P1, "Gaze of the Gorgon", GAZE);
    let mut runner = s.build();
    attack_block(&mut runner, &[a], &[(b, a)]);
    runner.cast(gaze).target_objects(&[a]).resolve();
    drain(&mut runner);
    pass_priority_to(&mut runner, P1);
    runner.cast(gaze2).target_objects(&[b]).resolve();
    drain(&mut runner);
    assert_eq!(runner.state().delayed_triggers.len(), 2, "reach");
    past_end_of_combat(&mut runner);
    assert_eq!(zone(&runner, b), Zone::Battlefield, "B's shield replaces");
    assert_eq!(zone(&runner, a), Zone::Battlefield, "A's shield replaces");
}

// ── V9, V11: Glyph of Doom ─────────────────────────────────────────────

/// CR 509.1g: Glyph of Doom reads one direction. A Wall blocks A: A dies, the Wall
/// stays. A Wall that attacks and is blocked by X: Glyph names nothing, and
/// Venomous Breath on the same board, the reach guard, kills X.
#[test]
fn glyph_reads_only_the_creatures_its_wall_blocked() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 1, 5).id();
    let a2 = s.add_creature(P0, "A2", 1, 5).id();
    let wall = s
        .add_creature(P1, "Wall", 1, 6)
        .with_subtypes(vec!["Wall"])
        .id();
    let x = s.add_creature(P1, "X", 1, 6).id();
    let glyph = free_spell(&mut s, P0, "Glyph of Doom", GLYPH);
    let mut runner = s.build();
    attack_block(&mut runner, &[a, a2], &[(wall, a), (x, a2)]);
    runner.cast(glyph).target_objects(&[wall]).resolve();
    drain(&mut runner);
    past_end_of_combat(&mut runner);
    assert_eq!(zone(&runner, a), Zone::Graveyard);
    assert_eq!(zone(&runner, a2), Zone::Battlefield);
    assert_eq!(zone(&runner, wall), Zone::Battlefield);
    assert_eq!(zone(&runner, x), Zone::Battlefield);

    for (text, x_dies) in [(GLYPH, false), (BREATH, true)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let wall = s
            .add_creature(P0, "Wall", 1, 6)
            .with_subtypes(vec!["Wall"])
            .id();
        let x = s.add_creature(P1, "X", 1, 6).id();
        let spell = free_spell(&mut s, P0, "Spell", text);
        let mut runner = s.build();
        attack_block(&mut runner, &[wall], &[(x, wall)]);
        runner.cast(spell).target_objects(&[wall]).resolve();
        drain(&mut runner);
        past_end_of_combat(&mut runner);
        assert_eq!(
            zone(&runner, x) == Zone::Graveyard,
            x_dies,
            "X dies = {x_dies}"
        );
    }
}

/// CR 115.1a: Glyph of Doom announces one target, which must be a Wall.
#[test]
fn glyph_targets_one_wall() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let wall = s
        .add_creature(P1, "Wall", 1, 6)
        .with_subtypes(vec!["Wall"])
        .id();
    let wall2 = s
        .add_creature(P1, "Wall Two", 1, 6)
        .with_subtypes(vec!["Wall"])
        .id();
    let x = s.add_creature(P1, "X", 1, 6).id();
    let glyph = free_spell(&mut s, P0, "Glyph of Doom", GLYPH);
    let mut runner = s.build();
    let card_id = runner.state().objects[&glyph].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: glyph,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast");
    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!("target prompt: {:?}", runner.state().waiting_for);
    };
    assert_eq!(target_slots.len(), 1);
    let legal = &target_slots[0].legal_targets;
    assert!(legal.contains(&TargetRef::Object(wall)));
    assert!(legal.contains(&TargetRef::Object(wall2)));
    assert!(!legal.contains(&TargetRef::Object(x)), "not a Wall");
}

// ── V12, V13, V30, V31, V32: Ride Down ─────────────────────────────────

/// CR 509.1h + CR 702.19d: Ride Down destroys B, and A, which B blocked, gains
/// trample. A stays blocked, so with no blocker all 5 of its damage tramples over.
/// A2, blocked by B2, gains nothing.
#[test]
fn ride_down_grants_trample_to_the_creatures_its_target_blocked() {
    let mut x = blocked_board(RIDE, 5);
    let p1_before = life(&x.runner, P1);
    x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
    drain(&mut x.runner);
    assert_eq!(zone(&x.runner, x.b), Zone::Graveyard);
    assert!(tramples(&x.runner, x.a));
    assert!(!tramples(&x.runner, x.a2));
    let effects = &x.runner.state().transient_continuous_effects;
    assert_eq!(effects.len(), 1, "reach: one grant");
    assert_eq!(
        effects[0].affected,
        TargetFilter::SpecificObject { id: x.a },
        "the grant is latched to A"
    );
    past_end_of_combat(&mut x.runner);
    assert_eq!(life(&x.runner, P1), p1_before - 5);
}

/// CR 509.4: a creature put onto the battlefield blocking never blocked, so Ride
/// Down on Mirror Match's token gives A no trample, and A stays blocked. In the
/// same combat, Ride Down on the declared blocker B gives A2 trample. P1's life is
/// unchanged: A deals no damage, and A2's 5 goes to its 5-toughness token.
#[test]
fn ride_down_on_a_token_put_onto_the_battlefield_blocking_grants_nothing() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 5, 5).id();
    let a2 = s.add_creature(P0, "A2", 5, 5).id();
    let b = s.add_creature(P1, "B", 1, 1).id();
    let mirror = free_spell(&mut s, P1, "Mirror Match", MIRROR_MATCH);
    let ride = free_spell(&mut s, P0, "Ride Down", RIDE);
    let ride2 = free_spell(&mut s, P0, "Ride Down", RIDE);
    let mut runner = s.build();
    attack_block(&mut runner, &[a, a2], &[(b, a2)]);
    pass_priority_to(&mut runner, P1);
    runner.cast(mirror).resolve();
    drain(&mut runner);
    let token = mirror_token(&runner, a);
    assert_ne!(token, b);
    runner.cast(ride).target_objects(&[token]).resolve();
    drain(&mut runner);
    assert!(
        runner
            .state()
            .objects
            .get(&token)
            .is_none_or(|object| object.zone != Zone::Battlefield),
        "reach: the token was destroyed"
    );
    assert!(!tramples(&runner, a), "CR 509.4: the token never blocked");
    runner.cast(ride2).target_objects(&[b]).resolve();
    drain(&mut runner);
    assert!(tramples(&runner, a2), "the declared blocker did");
    let p1_before = life(&runner, P1);
    past_end_of_combat(&mut runner);
    assert_eq!(life(&runner, P1), p1_before);
}

/// CR 702.12b: an indestructible target survives, and Ride Down's grant still
/// happens.
#[test]
fn ride_down_grants_when_its_target_survives() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 8, 5).id();
    let b = s.add_creature(P1, "B", 0, 6).indestructible().id();
    let ride = free_spell(&mut s, P0, "Ride Down", RIDE);
    let mut runner = s.build();
    attack_block(&mut runner, &[a], &[(b, a)]);
    runner.cast(ride).target_objects(&[b]).resolve();
    drain(&mut runner);
    assert_eq!(zone(&runner, b), Zone::Battlefield, "reach: B survived");
    assert!(tramples(&runner, a));
}

/// CR 608.2b: a spell whose only target is gone does not resolve. Murder on B in
/// response to Ride Down: no trample. Murder on A in response to Gaze of the
/// Gorgon: no delayed ability, and B survives.
#[test]
fn a_fizzled_spell_grants_and_schedules_nothing() {
    let mut x = blocked_board(RIDE, 5);
    x.runner.cast(x.spell).target_objects(&[x.b]).commit();
    x.runner.cast(x.murder).target_objects(&[x.b]).resolve();
    drain(&mut x.runner);
    assert_eq!(
        zone(&x.runner, x.b),
        Zone::Graveyard,
        "reach: the target is gone"
    );
    assert!(!tramples(&x.runner, x.a));

    let mut x = blocked_board(GAZE, 1);
    x.runner.cast(x.spell).target_objects(&[x.a]).commit();
    x.runner.cast(x.murder).target_objects(&[x.a]).resolve();
    drain(&mut x.runner);
    assert_eq!(
        zone(&x.runner, x.a),
        Zone::Graveyard,
        "reach: the target is gone"
    );
    assert!(x.runner.state().delayed_triggers.is_empty());
    past_end_of_combat(&mut x.runner);
    assert_eq!(zone(&x.runner, x.b), Zone::Battlefield);
}

/// CR 611.2c + CR 400.7: the grant is latched to the creatures it affected. A
/// blocked by B1 and B2, Ride Down on B1, then Ephemerate on A: the returned A has
/// no trample. Palace Guard blocking two attackers: both trample, one latched
/// grant each.
#[test]
fn ride_down_grant_is_latched_to_each_creature() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 5, 5).id();
    let b1 = s.add_creature(P1, "B1", 1, 1).id();
    let b2 = s.add_creature(P1, "B2", 1, 1).id();
    let ride = free_spell(&mut s, P0, "Ride Down", RIDE);
    let ephemerate = free_spell(&mut s, P0, "Ephemerate", EPHEMERATE);
    let mut runner = s.build();
    attack_block(&mut runner, &[a], &[(b1, a), (b2, a)]);
    runner.cast(ride).target_objects(&[b1]).resolve();
    drain(&mut runner);
    assert!(tramples(&runner, a), "reach");
    runner.cast(ephemerate).target_objects(&[a]).resolve();
    drain(&mut runner);
    assert_eq!(zone(&runner, a), Zone::Battlefield);
    assert!(!tramples(&runner, a), "a new object gets no grant");

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 5, 5).id();
    let a2 = s.add_creature(P0, "A2", 5, 5).id();
    let guard = s
        .add_creature_from_oracle(P1, "Palace Guard", 1, 4, PALACE_GUARD)
        .id();
    let ride = free_spell(&mut s, P0, "Ride Down", RIDE);
    let mut runner = s.build();
    attack_block(&mut runner, &[a, a2], &[(guard, a), (guard, a2)]);
    runner.cast(ride).target_objects(&[guard]).resolve();
    drain(&mut runner);
    assert!(tramples(&runner, a));
    assert!(tramples(&runner, a2));
    assert_eq!(runner.state().transient_continuous_effects.len(), 2);
}

// ── V14, V16: tokens put onto the battlefield blocking ─────────────────

/// CR 509.4: Mirror Match's token never blocked, so it is no history partner for
/// Gaze of the Gorgon. Gaze is P1's, so P1 orders its delayed ability against
/// Mirror Match's exile. In the order where Gaze resolves first, B dies while the
/// token is still on the battlefield with the exile on the stack.
#[test]
fn gaze_ignores_a_token_put_onto_the_battlefield_blocking() {
    let mut gaze_resolved_first = false;
    for reverse in [false, true] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let a = s.add_creature(P0, "A", 0, 5).id();
        let b = s.add_creature(P1, "B", 0, 6).id();
        let mirror = free_spell(&mut s, P1, "Mirror Match", MIRROR_MATCH);
        let gaze = free_spell(&mut s, P1, "Gaze of the Gorgon", GAZE);
        let mut runner = s.build();
        attack_block(&mut runner, &[a], &[(b, a)]);
        pass_priority_to(&mut runner, P1);
        runner.cast(mirror).resolve();
        drain(&mut runner);
        let token = mirror_token(&runner, a);
        assert_ne!(token, b);
        pass_priority_to(&mut runner, P1);
        runner.cast(gaze).target_objects(&[a]).resolve();
        drain(&mut runner);
        pass_until(&mut runner, "end of combat", |s| {
            s.phase == Phase::EndCombat || matches!(s.waiting_for, WaitingFor::OrderTriggers { .. })
        });
        for _ in 0..16 {
            if zone(&runner, b) == Zone::Graveyard {
                break;
            }
            match runner.state().waiting_for {
                WaitingFor::OrderTriggers { ref triggers, .. } => {
                    let mut order: Vec<usize> = (0..triggers.len()).collect();
                    if reverse {
                        order.reverse();
                    }
                    runner
                        .act(GameAction::OrderTriggers { order })
                        .expect("order triggers");
                }
                _ => {
                    runner.act(GameAction::PassPriority).expect("pass priority");
                }
            }
        }
        assert_eq!(zone(&runner, b), Zone::Graveyard, "the declared blocker");
        if runner.state().stack.len() == 1 {
            gaze_resolved_first = true;
            assert_eq!(
                runner.state().objects.get(&token).map(|object| object.zone),
                Some(Zone::Battlefield),
                "CR 509.4: the token never blocked, and its exile is still pending"
            );
        }
    }
    assert!(
        gaze_resolved_first,
        "reach: one order resolves Gaze before the exile"
    );
}

/// CR 509.4: Glyph of Doom on Mirror Match's copy of an attacking Wall: the token
/// blocked nothing, so the attacking Wall survives.
#[test]
fn glyph_on_a_token_wall_reads_no_block() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let wall = s
        .add_creature(P0, "Attacking Wall", 0, 6)
        .with_subtypes(vec!["Wall"])
        .id();
    // P1 needs a creature, or no block declaration is offered.
    s.add_creature(P1, "Spare", 0, 1);
    let mirror = free_spell(&mut s, P1, "Mirror Match", MIRROR_MATCH);
    let glyph = free_spell(&mut s, P0, "Glyph of Doom", GLYPH);
    let mut runner = s.build();
    attack_block(&mut runner, &[wall], &[]);
    pass_priority_to(&mut runner, P1);
    runner.cast(mirror).resolve();
    drain(&mut runner);
    let token = mirror_token(&runner, wall);
    runner.cast(glyph).target_objects(&[token]).resolve();
    drain(&mut runner);
    assert_eq!(
        runner
            .state()
            .delayed_triggers
            .iter()
            .filter(|delayed| delayed.ability.controller == P0)
            .count(),
        1,
        "reach: Glyph's delayed ability"
    );
    past_end_of_combat(&mut runner);
    assert_eq!(
        runner
            .state()
            .delayed_triggers
            .iter()
            .filter(|delayed| delayed.ability.controller == P0)
            .count(),
        0,
        "reach: Glyph's delayed ability fired and resolved"
    );
    assert_eq!(zone(&runner, wall), Zone::Battlefield);
}

// ── V17: Joven's Ferrets ───────────────────────────────────────────────

/// CR 509.1g: Joven's Ferrets taps the creatures that blocked it. Y blocked the
/// Ferrets and is tapped; Z blocked A2 and a bystander did nothing, and both stay
/// untapped. Hostile: the Ferrets block A, which blocked nothing, so A (vigilant,
/// hence untapped) stays untapped.
#[test]
fn ferrets_tap_only_the_creatures_that_blocked_them() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let ferrets = s
        .add_creature_from_oracle(P0, "Joven's Ferrets", 1, 1, FERRETS)
        .id();
    let a2 = s.add_creature(P0, "A2", 1, 5).id();
    let y = s.add_creature(P1, "Y", 0, 6).id();
    let z = s.add_creature(P1, "Z", 0, 6).id();
    let bystander = s.add_creature(P1, "Bystander", 1, 1).id();
    let mut runner = s.build();
    attack_block(&mut runner, &[ferrets, a2], &[(y, ferrets), (z, a2)]);
    past_end_of_combat(&mut runner);
    assert!(tapped(&runner, y), "Y blocked the Ferrets");
    assert!(!tapped(&runner, z), "Z blocked another creature");
    assert!(!tapped(&runner, bystander));

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 0, 5).vigilance().id();
    let ferrets = s
        .add_creature_from_oracle(P1, "Joven's Ferrets", 1, 6, FERRETS)
        .id();
    let mut runner = s.build();
    attack_block(&mut runner, &[a], &[(ferrets, a)]);
    assert!(
        !tapped(&runner, a),
        "reach: the vigilant attacker is untapped"
    );
    past_end_of_combat(&mut runner);
    assert!(!tapped(&runner, a), "A did not block the Ferrets");
}

// ── V19, V20: two spells, redirect ─────────────────────────────────────

/// CR 603.3b: two Venomous Breaths on different targets each read their own
/// frozen target. Their delayed abilities trigger together, so one ordering
/// prompt, and both blockers die.
#[test]
fn two_breaths_read_their_own_targets() {
    let mut x = blocked_board(BREATH, 1);
    x.runner.cast(x.spell).target_objects(&[x.a]).resolve();
    drain(&mut x.runner);
    x.runner.cast(x.spell2).target_objects(&[x.a2]).resolve();
    drain(&mut x.runner);
    assert_eq!(past_end_of_combat(&mut x.runner), 1, "one ordering prompt");
    assert_eq!(zone(&x.runner, x.b), Zone::Graveyard);
    assert_eq!(zone(&x.runner, x.b2), Zone::Graveyard);
}

/// CR 115.7d: a spell's new target is its target. Ride Down moved from B to B2:
/// B2 dies and A2 tramples, A does not. Gaze of the Gorgon moved from A to A2: B2
/// dies and B stays.
#[test]
fn a_redirected_spell_reads_its_new_target() {
    let mut x = blocked_board(RIDE, 5);
    x.runner.cast(x.spell).target_objects(&[x.b]).commit();
    let spell_on_stack = x.runner.state().stack.back().expect("Ride Down").id;
    x.runner
        .cast(x.redirect)
        .target_objects(&[spell_on_stack])
        .commit();
    drive_to_retarget(&mut x.runner);
    x.runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(x.b2))],
        })
        .expect("retarget");
    drain(&mut x.runner);
    assert_eq!(zone(&x.runner, x.b2), Zone::Graveyard);
    assert_eq!(zone(&x.runner, x.b), Zone::Battlefield);
    assert!(tramples(&x.runner, x.a2));
    assert!(!tramples(&x.runner, x.a));

    let mut x = blocked_board(GAZE, 1);
    x.runner.cast(x.spell).target_objects(&[x.a]).commit();
    let spell_on_stack = x.runner.state().stack.back().expect("Gaze").id;
    x.runner
        .cast(x.redirect)
        .target_objects(&[spell_on_stack])
        .commit();
    drive_to_retarget(&mut x.runner);
    x.runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(x.a2))],
        })
        .expect("retarget");
    drain(&mut x.runner);
    past_end_of_combat(&mut x.runner);
    assert_eq!(zone(&x.runner, x.b2), Zone::Graveyard);
    assert_eq!(zone(&x.runner, x.b), Zone::Battlefield);
}

// ── V21, V22, V23: windows and the turn bound ──────────────────────────

/// CR 500.8: "this turn" spans every combat of the turn. B blocks A in the first
/// Full Throttle combat; Venomous Breath on A in the second, where nothing blocks,
/// still kills B, and B2, which blocked nothing, stays.
#[test]
fn breath_this_turn_reaches_an_earlier_combat() {
    let (mut runner, a, b, b2, spell) = full_throttle_board(BREATH);
    to_combat_two(&mut runner, a, b);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(a, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("combat two attack");
    to_block_prompt(&mut runner);
    block(&mut runner, &[]);
    runner.cast(spell).target_objects(&[a]).resolve();
    drain(&mut runner);
    pass_until(&mut runner, "end of combat two", |s| {
        s.phase == Phase::EndCombat
    });
    pass_until(&mut runner, "fired", |s| {
        s.stack.is_empty() && s.phase != Phase::EndCombat
    });
    assert_eq!(zone(&runner, b), Zone::Graveyard);
    assert_eq!(zone(&runner, b2), Zone::Battlefield);
}

/// CR 511.3: "this combat" is the current combat only. W blocks A1 in the first
/// Full Throttle combat and A2 in the second; Ride Down on W in the second gives
/// A2 trample and A1 none.
#[test]
fn ride_down_this_combat_ignores_an_earlier_combat() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a1 = s.add_creature(P0, "A1", 5, 5).id();
    let a2 = s.add_creature(P0, "A2", 5, 5).id();
    let w = s.add_creature(P1, "W", 1, 7).id();
    let throttle = s
        .add_spell_to_hand_from_oracle(P0, "Full Throttle", false, FULL_THROTTLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let ride = free_spell(&mut s, P0, "Ride Down", RIDE);
    let mut runner = s.build();
    runner.cast(throttle).resolve();
    runner.advance_until_stack_empty();
    to_combat_two(&mut runner, a1, w);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(a2, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("combat two attack");
    to_block_prompt(&mut runner);
    block(&mut runner, &[(w, a2)]);
    runner.cast(ride).target_objects(&[w]).resolve();
    drain(&mut runner);
    assert_eq!(zone(&runner, w), Zone::Graveyard);
    assert!(tramples(&runner, a2), "this combat");
    assert!(!tramples(&runner, a1), "the first combat is another combat");
}

/// CR 603.7b: "this turn's next end of combat" lapses at cleanup. Gaze of the
/// Gorgon and Triton Tactics cast in the postcombat main phase leave a delayed
/// ability that is gone by the next turn and never fired: B stays. The twin cast
/// in combat fires and kills B.
#[test]
fn this_turns_next_end_of_combat_lapses_at_cleanup() {
    for cast_after_combat in [true, false] {
        let mut x = board(GAZE, 1);
        attack_block(&mut x.runner, &[x.a], &[(x.b, x.a)]);
        if cast_after_combat {
            past_end_of_combat(&mut x.runner);
        }
        x.runner.cast(x.spell).target_objects(&[x.a]).resolve();
        drain(&mut x.runner);
        assert_eq!(x.runner.state().delayed_triggers.len(), 1, "reach");
        if cast_after_combat {
            let turn = x.runner.state().turn_number;
            pass_until(&mut x.runner, "next turn", |s| {
                s.turn_number == turn + 1 && s.phase == Phase::PreCombatMain
            });
            assert!(x.runner.state().delayed_triggers.is_empty(), "lapsed");
            assert_eq!(zone(&x.runner, x.b), Zone::Battlefield, "never fired");
        } else {
            past_end_of_combat(&mut x.runner);
            assert_eq!(zone(&x.runner, x.b), Zone::Graveyard, "fired");
        }
    }

    let mut x = board(TRITON, 1);
    attack_block(&mut x.runner, &[x.a], &[(x.b, x.a)]);
    past_end_of_combat(&mut x.runner);
    x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
    drain(&mut x.runner);
    assert_eq!(x.runner.state().delayed_triggers.len(), 1, "reach");
    let turn = x.runner.state().turn_number;
    pass_until(&mut x.runner, "next turn", |s| {
        s.turn_number == turn + 1 && s.phase == Phase::PreCombatMain
    });
    assert!(
        x.runner.state().delayed_triggers.is_empty(),
        "Triton Tactics lapses too"
    );
}

/// CR 603.7b + CR 500.8: the delayed ability triggers once, at the next end of
/// combat. Venomous Breath cast in the first Full Throttle combat fires there,
/// and B2 blocking A in the second combat survives.
#[test]
fn breath_fires_at_the_next_end_of_combat_only() {
    let (mut runner, a, b, b2, spell) = full_throttle_board(BREATH);
    runner.advance_to_combat();
    attack(&mut runner, &[a]);
    to_block_prompt(&mut runner);
    block(&mut runner, &[(b, a)]);
    runner.cast(spell).target_objects(&[a]).resolve();
    drain(&mut runner);
    pass_until(&mut runner, "end of combat one", |s| {
        s.phase == Phase::EndCombat
    });
    pass_until(&mut runner, "fired", |s| {
        s.stack.is_empty()
            && !s
                .delayed_triggers
                .iter()
                .any(|delayed| delayed.ability.targets.len() == 1)
    });
    assert_eq!(
        zone(&runner, b),
        Zone::Graveyard,
        "fired at the first end of combat"
    );
    pass_until(&mut runner, "combat two", |s| {
        matches!(s.waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(a, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("combat two attack");
    to_block_prompt(&mut runner);
    block(&mut runner, &[(b2, a)]);
    pass_until(&mut runner, "end of combat two", |s| {
        s.phase == Phase::EndCombat
    });
    pass_until(&mut runner, "after", |s| {
        s.stack.is_empty() && s.phase != Phase::EndCombat
    });
    assert_eq!(zone(&runner, b2), Zone::Battlefield, "it triggered once");
}

/// CR 603.7b + CR 608.2h: Venomous Breath cast during an end of combat step waits
/// for the next one, and reads the whole turn when it resolves: B, which blocked A
/// in the first combat, and B2, which blocked it in the second, both die.
#[test]
fn breath_cast_during_end_of_combat_waits_for_the_next() {
    let (mut runner, a, b, b2, spell) = full_throttle_board(BREATH);
    runner.advance_to_combat();
    attack(&mut runner, &[a]);
    to_block_prompt(&mut runner);
    block(&mut runner, &[(b, a)]);
    pass_until(&mut runner, "end of combat one", |s| {
        s.phase == Phase::EndCombat
            && matches!(s.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    runner.cast(spell).target_objects(&[a]).resolve();
    drain(&mut runner);
    assert_eq!(runner.state().phase, Phase::EndCombat, "reach");
    assert_eq!(
        zone(&runner, b),
        Zone::Battlefield,
        "it does not fire in the step that created it"
    );
    pass_until(&mut runner, "combat two", |s| {
        matches!(s.waiting_for, WaitingFor::DeclareAttackers { .. })
    });
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(a, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("combat two attack");
    to_block_prompt(&mut runner);
    block(&mut runner, &[(b2, a)]);
    pass_until(&mut runner, "end of combat two", |s| {
        s.phase == Phase::EndCombat
    });
    pass_until(&mut runner, "after", |s| {
        s.stack.is_empty() && s.phase != Phase::EndCombat
    });
    assert_eq!(zone(&runner, b), Zone::Graveyard);
    assert_eq!(zone(&runner, b2), Zone::Graveyard);
}

// ── V27: Triton Tactics does not widen ─────────────────────────────────

/// CR 608.2c: Triton Tactics' clause names a partner no subject word covers, so
/// its delayed instruction fails closed instead of tapping every creature. On the
/// default board, with Triton on B, B2 is untapped after end of combat.
#[test]
fn triton_tactics_does_not_widen() {
    let mut x = blocked_board(TRITON, 1);
    x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
    drain(&mut x.runner);
    assert_eq!(
        x.runner.state().delayed_triggers.len(),
        1,
        "reach: the delayed ability exists"
    );
    assert!(!tapped(&x.runner, x.b2), "reach: blockers stay untapped");
    past_end_of_combat(&mut x.runner);
    assert!(
        x.runner.state().delayed_triggers.is_empty(),
        "reach: it fired"
    );
    assert!(!tapped(&x.runner, x.b2), "no widened tap");
}

/// Synthetic: Ride Down with its grant spelled "each creature that was blocked by a
/// Zombie", a partner no subject word covers. CR 608.2c + CR 611.2c: the broadcast
/// grant fails closed instead of giving every creature trample. B still dies, so
/// the spell resolved; neither attacker, nor B2, gains trample. No printed card has
/// this shape; Ride Down is the nearest printed card.
#[test]
fn a_broadcast_grant_with_an_unbound_partner_does_not_widen() {
    const UNBOUND_RIDE: &str = "Destroy target blocking creature. Each creature that was blocked by a Zombie this combat gains trample until end of turn.";
    let mut x = blocked_board(UNBOUND_RIDE, 5);
    x.runner.cast(x.spell).target_objects(&[x.b]).resolve();
    drain(&mut x.runner);
    assert_eq!(zone(&x.runner, x.b), Zone::Graveyard, "reach: it resolved");
    for (label, id) in [("A", x.a), ("A2", x.a2), ("B2", x.b2)] {
        assert!(!tramples(&x.runner, id), "{label}: no widened trample");
    }
}

// ── V10, V36: Heal the Scars ───────────────────────────────────────────

/// CR 608.2c + CR 701.19a: Heal the Scars gains the target's toughness when it
/// resolves (its ruling), and its shield is real: a later Murder is replaced and
/// the life total does not move again.
#[test]
fn heal_the_scars_gains_its_targets_toughness() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let t = s.add_creature(P0, "T", 2, 7).id();
    let heal = free_spell(&mut s, P0, "Heal the Scars", HEAL);
    let murder = free_spell(&mut s, P0, "Murder", MURDER);
    let mut runner = s.build();
    let before = life(&runner, P0);
    runner.cast(heal).target_objects(&[t]).resolve();
    drain(&mut runner);
    assert_eq!(life(&runner, P0), before + 7);
    runner.cast(murder).target_objects(&[t]).resolve();
    drain(&mut runner);
    assert_eq!(
        zone(&runner, t),
        Zone::Battlefield,
        "the shield replaced it"
    );
    assert_eq!(life(&runner, P0), before + 7);
}

/// CR 608.2b: no referent when the spell does not resolve. Murder on Sheltering
/// Word's target in response: no life.
#[test]
fn sheltering_word_gains_nothing_when_it_fizzles() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let t = s.add_creature(P0, "T", 2, 7).id();
    let word = free_spell(&mut s, P0, "Sheltering Word", SHELTERING_WORD);
    let murder = free_spell(&mut s, P0, "Murder", MURDER);
    let mut runner = s.build();
    let before = life(&runner, P0);
    runner.cast(word).target_objects(&[t]).commit();
    runner.cast(murder).target_objects(&[t]).resolve();
    drain(&mut runner);
    assert_eq!(
        zone(&runner, t),
        Zone::Graveyard,
        "reach: the target is gone"
    );
    assert_eq!(life(&runner, P0), before);
}

// ── V33: heads that never leave an event ───────────────────────────────

/// CR 608.2c: an instruction that leaves no event naming its target still names it
/// for the next instruction. Sheltering Word on a 2/7 gains 7.
#[test]
fn sheltering_word_gains_its_targets_toughness() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let t = s.add_creature(P0, "T", 2, 7).id();
    let word = free_spell(&mut s, P0, "Sheltering Word", SHELTERING_WORD);
    let mut runner = s.build();
    let before = life(&runner, P0);
    runner.cast(word).target_objects(&[t]).resolve();
    drain(&mut runner);
    assert!(
        runner.state().objects[&t].has_keyword(&Keyword::Hexproof),
        "reach"
    );
    assert_eq!(life(&runner, P0), before + 7);
}

/// CR 608.2c + CR 613.1: The Provider's counters count, so a 2/7 gains 9. The text
/// is hosted on a battlefield creature: the cost and the chain are the real ones,
/// and the card's type plays no part.
#[test]
fn the_provider_gains_toughness_after_its_counters() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let t = s.add_creature(P0, "T", 2, 7).id();
    let provider = s
        .add_creature_from_oracle(P0, "The Provider", 0, 1, THE_PROVIDER)
        .id();
    let mut runner = s.build();
    let before = life(&runner, P0);
    runner.activate(provider, 0).target_objects(&[t]).resolve();
    drain(&mut runner);
    assert_eq!(
        zone(&runner, provider),
        Zone::Exile,
        "reach: the cost was paid"
    );
    assert_eq!(runner.state().objects[&t].toughness, Some(9));
    assert_eq!(life(&runner, P0), before + 9);
}

/// CR 608.2c: Boros Fury-Shield with {R} spent deals the target attacker's power to
/// its controller; with no {R} the condition gates it. The cost is paid with a
/// real land.
#[test]
fn boros_fury_shield_reads_its_targets_power() {
    for (color, expected_loss) in [(ManaColor::Red, 3), (ManaColor::White, 0)] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let t = s.add_creature(P0, "T", 3, 3).id();
        let shield = s
            .add_spell_to_hand_from_oracle(P1, "Boros Fury-Shield", true, BOROS_FURY_SHIELD)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        s.add_basic_land(P1, color);
        let mut runner = s.build();
        runner.advance_to_combat();
        attack(&mut runner, &[t]);
        pass_until(&mut runner, "declare attackers priority", |s| {
            s.phase == Phase::DeclareAttackers
                && matches!(s.waiting_for, WaitingFor::Priority { player } if player == P0)
        });
        pass_priority_to(&mut runner, P1);
        let before = life(&runner, P0);
        runner.cast(shield).target_objects(&[t]).resolve();
        drain(&mut runner);
        assert_eq!(zone(&runner, shield), Zone::Graveyard, "reach: it resolved");
        assert_eq!(life(&runner, P0), before - expected_loss, "{color:?}");
    }
}

/// CR 608.2c: Imp's Mischief reads the spell it retargeted. A spell of mana value
/// 3 costs 3 life.
#[test]
fn imps_mischief_loses_the_retargeted_spells_mana_value() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let t = s.add_creature(P0, "T", 2, 7).id();
    let t2 = s.add_creature(P0, "T2", 2, 7).id();
    let murder = s
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let imp = free_spell(&mut s, P0, "Imp's Mischief", IMPS_MISCHIEF);
    s.with_mana_pool(P0, colorless(3));
    let mut runner = s.build();
    let before = life(&runner, P0);
    runner.cast(murder).target_objects(&[t]).commit();
    runner.cast(imp).target_objects(&[murder]).commit();
    drive_to_retarget(&mut runner);
    runner
        .act(GameAction::RetargetSpell {
            new_targets: vec![Some(TargetRef::Object(t2))],
        })
        .expect("retarget");
    drain(&mut runner);
    assert_eq!(
        zone(&runner, t2),
        Zone::Graveyard,
        "reach: the retarget took"
    );
    assert_eq!(zone(&runner, t), Zone::Battlefield);
    assert_eq!(life(&runner, P0), before - 3);
}

// ── V34: a target that survives ────────────────────────────────────────

/// A destroy spell and its target: who loses or gains, and by how much.
struct DestroyRow {
    name: &'static str,
    text: &'static str,
    artifact: bool,
    human: bool,
    /// (P0's change, P1's change), the same whether the target dies or survives.
    expected: (i32, i32),
}

/// CR 608.2c + CR 702.12b: an indestructible target survives the destroy, and the
/// next instruction still reads it (Feed the Swarm's, Phthisis', Divine Offering's
/// and Death's Caress' rulings). Each row's twin, where the target dies, gives the
/// same value through the destroy's event.
#[test]
fn destroy_class_reads_a_target_that_survived() {
    let rows = [
        DestroyRow {
            name: "Devour in Shadow",
            text: DEVOUR_IN_SHADOW,
            artifact: false,
            human: false,
            expected: (-7, 0),
        },
        DestroyRow {
            name: "Feed the Swarm",
            text: FEED_THE_SWARM,
            artifact: false,
            human: false,
            expected: (-4, 0),
        },
        DestroyRow {
            name: "Sever Soul",
            text: SEVER_SOUL,
            artifact: false,
            human: false,
            expected: (7, 0),
        },
        DestroyRow {
            name: "Phthisis",
            text: PHTHISIS,
            artifact: false,
            human: false,
            expected: (0, -9),
        },
        DestroyRow {
            name: "Divine Offering",
            text: DIVINE_OFFERING,
            artifact: true,
            human: false,
            expected: (4, 0),
        },
        DestroyRow {
            name: "Death's Caress",
            text: DEATHS_CARESS,
            artifact: false,
            human: true,
            expected: (7, 0),
        },
    ];
    for row in rows {
        for indestructible in [true, false] {
            let mut s = GameScenario::new();
            s.at_phase(Phase::PreCombatMain);
            let target = {
                let mut builder = if row.artifact {
                    s.add_artifact_from_oracle(P1, "T", "")
                } else {
                    s.add_creature(P1, "T", 2, 7)
                };
                builder.with_mana_cost(ManaCost::generic(4));
                if row.human {
                    builder.with_subtypes(vec!["Human"]);
                }
                if indestructible {
                    builder.indestructible();
                }
                builder.id()
            };
            let spell = free_spell(&mut s, P0, row.name, row.text);
            let mut runner = s.build();
            let (p0, p1) = (life(&runner, P0), life(&runner, P1));
            runner.cast(spell).target_objects(&[target]).resolve();
            drain(&mut runner);
            let expected_zone = if indestructible {
                Zone::Battlefield
            } else {
                Zone::Graveyard
            };
            assert_eq!(
                zone(&runner, target),
                expected_zone,
                "{}: reach, indestructible = {indestructible}",
                row.name
            );
            assert_eq!(
                (life(&runner, P0) - p0, life(&runner, P1) - p1),
                row.expected,
                "{}: indestructible = {indestructible}",
                row.name
            );
        }
    }
}

/// CR 608.2c: a creature spell that can't be countered survives the counter, and
/// the next instruction still reads it (Essence Backlash's ruling). Overwhelming
/// Intellect draws 3 for a mana value 3 spell; Essence Backlash deals the spell's
/// power, 4, to its controller.
#[test]
fn counter_class_reads_a_spell_that_survived() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let thing = s
        .add_creature_to_hand_from_oracle(P0, "Thing", 2, 2, UNCOUNTERABLE)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let intellect = free_spell(&mut s, P0, "Overwhelming Intellect", OVERWHELMING_INTELLECT);
    s.with_mana_pool(P0, colorless(3));
    s.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    let mut runner = s.build();
    runner.cast(thing).commit();
    runner.cast(intellect).target_objects(&[thing]).commit();
    let hand = runner.state().players[0].hand.len();
    drain(&mut runner);
    assert_eq!(
        zone(&runner, thing),
        Zone::Battlefield,
        "reach: not countered"
    );
    assert_eq!(runner.state().players[0].hand.len(), hand + 3);

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let thing = s
        .add_creature_to_hand_from_oracle(P0, "Thing", 4, 2, UNCOUNTERABLE)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let backlash = free_spell(&mut s, P0, "Essence Backlash", ESSENCE_BACKLASH);
    s.with_mana_pool(P0, colorless(3));
    let mut runner = s.build();
    let before = life(&runner, P0);
    runner.cast(thing).commit();
    runner.cast(backlash).target_objects(&[thing]).commit();
    drain(&mut runner);
    assert_eq!(
        zone(&runner, thing),
        Zone::Battlefield,
        "reach: not countered"
    );
    assert_eq!(life(&runner, P0), before - 4);
}

// ── V35: the next instruction only, and the root only ──────────────────

/// CR 608.2c: only the root instruction's next instruction reads the root's
/// target. Judge Unworthy's "that card" names the revealed card, so with an empty
/// library no card exists and no damage is dealt, not the target's mana value, 5.
/// No twin with a card on top: the reveal's referent reads 0 there with or
/// without the root's referent, a separate gap, so it cannot show this read.
#[test]
fn judge_unworthy_with_an_empty_library_deals_no_damage() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let t = s
        .add_creature(P0, "T", 2, 7)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let judge = free_spell(&mut s, P0, "Judge Unworthy", JUDGE_UNWORTHY);
    let mut runner = s.build();
    runner.advance_to_combat();
    attack(&mut runner, &[t]);
    pass_until(&mut runner, "declare attackers priority", |s| {
        s.phase == Phase::DeclareAttackers
            && matches!(s.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    assert!(
        runner.state().players[0].library.is_empty(),
        "reach: no card to reveal"
    );
    runner.cast(judge).target_objects(&[t]).commit();
    let events = resolve_keeping_scry_on_top(&mut runner);
    assert_eq!(zone(&runner, judge), Zone::Graveyard, "reach: it resolved");
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::DealDamage,
                source_id,
                ..
            } if *source_id == judge
        )),
        "reach: the damage instruction resolved; {events:?}"
    );
    assert_eq!(runner.state().objects[&t].damage_marked, 0);
}

/// CR 608.2c: a later instruction that chose its own target is not the root, so it
/// does not lend its target to the instruction after it. Hand-built: no printed
/// card has a targeted instruction between a reveal and its read (nearest: Pact
/// Weapon, whose parse gives its pump an `Any` target). The correct loss is 3, the
/// revealed card's mana value; this asserts only that the target's mana value, 5,
/// is not read, since the reveal's referent reads 0 here (the gap V35's Judge
/// Unworthy row names).
#[test]
fn a_non_root_target_lends_no_referent() {
    const TEXT: &str = "Draw a card and reveal it. Target creature gets +X/+X until end of turn and you lose X life, where X is that card's mana value.";
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let t = s
        .add_creature(P0, "T", 2, 2)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let drawn = s
        .add_spell_to_library_top(P0, "Bolt", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let spell = free_spell(&mut s, P0, "Spell", TEXT);
    let mut runner = s.build();
    assert!(
        !runner.state().objects[&spell]
            .abilities
            .iter()
            .any(|ability| {
                let mut node = Some(ability);
                while let Some(def) = node {
                    if matches!(*def.effect, Effect::Unimplemented { .. }) {
                        return true;
                    }
                    node = def.sub_ability.as_deref();
                }
                false
            }),
        "reach: the parse holds no Unimplemented"
    );
    let hand = runner.state().players[0].hand.len();
    let before = life(&runner, P0);
    let outcome = runner.cast(spell).target_objects(&[t]).resolve();
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::LoseLife,
                source_id,
                ..
            } if *source_id == spell
        )),
        "reach: the life-loss instruction resolved; {:?}",
        outcome.events()
    );
    drain(&mut runner);
    assert_eq!(zone(&runner, spell), Zone::Graveyard, "reach: it resolved");
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        zone(&runner, drawn),
        Zone::Hand,
        "reach: the card was drawn"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand,
        "drew one, cast one"
    );
    assert_ne!(
        life(&runner, P0),
        before - 5,
        "the target's mana value is not read"
    );
}

// ── V37: a dropped recipient stays unbound ─────────────────────────────

/// KNOWN-GAP PIN (CR 608.2c). Both cards parse with their printed "to target
/// creature" recipient dropped, so the damage instruction names the parent's own
/// target; Crush Underfoot's parse also reads "Choose a Giant creature you control"
/// as a target, which is why the Giant is announced here (CR 115.10a: "choose" does not
/// target). That dropped-recipient parse is a pre-existing gap tracked separately.
/// Rules-correct, the chosen Giant deals 4 to target creature T, and kicked Orim's
/// Thunder deals 4 to target creature X; the base parse cannot produce either. What
/// this row pins is only the `damages_parent_target` decline: the announced object
/// does not become the instruction's referent, so the damage instruction still
/// resolves (the positive guard) but deals nothing to it, where a referent would
/// size the damage from the announced object and send it back to that object.
#[test]
fn a_dropped_damage_recipient_takes_no_referent() {
    let damage_instruction_resolved = |events: &[GameEvent], spell: ObjectId| {
        events.iter().any(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::DealDamage,
                    source_id,
                    ..
                } if *source_id == spell
            )
        })
    };
    let damage_dealt = |events: &[GameEvent]| {
        events
            .iter()
            .any(|event| matches!(event, GameEvent::DamageDealt { .. }))
    };

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let giant = s
        .add_creature(P0, "Giant", 4, 9)
        .with_subtypes(vec!["Giant"])
        .id();
    s.add_creature(P1, "T", 2, 7);
    let crush = free_spell(&mut s, P0, "Crush Underfoot", CRUSH_UNDERFOOT);
    let mut runner = s.build();
    let outcome = runner.cast(crush).target_objects(&[giant]).resolve();
    assert!(
        damage_instruction_resolved(outcome.events(), crush),
        "reach: the damage instruction resolved; {:?}",
        outcome.events()
    );
    assert!(
        !damage_dealt(outcome.events()),
        "the declined instruction deals no damage"
    );
    drain(&mut runner);
    assert_eq!(zone(&runner, crush), Zone::Graveyard, "reach: it resolved");
    assert_eq!(runner.state().objects[&giant].damage_marked, 0);

    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let artifact = {
        let mut builder = s.add_creature(P1, "Artifact Creature", 2, 7);
        builder
            .as_artifact_creature()
            .with_mana_cost(ManaCost::generic(4))
            .indestructible();
        builder.id()
    };
    let x = s.add_creature(P1, "X", 2, 7).id();
    let orim = free_spell(&mut s, P0, "Orim's Thunder", ORIMS_THUNDER);
    s.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])],
    );
    let mut runner = s.build();
    let outcome = runner
        .cast(orim)
        .target_objects(&[artifact, x])
        .accept_optional()
        .resolve();
    assert!(
        damage_instruction_resolved(outcome.events(), orim),
        "reach: the kicked damage instruction resolved; {:?}",
        outcome.events()
    );
    assert!(
        !damage_dealt(outcome.events()),
        "the declined instruction deals no damage"
    );
    drain(&mut runner);
    assert_eq!(
        zone(&runner, artifact),
        Zone::Battlefield,
        "reach: it survived"
    );
    assert_eq!(runner.state().objects[&artifact].damage_marked, 0);
}

// ── V38, V39: triggered roots ──────────────────────────────────────────

/// CR 608.2c: Brightmare's trigger taps its target and gains that creature's
/// power. A target that is already tapped (it attacked) leaves no tap event and
/// still gives 4; the untapped twin gives 4 through the event.
#[test]
fn brightmare_reads_an_already_tapped_target() {
    for already_tapped in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let t = s.add_creature(P0, "T", 4, 7).id();
        let brightmare = s
            .add_creature_to_hand_from_oracle(P0, "Brightmare", 2, 3, BRIGHTMARE)
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = s.build();
        if already_tapped {
            runner.advance_to_combat();
            attack(&mut runner, &[t]);
            past_end_of_combat(&mut runner);
            assert!(tapped(&runner, t), "reach: the attacker is tapped");
        }
        let before = life(&runner, P0);
        runner.cast(brightmare).commit();
        settle(&mut runner, &[t], true);
        assert_eq!(zone(&runner, brightmare), Zone::Battlefield, "reach");
        assert!(tapped(&runner, t));
        assert_eq!(
            life(&runner, P0),
            before + 4,
            "already tapped = {already_tapped}"
        );
    }
}

/// CR 608.2c + CR 608.2h: a "you may" root still names its target after it resumes
/// from the choice. Lagonna-Band Storyteller puts a mana value 3 enchantment card
/// on top of the library: accept gains 3; decline gains nothing ("If you do"), and
/// the card stays in the graveyard.
#[test]
fn lagonna_band_storyteller_reads_its_target_after_the_choice() {
    for accept in [true, false] {
        let mut s = GameScenario::new();
        s.at_phase(Phase::PreCombatMain);
        let enchantment = s
            .add_spell_to_graveyard(P0, "Enchantment", false)
            .as_enchantment()
            .with_mana_cost(ManaCost::generic(3))
            .id();
        let storyteller = s
            .add_creature_to_hand_from_oracle(
                P0,
                "Lagonna-Band Storyteller",
                3,
                4,
                LAGONNA_BAND_STORYTELLER,
            )
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = s.build();
        let before = life(&runner, P0);
        runner.cast(storyteller).commit();
        assert!(
            settle(&mut runner, &[enchantment], accept),
            "reach: the optional choice was answered"
        );
        if accept {
            assert_eq!(zone(&runner, enchantment), Zone::Library, "reach");
            assert_eq!(life(&runner, P0), before + 3);
        } else {
            assert_eq!(zone(&runner, enchantment), Zone::Graveyard);
            assert_eq!(life(&runner, P0), before);
        }
    }
}

// ── A delayed ability's captured object is not a target ───────────────────

const BOLT_BEND: &str = "This spell costs {3} less to cast if you control a creature with power 4 or greater.\nChange the target of target spell or ability with a single target.";
const DEFLECTING_SWAT: &str = "If you control a commander, you may cast this spell without paying its mana cost.\nYou may choose new targets for target spell or ability.";

/// Venomous Breath's delayed ability on the stack at end of combat, above it
/// Murder on B. Returns the runner, the delayed entry, Murder, and the two
/// retarget spells in P0's hand.
fn breath_delayed_beside_murder() -> (GameRunner, ObjectId, ObjectId, ObjectId, ObjectId) {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let a = s.add_creature(P0, "A", 1, 5).id();
    let b = s.add_creature(P1, "B", 1, 6).id();
    // Another creature, so a captured slot would have a new choice.
    s.add_creature(P0, "C", 1, 5);
    let breath = free_spell(&mut s, P0, "Venomous Breath", BREATH);
    let murder = free_spell(&mut s, P0, "Murder", MURDER);
    let bend = free_spell(&mut s, P0, "Bolt Bend", BOLT_BEND);
    let swat = free_spell(&mut s, P0, "Deflecting Swat", DEFLECTING_SWAT);
    s.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    s.with_library_top(P1, &["Forest", "Forest", "Forest"]);
    let mut runner = s.build();
    attack_block(&mut runner, &[a], &[(b, a)]);
    runner.cast(breath).target_objects(&[a]).resolve();
    drain(&mut runner);
    pass_until(&mut runner, "end of combat", |s| {
        s.phase == Phase::EndCombat
    });
    assert_eq!(runner.state().stack.len(), 1, "reach: the delayed ability");
    let delayed = runner.state().stack[0].id;
    assert_eq!(
        runner.state().stack[0].ability().map(|a| a.targets.len()),
        Some(1),
        "reach: it carries the captured creature"
    );
    runner.cast(murder).target_objects(&[b]).commit();
    (runner, delayed, murder, bend, swat)
}

/// Bolt Bend's legal targets: the prompt's, or, when there is one, the
/// target it was cast with (CR 601.2c: a single legal choice is announced).
fn bolt_bend_targets(runner: &mut GameRunner, bend: ObjectId) -> Vec<TargetRef> {
    let card_id = runner.state().objects[&bend].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bend,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots[0].legal_targets.clone(),
        WaitingFor::Priority { .. } => {
            let top = runner.state().stack.last().expect("Bolt Bend on the stack");
            assert_eq!(top.source_id, bend, "reach: Bolt Bend was cast");
            top.ability().expect("a spell").targets.clone()
        }
        other => panic!("target prompt: {other:?}"),
    }
}

/// CR 115.9a + CR 115.10a: "with a single target" counts the times an object
/// was chosen as a target when the ability was put on the stack. Venomous
/// Breath's delayed ability names "it" and declares no target, so Bolt Bend
/// can't target it, while Murder, with its one chosen target, can.
#[test]
fn a_delayed_ability_with_no_declared_target_has_no_single_target() {
    let (mut runner, delayed, murder, bend, _) = breath_delayed_beside_murder();
    let legal = bolt_bend_targets(&mut runner, bend);
    assert_eq!(
        legal,
        vec![TargetRef::Object(murder)],
        "only Murder has a single target, not the delayed ability {delayed:?}"
    );
}

/// CR 115.7 + CR 115.10a: Deflecting Swat can target any ability, but the
/// delayed ability has no target to choose anew, so Swat resolves with no
/// retarget prompt. Over Murder, the same Swat asks for Murder's new target.
#[test]
fn choosing_new_targets_for_a_delayed_ability_offers_no_target() {
    for over_murder in [false, true] {
        let (mut runner, delayed, murder, _, swat) = breath_delayed_beside_murder();
        let target = if over_murder { murder } else { delayed };
        runner.cast(swat).target_objects(&[target]).commit();
        pass_until(&mut runner, "swat resolves", |s| {
            s.stack.len() == 2
                || matches!(
                    s.waiting_for,
                    WaitingFor::OptionalEffectChoice { .. } | WaitingFor::RetargetChoice { .. }
                )
        });
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .expect("choose new targets");
        }
        assert_eq!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::RetargetChoice { .. }
            ),
            over_murder,
            "over Murder: {over_murder}: whether Swat asks for a new target: {:?}",
            runner.state().waiting_for
        );
    }
}

// ── The announced creature reaches only the next instruction ─────────────

/// Heal the Scars' first two instructions, then Judge Unworthy's reveal and
/// damage. Constructed: no printed card composes them.
const HEAL_THEN_JUDGE: &str = "Regenerate target creature. You gain life equal to that creature's toughness. Scry 3, then reveal the top card of your library. This spell deals damage equal to that card's mana value to that creature.";

/// CR 608.2c + CR 609.3: the regenerate leaves no event naming its target, so
/// the life gain reads the announced creature (toughness 7). The reveal finds no
/// card in the empty library, so "that card's mana value" names nothing and no
/// damage is dealt, not the creature's mana value, 5.
#[test]
fn the_announced_creature_does_not_reach_past_the_next_instruction() {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    let t = s
        .add_creature(P0, "T", 2, 7)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let spell = free_spell(&mut s, P0, "Heal Then Judge", HEAL_THEN_JUDGE);
    let mut runner = s.build();
    assert!(
        runner.state().players[0].library.is_empty(),
        "reach: no card to reveal"
    );
    let before = life(&runner, P0);
    runner.cast(spell).target_objects(&[t]).commit();
    let events = resolve_keeping_scry_on_top(&mut runner);
    assert_eq!(zone(&runner, spell), Zone::Graveyard, "reach: it resolved");
    assert_eq!(
        life(&runner, P0),
        before + 7,
        "reach: the next instruction read it"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::DealDamage,
                source_id,
                ..
            } if *source_id == spell
        )),
        "reach: the damage instruction resolved; {events:?}"
    );
    assert_eq!(runner.state().objects[&t].damage_marked, 0);
}
