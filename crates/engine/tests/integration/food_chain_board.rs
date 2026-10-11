//! Board C: Food Chain with a creature whose own text lets it be cast from exile.
//!
//! A reusable four-seat board built from cards the committed integration card
//! fixture carries, placed with `GameScenarioDbExt::add_real_card` and driven
//! only through `game::engine::apply()`.
//!
//! | Card | Seat / zone | Why it is here |
//! |---|---|---|
//! | Food Chain | P0 battlefield | exiles the member creature as its cost and adds the mana that recasts it |
//! | Eternal Scourge (C1) or Squee, the Immortal (C2) | P0 battlefield | the member: its own text permits casting it from exile |
//! | basic Swamps (C1) or Mountains (C2), `LIBRARY_PER_SEAT` each | every seat's library | filler, so no seat decks out |
//!
//! The mana is only what Food Chain's activation adds: black for C1, red for C2.
//! The board starts in P0's precombat main phase.
//!
//! Verbatim Oracle text, as the fixture stores it:
//!   Food Chain: "Exile a creature you control: Add X mana of any one color,
//!     where X is 1 plus the exiled creature's mana value. Spend this mana only
//!     to cast creature spells."
//!   Eternal Scourge: "You may cast this card from exile. / When this creature
//!     becomes the target of a spell or ability an opponent controls, exile
//!     this creature."
//!   Squee, the Immortal: "You may cast this card from your graveyard or from
//!     exile."

use engine::ai_support::legal_actions;
use engine::database::card_db::CardDatabase;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastingVariant, ManaChoice, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const SEATS: [PlayerId; 4] = [P0, P1, PlayerId(2), PlayerId(3)];

/// Basic lands per seat; read by no assertion.
const LIBRARY_PER_SEAT: usize = 8;

#[derive(Clone, Copy, Debug)]
pub(super) enum BoardCMember {
    C1EternalScourge,
    C2SqueeTheImmortal,
}

impl BoardCMember {
    fn creature(self) -> &'static str {
        match self {
            Self::C1EternalScourge => "Eternal Scourge",
            Self::C2SqueeTheImmortal => "Squee, the Immortal",
        }
    }

    fn library_land(self) -> &'static str {
        match self {
            Self::C1EternalScourge => "Swamp",
            Self::C2SqueeTheImmortal => "Mountain",
        }
    }

    pub(super) fn mana(self) -> ManaType {
        match self {
            Self::C1EternalScourge => ManaType::Black,
            Self::C2SqueeTheImmortal => ManaType::Red,
        }
    }
}

/// The built board, with the ids a driver needs.
pub(super) struct FoodChainBoard {
    pub(super) runner: GameRunner,
    pub(super) food_chain: ObjectId,
    pub(super) creature: ObjectId,
}

struct BoardScenario {
    scenario: GameScenario,
    db: &'static CardDatabase,
    food_chain: ObjectId,
    creature: ObjectId,
}

fn scenario(member: BoardCMember) -> Option<BoardScenario> {
    let db = load_db()?;
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let food_chain = scenario.add_real_card(P0, "Food Chain", Zone::Battlefield, db);
    let creature = scenario.add_real_card(P0, member.creature(), Zone::Battlefield, db);
    for seat in SEATS {
        for _ in 0..LIBRARY_PER_SEAT {
            scenario.add_real_card(seat, member.library_land(), Zone::Library, db);
        }
    }
    Some(BoardScenario {
        scenario,
        db,
        food_chain,
        creature,
    })
}

/// Build the board. `None` when neither the committed fixture nor a full card
/// export is available, which is the only condition under which a driver skips.
pub(super) fn build(member: BoardCMember) -> Option<FoodChainBoard> {
    let BoardScenario {
        scenario,
        food_chain,
        creature,
        ..
    } = scenario(member)?;
    Some(FoodChainBoard {
        runner: scenario.build(),
        food_chain,
        creature,
    })
}

/// Activate Food Chain, exiling `creature` and taking the mana in `color`.
pub(super) fn exile_with_food_chain(
    runner: &mut GameRunner,
    food_chain: ObjectId,
    creature: ObjectId,
    color: ManaType,
) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: food_chain,
            ability_index: 0,
        })
        .expect("Food Chain activates");
    runner
        .act(GameAction::SelectCards {
            cards: vec![creature],
        })
        .expect("the exile cost accepts the creature");
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(color),
            count: 1,
        })
        .expect("Food Chain's color is chosen");
}

pub(super) fn cast_spell(runner: &mut GameRunner, object_id: ObjectId) -> Result<(), EngineError> {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .map(|_| ())
}

fn pass_until_stack_empty(runner: &mut GameRunner) {
    for _ in 0..16 {
        if runner.state().stack.is_empty() {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority passes");
    }
    panic!("stack did not empty: {:?}", runner.state().stack);
}

/// CR 604.6 + CR 113.6f + CR 400.7: Food Chain exiles the member, which is then
/// cast from exile under its own permission and resolves as a new object.
fn drive_member(member: BoardCMember) {
    let Some(FoodChainBoard {
        mut runner,
        food_chain,
        creature,
    }) = build(member)
    else {
        return;
    };
    let on_battlefield = runner.state().objects[&creature].incarnation;

    exile_with_food_chain(&mut runner, food_chain, creature, member.mana());
    let in_exile = &runner.state().objects[&creature];
    assert_eq!(in_exile.zone, Zone::Exile);
    assert!(in_exile.incarnation > on_battlefield);
    let exiled_incarnation = in_exile.incarnation;

    assert!(
        legal_actions(runner.state()).iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == creature)
        ),
        "{member:?}: the exiled member must be offered as a cast"
    );
    cast_spell(&mut runner, creature).expect("the member is cast from exile");
    let top = runner
        .state()
        .stack
        .last()
        .expect("the member is on the stack");
    assert_eq!(top.id, creature);
    assert!(matches!(
        top.kind,
        StackEntryKind::Spell {
            casting_variant: CastingVariant::Normal,
            ..
        }
    ));

    pass_until_stack_empty(&mut runner);
    let resolved = &runner.state().objects[&creature];
    assert_eq!(resolved.zone, Zone::Battlefield);
    assert!(resolved.incarnation > exiled_incarnation);
}

#[test]
fn c1_eternal_scourge_is_cast_from_exile_after_food_chain() {
    drive_member(BoardCMember::C1EternalScourge);
}

#[test]
fn c2_squee_is_cast_from_exile_after_food_chain() {
    drive_member(BoardCMember::C2SqueeTheImmortal);
}

/// A creature Food Chain exiles whose own text grants no exile permission
/// stays uncastable there.
#[test]
fn c1_food_chain_exiled_creature_without_permission_is_not_castable() {
    let Some(BoardScenario {
        mut scenario,
        db,
        food_chain,
        creature: scourge,
    }) = scenario(BoardCMember::C1EternalScourge)
    else {
        return;
    };
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let mut runner = scenario.build();
    for exiled in [bears, scourge] {
        exile_with_food_chain(&mut runner, food_chain, exiled, ManaType::Black);
    }
    assert_eq!(runner.state().objects[&bears].zone, Zone::Exile);

    let available = spell_objects_available_to_cast(runner.state(), P0);
    assert!(available.contains(&scourge));
    assert!(!available.contains(&bears));
    assert!(matches!(
        cast_spell(&mut runner, bears),
        Err(EngineError::InvalidAction(message)) if message == "Card is not in a castable zone"
    ));
}
