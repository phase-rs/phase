//! A batch a token-creation replacement appends ("those tokens plus that many
//! 1/1 green Squirrel creature tokens are created instead" — Chatterfang,
//! Squirrel General; "instead create one of each" — Academy Manufactor) is part
//! of the ONE modified CreateToken event, through the real pipeline.
//!
//! - CR 614.1a: "instead" yields one modified event.
//! - CR 616.1 + CR 616.1f: the affected player orders the replacements that
//!   apply to that one event and repeats the choice over the modified event, so
//!   a replacement applied after the appender applies to the appended batch too.
//! - CR 616.2: a replacement can become applicable to the event as the result of
//!   another replacement (Ojer Taq on the Squirrels of a Treasure event).
//! - CR 614.5: each replacement gets one opportunity at the event.
//! - CR 616.1g + CR 701.7b: a replacement of the tokens' entry (Grumgully,
//!   Mystic Reflection) can't be chosen until the creation's replacements
//!   (Chatterfang, Parallel Lives, Halving Season) have been; it then applies to
//!   every batch it matches.
//! - CR 608.2c: "that token" / "those tokens" name every token the event
//!   created (Chatterfang ruling: anything else the creating effect specifies
//!   applies to the original tokens and the Squirrels alike).
//!
//! On main, an appended batch was proposed as a separate nested event inside the
//! replacement applier; a nested ordering choice dropped it, and a nested
//! entry-counter pause left it half-delivered.

use engine::game::combat::creature_must_attack;
use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, EffectKind};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (MTGJSON).
const CHATTERFANG: &str = "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)\nIf one or more tokens would be created under your control, those tokens plus that many 1/1 green Squirrel creature tokens are created instead.\n{B}, Sacrifice X Squirrels: Target creature gets +X/-X until end of turn.";
const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, it creates twice that many of those tokens instead.";
const HALVING_SEASON: &str = "If an opponent would create one or more tokens, they create half that many of each of those kinds of tokens instead, rounded down.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";
const LEGION_WARBOSS: &str = "Mentor (Whenever this creature attacks, put a +1/+1 counter on target attacking creature with lesser power.)\nAt the beginning of combat on your turn, create a 1/1 red Goblin creature token. That token gains haste until end of turn and attacks this combat if able.";
const ACADEMY_MANUFACTOR: &str =
    "If you would create a Clue, Food, or Treasure token, instead create one of each.";
const STRIKE_IT_RICH: &str = "Create a Treasure token. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")\nFlashback {2}{R} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";
const LAZOTEP_PLATING: &str = "Amass Zombies 1. (Put a +1/+1 counter on an Army you control. It's also a Zombie. If you don't control an Army, create a 0/0 black Zombie Army creature token first.)\nYou and permanents you control gain hexproof until end of turn. (You and they can't be the targets of spells or abilities your opponents control.)";
const COGWORK_ASSEMBLER: &str = "{7}: Create a token that's a copy of target artifact. That token gains haste. Exile it at the beginning of the next end step.";
const MIND_STONE: &str = "{T}: Add {C}.\n{1}, {T}, Sacrifice this artifact: Draw a card.";
const GRUMGULLY: &str =
    "Each other non-Human creature you control enters with an additional +1/+1 counter on it.";
const HARDENED_SCALES: &str = "If one or more +1/+1 counters would be put on a creature you control, that many plus one +1/+1 counters are put on it instead.";
const BRANCHING_EVOLUTION: &str = "If one or more +1/+1 counters would be put on a creature you control, twice that many +1/+1 counters are put on that creature instead.";
const RAISE_THE_ALARM: &str = "Create two 1/1 white Soldier creature tokens.";
const MOONLIT_MEDITATION: &str = "Enchant artifact or creature you control\nThe first time you would create one or more tokens each turn, you may instead create that many tokens that are copies of enchanted permanent.";
const OJER_TAQ: &str = "Vigilance\nIf one or more creature tokens would be created under your control, three times that many of those tokens are created instead.\nWhen Ojer Taq dies, return it to the battlefield tapped and transformed under its owner's control.";
const PRINTLIFTER_OOZE: &str = "Deathtouch\nWhenever this creature or another creature you control is turned face up, create a 0/0 green Ooze creature token with trample. The token enters with X +1/+1 counters on it, where X is the number of other creatures you control.\nDisguise {3}{G}";

/// How one expected `ReplacementChoice` prompt is answered.
#[derive(Debug, Clone, Copy)]
enum Pick {
    /// An ordering prompt: apply the candidate from this source next.
    Source(&'static str),
    /// An optional replacement's accept branch.
    Accept,
    /// An optional replacement's decline branch.
    Decline,
}

/// One expected `ReplacementChoice` prompt: P0 is asked, it offers exactly
/// `candidates` options, and it is answered with `pick`.
#[derive(Debug, Clone, Copy)]
struct Prompt {
    candidates: usize,
    pick: Pick,
}

const fn order(candidates: usize, source: &'static str) -> Prompt {
    Prompt {
        candidates,
        pick: Pick::Source(source),
    }
}

const ACCEPT: Prompt = Prompt {
    candidates: 2,
    pick: Pick::Accept,
};

const DECLINE: Prompt = Prompt {
    candidates: 2,
    pick: Pick::Decline,
};

/// Where `settle` stops once every expected prompt has been answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Until {
    /// Priority with an empty stack.
    StackEmpty,
    /// The declare-attackers decision (CR 508.1).
    DeclareAttackers,
}

/// Answer `prompt`, asserting who is asked and how many options are offered.
fn answer(runner: &mut GameRunner, prompt: Prompt) -> Vec<GameEvent> {
    let WaitingFor::ReplacementChoice {
        player,
        candidates,
        kind,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected a replacement prompt answered with {:?}, got {:?}",
            prompt.pick,
            runner.state().waiting_for
        );
    };
    let offered: Vec<String> = candidates
        .iter()
        .map(|c| format!("{}: {}", c.source_name, c.description))
        .collect();
    // CR 616.1: the affected player chooses — P0, whose tokens / counters these
    // are, even for P1's Halving Season.
    assert_eq!(player, P0, "P0 makes the choice, offered {offered:?}");
    assert_eq!(
        candidates.len(),
        prompt.candidates,
        "prompt answered with {:?} offers {offered:?}",
        prompt.pick
    );
    let index = match prompt.pick {
        Pick::Source(name) => {
            assert_eq!(kind, ReplacementChoiceKind::Order, "offered {offered:?}");
            candidates
                .iter()
                .position(|c| c.source_name == name)
                .unwrap_or_else(|| panic!("{name} is offered: {offered:?}"))
        }
        Pick::Accept => {
            assert_eq!(kind, ReplacementChoiceKind::OptionalBranch);
            0
        }
        Pick::Decline => {
            assert_eq!(kind, ReplacementChoiceKind::OptionalBranch);
            assert_eq!(candidates[1].description, "Decline");
            1
        }
    };
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("the choice is accepted")
        .events
}

