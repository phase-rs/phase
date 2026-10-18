//! Composite costs whose legs use up chosen objects (issue #9784).
//!
//! Jarad, Golgari Lich Lord (Scryfall):
//!   "Sacrifice a Swamp and a Forest: Return this card from your graveyard to your hand."
//! Overgrown Tomb (Scryfall): Land — Swamp Forest.
//! Recurring Nightmare (Scryfall): "Sacrifice a creature, Return this enchantment to its
//!   owner's hand: Return target creature card from your graveyard to the battlefield.
//!   Activate only as a sorcery."
//!
//! CR 118.3: a player can't pay a cost without the resources to pay it fully.
//! CR 601.2h + CR 602.2b: partial payments are not allowed; unpayable costs can't be paid.
//! CR 701.21a: a sacrificed permanent moves to its owner's graveyard, so it can pay only
//! one sacrifice leg.

use engine::game::casting::can_activate_ability_now;
use engine::game::game_object::AttachTarget;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, ControllerRef,
    DiscardSelfScope, Effect, QuantityExpr, QuantityRef, SacrificeCost, TargetFilter, TargetRef,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const JARAD: &str = "Jarad gets +1/+1 for each creature card in your graveyard.\n{1}{B}{G}, Sacrifice another creature: Each opponent loses life equal to the sacrificed creature's power.\nSacrifice a Swamp and a Forest: Return this card from your graveyard to your hand.";
const OVERGROWN_TOMB: &str =
    "({T}: Add {B} or {G}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped.";
const RECURRING_NIGHTMARE: &str = "Sacrifice a creature, Return this enchantment to its owner's hand: Return target creature card from your graveyard to the battlefield. Activate only as a sorcery.";
const REST_IN_PEACE: &str = "When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead.";
const WHEEL_OF_SUN_AND_MOON: &str = "Enchant player\nIf a card would be put into enchanted player's graveyard from anywhere, instead that card is revealed and put on the bottom of that player's library.";

#[derive(Clone, Copy)]
enum Lands {
    /// One Overgrown Tomb.
    TombOnly,
    /// A basic Swamp and a basic Forest.
    BasicPair,
    /// One Overgrown Tomb and a basic Swamp.
    TombAndBasicSwamp,
}

struct JaradBoard {
    runner: GameRunner,
    jarad: ObjectId,
    tomb: Option<ObjectId>,
    basic_swamp: Option<ObjectId>,
    ability: usize,
}

fn jarad_board(lands: Lands) -> JaradBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jarad = scenario
        .add_creature_to_graveyard(P0, "Jarad, Golgari Lich Lord", 2, 2)
        .from_oracle_text(JARAD)
        .id();
    let mut tomb = None;
    let mut basic_swamp = None;
    match lands {
        Lands::TombOnly | Lands::TombAndBasicSwamp => {
            tomb = Some(
                scenario
                    .add_land_from_oracle(P0, "Overgrown Tomb", OVERGROWN_TOMB)
                    .with_subtypes(vec!["Swamp", "Forest"])
                    .id(),
            );
        }
        Lands::BasicPair => {}
    }
    match lands {
        Lands::BasicPair => {
            basic_swamp = Some(scenario.add_basic_land(P0, ManaColor::Black));
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        Lands::TombAndBasicSwamp => {
            basic_swamp = Some(scenario.add_basic_land(P0, ManaColor::Black));
        }
        Lands::TombOnly => {}
    }
    let runner = scenario.build();
    let ability = runner.state().objects[&jarad]
        .abilities
        .iter()
        .position(|a| a.activation_zone == Some(Zone::Graveyard))
        .expect("reach: Jarad's graveyard-return ability is found by its activation zone");
    let cost = runner.state().objects[&jarad].abilities[ability]
        .cost
        .clone();
    let sacrifice_legs = match &cost {
        Some(AbilityCost::Composite { costs }) => costs
            .iter()
            .filter(|c| matches!(c, AbilityCost::Sacrifice(_)))
            .count(),
        _ => 0,
    };
    assert_eq!(
        sacrifice_legs, 2,
        "reach: 'Sacrifice a Swamp and a Forest' parses as a Composite with two Sacrifice legs, got {cost:?}"
    );
    if let Some(tomb) = tomb {
        let subtypes = &runner.state().objects[&tomb].card_types.subtypes;
        assert!(
            subtypes.iter().any(|s| s == "Swamp") && subtypes.iter().any(|s| s == "Forest"),
            "reach: Overgrown Tomb is both a Swamp and a Forest, got {subtypes:?}"
        );
    }
    JaradBoard {
        runner,
        jarad,
        tomb,
        basic_swamp,
        ability,
    }
}

