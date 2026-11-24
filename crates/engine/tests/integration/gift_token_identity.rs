//! A gift token is a token (#10900).
//!
//! CR 702.174f: "Gift a tapped Fish" means "The chosen player creates a tapped
//! 1/1 blue Fish creature token." CR 111.2: the player who creates a token owns
//! and controls it. So the gift Fish is the recipient's token for every token
//! rule: their "creature tokens you control" statics pump it, their token
//! doublers apply to its creation (CR 614.1a), and once it leaves the
//! battlefield it ceases to exist (CR 704.5d).
//!
//! CR 702.174j: for an instant or sorcery the gift happens before the spell's
//! other effects, so the gift's completion is reported once its token
//! creation has settled — after any replacement choice on that creation — and
//! before the spell's own effect. The recipient's Treasure and Food are theirs
//! to activate (CR 111.10a, CR 111.10b, CR 602.2).

use engine::database::card_db::CardDatabase;
use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, ManaChoice, ReplacementChoiceKind, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

use crate::support::shared_card_db;

const INTO_THE_FLOOD_MAW: &str =
    "Gift a tapped Fish (You may promise an opponent a gift as you cast this spell. \
If you do, they create a tapped 1/1 blue Fish creature token before its other effects.)\n\
Return target creature an opponent controls to its owner's hand. If the gift was promised, \
instead return target nonland permanent an opponent controls to its owner's hand.";

const POLLEN_SHIELD_HARE: &str = "Creature tokens you control get +1/+1.";

const PARALLEL_LIVES: &str = "If an effect would create one or more tokens under your control, \
it creates twice that many of those tokens instead.";

const NOCTURNAL_HUNGER: &str =
    "Gift a Food (You may promise an opponent a gift as you cast this spell. \
If you do, they create a Food token before its other effects. It's an artifact with \
\"{2}, {T}, Sacrifice this token: You gain 3 life.\")\n\
Destroy target creature. If the gift wasn't promised, you lose 2 life.";

fn card_db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

fn mana_pool(types: &[ManaType]) -> Vec<ManaUnit> {
    types
        .iter()
        .map(|&mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

/// Casts `spell` with any optional cost paid and the gift promised to P1,
/// targets `target`, and resolves the stack. Returns every event produced.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) -> Vec<GameEvent> {
    let mut events = Vec::new();
    cast_promised(runner, spell, target, &mut events);
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the spell resolves without a resolution prompt, got {:?}",
        runner.state().waiting_for
    );
    events
}

/// Casts `spell` with any optional cost paid and the gift promised to P1,
/// targets `target`, and passes priority until the stack is empty or a
/// resolution prompt (a replacement choice or a branch choice) stops it.
fn cast_promised(
    runner: &mut GameRunner,
    spell: ObjectId,
    target: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    let card_id = runner.state().objects[&spell].card_id;
    events.extend(
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("the spell is castable")
            .events,
    );
    drive(runner, target, events);
}

/// Answers the cast's own prompts and passes priority until the stack is
/// empty or a resolution prompt stops the spell.
fn drive(runner: &mut GameRunner, target: ObjectId, events: &mut Vec<GameEvent>) {
    for _ in 0..100 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OptionalCostChoice { .. } => GameAction::DecideOptionalCost { index: 0 },
            WaitingFor::ChooseGiftRecipient { .. } => {
                GameAction::ChooseGiftRecipient { opponent: P1 }
            }
            WaitingFor::TargetSelection { .. } => GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            },
            WaitingFor::ManaPayment { .. } => GameAction::PassPriority,
            // Blind Obedience's extort: decline the optional payment.
            WaitingFor::OptionalEffectChoice { .. } => {
                GameAction::DecideOptionalEffect { accept: false }
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                GameAction::PassPriority
            }
            WaitingFor::Priority { .. }
            | WaitingFor::ReplacementChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::NamedChoice { .. } => return,
            other => panic!("unexpected prompt while casting: {other:?}"),
        };
        events.extend(runner.act(action).expect("the cast advances").events);
    }
    panic!("the spell never settled");
}