/// Answer exactly `prompts`, in order, passing priority in between, until
/// `until`. Returns every event produced.
fn settle(runner: &mut GameRunner, prompts: &[Prompt], until: Until) -> Vec<GameEvent> {
    let mut prompts = prompts.iter();
    let mut events = Vec::new();
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::ReplacementChoice { .. } => {
                let prompt = *prompts.next().unwrap_or_else(|| {
                    panic!("unexpected extra prompt: {:?}", runner.state().waiting_for)
                });
                events.extend(answer(runner, prompt));
            }
            WaitingFor::DeclareAttackers { .. } if until == Until::DeclareAttackers => break,
            WaitingFor::Priority { .. }
                if runner.state().stack.is_empty() && until == Until::StackEmpty =>
            {
                break
            }
            WaitingFor::Priority { .. } => {
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("passing priority is legal")
                        .events,
                );
            }
            other => panic!("unexpected decision {other:?}"),
        }
    }
    let unanswered: Vec<_> = prompts.collect();
    assert!(
        unanswered.is_empty(),
        "prompts never raised: {unanswered:?}"
    );
    events
}

/// Battlefield tokens with the subtype `subtype`.
fn tokens<'a>(runner: &'a GameRunner, subtype: &str) -> Vec<&'a GameObject> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == subtype))
        .collect()
}

/// Battlefield tokens named `name`.
fn tokens_named<'a>(runner: &'a GameRunner, name: &str) -> Vec<&'a GameObject> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.name == name)
        .collect()
}

/// The one battlefield token with the subtype `subtype`.
fn the_token(runner: &GameRunner, subtype: &str) -> ObjectId {
    let found = tokens(runner, subtype);
    assert_eq!(found.len(), 1, "exactly one {subtype} token");
    found[0].id
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn effect_resolved(events: &[GameEvent], kind: EffectKind) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::EffectResolved { kind: k, .. } if *k == kind))
        .count()
}

/// CR 608.2c: the source each `kind` `EffectResolved` names, in event order —
/// the instruction's own source, never the host of a replacement that appended
/// a batch to its event.
fn effect_resolved_sources(events: &[GameEvent], kind: EffectKind) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::EffectResolved {
                kind: k, source_id, ..
            } if *k == kind => Some(*source_id),
            _ => None,
        })
        .collect()
}

fn ledger(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().last_created_token_ids.clone()
}

fn on_battlefield(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().battlefield.contains(&id)
}

fn hasty(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].has_keyword(&Keyword::Haste)
}

/// Advance to the end step with no attackers and let its triggers resolve.
fn finish_at_end_step(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("declaring no attackers is legal");
        runner.advance_to_end_step();
    }
    assert_eq!(runner.state().phase, Phase::End);
    runner.advance_until_stack_empty();
}

// ---------------------------------------------------------------------------
// Legion Warboss: an ordering prompt over the compound event
// ---------------------------------------------------------------------------

/// P0: Chatterfang, Legion Warboss, Parallel Lives; P1: Halving Season.
fn warboss_board() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
    scenario.add_creature_from_oracle(P0, "Legion Warboss", 2, 2, LEGION_WARBOSS);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
}

/// CR 616.1 + CR 616.1f + CR 614.1a: Chatterfang makes the event "1 Goblin + 1
/// Squirrel"; Parallel Lives, chosen next, doubles each batch (2 + 2); Halving
/// Season, last, halves each kind (1 + 1). The Squirrel batch rides the same
/// ordering — on main it was dropped. "That token gains haste … and attacks
/// this combat if able" reaches both (CR 608.2c).
#[test]
fn chatterfang_squirrel_kept_through_parallel_lives_then_halving_season() {
    let mut runner = warboss_board();
    settle(
        &mut runner,
        &[
            order(3, "Chatterfang, Squirrel General"),
            order(2, "Parallel Lives"),
        ],
        Until::DeclareAttackers,
    );

    let goblin = the_token(&runner, "Goblin");
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(ledger(&runner), vec![goblin, squirrel]);
    let WaitingFor::DeclareAttackers {
        valid_attacker_ids, ..
    } = &runner.state().waiting_for
    else {
        unreachable!("settled at the declare-attackers decision");
    };
    for (token, label) in [(goblin, "Goblin"), (squirrel, "Squirrel")] {
        assert!(hasty(&runner, token), "the {label} gains haste");
        assert!(
            valid_attacker_ids.contains(&token),
            "the {label} can attack"
        );
        assert!(
            creature_must_attack(runner.state(), token),
            "the {label} attacks this combat if able"
        );
    }
}

/// Non-commuting order on the same board: Halving Season after Chatterfang
/// halves each one-token kind to nothing, and Parallel Lives doubles nothing.
#[test]
fn chatterfang_squirrel_then_halving_season_first_creates_nothing() {
    let mut runner = warboss_board();
    settle(
        &mut runner,
        &[
            order(3, "Chatterfang, Squirrel General"),
            order(2, "Halving Season"),
        ],
        Until::DeclareAttackers,
    );
    assert!(tokens(&runner, "Goblin").is_empty());
    assert!(tokens(&runner, "Squirrel").is_empty());
}

/// Parallel Lives first makes 2 Goblins; Chatterfang and Halving Season then
/// commute (1 Goblin + 1 Squirrel either way), so there is one prompt only.
#[test]
fn chatterfang_squirrel_with_parallel_lives_first() {
    let mut runner = warboss_board();
    settle(
        &mut runner,
        &[order(3, "Parallel Lives")],
        Until::DeclareAttackers,
    );
    let goblin = the_token(&runner, "Goblin");
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(ledger(&runner), vec![goblin, squirrel]);
}

// ---------------------------------------------------------------------------
// Academy Manufactor: "instead create one of each"
// ---------------------------------------------------------------------------