fn sacrifice_choices(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            choices,
            ..
        } => choices.clone(),
        other => panic!("expected a sacrifice cost prompt, got {other:?}"),
    }
}

#[test]
fn jarad_graveyard_return_needs_two_distinct_lands() {
    // Control: a separate Swamp and Forest make the ability legal.
    let board = jarad_board(Lands::BasicPair);
    assert!(
        can_activate_ability_now(board.runner.state(), P0, board.jarad, board.ability),
        "reach: with a basic Swamp and a basic Forest the graveyard ability is activatable"
    );

    // One Swamp Forest can pay only one of the two sacrifice legs.
    let board = jarad_board(Lands::TombOnly);
    assert!(
        !can_activate_ability_now(board.runner.state(), P0, board.jarad, board.ability),
        "a single Swamp Forest can't pay 'Sacrifice a Swamp and a Forest' (CR 118.3 / CR 601.2h)"
    );
}

#[test]
fn jarad_single_dual_land_activation_is_refused_without_payment() {
    let JaradBoard {
        mut runner,
        jarad,
        tomb,
        ability,
        ..
    } = jarad_board(Lands::TombOnly);
    let tomb = tomb.unwrap();
    let activated = runner.act(GameAction::ActivateAbility {
        source_id: jarad,
        ability_index: ability,
    });
    assert!(
        activated.is_err(),
        "the activation is refused before any payment, got {activated:?} / {:?}",
        runner.state().waiting_for
    );
    let state = runner.state();
    assert_eq!(state.objects[&jarad].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&tomb].zone,
        Zone::Battlefield,
        "no partial payment (CR 601.2h): Overgrown Tomb is not sacrificed"
    );
}

/// With an Overgrown Tomb and a basic Swamp the cost is payable (basic Swamp for the
/// Swamp leg, Tomb for the Forest leg). The Swamp leg must not offer the Tomb, or the
/// Forest leg is stranded.
#[test]
fn jarad_swamp_leg_keeps_the_dual_land_for_the_forest_leg() {
    let JaradBoard {
        mut runner,
        jarad,
        tomb,
        basic_swamp,
        ability,
    } = jarad_board(Lands::TombAndBasicSwamp);
    let (tomb, basic_swamp) = (tomb.unwrap(), basic_swamp.unwrap());
    assert!(
        can_activate_ability_now(runner.state(), P0, jarad, ability),
        "a basic Swamp plus a Swamp Forest pays both legs"
    );
    runner
        .act(GameAction::ActivateAbility {
            source_id: jarad,
            ability_index: ability,
        })
        .expect("activation starts");

    assert_eq!(
        sacrifice_choices(&runner),
        vec![basic_swamp],
        "the Swamp leg offers only the basic Swamp; the Tomb is needed for the Forest leg"
    );

    // A forged pick of the Tomb for the Swamp leg is refused with nothing paid.
    let forged = runner.act(GameAction::SelectCards { cards: vec![tomb] });
    assert!(forged.is_err(), "the Tomb is not a legal Swamp-leg choice");
    assert_eq!(runner.state().objects[&tomb].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&basic_swamp].zone, Zone::Battlefield);

    runner
        .act(GameAction::SelectCards {
            cards: vec![basic_swamp],
        })
        .expect("the basic Swamp pays the Swamp leg");
    assert_eq!(sacrifice_choices(&runner), vec![tomb]);
    runner
        .act(GameAction::SelectCards { cards: vec![tomb] })
        .expect("the Tomb pays the Forest leg");

    for _ in 0..5 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    let state = runner.state();
    assert_eq!(state.objects[&basic_swamp].zone, Zone::Graveyard);
    assert_eq!(state.objects[&tomb].zone, Zone::Graveyard);
    assert_eq!(
        state.objects[&jarad].zone,
        Zone::Hand,
        "the ability resolves and returns Jarad"
    );
}