/// P0 holds Into the Flood Maw and Murder; P1 controls Pollen-Shield Hare and
/// a Grizzly Bears (the bounce target). `doubler` puts Parallel Lives under
/// that player's control.
fn setup(doubler: Option<PlayerId>) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flood_maw = scenario
        .add_spell_to_hand(P0, "Into the Flood Maw", true)
        .from_oracle_text_with_keywords(&["Gift"], INTO_THE_FLOOD_MAW)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        })
        .id();
    // Murder, {1}{B}{B}: "Destroy target creature."
    let destroy = scenario.add_real_card(P0, "Murder", Zone::Hand, card_db());
    scenario.add_creature_from_oracle(P1, "Pollen-Shield Hare", 2, 2, POLLEN_SHIELD_HARE);
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    if let Some(player) = doubler {
        scenario.add_enchantment_from_oracle(player, "Parallel Lives", PARALLEL_LIVES);
    }
    scenario.with_mana_pool(
        P0,
        mana_pool(&[
            ManaType::Blue,
            ManaType::Black,
            ManaType::Black,
            ManaType::Black,
        ]),
    );
    (scenario.build(), flood_maw, destroy, bears)
}

/// The Fish objects P1 controls on the battlefield.
fn p1_fish(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = &runner.state().objects[id];
            obj.controller == P1 && obj.card_types.subtypes.iter().any(|s| s == "Fish")
        })
        .collect()
}

#[test]
fn gift_fish_is_the_recipients_token_and_ceases_to_exist_when_it_dies() {
    let (mut runner, flood_maw, destroy, bears) = setup(None);

    let events = cast_and_resolve(&mut runner, flood_maw, bears);

    let fish = p1_fish(&runner);
    let [fish] = fish.as_slice() else {
        panic!("one gift Fish for P1, got {fish:?}");
    };
    let fish = *fish;
    // CR 702.174j: with no replacement pausing the creation, the gift still
    // completes once, after the Fish and before the spell's bounce.
    assert_gift_settles_before(&events, &[fish], bounce_index(&events, bears));
    let obj = &runner.state().objects[&fish];
    assert!(obj.is_token, "CR 111.1: the gift Fish is a token");
    assert_eq!(obj.owner, P1, "CR 111.2: the recipient owns it");
    assert!(obj.tapped, "CR 702.174f: it enters tapped");
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(2), Some(2)),
        "P1's Pollen-Shield Hare pumps P1's creature token"
    );

    runner.cast(destroy).target_object(fish).commit();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&destroy].zone,
        Zone::Graveyard,
        "the destroy spell resolved"
    );
    assert!(
        !runner.state().battlefield.contains(&fish),
        "the gift Fish was destroyed"
    );
    assert_eq!(
        runner.state().objects.get(&fish).map(|obj| obj.zone),
        None,
        "CR 704.5d: the dead gift Fish ceases to exist instead of staying in a zone"
    );
    assert!(!runner.state().players[1].graveyard.contains(&fish));
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Hand,
        "the spell's own effect still resolved after the gift"
    );
}

/// CR 111.2 + CR 614.1a: the recipient creates the Fish, so the recipient's
/// Parallel Lives doubles it and the gift giver's does not.
#[test]
fn gift_fish_creation_meets_the_recipients_parallel_lives_only() {
    let (mut runner, flood_maw, _, bears) = setup(Some(P1));
    cast_and_resolve(&mut runner, flood_maw, bears);
    assert_eq!(
        p1_fish(&runner).len(),
        2,
        "P1's Parallel Lives doubles P1's gift Fish"
    );

    let (mut runner, flood_maw, _, bears) = setup(Some(P0));
    cast_and_resolve(&mut runner, flood_maw, bears);
    assert_eq!(
        p1_fish(&runner).len(),
        1,
        "P0's Parallel Lives does not apply"
    );
}

// ---------------------------------------------------------------------------
// Promised gifts through the real card data
// ---------------------------------------------------------------------------

/// A promised-gift board: P0 holds `spell` with `mana` floating and controls
/// `p0_cards`; P1 controls `p1_cards` and Grizzly Bears, the spell's target.
struct GiftBoard {
    runner: GameRunner,
    spell: ObjectId,
    bears: ObjectId,
    events: Vec<GameEvent>,
}

fn gift_board(spell: &str, mana: &[ManaType], p0_cards: &[&str], p1_cards: &[&str]) -> GiftBoard {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // The Bears enter first, so no permanent of the board applies to their
    // entry (Grumgully's counter would otherwise ask for an order).
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    for name in p0_cards {
        scenario.add_real_card(P0, name, Zone::Battlefield, db);
    }
    for name in p1_cards {
        scenario.add_real_card(P1, name, Zone::Battlefield, db);
    }
    let spell = scenario.add_real_card(P0, spell, Zone::Hand, db);
    scenario.with_mana_pool(P0, mana_pool(mana));
    GiftBoard {
        runner: scenario.build(),
        spell,
        bears,
        events: Vec::new(),
    }
}