/// P0: Academy Manufactor, Parallel Lives, Strike It Rich in hand;
/// P1: Halving Season. Casts Strike It Rich up to its first prompt.
fn manufactor_board() -> (GameRunner, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Academy Manufactor", 1, 3, ACADEMY_MANUFACTOR)
        .as_artifact_creature();
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strike It Rich", false, STRIKE_IT_RICH)
        .id();
    let mut runner = scenario.build();
    let events = runner.cast(spell).resolve().events().to_vec();
    (runner, events)
}

/// CR 614.1a + CR 616.1f: Manufactor's added Clue and Food batches belong to the
/// event, so Parallel Lives and Halving Season, ordered after it, apply to them
/// too: one of each. On main only the Treasure survived.
#[test]
fn academy_manufactor_batches_follow_the_same_order() {
    let (mut runner, _) = manufactor_board();
    settle(
        &mut runner,
        &[order(3, "Academy Manufactor"), order(2, "Parallel Lives")],
        Until::StackEmpty,
    );
    let treasure = the_token(&runner, "Treasure");
    let clue = the_token(&runner, "Clue");
    let food = the_token(&runner, "Food");
    assert_eq!(ledger(&runner), vec![treasure, clue, food]);
}

/// Sibling order: Halving Season right after Manufactor halves each one-token
/// kind to nothing.
#[test]
fn academy_manufactor_then_halving_season_creates_nothing() {
    let (mut runner, _) = manufactor_board();
    settle(
        &mut runner,
        &[order(3, "Academy Manufactor"), order(2, "Halving Season")],
        Until::StackEmpty,
    );
    for kind in ["Treasure", "Clue", "Food"] {
        assert!(tokens(&runner, kind).is_empty(), "no {kind}");
    }
}

/// Sibling order: Parallel Lives first; Manufactor and Halving Season then
/// commute (one of each).
#[test]
fn academy_manufactor_with_parallel_lives_first() {
    let (mut runner, _) = manufactor_board();
    settle(
        &mut runner,
        &[order(3, "Parallel Lives")],
        Until::StackEmpty,
    );
    for kind in ["Treasure", "Clue", "Food"] {
        assert_eq!(tokens(&runner, kind).len(), 1, "one {kind}");
    }
}

// ---------------------------------------------------------------------------
// Amass route
// ---------------------------------------------------------------------------

/// CR 701.47a + CR 614.1a: Lazotep Plating's Army creation is a CreateToken
/// event too. Chatterfang → Parallel Lives → Halving Season leaves one Army and
/// one Squirrel; Halving Season then halves amass's one counter to none, so the
/// 0/0 Army dies (CR 704.5f). On main the Squirrel was dropped.
#[test]
fn amass_army_and_squirrel_share_the_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Lazotep Plating", true, LAZOTEP_PLATING)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    settle(
        &mut runner,
        &[
            order(3, "Chatterfang, Squirrel General"),
            order(2, "Parallel Lives"),
        ],
        Until::StackEmpty,
    );

    let squirrel = the_token(&runner, "Squirrel");
    let created = ledger(&runner);
    assert_eq!(created.len(), 2, "one Army and one Squirrel: {created:?}");
    assert_eq!(created[1], squirrel, "the Army's batch comes first");
    assert!(
        !on_battlefield(&runner, created[0]),
        "the 0/0 Army got no counter and died"
    );
    assert!(tokens(&runner, "Army").is_empty());
}

// ---------------------------------------------------------------------------
// Copy route (Cogwork Assembler)
// ---------------------------------------------------------------------------

