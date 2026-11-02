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

use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::stickers::set_player_sticker_sheets;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, LetterQuery, NameStickerSet, QuantityExpr, QuantityRef,
    StickerTicketCostPayment, SubAbilityLink, TargetFilter, TargetRef, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AutoMayChoice, MayTriggerAutoChoiceScope, PersistedGameState, PersistedRestoreFinalization,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::proposed_event::AppliedReplacementKey;
use engine::types::stickers::{AppliedSticker, StickerKind, StickerLocator};
use engine::types::triggers::TriggerMode;
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

const SCAMPIRE: &str = "When Scampire enters, you get {TK}, then you may put a sticker on a creature card in your graveyard.\n{3}{B}: Return target creature card with a sticker on it from your graveyard to the battlefield. That creature gains haste. Exile it at the beginning of the next end step. Activate only as a sorcery.";
const FINISHING_MOVE: &str = "You get {TK}{TK}, then you may put a sticker on a nonland permanent you own. Target creature you control deals damage equal to its power to target creature you don't control.";
const PRIZE_WALL: &str = "Defender\n{U}, {T}: You get {TK}.\n{4}{U}, {T}: You may put a sticker on a nonland permanent you own. Activate only as a sorcery.";

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
                player,
                controller,
                branch_descriptions,
                ..
            } => {
                assert_eq!(player, P0, "same-controller source choices belong to P0");
                assert_eq!(controller, P0, "placing authority remains P0");
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
            WaitingFor::ChooseFromZoneChoice {
                cards,
                count,
                up_to,
                ..
            } => {
                assert_eq!(count, 1);
                assert!(!up_to);
                let chosen = answers
                    .targets
                    .iter()
                    .find(|id| cards.contains(id))
                    .expect("the intended recipient must be offered");
                GameAction::SelectCards {
                    cards: vec![*chosen],
                }
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
    assert_eq!(
        outcome.branch_prompts, 2,
        "distinct locator and position decisions: {outcome:?}"
    );
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

fn assert_placement_idle(runner: &GameRunner) {
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(
        state.resolution_stack.is_empty(),
        "all placement continuations must drain"
    );
    assert!(state.pending_trigger.is_none());
    assert!(state.pending_trigger_entry.is_none());
    assert!(state.deferred_triggers.is_empty());
}

fn restore_placement(runner: &GameRunner, trusted: bool) -> GameRunner {
    let saved = if trusted {
        PersistedGameState::capture(runner.state().clone())
    } else {
        PersistedGameState::Raw(Box::new(runner.state().clone()))
    };
    let decoded: PersistedGameState =
        serde_json::from_value(serde_json::to_value(saved).unwrap()).unwrap();
    let state = decoded
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("a real placement pause is resumable")
        .finalize_immediately()
        .expect("checked restore finalizes");
    GameRunner::from_state(state)
}

#[test]
fn scampire_ticket_survives_empty_no_sheet_decline_and_graveyard_choice() {
    for (population, sheet, accept, restore) in [
        (0, true, true, None),
        (1, false, true, None),
        (1, true, false, None),
        (1, true, true, None),
        (2, true, true, None),
        (2, true, true, Some(false)),
        (2, true, true, Some(true)),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand(P0, "Scampire", 1, 3)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 2,
            })
            .from_oracle_text(SCAMPIRE)
            .id();
        let candidates: Vec<_> = (0..population)
            .map(|_| {
                scenario
                    .add_creature(P0, "Runeclaw Bear", 2, 2)
                    .controlled_by(P1)
                    .id()
            })
            .collect();
        let land = scenario.add_land_to_graveyard(P0, "Forest").id();
        let other = scenario
            .add_creature_to_graveyard(P1, "Centaur Courser", 3, 3)
            .id();
        let orphan = scenario
            .add_creature_to_graveyard(P0, "Silvercoat Lion", 2, 2)
            .id();
        scenario.with_mana_pool(
            P0,
            mana(&[ManaType::Black, ManaType::Colorless, ManaType::Colorless]),
        );
        let mut runner = scenario.build();
        for id in &candidates {
            let mut events = Vec::new();
            move_to_zone(runner.state_mut(), *id, Zone::Graveyard, &mut events);
            assert!(events.iter().any(
                |event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if object_id == id)
            ));
            assert!(runner.state().players[0].graveyard.contains(id));
            assert_eq!(runner.state().objects[id].owner, P0);
            assert_eq!(
                runner.state().lki_cache[id].controller,
                P1,
                "real battlefield departure retains the opponent controller's LKI"
            );
        }
        runner.state_mut().players[0]
            .graveyard
            .retain(|id| *id != orphan);
        if sheet {
            select_sheet(&mut runner, UNIQUE_SHEET);
        }
        runner.cast(source).commit();
        let chosen = candidates.last().copied();
        let mut entered = false;
        let mut trigger_seen = false;
        let mut optional = 0;
        let mut recipient_prompts = 0;
        let mut locator = 0;
        let mut position = 0;
        let mut placements = Vec::new();
        for _ in 0..30 {
            let state = runner.state();
            if !entered && state.objects[&source].zone == Zone::Battlefield {
                entered = true;
                assert!(
                    state
                        .stack
                        .iter()
                        .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
                    "the actual entry must admit its mandatory Ticket trigger"
                );
                assert_eq!(
                    state.players[0].player_counter(&PlayerCounterKind::Ticket),
                    0
                );
            }
            trigger_seen |= state
                .stack
                .iter()
                .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }));
            if trigger_seen
                && state.stack.is_empty()
                && matches!(state.waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match state.waiting_for.clone() {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::OptionalEffectChoice { player, .. } => {
                    assert_eq!(player, P0);
                    assert_eq!(
                        state.players[0].player_counter(&PlayerCounterKind::Ticket),
                        1
                    );
                    optional += 1;
                    GameAction::DecideOptionalEffect { accept }
                }
                WaitingFor::ChooseFromZoneChoice {
                    player,
                    cards,
                    count,
                    up_to,
                    ..
                } => {
                    assert_eq!(player, P0);
                    assert_eq!(count, 1);
                    assert!(!up_to);
                    assert_eq!(cards.len(), candidates.len());
                    assert!(candidates.iter().all(|id| cards.contains(id)));
                    assert!(!cards.contains(&land));
                    assert!(!cards.contains(&other));
                    assert!(!cards.contains(&orphan));
                    recipient_prompts += 1;
                    GameAction::SelectCards {
                        cards: vec![chosen.unwrap()],
                    }
                }
                WaitingFor::ChooseOneOfBranch {
                    player,
                    controller,
                    branches,
                    branch_descriptions,
                    ..
                } => {
                    assert_eq!(
                        (player, controller),
                        (P0, P0),
                        "graveyard cards use owner, never stale controller"
                    );
                    let positions = branches.iter().all(|b| {
                        matches!(
                            b.effect.as_ref(),
                            engine::types::ability::Effect::ApplySticker { .. }
                        )
                    });
                    let index = if positions {
                        position += 1;
                        branch_descriptions
                            .iter()
                            .position(|d| d == "Name — Charmed at position 2")
                            .expect("chosen name's nondefault position")
                    } else {
                        locator += 1;
                        branch_descriptions
                            .iter()
                            .position(|d| d == "Name — Charmed")
                            .expect("non-first discrete name locator")
                    };
                    GameAction::ChooseBranch { index }
                }
                other => panic!("no announcement targeting for Scampire: {other:?}"),
            };
            if let Some(trusted) = restore {
                if !matches!(action, GameAction::PassPriority) {
                    runner = restore_placement(&runner, trusted);
                }
            }
            placements.extend(
                runner
                    .act(action)
                    .unwrap()
                    .events
                    .into_iter()
                    .filter(|e| matches!(e, GameEvent::StickerPlaced { .. })),
            );
        }
        assert!(entered && trigger_seen);
        assert_placement_idle(&runner);
        assert_eq!(
            runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
            1
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        if population > 0 && sheet && accept {
            assert_eq!((locator, position), (1, 1));
            assert_eq!(recipient_prompts, usize::from(population > 1));
            assert_eq!(
                placements,
                vec![GameEvent::StickerPlaced {
                    player_id: P0,
                    object_id: chosen.unwrap(),
                    kind: StickerKind::Name
                }]
            );
            assert!(
                matches!(&runner.state().objects[&chosen.unwrap()].stickers[0], AppliedSticker::Name { locator, position: 1, .. } if locator.index == 1 && locator.sheet == UNIQUE_SHEET)
            );
            assert_eq!(
                runner.state().objects[&chosen.unwrap()].zone,
                Zone::Graveyard
            );
        } else {
            assert!(placements.is_empty());
            assert_eq!((locator, position, recipient_prompts), (0, 0, 0));
            assert_eq!(optional, usize::from(population > 0 && sheet));
            assert!(runner.state().placed_sticker_this_resolution.is_none());
        }
    }
}

