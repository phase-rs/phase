//! Runtime coverage for enters triggers that put a sticker "on it".
//!
//! The object such a trigger puts its sticker on is the entered object, seeded
//! into the trigger's `ParentTarget` when the trigger is put on the stack
//! (CR 603.6), together with a pin on that object's incarnation. A source that
//! left and returned before the trigger resolves is a new object the original
//! trigger no longer finds, so that trigger places nothing (CR 400.7 + CR
//! 603.6). The controller chooses a sticker that is not on any object they own
//! (CR 123.3); a "you may" instruction is chosen on resolution (CR 603.5); the
//! sticker the instruction placed is "that sticker" for the rest of the
//! resolution (CR 608.2c).
//!
//! Every card is built from its verbatim Oracle text and driven through the
//! `apply()` pipeline; the letter/vowel mana of `_____ Goblin` is a later phase.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::stickers::set_player_sticker_sheets;
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::stickers::{AppliedSticker, StickerKind};
use engine::types::zones::Zone;

const GOBLIN: &str = "When this creature enters, you may put a name sticker on it. Add {R} for each unique vowel on that sticker. (The vowels are A, E, I, O, U, and Y.)";

const ROCKETSHIP: &str = "Flying\nWhen this Vehicle enters, you may put up to two name stickers on it.\nWhenever this Vehicle attacks, choose a letter. This Vehicle gets +1/+1 until end of turn for each name sticker on it that begins with the chosen letter.\nCrew 2";

const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.";

const STICKER_SHEET: &str = "Ancestral Hot Dog Minotaur";

fn mana(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|color| ManaUnit::new(*color, ObjectId(0), false, vec![]))
        .collect()
}

fn select_sheet(runner: &mut GameRunner, sheet: &str) {
    set_player_sticker_sheets(runner.state_mut(), P0, &[sheet.to_string()]);
}

/// A spell cast in response to the source's own enters trigger.
struct Response {
    spell: ObjectId,
    targets: Vec<ObjectId>,
}

#[derive(Debug)]
struct Drive {
    stickers: usize,
    kinds: Vec<StickerKind>,
    stickers_on_source: Vec<AppliedSticker>,
    /// The resolution's "that sticker" record once the stack is empty.
    record: Option<AppliedSticker>,
    branch_prompts: usize,
    count_prompts: usize,
    optional_prompts: usize,
    stack_empty: bool,
}

/// Drive the committed cast of `source` until its enters trigger(s) have
/// resolved and the stack is empty, answering every prompt through `apply()`.
/// A `response` is cast (by declared intent) while the source's enters trigger
/// waits on top of the stack.
fn drive(
    runner: &mut GameRunner,
    source: ObjectId,
    accept: bool,
    mut response: Option<Response>,
) -> Drive {
    let mut trigger_seen = false;
    let mut branch_prompts = 0;
    let mut count_prompts = 0;
    let mut optional_prompts = 0;
    for _ in 0..80 {
        let state = runner.state();
        let is_source_trigger =
            |kind: &StackEntryKind| matches!(kind, StackEntryKind::TriggeredAbility { .. });
        let source_trigger_on_top = state
            .stack
            .last()
            .is_some_and(|entry| entry.source_id == source && is_source_trigger(&entry.kind));
        trigger_seen |= state
            .stack
            .iter()
            .any(|entry| entry.source_id == source && is_source_trigger(&entry.kind));
        if trigger_seen
            && state.stack.is_empty()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match state.waiting_for.clone() {
            WaitingFor::Priority { .. }
                if source_trigger_on_top
                    && state.objects[&source].zone == Zone::Battlefield
                    && response.is_some() =>
            {
                let Response { spell, targets } = response.take().unwrap();
                runner.cast(spell).target_objects(&targets).commit();
                continue;
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_prompts += 1;
                GameAction::DecideOptionalEffect { accept }
            }
            WaitingFor::ChooseOneOfBranch {
                branch_descriptions,
                ..
            } => {
                branch_prompts += 1;
                if branch_descriptions
                    .iter()
                    .any(|description| description.contains("Do not put a sticker"))
                {
                    count_prompts += 1;
                }
                let index = branch_descriptions
                    .iter()
                    .position(|description| description.contains("Hot Dog"))
                    .or_else(|| {
                        branch_descriptions
                            .iter()
                            .position(|description| description.contains("Put 1 sticker"))
                    })
                    .unwrap_or(0);
                GameAction::ChooseBranch { index }
            }
            other => panic!("unexpected prompt while resolving the enters trigger: {other:?}"),
        };
        runner.act(action).expect("prompt answer must be legal");
    }
    assert!(
        trigger_seen,
        "the source's enters trigger must go on the stack"
    );
    let state = runner.state();
    let object = &state.objects[&source];
    Drive {
        stickers: object.stickers.len(),
        kinds: object
            .stickers
            .iter()
            .map(|sticker| sticker.kind())
            .collect(),
        stickers_on_source: object.stickers.clone(),
        record: state.placed_sticker_this_resolution.clone(),
        branch_prompts,
        count_prompts,
        optional_prompts,
        stack_empty: state.stack.is_empty(),
    }
}