struct NightmareBoard {
    runner: GameRunner,
    nightmare: ObjectId,
    bear: Option<ObjectId>,
    ability: usize,
}

/// The creatures beside the animated Recurring Nightmare.
#[derive(Clone, Copy)]
enum NightmareFodder {
    /// The Nightmare is P0's only creature.
    NightmareOnly,
    /// A Grizzly Bears is also on the battlefield.
    Bear,
}

/// Recurring Nightmare turned into a creature (an Opalescence-style board) is both a
/// creature it could sacrifice and the permanent its other leg returns to hand.
fn nightmare_board(fodder: NightmareFodder) -> NightmareBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nightmare = scenario
        .add_creature(P0, "Recurring Nightmare", 3, 3)
        .from_oracle_text(RECURRING_NIGHTMARE)
        .id();
    let bear = match fodder {
        NightmareFodder::NightmareOnly => None,
        NightmareFodder::Bear => Some(scenario.add_creature(P0, "Grizzly Bears", 2, 2).id()),
    };
    scenario.add_creature_to_graveyard(P0, "Gravedigger", 2, 2);
    let mut runner = scenario.build();
    let obj = runner.state_mut().objects.get_mut(&nightmare).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.base_card_types.core_types.push(CoreType::Enchantment);
    let ability = 0;
    let cost = runner.state().objects[&nightmare].abilities[ability]
        .cost
        .clone();
    let (sacrifice, bounce) = match &cost {
        Some(AbilityCost::Composite { costs }) => (
            costs.iter().any(|c| matches!(c, AbilityCost::Sacrifice(_))),
            costs
                .iter()
                .any(|c| matches!(c, AbilityCost::ReturnToHand { .. })),
        ),
        _ => (false, false),
    };
    assert!(
        sacrifice && bounce,
        "reach: Recurring Nightmare's cost is Composite[Sacrifice, ReturnToHand], got {cost:?}"
    );
    NightmareBoard {
        runner,
        nightmare,
        bear,
        ability,
    }
}

#[test]
fn recurring_nightmare_creature_cant_pay_both_legs_alone() {
    let board = nightmare_board(NightmareFodder::Bear);
    assert!(
        can_activate_ability_now(board.runner.state(), P0, board.nightmare, board.ability),
        "reach: with another creature to sacrifice the ability is activatable"
    );
    let board = nightmare_board(NightmareFodder::NightmareOnly);
    assert!(
        !can_activate_ability_now(board.runner.state(), P0, board.nightmare, board.ability),
        "the Nightmare can't be both the sacrificed creature and the returned enchantment"
    );
}