#[test]
fn finishing_move_keeps_two_targets_and_splits_name_actors_through_restore() {
    for restore_stage in 0..=4 {
        for trusted in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let spell = scenario
                .add_spell_to_hand_from_oracle(P0, "Finishing Move", false, FINISHING_MOVE)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 2,
                })
                .id();
            let source = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
            let damaged = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
            let recipient = scenario
                .add_creature(P0, "Silvercoat Lion", 2, 2)
                .controlled_by(P1)
                .with_keyword(Keyword::Shroud)
                .id();
            let alternate = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
            let land = scenario.add_basic_land(P0, ManaColor::Green);
            scenario.with_mana_pool(
                P0,
                mana(&[ManaType::Green, ManaType::Colorless, ManaType::Colorless]),
            );
            let mut runner = scenario.build();
            select_sheet(&mut runner, UNIQUE_SHEET);
            runner
                .act(GameAction::CastSpell {
                    object_id: spell,
                    card_id: runner.state().objects[&spell].card_id,
                    targets: vec![],
                    payment_mode: Default::default(),
                })
                .unwrap();
            for expected in [source, damaged] {
                let WaitingFor::TargetSelection {
                    ref target_slots,
                    ref selection,
                    ..
                } = runner.state().waiting_for
                else {
                    panic!("both printed targets must be announced")
                };
                assert_eq!(
                    target_slots.len(),
                    2,
                    "untargeted sticker recipient must not claim a third slot"
                );
                assert!(selection
                    .current_legal_targets
                    .contains(&TargetRef::Object(expected)));
                assert!(
                    !selection
                        .current_legal_targets
                        .contains(&TargetRef::Object(recipient)),
                    "shroud still excludes actual targets"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(expected)),
                    })
                    .unwrap();
            }
            assert!(runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == spell));
            for _ in 0..4 {
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player: P0, .. }
            ));
            assert_eq!(
                runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
                2
            );
            if restore_stage == 1 {
                runner = restore_placement(&runner, trusted);
            }
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            let WaitingFor::ChooseFromZoneChoice {
                ref cards,
                count: 1,
                up_to: false,
                player: P0,
                ..
            } = runner.state().waiting_for
            else {
                panic!("resolution must choose its own recipient")
            };
            assert!(
                cards.contains(&recipient) && cards.contains(&alternate) && cards.contains(&source)
            );
            assert!(!cards.contains(&damaged) && !cards.contains(&land));
            let legal = engine::ai_support::legal_actions(runner.state());
            for id in cards {
                let action = GameAction::SelectCards { cards: vec![*id] };
                assert!(legal.contains(&action));
                let mut cloned = runner.state().clone();
                engine::game::engine::apply(&mut cloned, P0, action).unwrap();
                assert!(matches!(
                    cloned.waiting_for,
                    WaitingFor::ChooseOneOfBranch { player: P0, .. }
                ));
            }
            for (actor, cards) in [
                (P1, vec![recipient]),
                (P0, vec![]),
                (P0, vec![recipient, recipient]),
                (P0, vec![recipient, alternate]),
                (P0, vec![damaged]),
            ] {
                let before = runner.state().waiting_for.clone();
                let frames = runner.state().resolution_stack.clone();
                assert!(engine::game::engine::apply(
                    runner.state_mut(),
                    actor,
                    GameAction::SelectCards { cards }
                )
                .is_err());
                assert_eq!(runner.state().waiting_for, before);
                assert_eq!(runner.state().resolution_stack, frames);
                assert_eq!(
                    runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
                    2
                );
            }
            if restore_stage == 2 {
                runner = restore_placement(&runner, trusted);
            }
            runner
                .act(GameAction::SelectCards {
                    cards: vec![recipient],
                })
                .unwrap();
            let WaitingFor::ChooseOneOfBranch {
                player,
                controller,
                ref branches,
                ref branch_descriptions,
                ref parent_occurrences,
                ..
            } = runner.state().waiting_for
            else {
                panic!("P0 chooses the discrete name")
            };
            assert_eq!((player, controller), (P0, P0));
            assert!(branches.len() >= 2);
            assert_eq!(
                parent_occurrences.occurrences()[0].0,
                TargetRef::Object(recipient)
            );
            assert!(parent_occurrences.occurrences()[0].1.is_some());
            let locator_index = branch_descriptions
                .iter()
                .position(|d| d == "Name — Charmed")
                .expect("second discrete name");
            assert!(locator_index > 0);
            assert!(engine::game::engine::apply(
                runner.state_mut(),
                P1,
                GameAction::ChooseBranch {
                    index: locator_index
                }
            )
            .is_err());
            if restore_stage == 3 {
                runner = restore_placement(&runner, trusted);
            }
            runner
                .act(GameAction::ChooseBranch {
                    index: locator_index,
                })
                .unwrap();
            let WaitingFor::ChooseOneOfBranch {
                player,
                controller,
                ref branches,
                ref branch_descriptions,
                ref parent_occurrences,
                ..
            } = runner.state().waiting_for
            else {
                panic!("P1 chooses only Charmed's position")
            };
            assert_eq!((player, controller), (P1, P0));
            assert_eq!(
                parent_occurrences.occurrences()[0].0,
                TargetRef::Object(recipient)
            );
            assert!(branches.iter().all(|branch| matches!(branch.effect.as_ref(), engine::types::ability::Effect::ApplySticker { target: engine::types::ability::TargetFilter::SpecificObject { id }, sticker: AppliedSticker::Name { locator, .. }, .. } if *id == recipient && locator.index == 1 && locator.sheet == UNIQUE_SHEET)));
            let position_index = branch_descriptions
                .iter()
                .position(|d| d == "Name — Charmed at position 2")
                .unwrap();
            assert!(position_index > 0);
            assert!(engine::ai_support::legal_actions(runner.state()).contains(
                &GameAction::ChooseBranch {
                    index: position_index
                }
            ));
            assert!(engine::game::engine::apply(
                runner.state_mut(),
                P0,
                GameAction::ChooseBranch {
                    index: position_index
                }
            )
            .is_err());
            if restore_stage == 4 {
                runner = restore_placement(&runner, trusted);
            }
            let result = engine::game::engine::apply(
                runner.state_mut(),
                P1,
                GameAction::ChooseBranch {
                    index: position_index,
                },
            )
            .unwrap();
            assert_eq!(result.events.iter().filter(|e| matches!(e, GameEvent::StickerPlaced { player_id: P0, object_id, kind: StickerKind::Name } if *object_id == recipient)).count(), 1);
            assert_eq!(
                result
                    .events
                    .iter()
                    .filter(|e| matches!(e, GameEvent::DamageDealt { .. }))
                    .count(),
                1
            );
            assert_placement_idle(&runner);
            let state = runner.state();
            assert_eq!(state.objects[&spell].zone, Zone::Graveyard);
            assert_eq!(state.objects[&source].damage_marked, 0);
            assert_eq!(state.objects[&damaged].damage_marked, 2);
            assert_eq!(
                state.players[0].player_counter(&PlayerCounterKind::Ticket),
                2
            );
            assert_eq!(
                state.players[1].player_counter(&PlayerCounterKind::Ticket),
                0
            );
            assert_eq!(state.objects[&recipient].stickers.len(), 1);
            assert!(
                matches!(&state.objects[&recipient].stickers[0], AppliedSticker::Name { locator, position: 1, .. } if locator.index == 1 && locator.sheet == UNIQUE_SHEET)
            );
            assert!(state.objects[&alternate].stickers.is_empty());
            assert_eq!(state.players[0].mana_pool.total(), 0);
        }
    }
}