impl GiftBoard {
    fn cast_promised(&mut self) {
        cast_promised(&mut self.runner, self.spell, self.bears, &mut self.events);
    }

    /// Submit `action` and keep passing priority until the stack is empty or
    /// the next resolution prompt.
    fn answer(&mut self, action: GameAction) {
        self.events
            .extend(self.runner.act(action).expect("the answer is legal").events);
        drive(&mut self.runner, self.bears, &mut self.events);
    }

    /// Submit the last answer of the spell's resolution: one action, with no
    /// priority pass after it. CR 117.2e: no player has priority while the
    /// spell resolves, so the spell finishes inside this answer — the stack
    /// and every parked resolution frame are empty and a player has priority.
    /// Appends the action's events to the board's and returns them.
    fn answer_settles(&mut self, action: GameAction) -> Vec<GameEvent> {
        let events = self.runner.act(action).expect("the answer is legal").events;
        self.events.extend(events.iter().cloned());
        let state = self.runner.state();
        assert!(
            state.stack.is_empty(),
            "the spell finished resolving inside the answer, stack {:?}",
            state.stack
        );
        assert!(
            state.resolution_stack.is_empty(),
            "no resolution work is left parked after the answer: {:?}",
            state.resolution_stack
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "a player has priority after the answer, got {:?}",
            state.waiting_for
        );
        events
    }

    /// The `ReplacementChoice` the spell's resolution is paused on: asked of
    /// `player`, offering a candidate from `source`.
    fn assert_replacement_prompt(&self, player: PlayerId, source: &str) {
        let WaitingFor::ReplacementChoice {
            player: asked,
            candidates,
            ..
        } = &self.runner.state().waiting_for
        else {
            panic!(
                "the gift's token creation asks for a replacement choice, got {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*asked, player, "CR 616.1: the affected player chooses");
        assert!(
            candidates.iter().any(|c| c.source_name == source),
            "{source} is offered: {candidates:?}"
        );
    }

    /// Index of the `Decline` answer of the optional prompt.
    fn decline_index(&self) -> usize {
        let WaitingFor::ReplacementChoice { candidates, .. } = &self.runner.state().waiting_for
        else {
            panic!("expected an optional replacement prompt");
        };
        candidates
            .iter()
            .position(|c| c.description == "Decline")
            .unwrap_or_else(|| panic!("no Decline among {candidates:?}"))
    }

    fn assert_settled(&self) {
        assert!(
            self.runner.state().stack.is_empty()
                && matches!(self.runner.state().waiting_for, WaitingFor::Priority { .. }),
            "the spell finished resolving, got {:?}",
            self.runner.state().waiting_for
        );
    }

    /// P1's battlefield tokens with the subtype `subtype`.
    fn p1_tokens(&self, subtype: &str) -> Vec<&GameObject> {
        p1_tokens(&self.runner, subtype)
    }
}

fn p1_tokens<'a>(runner: &'a GameRunner, subtype: &str) -> Vec<&'a GameObject> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| {
            o.is_token && o.controller == P1 && o.card_types.subtypes.iter().any(|s| s == subtype)
        })
        .collect()
}

/// The one P1 token with the subtype `subtype`, asserted to be P1's own token.
fn the_p1_token<'a>(runner: &'a GameRunner, subtype: &str) -> &'a GameObject {
    let found = p1_tokens(runner, subtype);
    let [token] = found.as_slice() else {
        panic!("exactly one {subtype} token for P1, got {found:?}");
    };
    assert!(token.is_token, "CR 111.1: the {subtype} is a token");
    assert_eq!(token.owner, P1, "CR 111.2: P1 created the {subtype}");
    token
}

fn gift_resolved(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::GiftDelivery,
                    ..
                }
            )
        })
        .count()
}

fn index_where(events: &[GameEvent], what: &str, pred: impl Fn(&GameEvent) -> bool) -> usize {
    events
        .iter()
        .position(pred)
        .unwrap_or_else(|| panic!("no {what} event among {events:?}"))
}

/// The first event that puts one of `created` onto the battlefield.
fn entry_index(events: &[GameEvent], created: &[ObjectId]) -> usize {
    index_where(events, "token entry", |e| match e {
        GameEvent::TokenCreated { object_id, .. } => created.contains(object_id),
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Battlefield,
            ..
        } => created.contains(object_id),
        _ => false,
    })
}