/// Stage `copies` copies of `_____ Goblin` in P0's hand with exactly {2}{R}
/// per copy in the pool.
fn goblin_scenario(scenario: &mut GameScenario, copies: usize) -> Vec<ObjectId> {
    scenario.at_phase(Phase::PreCombatMain);
    let goblins = (0..copies)
        .map(|_| {
            scenario
                .add_creature_to_hand(P0, "_____ Goblin", 2, 2)
                .with_subtypes(vec!["Goblin", "Guest"])
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Red],
                    generic: 2,
                })
                .from_oracle_text(GOBLIN)
                .id()
        })
        .collect();
    let pool: Vec<ManaType> = (0..copies)
        .flat_map(|_| [ManaType::Red, ManaType::Colorless, ManaType::Colorless])
        .collect();
    scenario.with_mana_pool(P0, mana(&pool));
    goblins
}

fn goblin_case(accept: bool, sheet: Option<&str>) -> (GameRunner, Drive) {
    let mut scenario = GameScenario::new();
    let goblin = goblin_scenario(&mut scenario, 1)[0];
    let mut runner = scenario.build();
    if let Some(sheet) = sheet {
        select_sheet(&mut runner, sheet);
    }
    runner.cast(goblin).commit();
    let outcome = drive(&mut runner, goblin, accept, None);
    (runner, outcome)
}

/// CR 603.6 + CR 123.3: the Goblin's enters trigger puts the chosen name
/// sticker on the Goblin; its trailing (later-phase) mana clause lets the
/// stack empty. CR 608.2c: the sticker it placed is still "that sticker" once
/// the trigger has finished resolving (the sticker choice is answered inside
/// the same resolution).
#[test]
fn blank_goblin_places_a_name_sticker_and_records_that_sticker() {
    let (_runner, accepted) = goblin_case(true, Some(STICKER_SHEET));
    assert_eq!(
        accepted.stickers, 1,
        "the Goblin must carry the name sticker it put on itself: {accepted:?}"
    );
    assert_eq!(accepted.kinds, vec![StickerKind::Name]);
    assert!(
        accepted.branch_prompts >= 1,
        "the sticker/position choice must be offered: {accepted:?}"
    );
    assert!(accepted.stack_empty, "the trigger must finish resolving");
    assert_eq!(
        accepted.record.as_ref(),
        accepted.stickers_on_source.first(),
        "that sticker is the placed sticker: {accepted:?}"
    );
    let record = accepted.record.as_ref().expect("a sticker was placed");
    assert_eq!(record.kind(), StickerKind::Name);
    assert_eq!(record.name_text(), Some("Hot Dog"));
}

/// CR 603.5: declining the "you may" places nothing; CR 123.3: with no sticker
/// sheet there is no sticker to choose, so nothing is placed either.
#[test]
fn blank_goblin_decline_or_no_candidate_leaves_no_record() {
    // Reach-guard: the same staging with an accept places the sticker.
    let (_runner, accepted) = goblin_case(true, Some(STICKER_SHEET));
    assert_eq!(accepted.stickers, 1, "{accepted:?}");
    assert!(accepted.record.is_some(), "{accepted:?}");

    let (_runner, declined) = goblin_case(false, Some(STICKER_SHEET));
    assert_eq!(
        declined.optional_prompts, 1,
        "the decline was offered: {declined:?}"
    );
    assert_eq!(declined.stickers, 0, "{declined:?}");
    assert_eq!(declined.branch_prompts, 0, "{declined:?}");
    assert!(declined.stack_empty);
    assert_eq!(
        declined.record, None,
        "CR 608.2c: no sticker, no that sticker"
    );

    let (_runner, no_sheet) = goblin_case(true, None);
    assert_eq!(no_sheet.stickers, 0, "{no_sheet:?}");
    assert_eq!(no_sheet.branch_prompts, 0, "{no_sheet:?}");
    assert!(no_sheet.stack_empty);
    assert_eq!(
        no_sheet.record, None,
        "CR 608.2c: no sticker, no that sticker"
    );
}