#[test]
fn finishing_move_decline_and_no_sheet_preserve_ticket_and_independent_damage() {
    for sheet in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Finishing Move", false, FINISHING_MOVE)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 2,
            })
            .id();
        let source = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
        let damaged = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
        scenario.with_mana_pool(
            P0,
            mana(&[ManaType::Green, ManaType::Colorless, ManaType::Colorless]),
        );
        let mut runner = scenario.build();
        if sheet {
            select_sheet(&mut runner, UNIQUE_SHEET);
        }
        runner
            .cast(spell)
            .target_objects(&[source, damaged])
            .commit();
        assert!(runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell));
        let mut optional = 0;
        for _ in 0..10 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::OptionalEffectChoice { .. } => {
                    optional += 1;
                    assert_eq!(
                        runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
                        2
                    );
                    GameAction::DecideOptionalEffect { accept: false }
                }
                ref other => panic!("decline/no-sheet cannot choose a sticker: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        assert_placement_idle(&runner);
        assert_eq!(optional, usize::from(sheet));
        assert_eq!(runner.state().objects[&damaged].damage_marked, 2);
        assert_eq!(runner.state().objects[&source].damage_marked, 0);
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        assert_eq!(
            runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
            2
        );
        assert!(runner.state().objects[&source].stickers.is_empty());
        assert!(runner.state().placed_sticker_this_resolution.is_none());
    }
}

#[test]
fn sticker_activations_keep_normal_costs_and_live_restrictions() {
    let tusk = "Whenever you put an ability sticker on a creature, put a +1/+1 counter on that creature.\n{2}{G}{W}, {T}: You get {TK}. You may put a sticker on a nonland permanent you own.";
    let bleater = "Whenever another creature you own enters, you get {TK}.\n{W}, {T}: You may put a sticker on a creature you own that entered this turn.";
    for (name, text, index, pool, ticket) in [
        (
            "Prize Wall",
            PRIZE_WALL,
            1,
            vec![
                ManaType::Blue,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
            0,
        ),
        (
            "Tusk and Whiskers",
            tusk,
            0,
            vec![
                ManaType::Green,
                ManaType::White,
                ManaType::Colorless,
                ManaType::Colorless,
            ],
            1,
        ),
        ("Park Bleater", bleater, 0, vec![ManaType::White], 0),
    ] {
        for (sheet, accept) in [(true, true), (true, false), (false, true)] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let (power, toughness) = match name {
                "Prize Wall" => (0, 4),
                "Tusk and Whiskers" => (4, 4),
                _ => (2, 2),
            };
            let source = scenario
                .add_creature_from_oracle(P0, name, power, toughness, text)
                .id();
            let first = scenario
                .add_creature(P0, "Runeclaw Bear", 2, 2)
                .with_summoning_sickness()
                .id();
            let recipient = scenario
                .add_creature(P0, "Silvercoat Lion", 2, 2)
                .with_summoning_sickness()
                .hexproof()
                .id();
            let old = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
            let other = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
            let land = scenario.add_basic_land(P0, ManaColor::Green);
            scenario.with_mana_pool(P0, mana(&pool));
            let mut runner = scenario.build();
            if sheet {
                select_sheet(&mut runner, UNIQUE_SHEET);
            }
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: index,
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                })
                .unwrap();
            assert!(runner.state().objects[&source].tapped);
            assert_eq!(runner.state().players[0].mana_pool.total(), 0);
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "no recipient announcement"
            );
            assert!(runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == source
                    && matches!(entry.kind, StackEntryKind::ActivatedAbility { .. })));
            let mut recipients = 0;
            let mut names = 0;
            let mut positions = 0;
            let mut optional = 0;
            for _ in 0..18 {
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                let action = match runner.state().waiting_for.clone() {
                    WaitingFor::Priority { .. } => GameAction::PassPriority,
                    WaitingFor::OptionalEffectChoice { .. } => {
                        optional += 1;
                        assert_eq!(
                            runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
                            ticket
                        );
                        GameAction::DecideOptionalEffect { accept }
                    }
                    WaitingFor::ChooseFromZoneChoice { cards, .. } => {
                        recipients += 1;
                        assert!(cards.contains(&first) && cards.contains(&recipient));
                        assert!(!cards.contains(&other) && !cards.contains(&land));
                        assert_eq!(cards.contains(&old), name != "Park Bleater");
                        assert_eq!(cards.contains(&source), name != "Park Bleater");
                        GameAction::SelectCards {
                            cards: vec![recipient],
                        }
                    }
                    WaitingFor::ChooseOneOfBranch {
                        player,
                        controller,
                        branches,
                        branch_descriptions,
                        ..
                    } => {
                        assert_eq!((player, controller), (P0, P0));
                        let position = branches.iter().all(|branch| {
                            matches!(
                                branch.effect.as_ref(),
                                engine::types::ability::Effect::ApplySticker { .. }
                            )
                        });
                        let expected = if position {
                            positions += 1;
                            "Name — Charmed at position 2"
                        } else {
                            names += 1;
                            "Name — Charmed"
                        };
                        GameAction::ChooseBranch {
                            index: branch_descriptions
                                .iter()
                                .position(|d| d == expected)
                                .unwrap(),
                        }
                    }
                    other => panic!("ordinary sticker activation: {other:?}"),
                };
                runner.act(action).unwrap();
            }
            assert_placement_idle(&runner);
            assert_eq!(
                runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
                ticket
            );
            assert_eq!(
                (recipients, names, positions),
                if sheet && accept {
                    (1, 1, 1)
                } else {
                    (0, 0, 0)
                }
            );
            assert_eq!(optional, usize::from(sheet));
            assert_eq!(
                runner.state().objects[&recipient].stickers.len(),
                usize::from(sheet && accept)
            );
        }
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wall = scenario
        .add_creature_from_oracle(P0, "Prize Wall", 0, 4, PRIZE_WALL)
        .id();
    scenario.with_mana_pool(P0, mana(&[ManaType::Blue]));
    let mut runner = scenario.build();
    runner.activate(wall, 0).resolve();
    assert_placement_idle(&runner);
    assert!(runner.state().objects[&wall].tapped);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_eq!(
        runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
        1
    );
    assert!(runner.state().objects[&wall].stickers.is_empty());
}