/// P0: Cogwork Assembler (seven mana floating), Mind Stone, Chatterfang, plus
/// whatever `extra` adds. Returns the runner, the Assembler and Mind Stone.
fn cogwork_board(
    assembler_is_creature: bool,
    extra: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let assembler = if assembler_is_creature {
        scenario
            .add_creature_from_oracle(P0, "Cogwork Assembler", 2, 3, COGWORK_ASSEMBLER)
            .as_artifact_creature()
            .id()
    } else {
        scenario
            .add_artifact_from_oracle(P0, "Cogwork Assembler", COGWORK_ASSEMBLER)
            .id()
    };
    let mind_stone = scenario
        .add_artifact_from_oracle(P0, "Mind Stone", MIND_STONE)
        .id();
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    extra(&mut scenario);
    scenario.with_mana_pool(
        P0,
        (0..7)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    (scenario.build(), assembler, mind_stone)
}

fn add_parallel_lives_and_opposing_halving_season(scenario: &mut GameScenario) {
    scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    scenario.add_enchantment_from_oracle(P1, "Halving Season", HALVING_SEASON);
}

fn add_grumgully_and_counter_doublers(scenario: &mut GameScenario) {
    scenario.add_creature_from_oracle(P0, "Grumgully, the Generous", 3, 3, GRUMGULLY);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
}

/// Activate Cogwork Assembler targeting `target` up to its first decision.
fn activate_cogwork(
    runner: &mut GameRunner,
    assembler: ObjectId,
    target: ObjectId,
) -> Vec<GameEvent> {
    let index = runner.state().objects[&assembler]
        .abilities
        .iter()
        .position(|ability| matches!(ability.kind, AbilityKind::Activated))
        .expect("Cogwork Assembler's activated ability");
    runner
        .activate(assembler, index)
        .target_object(target)
        .resolve()
        .events()
        .to_vec()
}

/// CR 707.2 + CR 614.1a + CR 608.2c: the copy event resumed after its ordering
/// choice. Chatterfang, then Parallel Lives, then Halving Season: one Mind Stone
/// copy and one Squirrel (on main the Squirrel was dropped), both the event's
/// referent with the copy first, so "That token gains haste" and "Exile it at
/// the beginning of the next end step" reach both (CR 702.10b, CR 603.7c).
#[test]
fn cogwork_copy_resume_keeps_squirrel_own_first() {
    let (mut runner, assembler, mind_stone) =
        cogwork_board(false, add_parallel_lives_and_opposing_halving_season);
    activate_cogwork(&mut runner, assembler, mind_stone);
    settle(
        &mut runner,
        &[
            order(3, "Chatterfang, Squirrel General"),
            order(2, "Parallel Lives"),
        ],
        Until::StackEmpty,
    );

    let copies = tokens_named(&runner, "Mind Stone");
    assert_eq!(copies.len(), 1, "one Mind Stone copy");
    let copy = copies[0].id;
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(ledger(&runner), vec![copy, squirrel]);
    assert!(hasty(&runner, copy), "the copy gains haste");
    assert!(hasty(&runner, squirrel), "the Squirrel gains haste");

    finish_at_end_step(&mut runner);
    assert!(!on_battlefield(&runner, copy), "the copy is exiled");
    assert!(!on_battlefield(&runner, squirrel), "the Squirrel is exiled");
}

/// Same board, Parallel Lives chosen first: Chatterfang and Halving Season then
/// apply without a prompt. On main the Squirrel existed but was not in the
/// copy's referent, so it was not hasty and survived the end step.
#[test]
fn cogwork_copy_resume_parallel_lives_first_includes_squirrel() {
    let (mut runner, assembler, mind_stone) =
        cogwork_board(false, add_parallel_lives_and_opposing_halving_season);
    activate_cogwork(&mut runner, assembler, mind_stone);
    settle(
        &mut runner,
        &[order(3, "Parallel Lives")],
        Until::StackEmpty,
    );

    let copies = tokens_named(&runner, "Mind Stone");
    assert_eq!(copies.len(), 1, "one Mind Stone copy");
    let copy = copies[0].id;
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(ledger(&runner), vec![copy, squirrel]);
    assert!(hasty(&runner, squirrel), "the Squirrel gains haste");

    finish_at_end_step(&mut runner);
    assert!(!on_battlefield(&runner, copy), "the copy is exiled");
    assert!(!on_battlefield(&runner, squirrel), "the Squirrel is exiled");
}

/// The copy batch itself pauses for its entry counters (Cogwork Assembler copies
/// itself, an artifact creature, so Grumgully applies to the copy). The
/// appended Squirrel is delivered first and stays in the referent; the stated
/// engine order for this case is the Squirrel, then the copy (the copy route
/// publishes after the ids already there). Counters with Halving Season last:
/// (1 + 1) × 2 / 2 = 2 on each. Grumgully, the copy's and the Squirrel's entry
/// replacement, is no longer offered among the creation replacements (CR
/// 616.1g). It applies to both batches after them without a prompt. On main
/// there was no Squirrel.
#[test]
fn copy_batch_pause_keeps_squirrel_in_referent() {
    let (mut runner, assembler, _) = cogwork_board(true, |scenario| {
        scenario.add_creature_from_oracle(P0, "Grumgully, the Generous", 3, 3, GRUMGULLY);
        scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
        scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
        add_parallel_lives_and_opposing_halving_season(scenario);
    });
    activate_cogwork(&mut runner, assembler, assembler);
    settle(
        &mut runner,
        &[
            order(3, "Chatterfang, Squirrel General"),
            order(2, "Parallel Lives"),
            // The Squirrel's entry counters, then the copy's.
            order(3, "Hardened Scales"),
            order(2, "Branching Evolution"),
            order(3, "Hardened Scales"),
            order(2, "Branching Evolution"),
        ],
        Until::StackEmpty,
    );

    let copies = tokens_named(&runner, "Cogwork Assembler");
    assert_eq!(copies.len(), 1, "one Cogwork Assembler copy");
    let copy = copies[0].id;
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(plus_counters(&runner, squirrel), 2);
    assert_eq!(plus_counters(&runner, copy), 2);
    assert_eq!(ledger(&runner), vec![squirrel, copy]);
    assert!(hasty(&runner, copy) && hasty(&runner, squirrel));

    finish_at_end_step(&mut runner);
    assert!(!on_battlefield(&runner, copy), "the copy is exiled");
    assert!(!on_battlefield(&runner, squirrel), "the Squirrel is exiled");
}

/// Drive path: no token prompt (Chatterfang alone, then Grumgully on the
/// appended creature batch, CR 616.2). The Squirrel's entry counters pause the
/// event inside the copy resolution; the rest of the event is delivered after
/// the answer and handed to the copy resolution. On main there was no counter
/// prompt, the Squirrel had no counters, and neither token was exiled.
fn copy_drive_appended_squirrel_pause(first: &'static str, squirrel_counters: u32) {
    let (mut runner, assembler, mind_stone) =
        cogwork_board(false, add_grumgully_and_counter_doublers);
    let mut events = activate_cogwork(&mut runner, assembler, mind_stone);
    events.extend(settle(&mut runner, &[order(2, first)], Until::StackEmpty));

    let copies = tokens_named(&runner, "Mind Stone");
    assert_eq!(copies.len(), 1, "one Mind Stone copy");
    let copy = copies[0].id;
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(plus_counters(&runner, squirrel), squirrel_counters);
    assert_eq!(plus_counters(&runner, copy), 0);
    assert_eq!(ledger(&runner), vec![copy, squirrel]);
    assert!(hasty(&runner, copy) && hasty(&runner, squirrel));
    assert_eq!(effect_resolved(&events, EffectKind::CopyTokenOf), 1);
    assert_eq!(effect_resolved(&events, EffectKind::Token), 0);

    finish_at_end_step(&mut runner);
    assert!(!on_battlefield(&runner, copy), "the copy is exiled");
    assert!(!on_battlefield(&runner, squirrel), "the Squirrel is exiled");
}

#[test]
fn copy_drive_appended_squirrel_pause_reaches_frame() {
    // (1 + 1) × 2.
    copy_drive_appended_squirrel_pause("Hardened Scales", 4);
}

#[test]
fn copy_drive_appended_squirrel_pause_branching_first() {
    // 1 × 2 + 1.
    copy_drive_appended_squirrel_pause("Branching Evolution", 3);
}

/// Resume path: the ordering choice parks the copy resolution; the event's
/// delivery detaches it, re-parks it under the Squirrel's counter pause, and
/// hands it the whole referent when the rest is delivered. Grumgully, which
/// applies only to the Squirrel batch (CR 616.2), is a replacement of the
/// tokens' entry, so it is not offered with the creation replacements and
/// applies after them (CR 616.1g). On main there was no Squirrel.
#[test]
fn copy_resume_appended_squirrel_pause_reaches_frame() {
    let (mut runner, assembler, mind_stone) = cogwork_board(false, |scenario| {
        add_grumgully_and_counter_doublers(scenario);
        add_parallel_lives_and_opposing_halving_season(scenario);
    });
    let mut events = activate_cogwork(&mut runner, assembler, mind_stone);
    events.extend(settle(
        &mut runner,
        &[
            order(3, "Chatterfang, Squirrel General"),
            order(2, "Parallel Lives"),
            order(3, "Hardened Scales"),
            order(2, "Branching Evolution"),
        ],
        Until::StackEmpty,
    ));

    let copies = tokens_named(&runner, "Mind Stone");
    assert_eq!(copies.len(), 1, "one Mind Stone copy");
    let copy = copies[0].id;
    let squirrel = the_token(&runner, "Squirrel");
    // (1 + 1) × 2 / 2 with Halving Season last.
    assert_eq!(plus_counters(&runner, squirrel), 2);
    assert_eq!(plus_counters(&runner, copy), 0);
    assert_eq!(ledger(&runner), vec![copy, squirrel]);
    assert!(hasty(&runner, copy) && hasty(&runner, squirrel));
    assert_eq!(effect_resolved(&events, EffectKind::CopyTokenOf), 1);
    assert_eq!(effect_resolved(&events, EffectKind::Token), 0);

    finish_at_end_step(&mut runner);
    assert!(!on_battlefield(&runner, copy), "the copy is exiled");
    assert!(!on_battlefield(&runner, squirrel), "the Squirrel is exiled");
}

// ---------------------------------------------------------------------------
// Moonlit Meditation: the "that many" of a whole-event substitution
// ---------------------------------------------------------------------------

/// P0: Chatterfang, Grizzly Bears enchanted by Moonlit Meditation, Raise the
/// Alarm. Casts Raise the Alarm up to its first prompt.
fn moonlit_board() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let moonlit = scenario
        .add_enchantment_from_oracle(P0, "Moonlit Meditation", MOONLIT_MEDITATION)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    let mut runner = scenario.build();
    runner.attach_as_bestowed_aura(moonlit, bears);
    runner.cast(spell).resolve();
    runner
}