/// Two Goblins cast in sequence: A accepts and is stickered, then B declines
/// (CR 603.5) and is not. CR 608.2c: B's resolution does not inherit A's
/// "that sticker".
#[test]
fn second_goblin_declining_does_not_inherit_the_first_goblins_sticker() {
    let mut scenario = GameScenario::new();
    let goblins = goblin_scenario(&mut scenario, 2);
    let (goblin_a, goblin_b) = (goblins[0], goblins[1]);
    let mut runner = scenario.build();
    select_sheet(&mut runner, STICKER_SHEET);

    runner.cast(goblin_a).commit();
    let a = drive(&mut runner, goblin_a, true, None);
    assert_eq!(a.stickers, 1, "{a:?}");
    assert!(a.stack_empty);
    assert_eq!(a.record.as_ref(), a.stickers_on_source.first(), "{a:?}");

    runner.cast(goblin_b).commit();
    let b = drive(&mut runner, goblin_b, false, None);
    assert_eq!(b.optional_prompts, 1, "{b:?}");
    assert_eq!(b.stickers, 0, "{b:?}");
    assert!(b.stack_empty);
    assert_eq!(b.record, None, "{b:?}");
    assert_eq!(runner.state().objects[&goblin_a].stickers.len(), 1);
}

/// CR 400.7 + CR 603.6: a Goblin blinked while its enters trigger waits is a
/// new object. The returned Goblin's own trigger places one sticker; the
/// original trigger no longer finds the Goblin and places nothing.
#[test]
fn blank_goblin_blinked_in_response_gets_one_sticker_from_its_new_trigger_only() {
    let mut scenario = GameScenario::new();
    let goblin = goblin_scenario(&mut scenario, 1)[0];
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    select_sheet(&mut runner, STICKER_SHEET);
    runner.cast(goblin).commit();
    let outcome = drive(
        &mut runner,
        goblin,
        true,
        Some(Response {
            spell: cloudshift,
            targets: vec![goblin],
        }),
    );
    assert_eq!(
        runner.state().objects[&cloudshift].zone,
        Zone::Graveyard,
        "the blink must have resolved"
    );
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Battlefield);
    // Reach-guard: the returned Goblin's own trigger placed its sticker.
    // CR 400.7: the original trigger places no second sticker.
    assert_eq!(outcome.stickers, 1, "{outcome:?}");
    assert_eq!(outcome.kinds, vec![StickerKind::Name]);
    assert_eq!(outcome.branch_prompts, 1, "{outcome:?}");
    assert!(outcome.stack_empty);
    // CR 608.2c: the stale original trigger resolves last and places nothing,
    // so it leaves no "that sticker".
    assert_eq!(outcome.record, None, "{outcome:?}");
}

/// CR 400.7 + CR 603.6 on the "up to" path: the source's currency is checked
/// before the count prompt, so the original trigger of a blinked Rocketship
/// offers no count choice and places nothing; the returned Rocketship's
/// trigger places one.
#[test]
fn rocketship_blinked_in_response_gets_stickers_from_its_new_trigger_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rocketship = scenario
        .add_artifact_to_hand_from_oracle(P0, "_____ _____ Rocketship", ROCKETSHIP)
        .with_subtypes(vec!["Vehicle"])
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(P0, mana(&[ManaType::Colorless; 4]));
    let mut runner = scenario.build();
    select_sheet(&mut runner, STICKER_SHEET);
    runner.cast(rocketship).commit();
    for unit in mana(&[ManaType::Blue, ManaType::Colorless, ManaType::Colorless]) {
        runner.state_mut().players[0].mana_pool.add(unit);
    }
    let outcome = drive(
        &mut runner,
        rocketship,
        true,
        Some(Response {
            spell: flicker,
            targets: vec![rocketship, land],
        }),
    );
    assert_eq!(
        runner.state().objects[&flicker].zone,
        Zone::Graveyard,
        "the blink must have resolved"
    );
    assert_eq!(runner.state().objects[&rocketship].zone, Zone::Battlefield);
    // Reach-guard: the returned Rocketship's trigger placed its sticker.
    // CR 400.7: the original trigger offers no count prompt and places nothing.
    assert_eq!(outcome.stickers, 1, "{outcome:?}");
    assert_eq!(outcome.count_prompts, 1, "{outcome:?}");
    assert!(outcome.stack_empty);
}
