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
//! `apply()` pipeline. The letter statistics the census faces read off "that
//! sticker" count the different vowels on it, where Y is a vowel and a
//! lowercase letter and its uppercase equivalent are the same letter (CR
//! 123.6e; letter occurrences, CR 123.6d).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::stickers::set_player_sticker_sheets;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::stickers::{AppliedSticker, StickerKind, StickerLocator};
use engine::types::zones::Zone;

const GOBLIN: &str = "When this creature enters, you may put a name sticker on it. Add {R} for each unique vowel on that sticker. (The vowels are A, E, I, O, U, and Y.)";

const O_SAURUS: &str = "Trample\nWhen this creature enters, you may put a name sticker on it. Put a +1/+1 counter on it for each unique vowel on that sticker. (The vowels are A, E, I, O, U, and Y.)";

const BIRD: &str = "Flying\nWhen this creature enters, you may put a name sticker on it. You gain X life, where X is the number of unique vowels on that sticker. (The vowels are A, E, I, O, U, and Y.)";

const WIZARDS: &str = "When this creature enters, you may put a name sticker on it, then look at the top X cards of your library, where X is the number of unique vowels on that sticker. Put one of those cards into your hand and the rest on the bottom of your library in any order. (The vowels are A, E, I, O, U, and Y.)";

const WOLF: &str = "When this creature enters, you may put a name sticker on it. When you do, up to X target creatures each get -1/-1 until end of turn, where X is the number of unique vowels on that sticker. (The vowels are A, E, I, O, U, and Y.)";

const ROCKETSHIP: &str = "Flying\nWhen this Vehicle enters, you may put up to two name stickers on it.\nWhenever this Vehicle attacks, choose a letter. This Vehicle gets +1/+1 until end of turn for each name sticker on it that begins with the chosen letter.\nCrew 2";

const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";

const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.";

const STICKER_SHEET: &str = "Ancestral Hot Dog Minotaur";
/// Name stickers "Unique", "Charmed", "Pants".
const UNIQUE_SHEET: &str = "Unique Charmed Pants";
/// Name stickers "Sassy", "Gremlin", "Blood".
const SASSY_SHEET: &str = "Sassy Gremlin Blood";
/// Name stickers "Deep-Fried", "Plague", "Myr".
const MYR_SHEET: &str = "Deep-Fried Plague Myr";

fn mana(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|color| ManaUnit::new(*color, ObjectId(0), false, vec![]))
        .collect()
}

fn select_sheet(runner: &mut GameRunner, sheet: &str) {
    set_player_sticker_sheets(runner.state_mut(), P0, &[sheet.to_string()]);
}

/// Something the driver does while the source's enters trigger waits on top
/// of the stack.
enum Interject {
    /// Cast `spell` (by declared intent) in response.
    Cast {
        spell: ObjectId,
        targets: Vec<ObjectId>,
    },
    /// Hand ownership of the source to another player; control is unchanged.
    SetOwner(PlayerId),
    /// Put `sticker` directly on the source, stamped with a fresh timestamp.
    PushSticker(AppliedSticker),
}

/// How the driver answers the prompts of the resolving trigger(s).
struct Answers<'a> {
    /// Accept every "you may" (CR 603.5).
    accept: bool,
    /// The name sticker to choose when it is offered.
    pick: &'a str,
    /// Objects to target, in order, for a reflexive trigger's target slots.
    targets: &'a [ObjectId],
}

impl<'a> Answers<'a> {
    fn new(accept: bool, pick: &'a str) -> Self {
        Answers {
            accept,
            pick,
            targets: &[],
        }
    }
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
    /// Red mana in P0's pool once the stack is empty.
    red: usize,
    /// +1/+1 counters on the source once the stack is empty.
    counters_p1p1: u32,
    /// P0's life total once the stack is empty.
    life: i32,
    /// `(cards.len(), keep_count, rest_destination)` of each `DigChoice`
    /// answered.
    digs: Vec<(usize, usize, Option<Zone>)>,
    /// `target_slots.len()` of each `TriggerTargetSelection` answered.
    trigger_target_slots: Vec<usize>,
}