#[test]
fn recurring_nightmare_sacrifice_leg_excludes_the_returned_permanent() {
    let NightmareBoard {
        mut runner,
        nightmare,
        bear,
        ability,
    } = nightmare_board(NightmareFodder::Bear);
    let bear = bear.unwrap();
    runner
        .act(GameAction::ActivateAbility {
            source_id: nightmare,
            ability_index: ability,
        })
        .expect("activation starts");
    // The single graveyard creature card may need an explicit target choice.
    if !matches!(runner.state().waiting_for, WaitingFor::PayCost { .. }) {
        let target = runner
            .state()
            .objects
            .values()
            .find(|o| o.zone == Zone::Graveyard && o.name == "Gravedigger")
            .map(|o| o.id)
            .expect("target card");
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            })
            .expect("target chosen");
    }
    assert_eq!(
        sacrifice_choices(&runner),
        vec![bear],
        "the Nightmare must stay on the battlefield to be returned to hand"
    );
    let forged = runner.act(GameAction::SelectCards {
        cards: vec![nightmare],
    });
    assert!(
        forged.is_err(),
        "sacrificing the Nightmare itself is refused"
    );
    assert_eq!(runner.state().objects[&nightmare].zone, Zone::Battlefield);
}

// --- Review probe board (r1, R1): a multi-object leg that overlaps another leg. ---
//
// Synthetic typed cost, not a printed card: "Sacrifice two Swamps and a Forest".
// With two Overgrown Tombs and one basic Swamp a joint assignment exists (basic
// Swamp + Tomb A for the Swamp leg, Tomb B for the Forest leg), but a player who
// picks both Tombs for the Swamp leg strands the Forest leg. Narrowing the
// prompt one object at a time can't prevent that set, so the shape fails closed
// at activation.

fn land_with_subtype(subtype: &str) -> TargetFilter {
    TypedFilter::land()
        .subtype(subtype.to_string())
        .controller(ControllerRef::You)
        .into()
}

fn two_swamps_and_a_forest() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Sacrifice(SacrificeCost::count(land_with_subtype("Swamp"), 2)),
            AbilityCost::Sacrifice(SacrificeCost::count(land_with_subtype("Forest"), 1)),
        ],
    })
}

/// P0's lands on the probe board.
#[derive(Clone, Copy)]
enum ProbeLands {
    /// Two Overgrown Tombs and one basic Swamp (the board under review).
    TwoTombsAndBasicSwamp,
    /// Two basic Swamps and one basic Forest: no leg overlaps (reach control).
    BasicSwampsAndForest,
}

/// Whether P1's graveyard replacements are on the battlefield.
#[derive(Clone, Copy)]
enum ProbeReplacements {
    /// Rest in Peace, and Wheel of Sun and Moon enchanting P0, already settled
    /// (the board as written; the Wheel's replacement is not implemented by the
    /// engine, so only Rest in Peace's is live).
    RestInPeaceAndWheel,
    /// Supplementary, not the board: two Rest in Peace, so a sacrifice must
    /// pause for P0 to order the two replacements (CR 616.1).
    TwoRestInPeace,
    None,
}

struct ProbeBoard {
    runner: GameRunner,
    source: ObjectId,
    lands: Vec<ObjectId>,
}

fn probe_board(lands: ProbeLands, replacements: ProbeReplacements) -> ProbeBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Synthetic Swamp-and-Forest Altar", "")
        .with_ability_definition(two_swamps_and_a_forest())
        .id();
    let land_ids = match lands {
        ProbeLands::TwoTombsAndBasicSwamp => {
            let mut ids: Vec<ObjectId> = (0..2)
                .map(|_| {
                    scenario
                        .add_land_from_oracle(P0, "Overgrown Tomb", OVERGROWN_TOMB)
                        .with_subtypes(vec!["Swamp", "Forest"])
                        .id()
                })
                .collect();
            ids.push(scenario.add_basic_land(P0, ManaColor::Black));
            ids
        }
        ProbeLands::BasicSwampsAndForest => vec![
            scenario.add_basic_land(P0, ManaColor::Black),
            scenario.add_basic_land(P0, ManaColor::Black),
            scenario.add_basic_land(P0, ManaColor::Green),
        ],
    };
    let (rest_in_peace_count, wheel) = match replacements {
        ProbeReplacements::RestInPeaceAndWheel => {
            scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
            let wheel = scenario
                .add_enchantment_from_oracle(P1, "Wheel of Sun and Moon", WHEEL_OF_SUN_AND_MOON)
                .with_subtypes(vec!["Aura"])
                .id();
            (1, Some(wheel))
        }
        ProbeReplacements::TwoRestInPeace => {
            for _ in 0..2 {
                scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
            }
            (2, None)
        }
        ProbeReplacements::None => (0, None),
    };
    let mut runner = scenario.build();
    if let Some(wheel) = wheel {
        runner
            .state_mut()
            .objects
            .get_mut(&wheel)
            .unwrap()
            .attached_to = Some(AttachTarget::Player(P0));
    }
    let live_rest_in_peace = runner
        .state()
        .battlefield
        .iter()
        .map(|id| &runner.state().objects[id])
        .filter(|obj| obj.name == "Rest in Peace" && !obj.replacement_definitions.is_empty())
        .count();
    assert_eq!(
        live_rest_in_peace, rest_in_peace_count,
        "reach: every Rest in Peace carries its graveyard replacement"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
            && runner.state().stack.is_empty(),
        "reach: P0 has priority with an empty stack"
    );
    ProbeBoard {
        runner,
        source,
        lands: land_ids,
    }
}

