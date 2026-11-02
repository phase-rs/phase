//! CR 601.2f + CR 205.3: Ancient Cellarspawn's "Each spell you cast that's a
//! Demon, Horror, or Nightmare costs {1} less to cast." through the production
//! cost route: legal-action discovery (`legal_actions`), the locked total of a
//! manual cast (`pending_cast.cost`, CR 601.2f), affordability and dispatch of
//! an automatic cast, and finalization (its own second ability reads the mana
//! actually spent). Spells other than the real cards are labelled synthetics.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ANCIENT_CELLARSPAWN: &str = "Each spell you cast that's a Demon, Horror, or Nightmare costs {1} less to cast.\nWhenever you cast a spell, if the amount of mana spent to cast it was less than its mana value, target opponent loses life equal to the difference.";
const THALIA: &str = "First strike\nNoncreature spells cost {1} more to cast.";

fn cost(generic: u32, black: usize) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black; black],
        generic,
    }
}

fn pool(colorless: usize, black: usize) -> Vec<ManaUnit> {
    let unit = |t| ManaUnit::new(t, ObjectId(0), false, vec![]);
    std::iter::repeat_n(unit(ManaType::Colorless), colorless)
        .chain(std::iter::repeat_n(unit(ManaType::Black), black))
        .collect()
}

/// P0 controls Ancient Cellarspawn; `caster` holds the spell `add` creates and
/// the mana in `mana`, at the caster's main phase with priority.
fn board(
    caster: PlayerId,
    mana: Vec<ManaUnit>,
    add: impl FnOnce(&mut GameScenario) -> ObjectId,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Ancient Cellarspawn", 3, 3, ANCIENT_CELLARSPAWN)
        .with_subtypes(vec!["Horror"]);
    let spell = add(&mut scenario);
    scenario.with_mana_pool(caster, mana);
    let mut runner = scenario.build();
    if caster != P0 {
        let state = runner.state_mut();
        state.active_player = caster;
        state.priority_player = caster;
        state.waiting_for = WaitingFor::Priority { player: caster };
    }
    (runner, spell)
}

fn creature_spell(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    subtypes: Vec<&str>,
    mana_cost: ManaCost,
) -> ObjectId {
    scenario
        .add_creature_to_hand(owner, name, 3, 3)
        .with_subtypes(subtypes)
        .with_mana_cost(mana_cost)
        .id()
}

fn offered(runner: &GameRunner, spell: ObjectId) -> bool {
    legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell),
    )
}

/// CR 601.2f: begin a manual cast and read the total it locked in.
fn locked(runner: &mut GameRunner, spell: ObjectId) -> ManaCost {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("begin the cast");
    runner
        .state()
        .pending_cast
        .as_ref()
        .map(|pending| pending.cost.clone())
        .expect("a manual cast parks at payment with its locked total")
}

/// Row 1: a Demon creature spell costing {2}{B} with exactly {1}{B}
/// available is offered, locks {1}{B}, and an automatic cast finalizes with 2
/// mana spent.
#[test]
fn a_listed_spell_is_offered_locks_the_reduced_cost_and_casts() {
    let (runner, demon) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon", vec!["Demon"], cost(2, 1))
    });
    assert!(offered(&runner, demon), "offered with only {{1}}{{B}}");
    let mut manual = runner;
    assert_eq!(locked(&mut manual, demon), cost(1, 1), "locked {{1}}{{B}}");

    let (mut runner, demon) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon", vec!["Demon"], cost(2, 1))
    });
    let card_id = runner.state().objects[&demon].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: demon,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the automatic cast is affordable");
    let spent = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::Spell {
                actual_mana_spent, ..
            } if entry.id == demon => Some(*actual_mana_spent),
            _ => None,
        });
    assert_eq!(spent, Some(2), "finalized with 2 mana spent");
}