/// Drive the committed cast of `source` until its enters trigger(s) have
/// resolved and the stack is empty, answering every prompt through `apply()`.
/// An `interject` fires while the source's enters trigger waits on top of the
/// stack.
fn drive(
    runner: &mut GameRunner,
    source: ObjectId,
    answers: Answers<'_>,
    mut interject: Option<Interject>,
) -> Drive {
    let mut trigger_seen = false;
    let mut branch_prompts = 0;
    let mut count_prompts = 0;
    let mut optional_prompts = 0;
    let mut digs = Vec::new();
    let mut trigger_target_slots = Vec::new();
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
                    && interject.is_some() =>
            {
                match interject.take().unwrap() {
                    Interject::Cast { spell, targets } => {
                        runner.cast(spell).target_objects(&targets).commit();
                    }
                    Interject::SetOwner(owner) => {
                        runner
                            .state_mut()
                            .objects
                            .get_mut(&source)
                            .expect("source exists")
                            .owner = owner;
                    }
                    Interject::PushSticker(mut sticker) => {
                        let fresh = runner.state_mut().next_timestamp();
                        match &mut sticker {
                            AppliedSticker::Name { timestamp, .. }
                            | AppliedSticker::Ability { timestamp, .. }
                            | AppliedSticker::PowerToughness { timestamp, .. }
                            | AppliedSticker::Art { timestamp, .. } => *timestamp = fresh,
                        }
                        runner
                            .state_mut()
                            .objects
                            .get_mut(&source)
                            .expect("source exists")
                            .stickers
                            .push(sticker);
                    }
                }
                continue;
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::OptionalEffectChoice { .. } => {
                optional_prompts += 1;
                GameAction::DecideOptionalEffect {
                    accept: answers.accept,
                }
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
                    .position(|description| description.contains(answers.pick))
                    .or_else(|| {
                        branch_descriptions
                            .iter()
                            .position(|description| description.contains("Put 1 sticker"))
                    })
                    .unwrap_or(0);
                GameAction::ChooseBranch { index }
            }
            WaitingFor::DigChoice {
                cards,
                keep_count,
                selectable_cards,
                rest_destination,
                ..
            } => {
                digs.push((cards.len(), keep_count, rest_destination));
                GameAction::SelectCards {
                    cards: selectable_cards.into_iter().take(keep_count).collect(),
                }
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                if selection.current_slot == 0 {
                    trigger_target_slots.push(target_slots.len());
                }
                let chosen = |object: &ObjectId| {
                    selection
                        .selected_slots
                        .contains(&Some(TargetRef::Object(*object)))
                };
                let target = answers
                    .targets
                    .iter()
                    .filter(|object| !chosen(object))
                    .map(|object| TargetRef::Object(*object))
                    .find(|target| selection.current_legal_targets.contains(target));
                GameAction::ChooseTarget { target }
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
        red: state.players[0].mana_pool.count_color(ManaType::Red),
        counters_p1p1: object
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        life: state.players[0].life,
        digs,
        trigger_target_slots,
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

fn goblin_case(accept: bool, sheet: Option<&str>, pick: &str) -> (GameRunner, Drive) {
    let mut scenario = GameScenario::new();
    let goblin = goblin_scenario(&mut scenario, 1)[0];
    let mut runner = scenario.build();
    if let Some(sheet) = sheet {
        select_sheet(&mut runner, sheet);
    }
    runner.cast(goblin).commit();
    let outcome = drive(&mut runner, goblin, Answers::new(accept, pick), None);
    (runner, outcome)
}

/// A census face other than the Goblin: its name, P/T, subtypes, colored
/// pips (paid from a pool of exactly its cost) and verbatim Oracle text.
struct Face {
    name: &'static str,
    power_toughness: (i32, i32),
    subtypes: &'static [&'static str],
    pips: &'static [(ManaCostShard, ManaType)],
    generic: u32,
    oracle: &'static str,
}

const O_SAURUS_FACE: Face = Face {
    name: "_____-o-saurus",
    power_toughness: (3, 3),
    subtypes: &["Alien", "Dinosaur"],
    pips: &[
        (ManaCostShard::Green, ManaType::Green),
        (ManaCostShard::Green, ManaType::Green),
    ],
    generic: 4,
    oracle: O_SAURUS,
};

const BIRD_FACE: Face = Face {
    name: "_____ Bird Gets the Worm",
    power_toughness: (2, 4),
    subtypes: &["Bird", "Guest"],
    pips: &[
        (ManaCostShard::White, ManaType::White),
        (ManaCostShard::White, ManaType::White),
    ],
    generic: 3,
    oracle: BIRD,
};

const WIZARDS_FACE: Face = Face {
    name: "Wizards of the _____",
    power_toughness: (3, 1),
    subtypes: &["Human", "Wizard", "Performer"],
    pips: &[
        (ManaCostShard::Blue, ManaType::Blue),
        (ManaCostShard::Blue, ManaType::Blue),
    ],
    generic: 2,
    oracle: WIZARDS,
};

const WOLF_FACE: Face = Face {
    name: "Wolf in _____ Clothing",
    power_toughness: (2, 3),
    subtypes: &["Wolf", "Guest"],
    pips: &[(ManaCostShard::Black, ManaType::Black)],
    generic: 3,
    oracle: WOLF,
};

/// Stage `face` in P0's hand with exactly its cost in the pool.
fn stage_face(scenario: &mut GameScenario, face: &Face) -> ObjectId {
    scenario.at_phase(Phase::PreCombatMain);
    let (power, toughness) = face.power_toughness;
    let id = scenario
        .add_creature_to_hand(P0, face.name, power, toughness)
        .with_subtypes(face.subtypes.to_vec())
        .with_mana_cost(ManaCost::Cost {
            shards: face.pips.iter().map(|(shard, _)| *shard).collect(),
            generic: face.generic,
        })
        .from_oracle_text(face.oracle)
        .id();
    let pool: Vec<ManaType> = face
        .pips
        .iter()
        .map(|(_, color)| *color)
        .chain(std::iter::repeat_n(
            ManaType::Colorless,
            face.generic as usize,
        ))
        .collect();
    scenario.with_mana_pool(P0, mana(&pool));
    id
}

/// Cast `face` (staged by `stage`), select `sheet` for P0 if any, and drive.
fn face_case(
    face: &Face,
    sheet: Option<&str>,
    answers: Answers<'_>,
    stage: impl FnOnce(&mut GameScenario),
) -> (GameRunner, ObjectId, Drive) {
    let mut scenario = GameScenario::new();
    let id = stage_face(&mut scenario, face);
    stage(&mut scenario);
    let mut runner = scenario.build();
    if let Some(sheet) = sheet {
        select_sheet(&mut runner, sheet);
    }
    runner.cast(id).commit();
    let outcome = drive(&mut runner, id, answers, None);
    (runner, id, outcome)
}

/// CR 123.6: exactly one name sticker on the entering creature, placed by its
/// own enters trigger, which finished resolving (charter C2.10's placement).
fn assert_one_name_sticker(outcome: &Drive) {
    assert_eq!(outcome.stickers, 1, "{outcome:?}");
    assert_eq!(outcome.kinds, vec![StickerKind::Name], "{outcome:?}");
    assert!(outcome.stack_empty, "{outcome:?}");
}

/// CR 603.6 + CR 123.3: the Goblin's enters trigger puts the chosen name
/// sticker on the Goblin; its trailing mana clause ("Add {R} for each unique
/// vowel on that sticker") then resolves and the stack empties. CR 608.2c:
/// the sticker it placed is still "that sticker" once
/// the trigger has finished resolving (the sticker choice is answered inside
/// the same resolution).
#[test]
fn blank_goblin_places_a_name_sticker_and_records_that_sticker() {
    let (_runner, accepted) = goblin_case(true, Some(STICKER_SHEET), "Hot Dog");
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
    let (_runner, accepted) = goblin_case(true, Some(STICKER_SHEET), "Hot Dog");
    assert_eq!(accepted.stickers, 1, "{accepted:?}");
    assert!(accepted.record.is_some(), "{accepted:?}");

    let (_runner, declined) = goblin_case(false, Some(STICKER_SHEET), "Hot Dog");
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

    let (_runner, no_sheet) = goblin_case(true, None, "Hot Dog");
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
    let a = drive(&mut runner, goblin_a, Answers::new(true, "Hot Dog"), None);
    assert_eq!(a.stickers, 1, "{a:?}");
    assert!(a.stack_empty);
    assert_eq!(a.record.as_ref(), a.stickers_on_source.first(), "{a:?}");

    runner.cast(goblin_b).commit();
    let b = drive(&mut runner, goblin_b, Answers::new(false, "Hot Dog"), None);
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
        Answers::new(true, "Hot Dog"),
        Some(Interject::Cast {
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
        Answers::new(true, "Hot Dog"),
        Some(Interject::Cast {
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

/// A name sticker from `sheet` with the given index and text, for pushing
/// directly onto an object (the driver stamps its timestamp).
fn name_sticker(sheet: &str, index: u8, text: &str) -> AppliedSticker {
    AppliedSticker::Name {
        locator: StickerLocator {
            sheet: sheet.to_string(),
            index,
        },
        text: text.to_string(),
        position: 0,
        timestamp: 0,
    }
}

/// CR 123.6e + CR 106.3: the Goblin adds {R} for each different vowel on the
/// sticker it just put, Y included: "Unique" has U, I, E → 3; "Sassy" has A, Y
/// → 2.
#[test]
fn blank_goblin_adds_red_for_each_unique_vowel_on_that_sticker() {
    let (_runner, unique) = goblin_case(true, Some(UNIQUE_SHEET), "Unique");
    assert_one_name_sticker(&unique);
    let record = unique.record.as_ref().expect("a sticker was placed");
    assert_eq!(record.name_text(), Some("Unique"), "{unique:?}");
    assert_eq!(unique.red, 3, "CR 123.6e: U, I, E: {unique:?}");

    let (_runner, sassy) = goblin_case(true, Some(SASSY_SHEET), "Sassy");
    assert_one_name_sticker(&sassy);
    assert_eq!(
        sassy.record.as_ref().and_then(AppliedSticker::name_text),
        Some("Sassy"),
        "{sassy:?}"
    );
    assert_eq!(sassy.red, 2, "CR 123.6e: A and Y are vowels: {sassy:?}");
}

/// CR 603.5 + CR 608.2c: declining the "you may" puts no sticker, so there is
/// no "that sticker" and nothing is added.
#[test]
fn blank_goblin_declining_adds_no_mana() {
    // Reach-guard: the same staging with an accept adds 3.
    let (_runner, accepted) = goblin_case(true, Some(UNIQUE_SHEET), "Unique");
    assert_one_name_sticker(&accepted);
    assert_eq!(accepted.red, 3, "{accepted:?}");

    let (_runner, declined) = goblin_case(false, Some(UNIQUE_SHEET), "Unique");
    assert_eq!(declined.optional_prompts, 1, "{declined:?}");
    assert_eq!(declined.stickers, 0, "{declined:?}");
    assert!(declined.stack_empty, "{declined:?}");
    assert_eq!(declined.red, 0, "CR 608.2c: no sticker put: {declined:?}");
}

/// CR 608.2c: "that sticker" is the sticker this resolution put. Goblin B's
/// declined resolution reads neither Goblin A's sticker from the previous
/// resolution nor a name sticker already on another permanent.
#[test]
fn second_goblin_declining_adds_no_mana_despite_prior_stickers() {
    let mut scenario = GameScenario::new();
    let goblins = goblin_scenario(&mut scenario, 2);
    let (goblin_a, goblin_b) = (goblins[0], goblins[1]);
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    let timestamp = runner.state_mut().next_timestamp();
    let mut hot_dog = name_sticker(STICKER_SHEET, 1, "Hot Dog");
    if let AppliedSticker::Name { timestamp: t, .. } = &mut hot_dog {
        *t = timestamp;
    }
    runner
        .state_mut()
        .objects
        .get_mut(&bear)
        .expect("bear exists")
        .stickers
        .push(hot_dog);
    select_sheet(&mut runner, UNIQUE_SHEET);

    runner.cast(goblin_a).commit();
    let a = drive(&mut runner, goblin_a, Answers::new(true, "Unique"), None);
    assert_one_name_sticker(&a);
    // A's +3, plus the {R} staged for B's cast (generic costs were paid with
    // the colorless mana, so that {R} is still in the pool).
    assert_eq!(a.red, 4, "{a:?}");

    runner.cast(goblin_b).commit();
    let b = drive(&mut runner, goblin_b, Answers::new(false, "Unique"), None);
    assert_eq!(b.optional_prompts, 1, "{b:?}");
    assert_eq!(b.stickers, 0, "{b:?}");
    assert!(b.stack_empty, "{b:?}");
    // B's {R} pip spent one red; B adds nothing (a leaked "Unique" would add 3).
    assert_eq!(b.red, 3, "CR 608.2c: B put no sticker: {b:?}");
    assert_eq!(runner.state().objects[&bear].stickers.len(), 1);
}

/// CR 123.3 + CR 608.2c: with no sticker sheet there is no sticker to put, so
/// there is no "that sticker" and nothing is added.
#[test]
fn blank_goblin_with_no_sticker_to_choose_adds_no_mana() {
    // Reach-guard: with a sheet the same accept adds 3.
    let (_runner, with_sheet) = goblin_case(true, Some(UNIQUE_SHEET), "Unique");
    assert_one_name_sticker(&with_sheet);
    assert_eq!(with_sheet.red, 3, "{with_sheet:?}");

    let (_runner, no_sheet) = goblin_case(true, None, "Unique");
    assert_eq!(no_sheet.stickers, 0, "{no_sheet:?}");
    assert!(no_sheet.stack_empty, "{no_sheet:?}");
    assert_eq!(no_sheet.red, 0, "CR 608.2c: no sticker put: {no_sheet:?}");
}

/// CR 400.7 + CR 608.2c: a Goblin blinked while its enters trigger waits is a
/// new object. The returned Goblin's own trigger puts "Unique" and adds 3; the
/// original trigger puts nothing, so it has no "that sticker" and adds 0.
#[test]
fn blinked_blank_goblin_stale_trigger_adds_no_mana() {
    let mut scenario = GameScenario::new();
    let goblin = goblin_scenario(&mut scenario, 1)[0];
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    select_sheet(&mut runner, UNIQUE_SHEET);
    runner.cast(goblin).commit();
    let outcome = drive(
        &mut runner,
        goblin,
        Answers::new(true, "Unique"),
        Some(Interject::Cast {
            spell: cloudshift,
            targets: vec![goblin],
        }),
    );
    // Reach-guard: the blink resolved and the returned Goblin is stickered.
    assert_eq!(runner.state().objects[&cloudshift].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&goblin].zone, Zone::Battlefield);
    assert_one_name_sticker(&outcome);
    assert_eq!(outcome.record, None, "{outcome:?}");
    assert_eq!(
        outcome.red, 3,
        "CR 400.7: only the returned Goblin's trigger adds mana: {outcome:?}"
    );
}

/// CR 123.3b + CR 608.2c: a player can't put a sticker on an object they
/// don't own, so a Goblin its controller doesn't own gets no sticker and adds
/// nothing; owned by its controller, the same Goblin adds 3.
#[test]
fn blank_goblin_its_controller_does_not_own_adds_no_mana() {
    // Reach-guard: owner == controller.
    let (_runner, owned) = goblin_case(true, Some(UNIQUE_SHEET), "Unique");
    assert_one_name_sticker(&owned);
    assert_eq!(owned.red, 3, "{owned:?}");

    let mut scenario = GameScenario::new();
    let goblin = goblin_scenario(&mut scenario, 1)[0];
    let mut runner = scenario.build();
    select_sheet(&mut runner, UNIQUE_SHEET);
    runner.cast(goblin).commit();
    let outcome = drive(
        &mut runner,
        goblin,
        Answers::new(true, "Unique"),
        Some(Interject::SetOwner(P1)),
    );
    assert_eq!(runner.state().objects[&goblin].owner, P1);
    assert_eq!(runner.state().objects[&goblin].controller, P0);
    assert_eq!(outcome.stickers, 0, "CR 123.3b: {outcome:?}");
    assert_eq!(outcome.branch_prompts, 0, "{outcome:?}");
    assert!(outcome.stack_empty, "{outcome:?}");
    assert_eq!(outcome.red, 0, "CR 123.3b: {outcome:?}");
}

/// CR 608.2c + CR 123.5: "that sticker" is only the sticker this instruction
/// put, not every name sticker on the Goblin: with "Sassy" already on it, the
/// Goblin that puts "Unique" adds 3 (U, I, E), not 5 (A, Y, U, I, E).
#[test]
fn blank_goblin_counts_only_the_sticker_it_just_put() {
    let mut scenario = GameScenario::new();
    let goblin = goblin_scenario(&mut scenario, 1)[0];
    let mut runner = scenario.build();
    select_sheet(&mut runner, UNIQUE_SHEET);
    runner.cast(goblin).commit();
    let outcome = drive(
        &mut runner,
        goblin,
        Answers::new(true, "Unique"),
        Some(Interject::PushSticker(name_sticker(
            SASSY_SHEET,
            0,
            "Sassy",
        ))),
    );
    // Reach-guard: the trigger put "Unique" beside the pushed "Sassy".
    assert_eq!(
        outcome.record.as_ref().and_then(AppliedSticker::name_text),
        Some("Unique"),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.kinds,
        vec![StickerKind::Name, StickerKind::Name],
        "{outcome:?}"
    );
    assert!(outcome.stack_empty, "{outcome:?}");
    assert_eq!(outcome.red, 3, "CR 608.2c: {outcome:?}");
}

/// CR 123.6e: _____-o-saurus puts a +1/+1 counter on itself for each unique
/// vowel on the sticker it just put; declining (CR 603.5) puts none.
#[test]
fn o_saurus_gets_a_counter_for_each_unique_vowel() {
    let (_runner, _, accepted) = face_case(
        &O_SAURUS_FACE,
        Some(UNIQUE_SHEET),
        Answers::new(true, "Unique"),
        |_| {},
    );
    assert_one_name_sticker(&accepted);
    assert_eq!(accepted.counters_p1p1, 3, "CR 123.6e: {accepted:?}");

    let (_runner, _, declined) = face_case(
        &O_SAURUS_FACE,
        Some(UNIQUE_SHEET),
        Answers::new(false, "Unique"),
        |_| {},
    );
    assert_eq!(declined.optional_prompts, 1, "{declined:?}");
    assert_eq!(declined.stickers, 0, "{declined:?}");
    assert_eq!(declined.counters_p1p1, 0, "CR 608.2c: {declined:?}");
}

/// CR 123.6e + CR 107.3i: _____ Bird Gets the Worm gains X life, where X is
/// the number of unique vowels on the sticker it just put; declining gains 0.
#[test]
fn bird_gets_the_worm_gains_life_for_each_unique_vowel() {
    let (runner, _, accepted) = face_case(
        &BIRD_FACE,
        Some(UNIQUE_SHEET),
        Answers::new(true, "Unique"),
        |_| {},
    );
    // P1 is untouched, so its life is the starting life total.
    let starting = runner.state().players[1].life;
    assert_one_name_sticker(&accepted);
    assert_eq!(accepted.life, starting + 3, "CR 123.6e: {accepted:?}");

    let (_runner, _, declined) = face_case(
        &BIRD_FACE,
        Some(UNIQUE_SHEET),
        Answers::new(false, "Unique"),
        |_| {},
    );
    assert_eq!(declined.optional_prompts, 1, "{declined:?}");
    assert_eq!(declined.stickers, 0, "{declined:?}");
    assert_eq!(declined.life, starting, "CR 608.2c: {declined:?}");
}

/// CR 123.6e + CR 107.3i: Wizards of the _____ looks at the top X cards,
/// where X is the number of unique vowels on the sticker it just put ("Unique"
/// → 3, "Myr" → 1), keeps one and puts the rest on the bottom.
#[test]
fn wizards_of_the_blank_looks_at_x_cards() {
    const LIBRARY: [&str; 5] = ["Card A", "Card B", "Card C", "Card D", "Card E"];
    let (runner, _, unique) = face_case(
        &WIZARDS_FACE,
        Some(UNIQUE_SHEET),
        Answers::new(true, "Unique"),
        |scenario| {
            scenario.with_library_top(P0, &LIBRARY);
        },
    );
    assert_one_name_sticker(&unique);
    assert_eq!(
        unique.digs,
        vec![(3, 1, Some(Zone::Library))],
        "CR 123.6e: {unique:?}"
    );
    let state = runner.state();
    let names = |ids: &im::Vector<ObjectId>| -> Vec<String> {
        ids.iter()
            .map(|id| state.objects[id].name.clone())
            .collect()
    };
    let hand = names(&state.players[0].hand);
    let library = names(&state.players[0].library);
    let kept: Vec<&str> = LIBRARY[..3]
        .iter()
        .copied()
        .filter(|name| hand.iter().any(|card| card == name))
        .collect();
    assert_eq!(kept.len(), 1, "one looked-at card kept: {hand:?}");
    let mut bottom: Vec<String> = library[library.len() - 2..].to_vec();
    bottom.sort();
    let mut rest: Vec<String> = LIBRARY[..3]
        .iter()
        .filter(|name| **name != kept[0])
        .map(|name| name.to_string())
        .collect();
    rest.sort();
    assert_eq!(bottom, rest, "the other two go to the bottom: {library:?}");
    assert_eq!(&library[..2], &["Card D", "Card E"]);

    let (_runner, _, myr) = face_case(
        &WIZARDS_FACE,
        Some(MYR_SHEET),
        Answers::new(true, "Myr"),
        |scenario| {
            scenario.with_library_top(P0, &LIBRARY);
        },
    );
    assert_one_name_sticker(&myr);
    assert_eq!(myr.digs, vec![(1, 1, Some(Zone::Library))], "{myr:?}");
}

/// CR 603.12 + CR 123.6e: when Wolf in _____ Clothing puts a sticker, its
/// reflexive trigger targets up to X creatures, X the number of unique vowels
/// on that sticker ("Unique" → 3). With no sticker put, it doesn't trigger.
#[test]
fn wolf_in_blank_clothing_targets_up_to_x_creatures() {
    let wolf_case = |sheet: Option<&str>| {
        let mut scenario = GameScenario::new();
        let wolf = stage_face(&mut scenario, &WOLF_FACE);
        let squires: Vec<ObjectId> = (0..4)
            .map(|_| scenario.add_creature(P1, "Squire", 1, 1).id())
            .collect();
        let mut runner = scenario.build();
        if let Some(sheet) = sheet {
            select_sheet(&mut runner, sheet);
        }
        runner.cast(wolf).commit();
        let answers = Answers {
            accept: true,
            pick: "Unique",
            targets: &squires,
        };
        let outcome = drive(&mut runner, wolf, answers, None);
        let dead = squires
            .iter()
            .filter(|squire| runner.state().objects[*squire].zone == Zone::Graveyard)
            .count();
        (outcome, dead)
    };

    let (unique, dead) = wolf_case(Some(UNIQUE_SHEET));
    assert_one_name_sticker(&unique);
    assert_eq!(
        unique.trigger_target_slots,
        vec![3],
        "CR 123.6e: up to 3 targets: {unique:?}"
    );
    assert_eq!(dead, 3, "{unique:?}");

    let (no_sheet, dead) = wolf_case(None);
    assert_eq!(no_sheet.stickers, 0, "{no_sheet:?}");
    assert!(no_sheet.trigger_target_slots.is_empty(), "{no_sheet:?}");
    assert!(no_sheet.stack_empty, "{no_sheet:?}");
    assert_eq!(dead, 0, "CR 603.12: {no_sheet:?}");
}