/// Answers every pending replacement-order choice with its first option and
/// returns how many it answered.
fn settle_replacement_choices(runner: &mut GameRunner) -> usize {
    let mut answered = 0;
    for _ in 0..10 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("replacement order chosen");
        answered += 1;
    }
    answered
}

#[test]
fn probe_multi_object_leg_overlapping_another_leg_is_refused_without_payment() {
    for replacements in [
        ProbeReplacements::RestInPeaceAndWheel,
        ProbeReplacements::TwoRestInPeace,
        ProbeReplacements::None,
    ] {
        let ProbeBoard {
            mut runner,
            source,
            lands,
        } = probe_board(ProbeLands::TwoTombsAndBasicSwamp, replacements);
        assert!(
            !can_activate_ability_now(runner.state(), P0, source, 0),
            "an overlapping two-object leg is not offered"
        );
        let activated = runner.act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        });
        assert!(
            activated.is_err(),
            "the activation is refused before any prompt, got {activated:?} / {:?}",
            runner.state().waiting_for
        );
        let state = runner.state();
        assert!(state.stack.is_empty());
        assert!(state.pending_cast.is_none(), "no payment is in flight");
        for land in lands {
            assert_eq!(
                state.objects[&land].zone,
                Zone::Battlefield,
                "no partial payment (CR 601.2h)"
            );
        }
    }
}

/// Reach control: the same synthetic cost over lands that no two legs share is
/// offered and paid in full, including through P1's graveyard replacements.
#[test]
fn probe_control_disjoint_lands_pay_through_the_replacements() {
    for replacements in [
        ProbeReplacements::RestInPeaceAndWheel,
        ProbeReplacements::TwoRestInPeace,
        ProbeReplacements::None,
    ] {
        let ProbeBoard {
            mut runner,
            source,
            lands,
        } = probe_board(ProbeLands::BasicSwampsAndForest, replacements);
        assert!(can_activate_ability_now(runner.state(), P0, source, 0));
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .expect("activation starts");
        let (swamps, forest) = (vec![lands[0], lands[1]], lands[2]);
        let mut first = sacrifice_choices(&runner);
        first.sort();
        let mut expected = swamps.clone();
        expected.sort();
        assert_eq!(first, expected, "the Swamp leg offers both basic Swamps");
        runner
            .act(GameAction::SelectCards { cards: swamps })
            .expect("the Swamps pay the Swamp leg");
        let mut paused = settle_replacement_choices(&mut runner);
        assert_eq!(sacrifice_choices(&runner), vec![forest]);
        runner
            .act(GameAction::SelectCards {
                cards: vec![forest],
            })
            .expect("the Forest pays the Forest leg");
        paused += settle_replacement_choices(&mut runner);
        if matches!(replacements, ProbeReplacements::TwoRestInPeace) {
            assert!(
                paused > 0,
                "reach: two Rest in Peace pause the sacrifice for replacement ordering"
            );
        }
        let state = runner.state();
        assert_eq!(state.stack.len(), 1, "the ability is on the stack");
        for land in lands {
            assert_ne!(state.objects[&land].zone, Zone::Battlefield);
        }
    }
}