/// CR 614.1a: after Chatterfang the event is 2 Soldiers + 2 Squirrels, so
/// Moonlit's "create that many tokens that are copies of enchanted permanent
/// instead" is four copies, and neither batch is created. On main it was two.
#[test]
fn moonlit_after_chatterfang_copies_every_token() {
    let mut runner = moonlit_board();
    settle(
        &mut runner,
        &[order(2, "Chatterfang, Squirrel General"), ACCEPT],
        Until::StackEmpty,
    );
    assert_eq!(tokens_named(&runner, "Grizzly Bears").len(), 4);
    assert!(tokens(&runner, "Soldier").is_empty());
    assert!(tokens(&runner, "Squirrel").is_empty());
}

/// Declining Moonlit after Chatterfang keeps the whole event: 2 Soldiers and 2
/// Squirrels (on main the Squirrels were dropped).
#[test]
fn moonlit_after_chatterfang_decline_keeps_soldiers_and_squirrels() {
    let mut runner = moonlit_board();
    settle(
        &mut runner,
        &[order(2, "Chatterfang, Squirrel General"), DECLINE],
        Until::StackEmpty,
    );
    assert_eq!(tokens(&runner, "Soldier").len(), 2);
    assert_eq!(tokens(&runner, "Squirrel").len(), 2);
    assert!(tokens_named(&runner, "Grizzly Bears").is_empty());
}

/// Moonlit first: the event becomes two copies, and Chatterfang, which has not
/// yet had its opportunity (CR 614.5), adds two Squirrels to it.
#[test]
fn moonlit_first_then_chatterfang_adds_squirrels() {
    let mut runner = moonlit_board();
    settle(
        &mut runner,
        &[order(2, "Moonlit Meditation"), ACCEPT],
        Until::StackEmpty,
    );
    assert_eq!(tokens_named(&runner, "Grizzly Bears").len(), 2);
    assert_eq!(tokens(&runner, "Squirrel").len(), 2);
    assert!(tokens(&runner, "Soldier").is_empty());
}

// ---------------------------------------------------------------------------
// Entry-counter pauses on the plain route
// ---------------------------------------------------------------------------

/// Play Printlifter Ooze face down and turn it face up; returns the events from
/// the turn-face-up action onward, up to its trigger's first decision.
fn flip_printlifter(runner: &mut GameRunner, printlifter: ObjectId) -> Vec<GameEvent> {
    let card_id = runner.state().objects[&printlifter].card_id;
    runner
        .act(GameAction::PlayFaceDown {
            object_id: printlifter,
            card_id,
        })
        .expect("play face down");
    runner.advance_until_stack_empty();
    let mut events = runner
        .act(GameAction::TurnFaceUp {
            object_id: printlifter,
            x: 0,
        })
        .expect("turn face up")
        .events;
    assert_eq!(
        runner.state().stack.len(),
        1,
        "reach guard: the turned-face-up trigger is on the stack"
    );
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("pass priority")
                .events,
        );
    }
    events
}

/// CR 614.1a + CR 608.2c: the event's own batch (the Ooze, X = 3: Printlifter,
/// the Bear and Chatterfang) pauses for its entry-counter ordering before the
/// appended Squirrel is created; the Squirrel is delivered after the answer and
/// the referent is the Ooze, then the Squirrel. On main the Squirrel was created
/// first and the referent was the Squirrel, then the Ooze.
fn appended_batch_after_entry_counter_pause(first: &'static str, ooze_counters: u32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    scenario.add_creature(P0, "Bear", 2, 2);
    scenario.add_enchantment_from_oracle(P0, "Hardened Scales", HARDENED_SCALES);
    scenario.add_enchantment_from_oracle(P0, "Branching Evolution", BRANCHING_EVOLUTION);
    let printlifter = scenario
        .add_creature_to_hand(P0, "Printlifter Ooze", 2, 2)
        .from_oracle_text_with_keywords(&["Deathtouch", "Disguise"], PRINTLIFTER_OOZE)
        .id();
    let mut runner = scenario.build();
    for _ in 0..12 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    let mut events = flip_printlifter(&mut runner, printlifter);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the Ooze's entry counters ask for an order"
    );
    assert!(
        tokens(&runner, "Squirrel").is_empty(),
        "the appended Squirrel is not created before the paused Ooze finishes"
    );
    events.extend(settle(&mut runner, &[order(2, first)], Until::StackEmpty));

    let ooze = the_token(&runner, "Ooze");
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(plus_counters(&runner, ooze), ooze_counters);
    assert_eq!(ledger(&runner), vec![ooze, squirrel]);
    assert_eq!(effect_resolved(&events, EffectKind::Token), 1);
}

#[test]
fn appended_batch_delivered_after_entry_counter_pause() {
    // (3 + 1) × 2.
    appended_batch_after_entry_counter_pause("Hardened Scales", 8);
}

#[test]
fn appended_batch_delivered_after_entry_counter_pause_branching_first() {
    // 3 × 2 + 1.
    appended_batch_after_entry_counter_pause("Branching Evolution", 7);
}