#[test]
fn rocketship_count_keeps_one_recipient_and_distinct_name_stickers() {
    for count in 0..=2 {
        for trusted in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario
                .add_artifact_to_hand_from_oracle(P0, "_____ _____ Rocketship", ROCKETSHIP)
                .with_subtypes(vec!["Vehicle"])
                .with_mana_cost(ManaCost::generic(4))
                .id();
            scenario.with_mana_pool(P0, mana(&[ManaType::Colorless; 4]));
            let mut runner = scenario.build();
            select_sheet(&mut runner, UNIQUE_SHEET);
            runner.cast(source).commit();
            for _ in 0..6 {
                if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                    break;
                }
                runner.act(GameAction::PassPriority).unwrap();
            }
            assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ));
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ChooseOneOfBranch { player: P0, .. }
            ));
            runner = restore_placement(&runner, trusted);
            runner
                .act(GameAction::ChooseBranch { index: count })
                .unwrap();
            for expected in ["Unique", "Charmed"].into_iter().take(count) {
                let WaitingFor::ChooseOneOfBranch {
                    player,
                    controller,
                    ref branch_descriptions,
                    ..
                } = runner.state().waiting_for
                else {
                    panic!("each sticker has a discrete locator choice")
                };
                assert_eq!((player, controller), (P0, P0));
                let index = branch_descriptions
                    .iter()
                    .position(|d| d == &format!("Name — {expected}"))
                    .unwrap();
                runner.act(GameAction::ChooseBranch { index }).unwrap();
                let WaitingFor::ChooseOneOfBranch {
                    player,
                    controller,
                    ref branch_descriptions,
                    ..
                } = runner.state().waiting_for
                else {
                    panic!("each name has its own position choice")
                };
                assert_eq!((player, controller), (P0, P0));
                let index = branch_descriptions
                    .iter()
                    .position(|d| d == &format!("Name — {expected} at position 2"))
                    .unwrap();
                runner = restore_placement(&runner, trusted);
                runner.act(GameAction::ChooseBranch { index }).unwrap();
            }
            assert_placement_idle(&runner);
            let stickers = &runner.state().objects[&source].stickers;
            assert_eq!(stickers.len(), count);
            assert_eq!(
                stickers
                    .iter()
                    .map(|s| s.locator().clone())
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                count
            );
            assert!(stickers
                .iter()
                .all(|s| matches!(s, AppliedSticker::Name { position: 1, .. })));
            assert_eq!(
                runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
                0
            );
            assert_eq!(
                runner.state().placed_sticker_this_resolution.is_some(),
                count > 0
            );
        }
    }
}

#[test]
fn sequential_placement_decline_clears_sticker_and_tracked_result() {
    for accept_second in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = scenario.add_creature(P0, "First recipient", 2, 2).id();
        let second = scenario.add_creature(P0, "Second recipient", 2, 2).id();
        let placement = |id| {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::PutSticker {
                    target: TargetFilter::SpecificObject { id },
                    kind: Some(StickerKind::Name),
                    count: QuantityExpr::Fixed { value: 1 },
                    max_ticket_cost: None,
                    ticket_cost_payment: StickerTicketCostPayment::PayNormally,
                },
            )
        };
        let life = |amount| {
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount,
                    player: TargetFilter::Controller,
                },
            )
        };
        let mut tail = life(QuantityExpr::Ref {
            qty: QuantityRef::NameStickerLetterCount {
                stickers: NameStickerSet::ThatSticker,
                letters: LetterQuery::UniqueVowels,
            },
        })
        .sub_ability(
            life(QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize,
            })
            .sub_ability(life(QuantityExpr::Fixed { value: 1 })),
        );
        tail.sub_link = SubAbilityLink::SequentialSibling;
        let second_def = placement(second).optional().sub_ability(tail);
        let spell = scenario
            .add_spell_to_hand(P0, "Composed placement fixture", false)
            .with_ability_definition(placement(first).sub_ability(second_def))
            .id();
        let mut runner = scenario.build();
        select_sheet(&mut runner, UNIQUE_SHEET);
        runner.cast(spell).commit();
        let mut optional = 0;
        let mut placements = 0;
        for _ in 0..20 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::OptionalEffectChoice { .. } => {
                    optional += 1;
                    assert_eq!(
                        runner.state().objects[&first].stickers.len(),
                        1,
                        "first producer really placed its sticker"
                    );
                    assert!(
                        runner
                            .state()
                            .tracked_object_sets
                            .values()
                            .any(|members| members == &vec![first]),
                        "first producer really published a nonempty set"
                    );
                    assert!(
                        runner.state().placed_sticker_this_resolution.is_none(),
                        "new instruction starts empty before its decision"
                    );
                    GameAction::DecideOptionalEffect {
                        accept: accept_second,
                    }
                }
                WaitingFor::ChooseOneOfBranch {
                    branch_descriptions,
                    ..
                } => {
                    let name = if placements == 0 { "Unique" } else { "Charmed" };
                    let index = branch_descriptions
                        .iter()
                        .position(|d| {
                            d == &format!("Name — {name}")
                                || d == &format!("Name — {name} at position 2")
                        })
                        .unwrap();
                    GameAction::ChooseBranch { index }
                }
                other => panic!("composed ordinary placement: {other:?}"),
            };
            placements += runner
                .act(action)
                .unwrap()
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::StickerPlaced { .. }))
                .count();
        }
        assert_placement_idle(&runner);
        assert_eq!(optional, 1);
        assert_eq!(placements, if accept_second { 2 } else { 1 });
        assert_eq!(
            runner.state().players[0].life,
            if accept_second { 24 } else { 21 }
        );
        assert_eq!(
            runner.state().objects[&second].stickers.len(),
            usize::from(accept_second)
        );
        assert_eq!(
            runner.state().placed_sticker_this_resolution.is_some(),
            accept_second
        );
    }
}