// --- Review probe board (r2, F1): discard legs outside the printed-order sequence. ---
//
// Synthetic typed costs, not printed cards. Only direct `Fixed` chosen hand-discard
// siblings are covered by the printed-order discard check; a dynamic count or a
// nested leg that shares cards with another leg must fail closed, or a pick of both
// Islands for the first leg strands the Island leg after a replacement pause.

fn hand_discard(count: QuantityExpr, filter: Option<TargetFilter>) -> AbilityCost {
    AbilityCost::Discard {
        count,
        filter,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    }
}

fn card_with_subtype(subtype: &str) -> Option<TargetFilter> {
    Some(TypedFilter::default().subtype(subtype.to_string()).into())
}

fn lands_you_control() -> QuantityExpr {
    QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TypedFilter::land().controller(ControllerRef::You).into(),
        },
    }
}

fn fixed(value: i32) -> QuantityExpr {
    QuantityExpr::Fixed { value }
}

/// The synthetic discard costs on the probe board.
#[derive(Clone, Copy, Debug)]
enum DiscardProbeCost {
    /// The board: `[Discard (lands you control) cards, Discard an Island card]`.
    DynamicAnyThenIsland,
    /// Control: `[Discard two cards, Discard an Island card]` (flat, Fixed).
    FixedTwoAnyThenIsland,
    /// Hole (b): `[Discard two cards, Composite[Discard an Island card]]`.
    NestedIsland,
    /// Control: `[Discard two Forest cards, Discard an Island card]` (disjoint).
    DisjointForestsThenIsland,
    /// Control: `[Discard an Island card, Discard a card]` (flat, Fixed,
    /// printed-order safe; the exemption still admits it).
    IslandThenAny,
    /// Review r3 (F2): `[Composite[Discard an Island card], Discard a card]`.
    NestedIslandThenAny,
    /// Review r3 (F2) control: `[Discard a card, Composite[Discard an Island card]]`.
    AnyThenNestedIsland,
}

impl DiscardProbeCost {
    fn cost(self) -> AbilityCost {
        let island = || hand_discard(fixed(1), card_with_subtype("Island"));
        let costs = match self {
            Self::DynamicAnyThenIsland => vec![hand_discard(lands_you_control(), None), island()],
            Self::FixedTwoAnyThenIsland => vec![hand_discard(fixed(2), None), island()],
            Self::NestedIsland => vec![
                hand_discard(fixed(2), None),
                AbilityCost::Composite {
                    costs: vec![island()],
                },
            ],
            Self::DisjointForestsThenIsland => {
                vec![
                    hand_discard(fixed(2), card_with_subtype("Forest")),
                    island(),
                ]
            }
            Self::IslandThenAny => vec![island(), hand_discard(fixed(1), None)],
            Self::NestedIslandThenAny => vec![
                AbilityCost::Composite {
                    costs: vec![island()],
                },
                hand_discard(fixed(1), None),
            ],
            Self::AnyThenNestedIsland => vec![
                hand_discard(fixed(1), None),
                AbilityCost::Composite {
                    costs: vec![island()],
                },
            ],
        };
        AbilityCost::Composite { costs }
    }

    /// Subtypes of the hand cards.
    fn hand(self) -> &'static [&'static str] {
        match self {
            Self::DisjointForestsThenIsland => &["Forest", "Forest", "Island"],
            Self::DynamicAnyThenIsland
            | Self::FixedTwoAnyThenIsland
            | Self::NestedIsland
            | Self::IslandThenAny => &["Island", "Island", "Forest"],
            Self::NestedIslandThenAny | Self::AnyThenNestedIsland => &["Island", "Forest"],
        }
    }
}