/// CR 616.2: Ojer Taq ("If one or more creature tokens would be created …")
/// does not apply to a Treasure event, but becomes applicable once Chatterfang
/// adds a creature batch to it — and then applies to that batch only.
#[test]
fn ojer_taq_triples_only_the_appended_creature_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    scenario.add_creature_from_oracle(P0, "Ojer Taq, Deepest Foundation", 6, 6, OJER_TAQ);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strike It Rich", false, STRIKE_IT_RICH)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    settle(&mut runner, &[], Until::StackEmpty);

    let treasure = the_token(&runner, "Treasure");
    assert_eq!(tokens(&runner, "Squirrel").len(), 3);
    let created = ledger(&runner);
    assert_eq!(created.len(), 4);
    assert_eq!(created[0], treasure, "the Treasure batch comes first");
}

/// P0: Chatterfang, Grumgully, Hardened Scales, Branching Evolution and Strike
/// It Rich in hand (plus `extra`). Casts Strike It Rich up to its first prompt;
/// returns the runner, the spell and the events so far.
fn strike_it_rich_counter_board(
    extra: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    add_grumgully_and_counter_doublers(&mut scenario);
    extra(&mut scenario);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strike It Rich", false, STRIKE_IT_RICH)
        .id();
    let mut runner = scenario.build();
    let events = runner.cast(spell).resolve().events().to_vec();
    (runner, spell, events)
}

/// CR 608.2c: the Treasure batch completes, then the appended Squirrel (a
/// noncreature event made a creature batch by Chatterfang, CR 616.2) pauses for
/// its Grumgully counter's ordering. That pause owes the instruction's one
/// terminal `EffectResolved` — on main a second one came from the nested event —
/// and it names Strike It Rich, not Chatterfang, whose Squirrel batch paused.
fn later_batch_pause(first: &'static str, squirrel_counters: u32) {
    let (mut runner, spell, mut events) = strike_it_rich_counter_board(|_| {});
    events.extend(settle(&mut runner, &[order(2, first)], Until::StackEmpty));

    let treasure = the_token(&runner, "Treasure");
    let squirrel = the_token(&runner, "Squirrel");
    assert_eq!(plus_counters(&runner, squirrel), squirrel_counters);
    assert_eq!(ledger(&runner), vec![treasure, squirrel]);
    assert_eq!(
        effect_resolved_sources(&events, EffectKind::Token),
        vec![spell]
    );
}

#[test]
fn later_batch_pause_emits_effect_resolved_once() {
    // (1 + 1) × 2.
    later_batch_pause("Hardened Scales", 4);
}

#[test]
fn later_batch_pause_branching_first() {
    // 1 × 2 + 1.
    later_batch_pause("Branching Evolution", 3);
}

/// With Parallel Lives, each of the two Squirrels pauses for its own counters;
/// the second pause nests inside the first's completion and the rest of the
/// event rides along. Two Treasures and two 4-counter Squirrels, in event order,
/// and one terminal `EffectResolved`, naming Strike It Rich.
#[test]
fn nested_counter_pause_keeps_event_order() {
    let (mut runner, spell, mut events) = strike_it_rich_counter_board(|scenario| {
        scenario.add_enchantment_from_oracle(P0, "Parallel Lives", PARALLEL_LIVES);
    });
    events.extend(settle(
        &mut runner,
        &[order(2, "Hardened Scales"), order(2, "Hardened Scales")],
        Until::StackEmpty,
    ));

    let treasures: Vec<ObjectId> = tokens(&runner, "Treasure").iter().map(|o| o.id).collect();
    let squirrels: Vec<ObjectId> = tokens(&runner, "Squirrel").iter().map(|o| o.id).collect();
    assert_eq!(treasures.len(), 2);
    assert_eq!(squirrels.len(), 2);
    for &squirrel in &squirrels {
        assert_eq!(plus_counters(&runner, squirrel), 4);
    }
    let created = ledger(&runner);
    assert_eq!(created.len(), 4);
    assert!(created[..2].iter().all(|id| treasures.contains(id)));
    assert!(created[2..].iter().all(|id| squirrels.contains(id)));
    assert_eq!(
        effect_resolved_sources(&events, EffectKind::Token),
        vec![spell]
    );
    assert!(created
        .iter()
        .all(|id| runner.state().objects[id].zone == Zone::Battlefield));
}

// ---------------------------------------------------------------------------
// A replacement that turns an appended batch into a copy batch (CR 616.2)
// ---------------------------------------------------------------------------

const MYSTIC_REFLECTION: &str = "Choose target nonlegendary creature. The next time one or more creatures or planeswalkers enter this turn, they enter as copies of the chosen creature.\nForetell {U} (During your turn, you may pay {2} and exile this card from your hand face down. Cast it on a later turn for its foretell cost.)";
const VANQUISHERS_BANNER: &str = "As this artifact enters, choose a creature type.\nCreatures you control of the chosen type get +1/+1.\nWhenever you cast a creature spell of the chosen type, draw a card.";
const SERRATED_ARROWS: &str = "This artifact enters with three arrowhead counters on it.\nAt the beginning of your upkeep, if there are no arrowhead counters on this artifact, sacrifice it.\n{T}, Remove an arrowhead counter from this artifact: Put a -1/-1 counter on target creature.";
const VORINCLEX: &str = "Trample, haste\nIf you would put one or more counters on a permanent or player, put twice that many of each of those kinds of counters on that permanent or player instead.\nIf an opponent would put one or more counters on a permanent or player, they put half that many of each of those kinds of counters on that permanent or player instead, rounded down.";

/// The Cogwork board plus Grizzly Bears and Mystic Reflection, which resolves
/// choosing the Bears, and the artifact `target` (plus `extra`). Returns the
/// runner, the Assembler and the target.
fn mystic_cogwork_board(
    target: (&'static str, &'static str),
    extra: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, ObjectId) {
    let mut ids = (ObjectId(0), ObjectId(0), ObjectId(0));
    let (mut runner, assembler, _) = cogwork_board(false, |scenario| {
        ids.0 = scenario
            .add_artifact_from_oracle(P0, target.0, target.1)
            .id();
        ids.1 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        ids.2 = scenario
            .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
            .id();
        extra(scenario);
    });
    let (target, bears, mystic) = ids;
    runner.cast(mystic).target_object(bears).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );
    (runner, assembler, target)
}