/// Row 2: a spell that isn't listed is not offered with {1}{B} and locks its
/// full {2}{B}; a dispatched automatic cast is rejected at affordability.
#[test]
fn an_unlisted_spell_is_not_discounted() {
    let (mut runner, bear) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Bear", vec!["Bear"], cost(2, 1))
    });
    assert!(!offered(&runner, bear), "not offered with {{1}}{{B}}");
    let card_id = runner.state().objects[&bear].card_id;
    assert!(
        runner
            .act(GameAction::CastSpell {
                object_id: bear,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .is_err()
            || runner.state().objects[&bear].zone == Zone::Hand,
        "the dispatched cast is rejected"
    );
    let (mut manual, bear) = board(P0, pool(2, 1), |s| {
        creature_spell(s, P0, "Bear", vec!["Bear"], cost(2, 1))
    });
    assert_eq!(locked(&mut manual, bear), cost(2, 1));
}

/// Row 3: a Demon Horror (two listed subtypes) is discounted once.
#[test]
fn overlapping_listed_subtypes_discount_once() {
    let (mut runner, spell) = board(P0, pool(3, 1), |s| {
        creature_spell(s, P0, "Demon Horror", vec!["Demon", "Horror"], cost(3, 1))
    });
    assert_eq!(locked(&mut runner, spell), cost(2, 1));
}

/// Row 4: a non-creature spell with a listed subtype (a labelled synthetic
/// Horror instant) is discounted too: the static names spells, not creatures.
#[test]
fn a_noncreature_spell_with_a_listed_subtype_is_discounted() {
    let (mut runner, spell) = board(P0, pool(1, 1), |s| {
        s.add_spell_to_hand_from_oracle(P0, "Horror Rite", true, "Draw a card.")
            .with_subtypes(vec!["Horror"])
            .with_mana_cost(cost(1, 1))
            .id()
    });
    assert_eq!(locked(&mut runner, spell), cost(0, 1));
}

/// Row 5: "spell you cast" — an opponent's Demon is not discounted.
#[test]
fn an_opponents_listed_spell_is_not_discounted() {
    let (mut runner, demon) = board(P1, pool(2, 1), |s| {
        creature_spell(s, P1, "Their Demon", vec!["Demon"], cost(2, 1))
    });
    assert_eq!(locked(&mut runner, demon), cost(2, 1));
}

/// Row 6: CR 601.2f — a generic reduction reduces only generic mana; a Demon
/// costing {B}{B} still costs {B}{B}.
#[test]
fn a_generic_reduction_cannot_reduce_colored_mana() {
    let (mut runner, demon) = board(P0, pool(0, 2), |s| {
        creature_spell(s, P0, "Pure Demon", vec!["Demon"], cost(0, 2))
    });
    assert_eq!(locked(&mut runner, demon), cost(0, 2));
}

/// Row 7: CR 601.2f — increases and reductions both apply. Thalia's {1} tax
/// on the Horror instant and Cellarspawn's {1} discount cancel out.
#[test]
fn a_tax_and_the_discount_both_apply() {
    let (mut runner, spell) = board(P0, pool(1, 1), |s| {
        s.add_creature_from_oracle(P1, "Thalia, Guardian of Thraben", 2, 1, THALIA);
        s.add_spell_to_hand_from_oracle(P0, "Horror Rite", true, "Draw a card.")
            .with_subtypes(vec!["Horror"])
            .with_mana_cost(cost(1, 1))
            .id()
    });
    assert_eq!(locked(&mut runner, spell), cost(1, 1));
}

fn adventure_face() -> engine::game::game_object::BackFaceData {
    engine::game::game_object::BackFaceData {
        is_swap_snapshot: false,
        trigger_printed_origins: Vec::new(),
        name: "Unlisted Adventure".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: {
            let mut card_type = CardType::default();
            card_type.core_types.push(CoreType::Instant);
            card_type.subtypes.push("Adventure".to_string());
            card_type
        },
        mana_cost: cost(1, 1),
        keywords: Vec::new(),
        abilities: vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![ManaColor::Black],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        layout_kind: None,
        parse_warnings: vec![],
    }
}

/// Row 8: CR 601.2f + CR 715.3 — the cost is the face being cast. A labelled
/// synthetic adventurer whose creature face is a Demon ({2}{B}) and whose
/// Adventure face is not ({1}{B}): with only {B}, the Adventure can't be cast
/// (no discount); with {1}{B}, the Demon face can.
#[test]
fn only_the_face_being_cast_is_read() {
    let adventurer = |runner: &mut GameRunner, card: ObjectId| {
        runner.state_mut().objects.get_mut(&card).unwrap().back_face = Some(adventure_face());
    };
    let (mut runner, card) = board(P0, pool(0, 1), |s| {
        creature_spell(s, P0, "Demon Adventurer", vec!["Demon"], cost(2, 1))
    });
    adventurer(&mut runner, card);
    assert!(
        runner
            .cast(card)
            .adventure_face(false)
            .try_resolve()
            .is_err()
            || runner.state().objects[&card].zone == Zone::Hand,
        "the unlisted Adventure face is not discounted"
    );
    let (mut runner, card) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon Adventurer", vec!["Demon"], cost(2, 1))
    });
    adventurer(&mut runner, card);
    runner.cast(card).adventure_face(true).resolve();
    assert_eq!(runner.state().objects[&card].zone, Zone::Battlefield);
}

/// Row 9: CR 601.2f — once the total is locked in, it doesn't change: the
/// Cellarspawn leaving during payment (moved directly, labelled) leaves the
/// locked {1}{B}, and the payment completes with it.
#[test]
fn the_locked_cost_survives_the_source_leaving_during_payment() {
    let (mut runner, demon) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon", vec!["Demon"], cost(2, 1))
    });
    assert_eq!(locked(&mut runner, demon), cost(1, 1));
    let cellarspawn = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Ancient Cellarspawn")
        .expect("Cellarspawn");
    {
        let state = runner.state_mut();
        engine::game::zones::move_to_zone(state, cellarspawn, Zone::Graveyard, &mut Vec::new());
    }
    assert_eq!(
        runner.state().pending_cast.as_ref().map(|p| p.cost.clone()),
        Some(cost(1, 1)),
        "still locked"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    runner.act(GameAction::PassPriority).expect("pay");
    assert_eq!(
        runner.state().objects[&demon].zone,
        Zone::Stack,
        "finalized"
    );
}

/// Ancient Cellarspawn's second ability ("… target opponent loses life equal to
/// the difference.") lost its stated subject and made its CONTROLLER lose the
/// life. Until that subject is threaded through, the clause fails closed: the
/// trigger carries an `Unimplemented` effect and a discounted cast no longer
/// costs its controller life.
#[test]
fn the_second_ability_fails_closed_instead_of_hitting_its_controller() {
    let (mut runner, demon) = board(P0, pool(1, 1), |s| {
        creature_spell(s, P0, "Demon", vec!["Demon"], cost(2, 1))
    });
    let cellarspawn = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Ancient Cellarspawn")
        .expect("Cellarspawn");
    let effects: Vec<_> = cellarspawn
        .trigger_definitions
        .iter_unchecked()
        .filter_map(|entry| entry.definition().execute.as_deref())
        .map(|execute| execute.effect.as_ref().clone())
        .collect();
    assert!(
        matches!(effects.as_slice(), [Effect::Unimplemented { .. }]),
        "{effects:?}"
    );
    let p0 = runner.life(P0);
    runner.cast(demon).resolve();
    assert_eq!(runner.state().objects[&demon].zone, Zone::Battlefield);
    assert_eq!(runner.life(P0), p0, "the controller loses nothing");
}