struct DiscardProbe {
    runner: GameRunner,
    source: ObjectId,
    hand: Vec<ObjectId>,
}

fn discard_probe(cost: DiscardProbeCost, rest_in_peace: usize) -> DiscardProbe {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Synthetic Discard Altar", "")
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: fixed(1),
                    player: TargetFilter::Controller,
                },
            )
            .cost(cost.cost()),
        )
        .id();
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let hand: Vec<ObjectId> = cost
        .hand()
        .iter()
        .enumerate()
        .map(|(i, subtype)| scenario.add_card_to_hand(P0, &format!("{subtype} card {i}")))
        .collect();
    for _ in 0..rest_in_peace {
        scenario.add_enchantment_from_oracle(P1, "Rest in Peace", REST_IN_PEACE);
    }
    let mut runner = scenario.build();
    for (id, subtype) in hand.iter().zip(cost.hand().iter()) {
        let obj = runner.state_mut().objects.get_mut(id).unwrap();
        for types in [&mut obj.card_types, &mut obj.base_card_types] {
            types.core_types = vec![CoreType::Land];
            types.subtypes = vec![subtype.to_string()];
        }
    }
    let state = runner.state();
    let live = state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .filter(|obj| obj.name == "Rest in Peace" && !obj.replacement_definitions.is_empty())
        .count();
    assert_eq!(
        live, rest_in_peace,
        "reach: every Rest in Peace replacement is live"
    );
    assert_eq!(
        resolve_quantity(state, &lands_you_control(), P0, source),
        2,
        "reach: the dynamic count resolves to 2"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
            && state.stack.is_empty(),
        "reach: P0 has priority with an empty stack"
    );
    DiscardProbe {
        runner,
        source,
        hand,
    }
}

fn discard_choices(runner: &GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Discard,
            choices,
            ..
        } => choices.clone(),
        other => panic!("expected a discard cost prompt, got {other:?}"),
    }
}

#[test]
fn probe_overlapping_discard_outside_the_printed_order_sequence_is_refused_without_payment() {
    for cost in [
        DiscardProbeCost::DynamicAnyThenIsland,
        DiscardProbeCost::FixedTwoAnyThenIsland,
        DiscardProbeCost::NestedIsland,
    ] {
        for rest_in_peace in [2, 0] {
            let DiscardProbe {
                mut runner,
                source,
                hand,
            } = discard_probe(cost, rest_in_peace);
            let (island_a, island_b) = (hand[0], hand[1]);
            let activated = runner.act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            });
            if activated.is_ok() {
                // The board's actions, so a regression shows what it pays.
                let first = discard_choices(&runner);
                assert!(
                    first.contains(&island_a) && first.contains(&island_b),
                    "reach: the first prompt offers both Islands ({cost:?})"
                );
                let _ = runner.act(GameAction::SelectCards {
                    cards: vec![island_a, island_b],
                });
                for _ in 0..6 {
                    if matches!(
                        runner.state().waiting_for,
                        WaitingFor::ReplacementChoice { .. }
                    ) {
                        let _ = runner.act(GameAction::ChooseReplacement { index: 0 });
                    }
                }
            }
            let state = runner.state();
            for card in &hand {
                assert_eq!(
                    state.objects[card].zone,
                    Zone::Hand,
                    "no partial payment (CR 601.2h): {cost:?} with {rest_in_peace} Rest in Peace, \
                     activate {activated:?}, now {:?}",
                    state.waiting_for
                );
            }
            assert!(
                activated.is_err(),
                "{cost:?} is refused at activation, before any prompt"
            );
            assert!(state.stack.is_empty() && state.pending_cast.is_none());
        }
    }
}