/// The two copy tokens the Cogwork event created — the copy of `target_name`
/// and the appended Squirrel batch, which entered as a copy of Grizzly Bears —
/// asserted to be its whole referent, in event order, both hasty, with one
/// terminal `EffectResolved{CopyTokenOf}`; then both are exiled at the next end
/// step.
fn assert_both_copy_batches_delivered(
    runner: &mut GameRunner,
    events: &[GameEvent],
    target_name: &str,
) {
    let target_copies = tokens_named(runner, target_name);
    assert_eq!(target_copies.len(), 1, "one {target_name} copy");
    let target_copy = target_copies[0].id;
    let bears_copies = tokens_named(runner, "Grizzly Bears");
    assert_eq!(
        bears_copies.len(),
        1,
        "the Squirrel batch enters as a copy of Grizzly Bears"
    );
    let bears_copy = bears_copies[0].id;
    assert!(tokens(runner, "Squirrel").is_empty());
    assert_eq!(ledger(runner), vec![target_copy, bears_copy]);
    assert!(hasty(runner, target_copy) && hasty(runner, bears_copy));
    assert_eq!(effect_resolved(events, EffectKind::CopyTokenOf), 1);
    assert_eq!(effect_resolved(events, EffectKind::Token), 0);

    finish_at_end_step(runner);
    assert!(
        !on_battlefield(runner, target_copy),
        "the {target_name} copy is exiled"
    );
    assert!(
        !on_battlefield(runner, bears_copy),
        "the Bears copy is exiled"
    );
}

/// CR 616.2 + CR 614.12a + CR 608.2c: Cogwork Assembler copies Vanquisher's
/// Banner (a noncreature, so Mystic Reflection does not apply yet). Chatterfang
/// adds a Squirrel, and Mystic Reflection then applies to that creature batch
/// only, so both batches are copy batches. The Banner copy pauses for its
/// as-enters creature-type choice with the Squirrel batch still to come; that
/// batch, entering as a copy of Grizzly Bears, is delivered once the Banner copy
/// enters, and "That token gains haste. Exile it at the beginning of the next
/// end step." reaches both. On the candidate before this the copy pause
/// panicked a debug build and dropped the later batch in release.
#[test]
fn copy_choice_pause_keeps_a_later_copy_batch() {
    let (mut runner, assembler, banner) =
        mystic_cogwork_board(("Vanquisher's Banner", VANQUISHERS_BANNER), |_| {});
    let mut events = activate_cogwork(&mut runner, assembler, banner);

    let WaitingFor::NamedChoice {
        player, options, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Banner copy asks for its creature type, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(
        tokens_named(&runner, "Grizzly Bears").is_empty(),
        "the later copy batch waits for the Banner copy's choice"
    );
    assert!(options.iter().any(|option| option == "Goblin"));
    events.extend(
        runner
            .act(GameAction::ChooseOption {
                choice: "Goblin".to_string(),
            })
            .expect("the creature type is accepted")
            .events,
    );
    events.extend(settle(&mut runner, &[], Until::StackEmpty));

    assert_both_copy_batches_delivered(&mut runner, &events, "Vanquisher's Banner");
}

/// The same shape through an entry-counter pause: Cogwork copies Serrated Arrows
/// (a noncreature). Chatterfang, then Parallel Lives, then P1's Halving Season
/// leave one Arrows copy and one Squirrel, which Mystic Reflection makes a copy
/// of Grizzly Bears (CR 616.1c applies it without a prompt). The Arrows copy's
/// three arrowhead counters pause for Vorinclex and Halving Season's order with
/// the Bears copy still to come.
fn copy_counter_pause(first: &'static str, arrowheads: u32) {
    let (mut runner, assembler, arrows) =
        mystic_cogwork_board(("Serrated Arrows", SERRATED_ARROWS), |scenario| {
            add_parallel_lives_and_opposing_halving_season(scenario);
            scenario.add_creature_from_oracle(P0, "Vorinclex, Monstrous Raider", 6, 6, VORINCLEX);
        });
    let mut events = activate_cogwork(&mut runner, assembler, arrows);
    events.extend(settle(
        &mut runner,
        &[
            order(3, "Chatterfang, Squirrel General"),
            order(2, "Parallel Lives"),
            order(2, first),
        ],
        Until::StackEmpty,
    ));

    let arrows_copy = tokens_named(&runner, "Serrated Arrows")[0].id;
    assert_eq!(
        runner.state().objects[&arrows_copy]
            .counters
            .get(&CounterType::Generic("arrowhead".to_string()))
            .copied(),
        Some(arrowheads)
    );
    assert_both_copy_batches_delivered(&mut runner, &events, "Serrated Arrows");
}

#[test]
fn copy_counter_pause_keeps_a_later_copy_batch() {
    // 3 × 2 / 2.
    copy_counter_pause("Vorinclex, Monstrous Raider", 3);
}

#[test]
fn copy_counter_pause_halving_season_first() {
    // 3 / 2 × 2.
    copy_counter_pause("Halving Season", 2);
}

/// CR 616.1g + CR 616.1c + CR 614.1a: Chatterfang's creation replacement
/// applies first, so the event is two Soldiers plus two Squirrels. Then Mystic
/// Reflection, the contained entry's copy effect and first among the entry's
/// replacements, makes all four Grizzly Bears copies and is used up once. Then
/// Grumgully's counter on each Bears copy pauses for its ordering. No CopyToken
/// resolution owns this event (the plain Effect::Token route), yet the
/// created-token referent names all four. The instruction ends with exactly one
/// `EffectResolved{Token}`, naming Raise the Alarm, not Chatterfang.
#[test]
fn copy_on_entry_after_chatterfang_copies_every_token() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    add_grumgully_and_counter_doublers(&mut scenario);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    let mut runner = scenario.build();
    runner.cast(mystic).target_object(bears).resolve();
    let mut events = runner.cast(spell).resolve().events().to_vec();
    // Each Bears copy asks for its counters' order.
    events.extend(settle(
        &mut runner,
        &[order(2, "Hardened Scales"); 4],
        Until::StackEmpty,
    ));

    assert!(tokens(&runner, "Soldier").is_empty());
    assert!(
        tokens(&runner, "Squirrel").is_empty(),
        "the Squirrels enter as Bears copies too"
    );
    let bears_copies: Vec<ObjectId> = tokens_named(&runner, "Grizzly Bears")
        .iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(
        bears_copies.len(),
        4,
        "all four tokens enter as Bears copies"
    );
    for &id in &bears_copies {
        // Grumgully's one counter, Hardened Scales first: (1 + 1) × 2.
        assert_eq!(plus_counters(&runner, id), 4);
    }
    // CR 608.2c: the referent is every token the event created, in event order
    // (the Soldiers' copies, then the Squirrels').
    assert_eq!(ledger(&runner), bears_copies);
    assert_eq!(
        effect_resolved_sources(&events, EffectKind::Token),
        vec![spell]
    );
}