#[test]
fn sticker_count_and_locator_pauses_preserve_live_trigger_amount() {
    for trusted in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let placement = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutSticker {
                target: TargetFilter::SelfRef,
                kind: Some(StickerKind::Name),
                count: QuantityExpr::UpTo {
                    max: Box::new(QuantityExpr::Fixed { value: 2 }),
                },
                max_ticket_cost: None,
                ticket_cost_payment: StickerTicketCostPayment::PayNormally,
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                player: TargetFilter::Controller,
            },
        ));
        let mut trigger = TriggerDefinition::new(TriggerMode::DamageReceived).execute(placement);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        let recipient = scenario
            .add_creature(P0, "Event amount fixture", 2, 9)
            .with_trigger_definition(trigger)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Lightning Bolt",
                true,
                "Lightning Bolt deals 3 damage to any target.",
            )
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            })
            .id();
        scenario.with_mana_pool(P0, mana(&[ManaType::Red]));
        let mut runner = scenario.build();
        select_sheet(&mut runner, UNIQUE_SHEET);
        runner.cast(spell).target_object(recipient).commit();
        let mut counts = 0;
        let mut locators = 0;
        let mut positions = 0;
        for _ in 0..20 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::ChooseOneOfBranch {
                    branch_descriptions,
                    trigger_context,
                    ..
                } => {
                    assert!(
                        trigger_context.is_some(),
                        "each pause carries the actual damage event"
                    );
                    let index = if let Some(index) = branch_descriptions
                        .iter()
                        .position(|d| d == "Put 1 sticker")
                    {
                        counts += 1;
                        index
                    } else if let Some(index) = branch_descriptions
                        .iter()
                        .position(|d| d == "Name — Charmed")
                    {
                        locators += 1;
                        index
                    } else {
                        positions += 1;
                        branch_descriptions
                            .iter()
                            .position(|d| d == "Name — Charmed at position 2")
                            .unwrap()
                    };
                    GameAction::ChooseBranch { index }
                }
                other => panic!("actual event-triggered fixture: {other:?}"),
            };
            if !matches!(action, GameAction::PassPriority) {
                runner = restore_placement(&runner, trusted);
            }
            runner.act(action).unwrap();
        }
        assert_placement_idle(&runner);
        assert_eq!((counts, locators, positions), (1, 1, 1));
        assert_eq!(runner.state().objects[&recipient].damage_marked, 3);
        assert_eq!(runner.state().objects[&recipient].stickers.len(), 1);
        assert_eq!(
            runner.state().players[0].life,
            23,
            "damage amount survives count, locator and position pauses"
        );
    }
}

#[test]
fn sole_name_locator_still_prompts_for_its_position() {
    for exhausted in [false, true] {
        let mut scenario = GameScenario::new();
        let source = goblin_scenario(&mut scenario, 1)[0];
        let holder = scenario.add_creature(P0, "Prior sticker holder", 2, 2).id();
        let mut runner = scenario.build();
        select_sheet(&mut runner, UNIQUE_SHEET);
        let mut used = vec![
            name_sticker(UNIQUE_SHEET, 0, "Unique"),
            name_sticker(UNIQUE_SHEET, 2, "Pants"),
        ];
        if exhausted {
            used.push(name_sticker(UNIQUE_SHEET, 1, "Charmed"));
        }
        runner
            .state_mut()
            .objects
            .get_mut(&holder)
            .unwrap()
            .stickers = used;
        runner.cast(source).commit();
        let outcome = drive(
            &mut runner,
            source,
            Answers::new(true, "Charmed at position 2"),
            None,
        );
        if exhausted {
            assert_eq!(
                (
                    outcome.branch_prompts,
                    outcome.optional_prompts,
                    outcome.stickers,
                    outcome.red
                ),
                (0, 0, 0, 0)
            );
            assert!(outcome.record.is_none());
        } else {
            assert_eq!(
                outcome.branch_prompts, 1,
                "the sole locator omits only its locator decision"
            );
            assert_one_name_sticker(&outcome);
            assert!(
                matches!(&outcome.stickers_on_source[0], AppliedSticker::Name { locator, position: 1, .. } if locator.index == 1)
            );
            assert_eq!(outcome.red, 2);
        }
        assert_placement_idle(&runner);
    }
}