/// Controls: a disjoint pair and a printed-order-safe pair are admitted and pay
/// in full through the Rest in Peace replacement pauses.
#[test]
fn probe_control_sequenced_and_disjoint_discards_pay_through_the_replacements() {
    for cost in [
        DiscardProbeCost::DisjointForestsThenIsland,
        DiscardProbeCost::IslandThenAny,
    ] {
        let DiscardProbe {
            mut runner,
            source,
            hand,
        } = discard_probe(cost, 2);
        assert!(can_activate_ability_now(runner.state(), P0, source, 0));
        runner
            .act(GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            })
            .expect("activation starts");
        let picks: Vec<Vec<ObjectId>> = match cost {
            DiscardProbeCost::DisjointForestsThenIsland => {
                vec![vec![hand[0], hand[1]], vec![hand[2]]]
            }
            DiscardProbeCost::IslandThenAny => vec![vec![hand[0]], vec![hand[2]]],
            DiscardProbeCost::DynamicAnyThenIsland
            | DiscardProbeCost::FixedTwoAnyThenIsland
            | DiscardProbeCost::NestedIsland
            | DiscardProbeCost::NestedIslandThenAny
            | DiscardProbeCost::AnyThenNestedIsland => unreachable!("not in this control"),
        };
        let mut paused = 0;
        for pick in picks {
            for card in &pick {
                assert!(discard_choices(&runner).contains(card), "{cost:?}");
            }
            runner
                .act(GameAction::SelectCards { cards: pick })
                .expect("the pick pays its leg");
            paused += settle_replacement_choices(&mut runner);
        }
        assert!(
            paused > 0,
            "reach: two Rest in Peace pause the discards ({cost:?})"
        );
        let state = runner.state();
        assert_eq!(state.stack.len(), 1, "the ability is on the stack");
        let discarded = match cost {
            DiscardProbeCost::DisjointForestsThenIsland => vec![hand[0], hand[1], hand[2]],
            _ => vec![hand[0], hand[2]],
        };
        for card in discarded {
            assert_eq!(state.objects[&card].zone, Zone::Exile, "{cost:?}");
        }
    }
}

/// Review r3 probe board (F2): a nested one-card Island leg beside a flat one-card
/// leg. The discard payment path removes the first *direct* discard leg, not the
/// nested leg its prompt found, so a nested discard leg that competes with another
/// discard leg for a card is refused before anything is paid (fail closed). The flat
/// pair stays payable.
#[test]
fn probe_nested_discard_leg_overlapping_another_discard_is_refused_without_payment() {
    for cost in [
        DiscardProbeCost::NestedIslandThenAny,
        DiscardProbeCost::AnyThenNestedIsland,
    ] {
        let DiscardProbe {
            mut runner,
            source,
            hand,
        } = discard_probe(cost, 2);
        let (island, forest) = (hand[0], hand[1]);
        assert!(
            !can_activate_ability_now(runner.state(), P0, source, 0),
            "{cost:?} is not offered"
        );
        let activated = runner.act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        });
        assert!(activated.is_err(), "{cost:?} is refused at activation");
        let state = runner.state();
        assert_eq!(state.objects[&island].zone, Zone::Hand, "{cost:?}");
        assert_eq!(state.objects[&forest].zone, Zone::Hand, "{cost:?}");
        assert!(state.stack.is_empty() && state.pending_cast.is_none());
    }
}

/// Reach guard for the board: each leaf alone is payable, so the refusal comes
/// from the joint check, not from a leaf.
#[test]
fn probe_nested_discard_board_leaves_are_each_payable() {
    let DiscardProbe { runner, source, .. } =
        discard_probe(DiscardProbeCost::NestedIslandThenAny, 2);
    for leaf in [
        hand_discard(fixed(1), card_with_subtype("Island")),
        hand_discard(fixed(1), None),
    ] {
        assert!(
            leaf.is_payable_for_activation(runner.state(), P0, source, Some(0)),
            "reach: {leaf:?} alone is payable"
        );
    }
}