fn bounce_index(events: &[GameEvent], bears: ObjectId) -> usize {
    index_where(
        events,
        "bounce",
        |e| matches!(e, GameEvent::ZoneChanged { object_id, to: Zone::Hand, .. } if *object_id == bears),
    )
}

fn damage_to_bears_index(events: &[GameEvent], bears: ObjectId) -> usize {
    index_where(events, "damage to the Bears", |e| {
        matches!(
            e,
            GameEvent::DamageDealt { target: TargetRef::Object(id), .. } if *id == bears
        )
    })
}

/// CR 702.174j: the gift reports its completion exactly once, after every
/// token it created entered, and before `spell_effect` (the index of the
/// spell's own first effect).
fn assert_gift_settles_before(events: &[GameEvent], created: &[ObjectId], spell_effect: usize) {
    assert_eq!(gift_resolved(events), 1, "the gift completes exactly once");
    let gift = index_where(events, "gift completion", |e| {
        matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::GiftDelivery,
                ..
            }
        )
    });
    for id in created {
        assert!(
            entry_index(events, &[*id]) < gift,
            "the gift completes after its token {id:?} entered"
        );
    }
    assert!(
        gift < spell_effect,
        "CR 702.174j: the gift completes before the spell's other effects"
    );
}

/// Into the Flood Maw promised to P1, who controls `p1_cards`; P0 controls
/// `p0_cards`. Cast up to the first resolution prompt.
fn flood_maw(p0_cards: &[&str], p1_cards: &[&str]) -> GiftBoard {
    let mut board = gift_board("Into the Flood Maw", &[ManaType::Blue], p0_cards, p1_cards);
    board.cast_promised();
    board
}

/// Blooming Blast promised to P1, who controls `p1_cards`; P0 controls
/// `p0_cards`. Cast up to the first resolution prompt.
fn blooming_blast(p0_cards: &[&str], p1_cards: &[&str]) -> GiftBoard {
    let mut board = gift_board(
        "Blooming Blast",
        &[ManaType::Red, ManaType::Red],
        p0_cards,
        p1_cards,
    );
    board.cast_promised();
    board
}

const JINNIE: &str = "Jinnie Fay, Jetmir's Second";
const CHATTERFANG: &str = "Chatterfang, Squirrel General";

/// Into the Flood Maw with P1's Jinnie Fay: the Fish creation pauses for P1's
/// "may instead", and the gift has not completed while it waits.
fn flood_maw_at_jinnie_prompt() -> GiftBoard {
    let board = flood_maw(&[], &[JINNIE]);
    board.assert_replacement_prompt(P1, JINNIE);
    assert_eq!(
        gift_resolved(&board.events),
        0,
        "the gift has not completed while its token creation waits for a choice"
    );
    assert!(
        board.runner.state().battlefield.contains(&board.bears),
        "the bounce waits for the gift"
    );
    board
}

/// R1 — CR 702.174j + CR 614.1a: declining Jinnie creates the tapped Fish, and
/// only then does the gift complete, before the bounce.
#[test]
fn declined_jinnie_gift_completes_after_the_fish() {
    let mut board = flood_maw_at_jinnie_prompt();
    let decline = board.decline_index();
    let answered = board.answer_settles(GameAction::ChooseReplacement { index: decline });

    let fish = the_p1_token(&board.runner, "Fish");
    assert!(fish.tapped, "CR 702.174f: the Fish enters tapped");
    let fish = fish.id;
    assert_gift_settles_before(&answered, &[fish], bounce_index(&answered, board.bears));
    assert_eq!(board.runner.state().objects[&board.bears].zone, Zone::Hand);
}