/// CR 616.1g + CR 616.1: two creation-replacement prompts (Chatterfang, then
/// Parallel Lives; P1's Halving Season last) settle the event as two Soldiers
/// plus two Squirrels before Mystic Reflection, a replacement of the contained
/// entry, can be chosen; it then makes every one of them a copy of Colossal
/// Dreadmaw. Neither prompt offers it.
#[test]
fn contained_copy_reaches_every_batch_after_outer_prompts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    add_parallel_lives_and_opposing_halving_season(&mut scenario);
    let dreadmaw = scenario.add_creature(P0, "Colossal Dreadmaw", 6, 6).id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    let mut runner = scenario.build();
    runner.cast(mystic).target_object(dreadmaw).resolve();
    runner.cast(spell).resolve();
    settle(
        &mut runner,
        &[
            order(3, "Chatterfang, Squirrel General"),
            order(2, "Parallel Lives"),
        ],
        Until::StackEmpty,
    );

    assert_eq!(
        tokens_named(&runner, "Colossal Dreadmaw").len(),
        4,
        "2 + 2, doubled, then halved: all four enter as Dreadmaw copies"
    );
    assert!(tokens(&runner, "Squirrel").is_empty());
    assert!(tokens(&runner, "Soldier").is_empty());
    assert_eq!(ledger(&runner).len(), 4);
}

/// Grumgully added before or after Chatterfang; Raise the Alarm.
fn grumgully_chatterfang_board(grumgully_first: bool) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let add_grumgully = |scenario: &mut GameScenario| {
        scenario.add_creature_from_oracle(P0, "Grumgully, the Generous", 3, 3, GRUMGULLY);
    };
    if grumgully_first {
        add_grumgully(&mut scenario);
    }
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    if !grumgully_first {
        add_grumgully(&mut scenario);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Raise the Alarm", true, RAISE_THE_ALARM)
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).resolve();
    runner
}

/// Every token the event creates (two Soldiers, two Squirrels) has Grumgully's
/// one counter.
fn assert_each_token_has_one_grumgully_counter(runner: &mut GameRunner) {
    settle(runner, &[], Until::StackEmpty);
    let soldiers = tokens(runner, "Soldier");
    let squirrels = tokens(runner, "Squirrel");
    assert_eq!(soldiers.len(), 2, "Raise the Alarm's two Soldiers");
    assert_eq!(squirrels.len(), 2, "Chatterfang adds two Squirrels");
    for token in soldiers.iter().chain(&squirrels) {
        assert_eq!(
            plus_counters(runner, token.id),
            1,
            "{} has Grumgully's counter",
            token.name
        );
    }
}

/// CR 616.1g: Grumgully's counter is a replacement of the tokens' entry, so it
/// can't be chosen until Chatterfang's creation replacement has been, whatever
/// order the two are found in. It then reaches the Squirrels too.
#[test]
fn entry_counter_replacement_waits_for_chatterfang() {
    assert_each_token_has_one_grumgully_counter(&mut grumgully_chatterfang_board(true));
}

/// Sibling of `entry_counter_replacement_waits_for_chatterfang` with Chatterfang
/// found first: the same result, so that row's flip is the CR 616.1g order, not
/// an artifact of the candidate order.
#[test]
fn entry_counter_replacement_after_chatterfang_found_first() {
    assert_each_token_has_one_grumgully_counter(&mut grumgully_chatterfang_board(false));
}

const METALLIC_MIMIC: &str = "As this creature enters, choose a creature type.\nThis creature is the chosen type in addition to its other types.\nEach other creature you control of the chosen type enters with an additional +1/+1 counter on it.";

/// CR 616.2 + CR 614.12a + CR 608.2c: Strike It Rich's Treasure event is not
/// one Mystic Reflection applies to; Chatterfang appends a Squirrel batch, and
/// Mystic Reflection then applies to that creature batch only, so the event's
/// last batch is a copy of Metallic Mimic. That copy pauses for its as-enters
/// creature-type choice on the plain Effect::Token route, with no CopyToken
/// resolution owning the event and no earlier pause to carry the instruction's
/// terminal event. The instruction still ends with exactly one
/// `EffectResolved{Token}` (on the candidate before this, none), naming Strike
/// It Rich, not Chatterfang, whose paused batch carries it.
#[test]
fn last_copy_batch_choice_pause_on_token_route_emits_effect_resolved_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Chatterfang, Squirrel General", 3, 3, CHATTERFANG);
    let mimic = scenario
        .add_creature_from_oracle(P0, "Metallic Mimic", 2, 1, METALLIC_MIMIC)
        .as_artifact_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .id();
    let mystic = scenario
        .add_spell_to_hand_from_oracle(P0, "Mystic Reflection", true, MYSTIC_REFLECTION)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Strike It Rich", false, STRIKE_IT_RICH)
        .id();
    let mut runner = scenario.build();
    runner.cast(mystic).target_object(mimic).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );
    let mut events = runner.cast(spell).resolve().events().to_vec();

    let WaitingFor::NamedChoice {
        player, options, ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the Mimic copy asks for its creature type, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0);
    assert!(options.iter().any(|option| option == "Goblin"));
    let treasure = the_token(&runner, "Treasure");
    assert!(
        tokens_named(&runner, "Metallic Mimic").is_empty(),
        "reach guard: the last batch, a Mimic copy, waits for its choice"
    );
    assert_eq!(
        effect_resolved(&events, EffectKind::Token),
        0,
        "the instruction has not finished while its last batch is paused"
    );
    events.extend(
        runner
            .act(GameAction::ChooseOption {
                choice: "Goblin".to_string(),
            })
            .expect("the creature type is accepted")
            .events,
    );
    events.extend(settle(&mut runner, &[], Until::StackEmpty));

    let mimic_copies = tokens_named(&runner, "Metallic Mimic");
    assert_eq!(
        mimic_copies.len(),
        1,
        "the Squirrel batch enters as a copy of Metallic Mimic"
    );
    let mimic_copy = mimic_copies[0].id;
    assert!(tokens(&runner, "Squirrel").is_empty());
    assert_eq!(ledger(&runner), vec![treasure, mimic_copy]);
    assert_eq!(
        effect_resolved_sources(&events, EffectKind::Token),
        vec![spell]
    );
    assert!(
        runner.state().resolution_stack.is_empty(),
        "no completion frame is left parked"
    );
}