#[test]
fn carnival_battlefield_population_and_paid_sticker_use_the_placer() {
    for paid in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_creature_to_hand(P0, "Carnival Carnivore", 4, 6)
            .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::Black], generic: 5 })
            .from_oracle_text("Deathtouch\nWhen Carnival Carnivore enters, you get {TK}, then you may put a sticker on a nonland permanent you own.").id();
        let recipient = scenario
            .add_creature(P0, "Silvercoat Lion", 2, 2)
            .controlled_by(P1)
            .hexproof()
            .id();
        let unowned = scenario
            .add_creature(P1, "Centaur Courser", 3, 3)
            .controlled_by(P0)
            .id();
        let phased = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
        let land = scenario.add_basic_land(P0, ManaColor::Green);
        scenario.with_mana_pool(
            P0,
            mana(&[
                ManaType::Black,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
            ]),
        );
        let mut runner = scenario.build();
        select_sheet(&mut runner, UNIQUE_SHEET);
        runner
            .state_mut()
            .objects
            .get_mut(&phased)
            .unwrap()
            .phase_status = engine::game::game_object::PhaseStatus::PhasedOut {
            cause: engine::game::game_object::PhaseOutCause::Directly,
        };
        runner.state_mut().players[0].add_player_counters(&PlayerCounterKind::Ticket, 1);
        runner.state_mut().players[1].add_player_counters(&PlayerCounterKind::Ticket, 5);
        runner.cast(source).commit();
        for _ in 0..6 {
            if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
            2
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        let WaitingFor::ChooseFromZoneChoice { ref cards, .. } = runner.state().waiting_for else {
            panic!("battlefield recipient choice")
        };
        assert!(cards.contains(&recipient) && cards.contains(&source));
        assert!(!cards.contains(&unowned) && !cards.contains(&phased) && !cards.contains(&land));
        runner
            .act(GameAction::SelectCards {
                cards: vec![recipient],
            })
            .unwrap();
        let WaitingFor::ChooseOneOfBranch {
            player: P0,
            controller: P0,
            ref branches,
            ref branch_descriptions,
            ..
        } = runner.state().waiting_for
        else {
            panic!("placer chooses sticker")
        };
        let index = if paid {
            branches
                .iter()
                .position(|b| {
                    matches!(
                        b.effect.as_ref(),
                        Effect::ApplySticker {
                            sticker: AppliedSticker::Ability { ticket_cost: 2, .. },
                            pay_ticket: true,
                            ..
                        }
                    )
                })
                .expect("affordable paid ability sticker")
        } else {
            branch_descriptions
                .iter()
                .position(|d| d == "Name — Charmed")
                .unwrap()
        };
        let result = runner.act(GameAction::ChooseBranch { index }).unwrap();
        let events = if paid {
            result.events
        } else {
            let WaitingFor::ChooseOneOfBranch {
                player: P1,
                controller: P0,
                ref branch_descriptions,
                ..
            } = runner.state().waiting_for
            else {
                panic!("recipient controller chooses name position")
            };
            let index = branch_descriptions
                .iter()
                .position(|d| d == "Name — Charmed at position 2")
                .unwrap();
            runner
                .act(GameAction::ChooseBranch { index })
                .unwrap()
                .events
        };
        assert_eq!(events.iter().filter(|e| matches!(e, GameEvent::StickerPlaced { player_id: P0, object_id, .. } if *object_id == recipient)).count(), 1);
        assert_placement_idle(&runner);
        assert_eq!(
            runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
            if paid { 0 } else { 2 }
        );
        assert_eq!(
            runner.state().players[1].player_counter(&PlayerCounterKind::Ticket),
            5
        );
        assert_eq!(runner.state().objects[&recipient].stickers.len(), 1);
    }
}

#[test]
fn pin_collection_uses_announced_x_and_places_without_ticket_payment() {
    let text = "When this Equipment enters, you may put an ability sticker with ticket cost X or less on it without paying that sticker's ticket cost.\nEquipped creature gets +1/+1 and has all abilities of ability stickers on this Equipment.\nEquip {2}";
    for x in [0, 2, 4] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_artifact_to_hand_from_oracle(P0, "Pin Collection", text)
            .with_subtypes(vec!["Equipment"])
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::White],
                generic: 0,
            })
            .id();
        let pool: Vec<_> = std::iter::once(ManaType::White)
            .chain(std::iter::repeat_n(ManaType::Colorless, x as usize))
            .collect();
        scenario.with_mana_pool(P0, mana(&pool));
        let mut runner = scenario.build();
        select_sheet(&mut runner, "Playable Delusionary Hydra");
        runner.cast(source).x(x).commit();
        let mut entered = false;
        let mut optional = 0;
        for _ in 0..14 {
            entered |= runner.state().objects[&source].zone == Zone::Battlefield;
            if entered
                && runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match runner.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::OptionalEffectChoice { .. } => {
                    optional += 1;
                    GameAction::DecideOptionalEffect { accept: true }
                }
                WaitingFor::ChooseOneOfBranch {
                    player,
                    controller,
                    branches,
                    ..
                } => {
                    assert_eq!((player, controller), (P0, P0));
                    assert!(branches.iter().all(|branch| matches!(branch.effect.as_ref(), Effect::ApplySticker { sticker: AppliedSticker::Ability { ticket_cost, .. }, pay_ticket: false, .. } if u32::from(*ticket_cost) <= x)));
                    let index = branches
                        .iter()
                        .position(|branch| {
                            matches!(
                                branch.effect.as_ref(),
                                Effect::ApplySticker {
                                    sticker: AppliedSticker::Ability { ticket_cost: 4, .. },
                                    ..
                                }
                            )
                        })
                        .expect("X4 admits the four-ticket ability");
                    GameAction::ChooseBranch { index }
                }
                other => panic!("actual X-cost equipment ETB: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        assert!(entered);
        assert_placement_idle(&runner);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(
            runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
            0
        );
        assert_eq!(optional, usize::from(x > 0));
        let stickers = &runner.state().objects[&source].stickers;
        if x == 0 {
            assert!(stickers.is_empty());
        } else {
            assert_eq!(stickers.len(), 1);
            assert!(
                matches!(stickers[0], AppliedSticker::Ability { ticket_cost, .. } if u32::from(ticket_cost) == x)
            );
        }
    }
}