/// Accept Jinnie on the gift Fish and choose branch `branch`. Returns the
/// board and the events of the `ChooseBranch` action, which settles the spell.
fn flood_maw_jinnie_accepts(branch: usize) -> (GiftBoard, Vec<GameEvent>) {
    let mut board = flood_maw_at_jinnie_prompt();
    board.answer(GameAction::ChooseReplacement { index: 0 });
    let WaitingFor::ChooseOneOfBranch { player, .. } = &board.runner.state().waiting_for else {
        panic!(
            "accepting asks which substitute, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "P1 chooses the substitute");
    assert_eq!(gift_resolved(&board.events), 0);
    let answered = board.answer_settles(GameAction::ChooseBranch { index: branch });
    assert!(
        board.p1_tokens("Fish").is_empty(),
        "CR 614.6: the replaced Fish is never created"
    );
    (board, answered)
}

/// R2 — CR 614.1a + CR 614.6 + the Jinnie ruling: "that many" is the one Fish,
/// and the substitute keeps the creating instruction's "tapped". The gift
/// completes after the Cat, before the bounce.
#[test]
fn accepted_jinnie_gift_creates_one_tapped_cat_then_completes() {
    let (board, answered) = flood_maw_jinnie_accepts(0);
    let cat = the_p1_token(&board.runner, "Cat");
    assert_eq!(cat.controller, P1);
    assert_eq!((cat.power, cat.toughness), (Some(2), Some(2)));
    assert!(cat.has_keyword(&Keyword::Haste));
    assert!(cat.color.contains(&ManaColor::Green));
    assert!(
        cat.tapped,
        "the gift's \"tapped\" applies to the substitute"
    );
    let cat = cat.id;
    assert_gift_settles_before(&answered, &[cat], bounce_index(&answered, board.bears));
}

/// R3 — the Dog branch of the same choice.
#[test]
fn accepted_jinnie_gift_creates_one_tapped_dog() {
    let (board, answered) = flood_maw_jinnie_accepts(1);
    let dog = the_p1_token(&board.runner, "Dog");
    assert_eq!((dog.power, dog.toughness), (Some(3), Some(1)));
    assert!(dog.has_keyword(&Keyword::Vigilance));
    assert!(
        dog.tapped,
        "the gift's \"tapped\" applies to the substitute"
    );
    let dog = dog.id;
    assert_gift_settles_before(&answered, &[dog], bounce_index(&answered, board.bears));
}

/// R5 — an untapped creating instruction (Gift a Treasure) gives an untapped
/// substitute; the gift completes before Blooming Blast's damage.
#[test]
fn accepted_jinnie_on_an_untapped_gift_creates_an_untapped_cat() {
    let mut board = blooming_blast(&[], &[JINNIE]);
    board.assert_replacement_prompt(P1, JINNIE);
    assert_eq!(gift_resolved(&board.events), 0);
    board.answer(GameAction::ChooseReplacement { index: 0 });
    let WaitingFor::ChooseOneOfBranch { player, .. } = &board.runner.state().waiting_for else {
        panic!(
            "accepting asks which substitute, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "P1 chooses the substitute");
    let answered = board.answer_settles(GameAction::ChooseBranch { index: 0 });

    assert!(board.p1_tokens("Treasure").is_empty());
    let cat = the_p1_token(&board.runner, "Cat");
    assert!(!cat.tapped, "nothing told the Treasure to enter tapped");
    let cat = cat.id;
    assert_gift_settles_before(
        &answered,
        &[cat],
        damage_to_bears_index(&answered, board.bears),
    );
    assert_eq!(board.runner.life(P1), 17, "the promised 3 damage to P1");
}

const HARDENED_SCALES: &str = "Hardened Scales";
const BRANCHING_EVOLUTION: &str = "Branching Evolution";

/// Into the Flood Maw promised to P1, who controls Grumgully, the Generous,
/// Hardened Scales and Branching Evolution:
///
/// - Grumgully: "Each other non-Human creature you control enters with an
///   additional +1/+1 counter on it."
/// - Hardened Scales: "If one or more +1/+1 counters would be put on a
///   creature you control, that many plus one +1/+1 counters are put on it
///   instead."
/// - Branching Evolution: "If one or more +1/+1 counters would be put on a
///   creature you control, twice that many +1/+1 counters are put on that
///   creature instead."
///
/// The gift Fish enters with Grumgully's counter, and CR 616.1 asks P1, the
/// Fish's controller, to order the two counter replacements. The gift has not
/// completed and the bounce has not happened while that choice waits.
fn flood_maw_at_counter_order_prompt() -> GiftBoard {
    let board = flood_maw(
        &[],
        &[
            "Grumgully, the Generous",
            HARDENED_SCALES,
            BRANCHING_EVOLUTION,
        ],
    );
    let WaitingFor::ReplacementChoice {
        player,
        candidates,
        kind,
        ..
    } = &board.runner.state().waiting_for
    else {
        panic!(
            "the gift Fish's entry counters ask for a replacement order, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(
        *player, P1,
        "CR 616.1: the affected object's controller orders"
    );
    assert_eq!(*kind, ReplacementChoiceKind::Order);
    let mut offered: Vec<&str> = candidates.iter().map(|c| c.source_name.as_str()).collect();
    offered.sort_unstable();
    assert_eq!(
        offered,
        [BRANCHING_EVOLUTION, HARDENED_SCALES],
        "the two counter replacements are offered: {candidates:?}"
    );
    assert_eq!(
        gift_resolved(&board.events),
        0,
        "the gift has not completed while its Fish's counters wait for an order"
    );
    assert!(
        board.runner.state().battlefield.contains(&board.bears),
        "the bounce waits for the gift"
    );
    board
}

/// Index of the order candidate from `source`.
fn candidate_index(board: &GiftBoard, source: &str) -> usize {
    let WaitingFor::ReplacementChoice { candidates, .. } = &board.runner.state().waiting_for else {
        panic!("expected a replacement prompt");
    };
    candidates
        .iter()
        .position(|c| c.source_name == source)
        .unwrap_or_else(|| panic!("no {source} among {candidates:?}"))
}

/// Order the Fish's counter replacements with `first` applied first, and
/// check that the answer settles the spell with the gift completing after the
/// Fish and before the bounce. Returns the Fish's +1/+1 counter count.
fn flood_maw_counters_ordered(first: &str) -> u32 {
    let mut board = flood_maw_at_counter_order_prompt();
    let index = candidate_index(&board, first);
    let answered = board.answer_settles(GameAction::ChooseReplacement { index });
    assert_eq!(
        gift_resolved(&answered),
        1,
        "the gift completes inside the answering action"
    );
    bounce_index(&answered, board.bears);

    let fish = the_p1_token(&board.runner, "Fish");
    assert_eq!(fish.controller, P1);
    assert!(fish.tapped, "CR 702.174f: the Fish enters tapped");
    let counters = fish
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    let fish = fish.id;
    assert_gift_settles_before(
        &board.events,
        &[fish],
        bounce_index(&board.events, board.bears),
    );
    counters
}

/// R21 — CR 702.174j + CR 616.1e/616.1f: Hardened Scales first, then
/// Branching Evolution: (1 + 1) × 2 = 4 counters. The gift completes after the
/// Fish settles and before the bounce.
#[test]
fn gift_fish_entry_counter_order_completes_the_gift_before_the_bounce() {
    assert_eq!(
        flood_maw_counters_ordered(HARDENED_SCALES),
        4,
        "Grumgully's counter reached the gift Fish, then +1, then doubled"
    );
}

/// R21 twin — Branching Evolution first, then Hardened Scales: 1 × 2 + 1 = 3
/// counters, with the same ordering of the gift.
#[test]
fn gift_fish_entry_counter_order_branching_first_completes_the_gift_before_the_bounce() {
    assert_eq!(
        flood_maw_counters_ordered(BRANCHING_EVOLUTION),
        3,
        "Grumgully's counter reached the gift Fish, then doubled, then +1"
    );
}

/// Into the Flood Maw promised to P1 after P0 resolved Mystic Reflection on
/// P0's Painter's Servant:
///
/// - Mystic Reflection: "Choose target nonlegendary creature. The next time
///   one or more creatures or planeswalkers enter this turn, they enter as
///   copies of the chosen creature."
/// - Painter's Servant: "As this creature enters, choose a color. All cards
///   that aren't on the battlefield, spells, and permanents are the chosen
///   color in addition to their other colors."
///
/// The gift Fish is the next creature to enter, so it enters as a copy of
/// Painter's Servant and its controller chooses a color before it enters.
/// Cast up to that choice.
fn flood_maw_mirrored_onto_painters_servant() -> GiftBoard {
    let db = card_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let servant = scenario.add_real_card(P0, "Painter's Servant", Zone::Battlefield, db);
    let mystic = scenario.add_real_card(P0, "Mystic Reflection", Zone::Hand, db);
    let spell = scenario.add_real_card(P0, "Into the Flood Maw", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        mana_pool(&[ManaType::Blue, ManaType::Blue, ManaType::Blue]),
    );
    let mut runner = scenario.build();
    runner.cast(mystic).target_object(servant).resolve();
    assert!(
        runner.state().stack.is_empty(),
        "reach guard: Mystic Reflection resolved"
    );
    let mut board = GiftBoard {
        runner,
        spell,
        bears,
        events: Vec::new(),
    };
    board.cast_promised();
    board
}

/// R25 — the Mystic Reflection rulings: "Any 'as [this permanent] enters the
/// battlefield' … abilities of the target creature will also work", and "if
/// another effect says, 'Creatures enter the battlefield tapped,' the
/// permanents will enter as tapped copies". CR 111.2 +
/// CR 109.5: P1 creates the Fish and controls it, so P1 makes the copy's
/// choice, and CR 614.12a: before it enters. CR 702.174j + CR 608.2c +
/// CR 117.2e: the answer finishes the spell — the copy enters, then the gift
/// completes, then the bounce.
#[test]
fn gift_fish_entering_as_a_painters_servant_copy_completes_the_gift_before_the_bounce() {
    let mut board = flood_maw_mirrored_onto_painters_servant();
    let WaitingFor::NamedChoice {
        player, options, ..
    } = &board.runner.state().waiting_for
    else {
        panic!(
            "the gift Fish's Painter's Servant copy asks for a color, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(
        *player, P1,
        "CR 109.5 + CR 111.2: the copy's controller, P1, chooses"
    );
    assert!(options.iter().any(|option| option == "Blue"), "{options:?}");
    assert_eq!(
        gift_resolved(&board.events),
        0,
        "the gift has not completed while its token's entry waits for a choice"
    );
    assert!(
        board.runner.state().battlefield.contains(&board.bears),
        "the bounce waits for the gift"
    );

    let answered = board.answer_settles(GameAction::ChooseOption {
        choice: "Blue".to_string(),
    });

    let fish = the_p1_token(&board.runner, "Scarecrow");
    assert_eq!(fish.controller, P1);
    assert_eq!(
        fish.name, "Painter's Servant",
        "the gift Fish entered as a copy of Painter's Servant"
    );
    assert!(
        fish.tapped,
        "CR 110.5a: tapped is not copied, so the gift's \"tapped\" stands"
    );
    let fish = fish.id;
    assert_gift_settles_before(&answered, &[fish], bounce_index(&answered, board.bears));
    assert_eq!(board.runner.state().objects[&board.bears].zone, Zone::Hand);
}

/// R7 — Chatterfang ruling: "Anything else specified in the effect creating
/// the tokens (such as tapped …) applies to both the original tokens and the
/// Squirrels." Both enter tapped under P1.
#[test]
fn chatterfang_squirrel_with_the_gift_fish_enters_tapped() {
    let board = flood_maw(&[], &[CHATTERFANG]);
    board.assert_settled();
    let fish = the_p1_token(&board.runner, "Fish");
    let squirrel = the_p1_token(&board.runner, "Squirrel");
    assert!(fish.tapped, "the Fish enters tapped");
    assert!(squirrel.tapped, "the Squirrel enters tapped too");
    let created = [fish.id, squirrel.id];
    assert_gift_settles_before(
        &board.events,
        &created,
        bounce_index(&board.events, board.bears),
    );
}

/// R8 — the untapped sibling: Gift a Treasure says nothing about tapped, so
/// the Treasure and the added Squirrel enter untapped.
#[test]
fn chatterfang_squirrel_with_an_untapped_gift_enters_untapped() {
    let board = blooming_blast(&[], &[CHATTERFANG]);
    board.assert_settled();
    let treasure = the_p1_token(&board.runner, "Treasure");
    let squirrel = the_p1_token(&board.runner, "Squirrel");
    assert!(!treasure.tapped);
    assert!(!squirrel.tapped);
}

/// R10 — the Divine Visitation and Chatterfang rulings ("Anything else
/// specified in the effect creating the token (such as tapped …) still
/// applies" / "applies to both the original tokens and the Squirrels"): with
/// both under P1, every token the gift creates enters tapped under P1, and the
/// gift completes after them, before the bounce. The two replacements may be
/// applied in either order and this row pins neither the order nor the counts:
/// any ordering prompt is answered with the first candidate.
#[test]
fn divine_visitation_and_chatterfang_keep_the_gift_tapped() {
    let mut board = flood_maw(&[], &["Divine Visitation", CHATTERFANG]);
    while matches!(
        board.runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        board.answer(GameAction::ChooseReplacement { index: 0 });
    }
    board.assert_settled();

    let angels = board.p1_tokens("Angel");
    assert!(!angels.is_empty(), "reach: Divine Visitation applied");
    assert!(board.p1_tokens("Fish").is_empty(), "the Fish was replaced");
    let mut created: Vec<&GameObject> = angels;
    created.extend(board.p1_tokens("Squirrel"));
    for token in &created {
        assert!(token.tapped, "{} {:?} enters tapped", token.name, token.id);
        assert!(token.is_token && token.owner == P1);
    }
    let created: Vec<ObjectId> = created.iter().map(|token| token.id).collect();
    assert_gift_settles_before(
        &board.events,
        &created,
        bounce_index(&board.events, board.bears),
    );
}

/// R11 — a later batch-specific override still wins: P0's Blind Obedience
/// ("Artifacts and creatures your opponents control enter tapped") taps both
/// P1's Treasure and the Squirrel Chatterfang adds to it.
#[test]
fn blind_obedience_still_taps_the_untapped_gift_and_its_squirrel() {
    let mut board = gift_board(
        "Blooming Blast",
        &[ManaType::Red, ManaType::Red],
        &["Blind Obedience"],
        &[CHATTERFANG],
    );
    board.cast_promised();
    board.assert_settled();
    let treasure = the_p1_token(&board.runner, "Treasure");
    let squirrel = the_p1_token(&board.runner, "Squirrel");
    assert!(treasure.tapped, "Blind Obedience taps the Treasure");
    assert!(squirrel.tapped, "Blind Obedience taps the Squirrel");
}

/// R12 — CR 111.10a + CR 602.2: the gift Treasure is P1's. P0 can't activate
/// it; P1, with priority, sacrifices it for one mana of any color.
#[test]
fn recipient_activates_the_gift_treasure() {
    let mut board = blooming_blast(&[], &[]);
    board.assert_settled();
    assert_eq!(board.runner.life(P1), 17, "the promised 3 damage to P1");
    let treasure = the_p1_token(&board.runner, "Treasure").id;

    // CR 602.2: only the Treasure's controller may activate its ability.
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P0
    ));
    assert!(
        board
            .runner
            .act(GameAction::ActivateAbility {
                source_id: treasure,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err(),
        "P0 does not control P1's Treasure"
    );
    // CR 117.3d: P0 passes, so P1 receives priority.
    board
        .runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority");
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));

    let red_before = board.runner.state().players[1]
        .mana_pool
        .count_color(ManaType::Red);
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: treasure,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 activates the Treasure");
    let WaitingFor::ChooseManaColor { player, .. } = &board.runner.state().waiting_for else {
        panic!(
            "the Treasure asks for a color, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    board
        .runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        })
        .expect("red is a color");

    assert_eq!(
        board.runner.state().players[1]
            .mana_pool
            .count_color(ManaType::Red),
        red_before + 1,
        "CR 605.3b: P1 gets the mana"
    );
    assert!(
        !board.runner.state().battlefield.contains(&treasure),
        "CR 111.10a: the Treasure was sacrificed"
    );
    assert!(
        !board.runner.state().players[1]
            .graveyard
            .contains(&treasure),
        "CR 704.5d: the sacrificed Treasure ceases to exist"
    );
}

/// R13 — CR 111.10b: P1 pays {2}, taps and sacrifices the gift Food and gains
/// 3 life. Nocturnal Hunger was promised, so P0 does not lose 2 life.
#[test]
fn recipient_activates_the_gift_food() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, card_db());
    let hunger = scenario
        .add_spell_to_hand(P0, "Nocturnal Hunger", true)
        .from_oracle_text_with_keywords(&["Gift"], NOCTURNAL_HUNGER)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 2,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        mana_pool(&[ManaType::Black, ManaType::Black, ManaType::Black]),
    );
    scenario.with_mana_pool(P1, mana_pool(&[ManaType::Colorless, ManaType::Colorless]));
    let mut runner = scenario.build();

    let events = cast_and_resolve(&mut runner, hunger, bears);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Graveyard);
    assert_eq!(runner.life(P0), 20, "the gift was promised: no life loss");
    let food = the_p1_token(&runner, "Food").id;
    assert_gift_settles_before(
        &events,
        &[food],
        index_where(&events, "the Bears' destruction", |e| {
            matches!(
                e,
                GameEvent::ZoneChanged { object_id, to: Zone::Graveyard, .. } if *object_id == bears
            )
        }),
    );

    runner.act(GameAction::PassPriority).expect("P0 passes");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    runner
        .act(GameAction::ActivateAbility {
            source_id: food,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P1 activates the Food");
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the Food's ability is on the stack"
    );
    assert!(
        !runner.state().battlefield.contains(&food),
        "CR 111.10b: the Food is sacrificed as a cost"
    );
    assert_eq!(
        runner.state().players[1].mana_pool.total(),
        0,
        "P1 paid {{2}}"
    );
    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 23, "P1 gains 3 life");
    assert_eq!(runner.life(P0), 20);
}