#[test]
fn finishing_move_retargets_only_its_two_printed_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Finishing Move", false, FINISHING_MOVE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        })
        .id();
    let redirect = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Redirect",
            true,
            "You may choose new targets for target spell.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    let original_source = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let original_victim = scenario.add_creature(P1, "Centaur Courser", 3, 3).id();
    let new_source = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let new_victim = scenario.add_creature(P1, "Pillarfield Ox", 2, 4).id();
    scenario.with_mana_pool(
        P0,
        mana(&[
            ManaType::Green,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
            ManaType::Blue,
        ]),
    );
    let mut runner = scenario.build();
    select_sheet(&mut runner, UNIQUE_SHEET);
    runner
        .cast(spell)
        .target_objects(&[original_source, original_victim])
        .commit();
    let entry = runner.state().stack.last().unwrap().id;
    runner.cast(redirect).target_object(entry).commit();
    let mut retargets = 0;
    let mut optionals = 0;
    for _ in 0..20 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::RetargetChoice {
                current_targets,
                slots,
                slot_pools,
                ..
            } => {
                assert_eq!(
                    current_targets,
                    &vec![
                        TargetRef::Object(original_source),
                        TargetRef::Object(original_victim)
                    ]
                );
                assert_eq!(slots.len(), 2);
                assert_eq!(slot_pools.len(), 2);
                assert!(slot_pools[0].contains(&TargetRef::Object(new_source)));
                assert!(slot_pools[1].contains(&TargetRef::Object(new_victim)));
                retargets += 1;
                GameAction::RetargetSpell {
                    new_targets: vec![
                        Some(TargetRef::Object(new_source)),
                        Some(TargetRef::Object(new_victim)),
                    ],
                }
            }
            WaitingFor::OptionalEffectChoice { source_id, .. } => {
                optionals += 1;
                GameAction::DecideOptionalEffect {
                    accept: *source_id == redirect,
                }
            }
            other => panic!("retarget production entry: {other:?}"),
        };
        runner.act(action).unwrap();
    }
    assert_placement_idle(&runner);
    assert_eq!(retargets, 1);
    assert!(optionals >= 1);
    assert_eq!(runner.state().objects[&new_victim].damage_marked, 3);
    for id in [original_source, original_victim, new_source] {
        assert_eq!(runner.state().objects[&id].damage_marked, 0);
    }
    assert_eq!(
        runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
        2
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

#[test]
fn scampire_remembered_live_origin_keeps_its_ticket_when_population_becomes_empty() {
    for choice in [AutoMayChoice::Accept, AutoMayChoice::Decline] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature_to_hand(P0, "Scampire", 1, 3)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 2,
            })
            .from_oracle_text(SCAMPIRE)
            .id();
        let recipient = scenario
            .add_creature_to_graveyard(P0, "Runeclaw Bear", 2, 2)
            .id();
        scenario.with_mana_pool(
            P0,
            mana(&[ManaType::Black, ManaType::Colorless, ManaType::Colorless]),
        );
        let mut runner = scenario.build();
        select_sheet(&mut runner, UNIQUE_SHEET);
        runner.cast(source).commit();
        for _ in 0..6 {
            if runner.state().objects[&source].zone == Zone::Battlefield {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
        assert!(runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })));
        let admitted = runner.state().clone();
        for _ in 0..6 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ) {
                break;
            }
            runner.act(GameAction::PassPriority).unwrap();
        }
        let WaitingFor::OptionalEffectChoice {
            may_trigger_key: Some(ref key),
            ..
        } = runner.state().waiting_for
        else {
            panic!("actual live keyed Scampire origin");
        };
        assert_eq!(key.source_id, source);
        assert_eq!(
            runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
            1
        );
        runner
            .act(GameAction::DecideOptionalEffectAndRemember {
                choice,
                scope: MayTriggerAutoChoiceScope::ExactInstance,
            })
            .unwrap();
        let preferences = runner.state().may_trigger_auto_choices.clone();
        assert_eq!(preferences.len(), 1);
        for _ in 0..10 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match &runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::ChooseOneOfBranch {
                    branch_descriptions,
                    ..
                } => GameAction::ChooseBranch {
                    index: branch_descriptions
                        .iter()
                        .position(|d| d == "Name — Charmed" || d == "Name — Charmed at position 2")
                        .unwrap(),
                },
                other => panic!("remembered live placement: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        assert_placement_idle(&runner);
        assert_eq!(
            runner.state().objects[&recipient].stickers.len(),
            usize::from(choice == AutoMayChoice::Accept)
        );
        // Reuse the actual admitted origin, carrying only the preference established
        // by its real prompt. Its recipient leaves before that saved trigger resolves.
        let mut empty = GameRunner::from_state(admitted);
        empty.state_mut().may_trigger_auto_choices = preferences;
        let mut events = Vec::new();
        move_to_zone(empty.state_mut(), recipient, Zone::Exile, &mut events);
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::ZoneChanged { object_id, .. } if *object_id == recipient)
        ));
        for _ in 0..8 {
            if empty.state().stack.is_empty()
                && matches!(empty.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            assert!(
                matches!(empty.state().waiting_for, WaitingFor::Priority { .. }),
                "empty remembered branch must finish without a choice"
            );
            empty.act(GameAction::PassPriority).unwrap();
        }
        assert_placement_idle(&empty);
        assert_eq!(
            empty.state().players[0].player_counter(&PlayerCounterKind::Ticket),
            1
        );
        assert!(empty.state().objects[&recipient].stickers.is_empty());
        assert!(empty.state().placed_sticker_this_resolution.is_none());
    }
}

#[test]
fn typed_paid_sticker_siblings_require_tickets_and_preserve_free_placement() {
    for kind in [StickerKind::Ability, StickerKind::PowerToughness] {
        for (tickets, payment) in [
            (0, StickerTicketCostPayment::PayNormally),
            (2, StickerTicketCostPayment::PayNormally),
            (0, StickerTicketCostPayment::WithoutPaying),
        ] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let recipient = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
            let spell = scenario
                .add_spell_to_hand(P0, "Typed sticker fixture", false)
                .with_ability_definition(
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::PutSticker {
                            target: TargetFilter::SpecificObject { id: recipient },
                            kind: Some(kind),
                            count: QuantityExpr::Fixed { value: 1 },
                            max_ticket_cost: Some(QuantityExpr::Fixed { value: 2 }),
                            ticket_cost_payment: payment,
                        },
                    )
                    .optional(),
                )
                .id();
            let mut runner = scenario.build();
            select_sheet(&mut runner, "Playable Delusionary Hydra");
            runner.state_mut().players[0].add_player_counters(&PlayerCounterKind::Ticket, tickets);
            runner.cast(spell).commit();
            let mut placements = 0;
            let mut optional = 0;
            for _ in 0..10 {
                if runner.state().stack.is_empty()
                    && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                {
                    break;
                }
                let action = match runner.state().waiting_for {
                    WaitingFor::Priority { .. } => GameAction::PassPriority,
                    WaitingFor::OptionalEffectChoice { .. } => {
                        optional += 1;
                        GameAction::DecideOptionalEffect { accept: true }
                    }
                    WaitingFor::ChooseOneOfBranch { .. } => GameAction::ChooseBranch { index: 0 },
                    ref other => panic!("typed payment control: {other:?}"),
                };
                placements += runner
                    .act(action)
                    .unwrap()
                    .events
                    .iter()
                    .filter(|e| matches!(e, GameEvent::StickerPlaced { .. }))
                    .count();
            }
            let placed = tickets == 2 || payment == StickerTicketCostPayment::WithoutPaying;
            assert_placement_idle(&runner);
            assert_eq!(optional, usize::from(placed));
            assert_eq!(placements, usize::from(placed));
            assert_eq!(
                runner.state().objects[&recipient].stickers.len(),
                usize::from(placed)
            );
            assert_eq!(
                runner.state().players[0].player_counter(&PlayerCounterKind::Ticket),
                0
            );
        }
    }
}

#[test]
fn fixed_count_placement_preserves_replacement_stamp_across_each_nested_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let recipient = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand(P0, "Fixed-count placement fixture", false)
        .with_ability_definition(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutSticker {
                target: TargetFilter::SpecificObject { id: recipient },
                kind: Some(StickerKind::Name),
                count: QuantityExpr::Fixed { value: 2 },
                max_ticket_cost: None,
                ticket_cost_payment: StickerTicketCostPayment::PayNormally,
            },
        ))
        .id();
    let mut runner = scenario.build();
    select_sheet(&mut runner, UNIQUE_SHEET);
    runner.cast(spell).commit();
    let stamp = AppliedReplacementKey::Floating { index: 17 };
    runner
        .state_mut()
        .stack
        .iter_mut()
        .last()
        .unwrap()
        .ability_mut()
        .unwrap()
        .replacement_applied
        .insert(stamp);
    let mut placements = 0;
    let mut pauses = 0;
    for _ in 0..12 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::ChooseOneOfBranch {
                replacement_applied,
                branch_descriptions,
                parent_occurrences,
                ..
            } => {
                assert!(
                    replacement_applied.contains(&stamp),
                    "fixed-count expansion must carry the incoming replacement provenance"
                );
                let occurrences = parent_occurrences.occurrences();
                assert_eq!(
                    occurrences
                        .iter()
                        .filter(|(target, _)| matches!(target, TargetRef::Object(_)))
                        .cloned()
                        .collect::<Vec<_>>(),
                    vec![(
                        TargetRef::Object(recipient),
                        Some(ObjectIncarnationRef::from_object(
                            &runner.state().objects[&recipient]
                        ))
                    )]
                );
                assert!(occurrences.iter().all(|(target, _)| match target {
                    TargetRef::Object(id) => *id == recipient,
                    TargetRef::Player(player) => *player == P0,
                }));
                pauses += 1;
                let name = if placements == 0 { "Unique" } else { "Charmed" };
                let index = branch_descriptions
                    .iter()
                    .position(|d| {
                        d == &format!("Name — {name}")
                            || d == &format!("Name — {name} at position 2")
                    })
                    .unwrap();
                GameAction::ChooseBranch { index }
            }
            other => panic!("fixed-count placement: {other:?}"),
        };
        placements += runner
            .act(action)
            .unwrap()
            .events
            .iter()
            .filter(|e| matches!(e, GameEvent::StickerPlaced { .. }))
            .count();
    }
    assert_placement_idle(&runner);
    assert_eq!((pauses, placements), (4, 2));
    let stickers = &runner.state().objects[&recipient].stickers;
    assert_eq!(stickers.len(), 2);
    assert!(
        matches!(&stickers[0], AppliedSticker::Name { locator, position: 1, .. } if locator.index == 0)
    );
    assert!(
        matches!(&stickers[1], AppliedSticker::Name { locator, position: 1, .. } if locator.index == 1)
    );
}

#[test]
fn angelic_harold_auto_accept_keeps_untargeted_optional_root_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature_to_hand(P0, "Angelic Harold", 2, 2)
        .with_mana_cost(ManaCost::Cost { shards: vec![ManaCostShard::White, ManaCostShard::Blue], generic: 1 })
        .from_oracle_text("Flying\nWhen Angelic Harold enters, you may put a name sticker on a nonland permanent you own.\nEach creature you control with three or more words in its name gets +1/+1.").id();
    let recipient = scenario.add_creature(P0, "Silvercoat Lion", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        mana(&[ManaType::White, ManaType::Blue, ManaType::Colorless]),
    );
    let mut runner = scenario.build();
    select_sheet(&mut runner, UNIQUE_SHEET);
    runner.cast(source).commit();
    assert_eq!(runner.state().objects[&source].zone, Zone::Stack);
    let committed = runner.state().clone();
    // Inline Oracle fixtures have no printed metadata for SameCard. Replaying
    // the same actual entry preserves its dynamically generated exact origin;
    // only the preference learned at the live prompt is transferred.
    for replayed in [false, true] {
        let mut entered = false;
        let mut optional = 0;
        let mut recipients = 0;
        let mut placements = 0;
        for _ in 0..20 {
            if !entered && runner.state().objects[&source].zone == Zone::Battlefield {
                entered = true;
                let trigger = runner.state().stack.iter().find(|entry| entry.source_id == source && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })).expect("remembered accept must admit the real optional ETB trigger, despite zero announcement slots");
                let ability = trigger.ability().unwrap();
                assert!(
                    ability.optional && ability.sub_ability.is_none(),
                    "reach the optional-root/no-subability feasibility branch"
                );
                assert_eq!(
                    ability.target_choice_timing,
                    engine::types::ability::TargetChoiceTiming::Resolution
                );
            }
            if entered
                && runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            let action = match &runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::OptionalEffectChoice {
                    may_trigger_key: Some(key),
                    same_card_may_trigger_choice_available,
                    ..
                } => {
                    assert!(
                        !replayed,
                        "the learned exact-instance preference must replace this decision"
                    );
                    assert_eq!(key.source_id, source);
                    assert!(
                        !same_card_may_trigger_choice_available,
                        "no fabricated printed provenance"
                    );
                    optional += 1;
                    GameAction::DecideOptionalEffectAndRemember {
                        choice: AutoMayChoice::Accept,
                        scope: MayTriggerAutoChoiceScope::ExactInstance,
                    }
                }
                WaitingFor::ChooseFromZoneChoice {
                    cards,
                    count,
                    up_to,
                    ..
                } => {
                    assert_eq!(cards.len(), 2);
                    assert!(cards.contains(&source) && cards.contains(&recipient));
                    assert_eq!((*count, *up_to), (1, false));
                    recipients += 1;
                    GameAction::SelectCards {
                        cards: vec![recipient],
                    }
                }
                WaitingFor::ChooseOneOfBranch {
                    player,
                    controller,
                    branch_descriptions,
                    ..
                } => {
                    assert_eq!((*player, *controller), (P0, P0));
                    GameAction::ChooseBranch {
                        index: branch_descriptions
                            .iter()
                            .position(|d| {
                                d == "Name — Charmed" || d == "Name — Charmed at position 2"
                            })
                            .unwrap(),
                    }
                }
                other => {
                    panic!("real optional-root entry must not announce a recipient: {other:?}")
                }
            };
            placements += runner.act(action).unwrap().events.iter().filter(|e| matches!(e, GameEvent::StickerPlaced { player_id: P0, object_id, .. } if *object_id == recipient)).count();
        }
        assert_placement_idle(&runner);
        assert!(entered);
        assert_eq!(
            (optional, recipients, placements),
            (usize::from(!replayed), 1, 1)
        );
        assert!(
            matches!(&runner.state().objects[&recipient].stickers[0], AppliedSticker::Name { locator, position: 1, .. } if locator.index == 1 && locator.sheet == UNIQUE_SHEET)
        );
        assert!(runner.state().objects[&source].stickers.is_empty());
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_eq!(runner.state().may_trigger_auto_choices.len(), 1);
        if !replayed {
            let learned = runner.state().may_trigger_auto_choices.clone();
            runner = GameRunner::from_state(committed.clone());
            runner.state_mut().may_trigger_auto_choices = learned;
        }
    }
}
