//! Dack Fayden, Helping Hand — end to end (verbatim Oracle text):
//!
//! "When Dack Fayden enters, reveal cards from the top of your library until
//! you reveal X creature cards, where X is the number of opponents you have.
//! Put those creature cards onto the battlefield, then shuffle. They're goaded
//! for the rest of the game. For each of those permanents, choose a different
//! opponent. Each opponent gains control of the permanent for which they were
//! chosen."
//!
//! Derived reading, clause by clause:
//! 1. Reveal until X = #opponents creature cards are revealed (CR 701.20a,
//!    CR 608.2h); revealing does not move cards (CR 701.20b).
//! 2. The found creature cards (X, or fewer if the library runs out —
//!    CR 609.3) enter under Dack's controller (CR 110.2a); then the library is
//!    shuffled (CR 701.24a).
//! 3. "They" names exactly those permanents (CR 608.2c). Each is goaded by the
//!    ability's controller (CR 701.15b) for the rest of the game (the stated
//!    duration overrides CR 701.15a's default; CR 101.1, CR 611.2a).
//! 4. For each of those permanents, Dack's controller chooses an opponent not
//!    chosen for another of them (CR 608.2d), comparing only against this
//!    ability's own earlier choices (CR 608.2c); an entering creature's own "As
//!    ~ enters, choose a player" (CR 614.1c, CR 614.12a) is not one of them.
//! 5. Each chosen opponent gains control of exactly the permanent chosen for
//!    them, with no expiry (CR 611.1, CR 611.2a, CR 613.1b). All choices are
//!    made first; the control changes are then processed simultaneously in one
//!    action (CR 608.2c, CR 608.2f). No state-based action check, priority
//!    window or triggered-ability placement happens between the choices
//!    (CR 704.3, CR 117.5, CR 603.3). A "when you lose control" trigger looks
//!    back and is controlled by the player who controlled its source
//!    (CR 603.10d + CR 603.3a).
//!
//! Pairing rule: the opponent prompt does not name the permanent it is for, so
//! each row derives the pairing from the iteration's own population — the
//! i-th answer belongs to the i-th member of the published chain set, read at
//! the first prompt. That this order equals reveal order is recorded as an
//! observation, never asserted as a rule.
//!
//! Every library below is checked against the run's reveal-order constraint:
//! the only creature whose entry raises a choice (True-Name Nemesis) is always
//! the last kept creature, and no library holds a Devour creature. No found
//! creature has an enters trigger and no board holds an enters observer; the
//! only asserted trigger (Khârn's) comes from the final apply.

use crate::for_each_of_those_population::{
    finish_muster_library, goad_tces, stage_muster_library, zone_of, MusterCard, REVEAL_MUSTER,
};
use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::turns::{execute_cleanup, execute_untap, start_next_turn};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ChoiceAssignmentSide, ChoiceType, ChosenAttribute, Duration, Effect,
    EffectKind, PlayerChoiceDistinctness, PlayerFilter, QuantityExpr, QuantityRef, SubAbilityLink,
    TargetFilter, TargetSelectionMode,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolution::{ResolutionStateWire, RESOLUTION_STATE_WIRE_VERSION};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

const DACK_NAME: &str = "Dack Fayden, Helping Hand";

/// Dack Fayden, Helping Hand, verbatim Oracle text.
const DACK: &str = "When Dack Fayden enters, reveal cards from the top of your library until you \
     reveal X creature cards, where X is the number of opponents you have. Put those creature \
     cards onto the battlefield, then shuffle. They're goaded for the rest of the game. For each \
     of those permanents, choose a different opponent. Each opponent gains control of the \
     permanent for which they were chosen.";

/// True-Name Nemesis, verbatim Oracle text.
const TRUE_NAME_NEMESIS: &str =
    "As this creature enters, choose a player.\nThis creature has protection from the chosen player.";

/// Khârn the Betrayer, verbatim Oracle text.
const KHARN: &str = "Berzerker \u{2014} Khârn the Betrayer attacks or blocks each combat if \
     able.\nSigil of Corruption \u{2014} When you lose control of Khârn the Betrayer, draw two \
     cards.\nThe Betrayer \u{2014} If damage would be dealt to Khârn the Betrayer, prevent that \
     damage and an opponent of your choice gains control of it.";

/// Scrambleverse, verbatim Oracle text (scope fence).
const SCRAMBLEVERSE: &str = "For each nonland permanent, choose a player at random. Then each \
     player gains control of each permanent for which they were chosen. Untap those permanents.";

/// The standard Dack library, top to bottom: three kept creatures among two
/// misses, a matching creature beyond X = 3, and a final miss.
const DACK_LIBRARY: &[MusterCard] = &[
    MusterCard::Sorcery("Miss One"),
    MusterCard::Creature("Creature A"),
    MusterCard::Land("Miss Two"),
    MusterCard::Creature("Creature B"),
    MusterCard::Creature("Creature C"),
    MusterCard::Creature("Creature D"),
    MusterCard::Sorcery("Miss Three"),
];

fn nemesis() -> MusterCard {
    MusterCard::OracleCreature {
        name: "True-Name Nemesis",
        oracle: TRUE_NAME_NEMESIS,
        keywords: &[],
        power: 3,
        toughness: 1,
    }
}

fn kharn() -> MusterCard {
    MusterCard::OracleCreature {
        name: "Khârn the Betrayer",
        oracle: KHARN,
        keywords: &["Berzerker", "Sigil of Corruption", "The Betrayer"],
        power: 5,
        toughness: 1,
    }
}

struct DackBoard {
    runner: GameRunner,
    dack: ObjectId,
    library: Vec<(&'static str, ObjectId)>,
}

impl DackBoard {
    fn card(&self, name: &str) -> ObjectId {
        self.library
            .iter()
            .find(|(card, _)| *card == name)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no staged library card named {name}"))
    }
}

/// `players` players (P0 vs the rest); Dack in P0's hand at {0}; P0's library
/// staged from `library_top_first`.
fn dack_board(players: u8, library_top_first: &[MusterCard]) -> DackBoard {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 2026)
    };
    scenario.at_phase(Phase::PreCombatMain);
    let dack = scenario
        .add_creature_to_hand(P0, DACK_NAME, 4, 6)
        .as_legendary()
        .with_subtypes(vec!["Human", "Advisor"])
        .with_mana_cost(ManaCost::generic(0))
        .from_oracle_text(DACK)
        .id();
    let library = stage_muster_library(&mut scenario, P0, library_top_first);
    let mut runner = scenario.build();
    finish_muster_library(&mut runner, P0, library_top_first, &library);
    DackBoard {
        runner,
        dack,
        library,
    }
}

/// A pending `NamedChoice` prompt.
struct Prompt {
    player: PlayerId,
    choice_type: ChoiceType,
    options: Vec<String>,
    bound: Option<ObjectId>,
}

fn prompt(runner: &GameRunner) -> Option<Prompt> {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            source,
            ..
        } => Some(Prompt {
            player: *player,
            choice_type: choice_type.clone(),
            options: sorted(options.clone()),
            bound: source
                .as_ref()
                .map(|source| source.prompt.identity.reference.object_id),
        }),
        _ => None,
    }
}

fn sorted(mut options: Vec<String>) -> Vec<String> {
    options.sort();
    options
}

fn option_set(players: &[PlayerId]) -> Vec<String> {
    sorted(players.iter().map(|p| p.0.to_string()).collect())
}

/// Answers the pending prompt with `player`; returns the apply's events.
fn answer(runner: &mut GameRunner, player: PlayerId) -> Vec<GameEvent> {
    runner
        .act(GameAction::ChooseOption {
            choice: player.0.to_string(),
        })
        .expect("answering a legal player must succeed")
        .events
}

/// Reach-guard: the pending prompt is Dack's controller's opponent choice.
fn expect_opponent_prompt(runner: &GameRunner, ordinal: usize) -> Prompt {
    let p = prompt(runner).unwrap_or_else(|| {
        panic!(
            "reach-guard: prompt {ordinal} must be a NamedChoice, got {:?}",
            runner.state().waiting_for
        )
    });
    assert_eq!(p.player, P0, "reach-guard: prompt {ordinal} is P0's choice");
    assert!(
        matches!(p.choice_type, ChoiceType::Opponent { .. }),
        "reach-guard: prompt {ordinal} is an opponent choice, got {:?}",
        p.choice_type
    );
    p
}

/// Walks Dack's opponent prompts, answering them in order; returns each
/// prompt's option set.
fn answer_opponent_prompts(runner: &mut GameRunner, answers: &[PlayerId]) -> Vec<Vec<String>> {
    let mut seen = Vec::new();
    for (index, pick) in answers.iter().enumerate() {
        let p = expect_opponent_prompt(runner, index + 1);
        seen.push(p.options);
        answer(runner, *pick);
    }
    assert!(
        prompt(runner).is_none(),
        "exactly {} opponent prompts, got a further {:?}",
        answers.len(),
        runner.state().waiting_for
    );
    seen
}

/// The population the iteration runs over: the published chain tracked set,
/// read while the resolution is paused on a prompt. The i-th answer goes to
/// its i-th member (pairing rule).
fn iterated_population(runner: &GameRunner) -> Vec<ObjectId> {
    let state = runner.state();
    let id = state
        .chain_tracked_set_id
        .expect("reach-guard: the reveal published the chain tracked set");
    state.tracked_object_sets[&id].clone()
}

fn controller(runner: &GameRunner, id: ObjectId) -> PlayerId {
    runner.state().objects[&id].controller
}

fn in_p0_library(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .library
        .contains(&id)
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn refresh_layers(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.layers_dirty.mark_full();
    evaluate_layers(state);
}

/// `(object, old, new)` of every control-change event.
fn control_changes(events: &[GameEvent]) -> Vec<(ObjectId, PlayerId, PlayerId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ControllerChanged {
                object_id,
                old_controller,
                new_controller,
            } => Some((*object_id, *old_controller, *new_controller)),
            _ => None,
        })
        .collect()
}

/// The sources of the `EffectResolved{GiveControl}` events.
fn handoff_resolutions(events: &[GameEvent]) -> Vec<ObjectId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::EffectResolved {
                kind: EffectKind::GiveControl,
                source_id,
                ..
            } => Some(*source_id),
            _ => None,
        })
        .collect()
}

// --- parse rows (A3.2) ------------------------------------------------------------

fn parsed_trigger_chain(oracle: &str) -> AbilityDefinition {
    let parsed = parse_oracle_text(
        oracle,
        DACK_NAME,
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Advisor".to_string()],
    );
    assert_eq!(parsed.triggers.len(), 1, "{:?}", parsed.triggers);
    *parsed.triggers[0]
        .execute
        .clone()
        .expect("the enters trigger has a body")
}

fn sorcery_chain(oracle: &str) -> AbilityDefinition {
    parse_oracle_text(oracle, "Instrument", &[], &["Sorcery".to_string()], &[])
        .abilities
        .into_iter()
        .next()
        .expect("one spell ability")
}

fn chain_nodes(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut nodes = vec![def];
    let mut cursor = def.sub_ability.as_deref();
    while let Some(node) = cursor {
        nodes.push(node);
        cursor = node.sub_ability.as_deref();
    }
    nodes
}

fn last_node(def: &AbilityDefinition) -> &AbilityDefinition {
    chain_nodes(def).last().copied().expect("non-empty chain")
}

/// The absorbed handoff: a `SequentialSibling` `GiveControl` of every object of
/// the resolving ability's assignment to the player chosen for it.
fn is_assignment_handoff(node: &AbilityDefinition) -> bool {
    node.sub_link == SubAbilityLink::SequentialSibling
        && *node.effect
            == Effect::GiveControl {
                target: TargetFilter::ChoiceAssignment {
                    side: ChoiceAssignmentSide::Objects,
                },
                recipient: TargetFilter::ChoiceAssignment {
                    side: ChoiceAssignmentSide::PlayerForObject,
                },
            }
}

/// The admission refusal: the grammar after a choice that records no
/// assignment.
fn is_assignment_gap(node: &AbilityDefinition) -> bool {
    matches!(
        &*node.effect,
        Effect::Unimplemented { name, .. } if name == "choice_assignment_antecedent"
    )
}

fn has_handoff(def: &AbilityDefinition) -> bool {
    chain_nodes(def).into_iter().any(is_assignment_handoff)
}

fn has_gap(def: &AbilityDefinition) -> bool {
    chain_nodes(def).into_iter().any(is_assignment_gap)
}

fn tracked_set_repeat() -> Option<QuantityExpr> {
    Some(QuantityExpr::Ref {
        qty: QuantityRef::TrackedSetSize,
    })
}

/// `REVEAL_MUSTER` with its for-each body replaced (labelled synthetic
/// instrument, not a card).
fn muster_body(body: &str) -> String {
    REVEAL_MUSTER.replace("put a +1/+1 counter on that creature.", body)
}

/// A3.2 (U5c + full card, SHAPE): CR 608.2c + CR 608.2d + CR 608.2f +
/// CR 611.2a — Dack's trigger chain is reveal → shuffle → plural goad → a
/// distinct opponent choice repeated over the reveal's kept set → the
/// assignment handoff, linked as a following sibling of the repetition (it
/// runs once, after the last choice), with no player-scope fan-out and no gap.
#[test]
fn dack_parses_to_the_assignment_handoff_chain() {
    let def = parsed_trigger_chain(DACK);
    let nodes = chain_nodes(&def);
    assert_eq!(nodes.len(), 5, "{nodes:#?}");
    assert!(
        matches!(
            &*nodes[0].effect,
            Effect::RevealUntil {
                kept_destination: Zone::Battlefield,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount { .. }
                },
                ..
            }
        ),
        "{:?}",
        nodes[0].effect
    );
    assert!(matches!(&*nodes[1].effect, Effect::Shuffle { .. }));
    assert!(
        matches!(
            &*nodes[2].effect,
            Effect::GenericEffect {
                static_abilities,
                duration: Some(Duration::Permanent),
                target: None,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].affected == Some(TargetFilter::ParentTarget)
        ),
        "{:?}",
        nodes[2].effect
    );
    assert_eq!(nodes[3].repeat_for, tracked_set_repeat());
    assert_eq!(
        *nodes[3].effect,
        Effect::Choose {
            choice_type: ChoiceType::Opponent {
                restriction: None,
                distinctness: PlayerChoiceDistinctness::DistinctFromPriorChoices,
            },
            persist: false,
            selection: TargetSelectionMode::Chosen,
        }
    );
    assert!(
        is_assignment_handoff(nodes[4]),
        "the handoff is the repetition's following sibling: {:?}",
        nodes[4]
    );
    assert!(
        nodes.iter().all(|node| node.player_scope.is_none()),
        "no player-scope fan-out"
    );
    assert!(
        !nodes
            .iter()
            .any(|node| matches!(*node.effect, Effect::Unimplemented { .. })),
        "zero Unimplemented"
    );

    // Reach-guard: the same grammar after the building-block instrument is
    // absorbed into the same tail.
    let instrument = sorcery_chain(&muster_body(
        "choose a different opponent. Each opponent gains control of the permanent for which \
         they were chosen.",
    ));
    let last = last_node(&instrument);
    assert!(is_assignment_handoff(last), "{last:?}");
}

/// A3.2 class boundary (U5c): Scrambleverse keeps its prior parse (fence); the
/// plural "of each permanent" form keeps its prior parse while the singular
/// one after the same random `ObjectCount` choice is refused by the antecedent
/// admission, as are a non-repeated choice and a random per-object choice; a
/// sentence after a non-choice instruction and a subject that disagrees with
/// the choice keep their prior parse. Every synthetic text is labelled; every
/// negative is paired with a positive reach-guard.
#[test]
fn choice_assignment_handoff_class_boundary() {
    // Fence: Scrambleverse verbatim keeps its PHASE_BASE parse.
    let scramble = sorcery_chain(SCRAMBLEVERSE);
    let nodes = chain_nodes(&scramble);
    assert!(
        matches!(
            &*nodes[0].effect,
            Effect::Choose {
                choice_type: ChoiceType::Player { .. },
                selection: TargetSelectionMode::Random,
                ..
            }
        ) && matches!(
            nodes[0].repeat_for,
            Some(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { .. }
            })
        ),
        "{:?}",
        nodes[0]
    );
    assert!(matches!(&*nodes[1].effect, Effect::GainControlAll { .. }));
    assert!(matches!(&*nodes[2].effect, Effect::SetTapState { .. }));
    assert!(!has_handoff(&scramble) && !has_gap(&scramble));

    // Synthetic grammar pair (labelled, not a card).
    let each = "For each nonland permanent, choose a player at random. Each player gains control \
                of each permanent for which they were chosen.";
    let each_def = sorcery_chain(each);
    assert!(
        matches!(
            &*chain_nodes(&each_def)[1].effect,
            Effect::GainControlAll { .. }
        ),
        "the plural form keeps its prior parse"
    );
    assert!(!has_handoff(&each_def) && !has_gap(&each_def));
    let the_def = sorcery_chain(&each.replace("of each permanent", "of the permanent"));
    assert!(
        is_assignment_gap(last_node(&the_def)),
        "CR 608.2c: a random ObjectCount choice records no assignment: {:?}",
        last_node(&the_def)
    );

    // Admission: a non-repeated choice and a random per-object choice record no
    // assignment (synthetic, labelled).
    let single = sorcery_chain(
        "Choose an opponent. Each opponent gains control of the permanent for which they were \
         chosen.",
    );
    assert!(is_assignment_gap(last_node(&single)), "{single:?}");
    let random = sorcery_chain(&muster_body(
        "choose an opponent at random. Each opponent gains control of the permanent for which \
         they were chosen.",
    ));
    let random_nodes = chain_nodes(&random);
    assert!(
        matches!(
            &*random_nodes[3].effect,
            Effect::Choose {
                selection: TargetSelectionMode::Random,
                ..
            }
        ) && random_nodes[3].repeat_for == tracked_set_repeat(),
        "reach-guard: the random choice is repeated over the tracked set"
    );
    assert!(is_assignment_gap(last_node(&random)), "{random:?}");

    // Reach-guard: an interactive per-object choice with an agreeing subject is
    // absorbed (opponent/opponent and player/player).
    let player_choice = |subject: &str| {
        muster_body(&format!(
            "choose a different player. {subject} gains control of the permanent for which they \
             were chosen."
        ))
    };
    let agreeing = sorcery_chain(&player_choice("Each player"));
    assert!(is_assignment_handoff(last_node(&agreeing)), "{agreeing:?}");

    // A sentence after a non-choice instruction keeps its prior parse.
    let after_counter = sorcery_chain(&muster_body(
        "put a +1/+1 counter on that creature. Each opponent gains control of the permanent for \
         which they were chosen.",
    ));
    assert!(
        chain_nodes(&after_counter)
            .iter()
            .any(|node| matches!(&*node.effect, Effect::PutCounter { .. })),
        "reach-guard: the counter body parsed"
    );
    let tail = last_node(&after_counter);
    assert!(
        matches!(&*tail.effect, Effect::GainControl { .. })
            && tail.player_scope == Some(PlayerFilter::Opponent),
        "{tail:?}"
    );
    assert!(!has_handoff(&after_counter) && !has_gap(&after_counter));

    // A subject that disagrees with the choice keeps its prior parse.
    let mismatched = sorcery_chain(&player_choice("Each opponent"));
    let tail = last_node(&mismatched);
    assert!(
        matches!(&*tail.effect, Effect::GainControl { .. })
            && tail.player_scope == Some(PlayerFilter::Opponent),
        "{tail:?}"
    );
    assert!(!has_handoff(&mismatched) && !has_gap(&mismatched));
}

// --- runtime rows -------------------------------------------------------------------

/// Put the game in `active`'s declare-attackers step with the production
/// payload; returns the payload's attackers.
fn arm_combat(runner: &mut GameRunner, active: PlayerId) -> Vec<ObjectId> {
    let state = runner.state_mut();
    state.active_player = active;
    state.priority_player = active;
    state.phase = Phase::DeclareAttackers;
    state.layers_dirty.mark_full();
    evaluate_layers(state);
    state.waiting_for = build_declare_attackers_waiting_for(state);
    match &state.waiting_for {
        WaitingFor::DeclareAttackers {
            player,
            valid_attacker_ids,
            ..
        } => {
            assert_eq!(*player, active, "the payload belongs to {active:?}");
            valid_attacker_ids.clone()
        }
        other => panic!("expected DeclareAttackers, got {other:?}"),
    }
}

/// A declaration on a clone (a successful one advances past the step).
fn declare_on_clone(runner: &GameRunner, attacks: &[(ObjectId, PlayerId)]) -> Result<(), String> {
    let mut probe = GameRunner::from_state(runner.state().clone());
    let attacks: Vec<(ObjectId, AttackTarget)> = attacks
        .iter()
        .map(|&(id, p)| (id, AttackTarget::Player(p)))
        .collect();
    probe
        .declare_attackers(&attacks)
        .map(|_| ())
        .map_err(|e| format!("{e:?}"))
}

/// CR 701.15b + CR 508.1d: the goaded creature `id` controlled by P1 must
/// attack, and must attack a player other than its goader P0.
fn assert_goaded_by_p0(runner: &mut GameRunner, id: ObjectId, label: &str) {
    let attackers = arm_combat(runner, P1);
    assert!(
        attackers.contains(&id),
        "{label}: the creature can attack for P1"
    );
    assert!(
        declare_on_clone(runner, &[]).is_err(),
        "{label}: CR 508.1d — declaring no attack with the able goaded creature is refused"
    );
    assert!(
        declare_on_clone(runner, &[(id, P0)]).is_err(),
        "{label}: CR 701.15b — it must attack a player other than its goader P0"
    );
    declare_on_clone(runner, &[(id, P2)])
        .unwrap_or_else(|e| panic!("{label}: attacking P2 is allowed: {e}"));
}

/// A3.3 (U1 + U2, runtime; C3.3): CR 701.15b + CR 611.2a — each found
/// creature is goaded by Dack's controller for the rest of the game: its new
/// controller P1 must attack and may not attack P0, still after P0's next turn
/// has begun (the duration is not a turn boundary). Dack itself is not goaded.
#[test]
fn dack_goad_binds_the_new_controller_for_the_rest_of_the_game() {
    let mut board = dack_board(4, DACK_LIBRARY);
    board.runner.cast(board.dack).resolve();
    expect_opponent_prompt(&board.runner, 1);
    let population = iterated_population(&board.runner);
    assert_eq!(population.len(), 3, "{population:?}");
    answer_opponent_prompts(&mut board.runner, &[P2, P3, P1]);
    let received_by_p1 = population[2];

    // Reach-guard: the found creatures entered; misses and D stay in the library.
    for name in ["Creature A", "Creature B", "Creature C"] {
        assert_eq!(zone_of(&board.runner, board.card(name)), Zone::Battlefield);
    }
    for name in ["Miss One", "Miss Two", "Creature D", "Miss Three"] {
        let id = board.card(name);
        assert!(
            in_p0_library(&board.runner, id),
            "{name} stays in the library"
        );
        assert!(
            goad_tces(board.runner.state(), id).is_empty(),
            "{name} not goaded"
        );
    }
    assert!(
        goad_tces(board.runner.state(), board.dack).is_empty(),
        "CR 608.2c: Dack is not one of \"those permanents\""
    );
    // Owner P0 ≠ controller P1; the goader stays the ability's controller P0.
    assert_eq!(controller(&board.runner, received_by_p1), P1);
    assert_eq!(board.runner.state().objects[&received_by_p1].owner, P0);
    assert_eq!(
        goad_tces(board.runner.state(), received_by_p1),
        vec![(P0, Duration::Permanent)]
    );

    // P0's cleanup, then P1's turn begins (CR 302.6: P1 has controlled the
    // creature continuously since the turn began).
    let mut events = Vec::new();
    execute_cleanup(board.runner.state_mut(), &mut events);
    start_next_turn(board.runner.state_mut(), &mut events);
    refresh_layers(&mut board.runner);
    assert_eq!(board.runner.state().active_player, P1);
    assert_goaded_by_p0(&mut board.runner, received_by_p1, "P1's turn");

    // C3.3: P0's next turn begins (its untap step is where a goad lasting
    // "until your next turn" would end); the goad is still in force on P1's
    // next combat.
    {
        let state = board.runner.state_mut();
        state.active_player = P0;
        state.turn_number += 3;
    }
    execute_untap(board.runner.state_mut(), &mut events);
    refresh_layers(&mut board.runner);
    assert_eq!(
        goad_tces(board.runner.state(), received_by_p1),
        vec![(P0, Duration::Permanent)],
        "C3.3: no turn boundary ends the goad"
    );
    assert_goaded_by_p0(&mut board.runner, received_by_p1, "after P0's next untap");
}

/// A3.4 (U3 + U4 + U5; C3.1/C3.2/C3.8): CR 608.2c + CR 608.2d + CR 608.2f —
/// the prompts' option sets shrink and an already-chosen opponent is rejected;
/// while any choice is pending every found permanent is still P0's (no handoff
/// before the last answer); afterwards each opponent controls exactly the
/// permanent chosen for them, past end of turn.
#[test]
fn dack_assigns_each_permanent_to_a_different_opponent() {
    let mut board = dack_board(4, DACK_LIBRARY);
    board.runner.cast(board.dack).resolve();
    let a = board.card("Creature A");
    let b = board.card("Creature B");
    let c = board.card("Creature C");
    let found = [a, b, c];
    let all_p0 = |runner: &GameRunner, ordinal: usize| {
        for id in found {
            assert_eq!(zone_of(runner, id), Zone::Battlefield);
            assert_eq!(
                controller(runner, id),
                P0,
                "CR 608.2c: no permanent is handed off while prompt {ordinal} is pending"
            );
        }
    };

    let first = expect_opponent_prompt(&board.runner, 1);
    let population = iterated_population(&board.runner);
    assert_eq!(population, vec![a, b, c], "observation: reveal order");
    assert_eq!(first.options, option_set(&[P1, P2, P3]));
    all_p0(&board.runner, 1);
    answer(&mut board.runner, P2);
    let second = expect_opponent_prompt(&board.runner, 2);
    assert_eq!(second.options, option_set(&[P1, P3]));
    all_p0(&board.runner, 2);
    assert!(
        board
            .runner
            .act(GameAction::ChooseOption {
                choice: P2.0.to_string(),
            })
            .is_err(),
        "CR 608.2d: an already-chosen opponent is rejected"
    );
    answer(&mut board.runner, P3);
    let third = expect_opponent_prompt(&board.runner, 3);
    assert_eq!(third.options, option_set(&[P1]));
    all_p0(&board.runner, 3);
    answer(&mut board.runner, P1);
    assert!(prompt(&board.runner).is_none(), "exactly three prompts");

    let assert_pairing = |runner: &GameRunner| {
        for (member, recipient) in population.iter().zip([P2, P3, P1]) {
            assert_eq!(zone_of(runner, *member), Zone::Battlefield);
            assert_eq!(
                controller(runner, *member),
                recipient,
                "CR 611.2a: member ↔ its chosen opponent"
            );
        }
    };
    assert_pairing(&board.runner);
    assert_eq!(
        board
            .runner
            .state()
            .battlefield
            .iter()
            .filter(|id| population.contains(id) && controller(&board.runner, **id) == P0)
            .count(),
        0,
        "P0 controls none of them"
    );
    assert!(in_p0_library(&board.runner, board.card("Creature D")));

    // Control has no expiry.
    let mut events = Vec::new();
    execute_cleanup(board.runner.state_mut(), &mut events);
    refresh_layers(&mut board.runner);
    assert_pairing(&board.runner);
}

/// A3.4 hostile (True-Name Nemesis revealed LAST): CR 614.12a + CR 608.2c —
/// Nemesis's own "choose a player" is its linked choice, not one of Dack's: P1,
/// chosen for Nemesis, is still in Dack's first option set; every found
/// permanent stays P0's while Dack's choices are pending, and each recipient
/// receives exactly the permanent chosen for it.
#[test]
fn dack_with_true_name_nemesis_revealed_last() {
    let library = [
        MusterCard::Sorcery("Miss One"),
        MusterCard::Creature("Creature A"),
        MusterCard::Land("Miss Two"),
        MusterCard::Creature("Creature B"),
        nemesis(),
        MusterCard::Creature("Creature D"),
        MusterCard::Sorcery("Miss Three"),
    ];
    let mut board = dack_board(4, &library);
    let nemesis_id = board.card("True-Name Nemesis");
    let a = board.card("Creature A");
    let b = board.card("Creature B");
    board.runner.cast(board.dack).resolve();

    let foreign = prompt(&board.runner).expect("reach-guard: Nemesis's as-enters prompt");
    assert!(matches!(foreign.choice_type, ChoiceType::Player { .. }));
    assert_eq!(
        foreign.bound,
        Some(nemesis_id),
        "bound to the entering Nemesis"
    );
    answer(&mut board.runner, P1);

    let first = expect_opponent_prompt(&board.runner, 1);
    assert_eq!(
        first.options,
        option_set(&[P1, P2, P3]),
        "CR 614.12a: Nemesis's P1 is not one of Dack's prior choices"
    );
    let population = iterated_population(&board.runner);
    assert_eq!(
        population,
        vec![a, b, nemesis_id],
        "observation: reveal order"
    );
    for (ordinal, pick) in [(1, P2), (2, P3), (3, P1)] {
        expect_opponent_prompt(&board.runner, ordinal);
        for id in [a, b, nemesis_id] {
            assert_eq!(
                controller(&board.runner, id),
                P0,
                "prompt {ordinal}: nothing handed off yet"
            );
        }
        answer(&mut board.runner, pick);
    }
    assert!(
        prompt(&board.runner).is_none(),
        "exactly three Dack prompts"
    );

    // Reach-guard: every found creature, Nemesis included, entered, and
    // Nemesis persisted its own choice.
    for kept in [a, b, nemesis_id] {
        assert_eq!(zone_of(&board.runner, kept), Zone::Battlefield);
    }
    assert!(board.runner.state().objects[&nemesis_id]
        .chosen_attributes
        .contains(&ChosenAttribute::Player(P1)));
    for (member, recipient) in population.iter().zip([P2, P3, P1]) {
        assert_eq!(controller(&board.runner, *member), recipient);
    }
    assert!(in_p0_library(&board.runner, board.card("Creature D")));
    assert!(board.runner.state().resolution_stack.is_empty());
}

/// A3.5 (CR 609.3): fewer creature cards than opponents — one prompt per
/// found permanent, distinct opponents, and the remaining opponent gets
/// nothing; in a two-player game (X = 1) the single creature goes to the only
/// opponent.
#[test]
fn dack_with_fewer_creatures_than_opponents() {
    let library = [
        MusterCard::Sorcery("Miss One"),
        MusterCard::Creature("Creature A"),
        MusterCard::Land("Miss Two"),
        MusterCard::Creature("Creature B"),
        MusterCard::Sorcery("Miss Three"),
    ];
    let mut board = dack_board(4, &library);
    board.runner.cast(board.dack).resolve();
    let a = board.card("Creature A");
    let b = board.card("Creature B");
    expect_opponent_prompt(&board.runner, 1);
    assert_eq!(iterated_population(&board.runner), vec![a, b]);
    let seen = answer_opponent_prompts(&mut board.runner, &[P3, P1]);
    assert_eq!(seen, vec![option_set(&[P1, P2, P3]), option_set(&[P1, P2])]);
    for kept in [a, b] {
        assert_eq!(zone_of(&board.runner, kept), Zone::Battlefield);
    }
    assert_eq!(controller(&board.runner, a), P3);
    assert_eq!(controller(&board.runner, b), P1);
    assert!(
        board
            .runner
            .state()
            .battlefield
            .iter()
            .all(|id| controller(&board.runner, *id) != P2),
        "the third opponent controls none of them"
    );
    for name in ["Miss One", "Miss Two", "Miss Three"] {
        assert!(in_p0_library(&board.runner, board.card(name)));
    }

    // Two players: X = 1.
    let library = [
        MusterCard::Sorcery("Miss One"),
        MusterCard::Creature("Creature A"),
        MusterCard::Creature("Creature B"),
    ];
    let mut board = dack_board(2, &library);
    board.runner.cast(board.dack).resolve();
    let only = expect_opponent_prompt(&board.runner, 1);
    assert_eq!(only.options, option_set(&[P1]));
    answer(&mut board.runner, P1);
    assert!(prompt(&board.runner).is_none(), "exactly one prompt");
    assert_eq!(controller(&board.runner, board.card("Creature A")), P1);
    assert!(in_p0_library(&board.runner, board.card("Creature B")));
}

/// A3.5 zero-creature leg (CR 609.3; labelled base-green sibling): with no
/// creature card in the library there is no prompt and the resolution
/// completes. Reach-guard: Dack's trigger was on the stack and its reveal and
/// shuffle ran; no control changed.
#[test]
fn dack_with_no_creature_card_reveals_shuffles_and_prompts_nothing() {
    let library = [
        MusterCard::Sorcery("Miss One"),
        MusterCard::Land("Miss Two"),
        MusterCard::Sorcery("Miss Three"),
    ];
    let mut board = dack_board(4, &library);
    let library_ids: Vec<ObjectId> = board.library.iter().map(|(_, id)| *id).collect();
    drop(board.runner.cast(board.dack).commit());
    for _ in 0..8 {
        if zone_of(&board.runner, board.dack) == Zone::Battlefield {
            break;
        }
        board
            .runner
            .act(GameAction::PassPriority)
            .expect("passing priority resolves Dack");
    }
    assert_eq!(zone_of(&board.runner, board.dack), Zone::Battlefield);
    let top = board
        .runner
        .state()
        .stack
        .last()
        .cloned()
        .expect("reach-guard: Dack's trigger is on the stack");
    assert_eq!(top.source_id, board.dack);
    assert!(matches!(top.kind, StackEntryKind::TriggeredAbility { .. }));

    let mut events = Vec::new();
    for _ in 0..8 {
        if board.runner.state().stack.is_empty() {
            break;
        }
        assert!(prompt(&board.runner).is_none(), "no NamedChoice prompt");
        let result = board
            .runner
            .act(GameAction::PassPriority)
            .expect("passing priority resolves the trigger");
        events.extend(result.events);
    }
    assert!(board.runner.state().stack.is_empty());
    assert!(prompt(&board.runner).is_none());
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardsRevealed { player, card_ids, .. }
                if *player == P0 && library_ids.iter().all(|id| card_ids.contains(id))
        )),
        "reach-guard: the whole library was revealed: {events:?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        GameEvent::EffectResolved {
            kind: EffectKind::Shuffle,
            source_id,
            ..
        } if *source_id == board.dack
    )));
    assert!(control_changes(&events).is_empty(), "no control change");
    for id in library_ids {
        assert!(in_p0_library(&board.runner, id));
    }
}

/// A3.7(a) (C3.4, trigger placement): CR 603.3 + CR 603.3a + CR 603.10d —
/// Khârn, assigned in the first repetition, stays P0's until the last choice;
/// its "When you lose control" trigger is then put on the stack under P0 (who
/// controlled it) and draws P0 two cards.
#[test]
fn dack_control_change_trigger_waits_for_the_whole_resolution() {
    let library = [
        MusterCard::Sorcery("Miss One"),
        kharn(),
        MusterCard::Land("Miss Two"),
        MusterCard::Creature("Creature B"),
        MusterCard::Creature("Creature C"),
        MusterCard::Sorcery("Miss Three"),
        MusterCard::Sorcery("Miss Four"),
    ];
    let mut board = dack_board(4, &library);
    let kharn_id = board.card("Khârn the Betrayer");
    board.runner.cast(board.dack).resolve();
    let hand_before = hand_size(&board.runner, P0);

    expect_opponent_prompt(&board.runner, 1);
    let population = iterated_population(&board.runner);
    assert_eq!(
        population.first(),
        Some(&kharn_id),
        "pairing rule: Khârn is member 0"
    );
    answer(&mut board.runner, P1);
    for (ordinal, pick) in [(2, P2), (3, P3)] {
        // Reach-guard: two prompts follow Khârn's choice.
        expect_opponent_prompt(&board.runner, ordinal);
        assert_eq!(
            controller(&board.runner, kharn_id),
            P0,
            "prompt {ordinal}: Khârn is not handed off before the last choice"
        );
        assert!(
            board
                .runner
                .state()
                .stack
                .iter()
                .all(|entry| entry.source_id != kharn_id),
            "CR 603.3: no Khârn trigger mid-resolution"
        );
        assert_eq!(hand_size(&board.runner, P0), hand_before);
        answer(&mut board.runner, pick);
    }
    assert!(prompt(&board.runner).is_none());
    assert_eq!(controller(&board.runner, kharn_id), P1);

    let entry = board
        .runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == kharn_id)
        .cloned()
        .expect("Khârn's trigger is placed after the resolution");
    assert_eq!(
        entry.controller, P0,
        "CR 603.3a + CR 603.10d: the player who controlled Khârn"
    );
    board.runner.resolve_top();
    assert_eq!(
        hand_size(&board.runner, P0),
        hand_before + 2,
        "Sigil of Corruption draws P0 two cards"
    );
}

/// A3.7(b) (C3.4, no SBA mid-resolution): CR 704.3 + CR 704.5j — two
/// same-named legendary creatures both enter under P0 and stay P0's at every
/// prompt with no legend-rule choice raised; after the last choice they are
/// controlled by two distinct opponents and both survive.
#[test]
fn dack_two_same_named_legends_survive_the_resolution() {
    let library = [
        MusterCard::Legendary("Isamaru, Hound of Konda"),
        MusterCard::Legendary("Isamaru, Hound of Konda"),
        MusterCard::Creature("Creature C"),
        MusterCard::Sorcery("Miss One"),
    ];
    let mut board = dack_board(4, &library);
    let first = board.library[0].1;
    let second = board.library[1].1;
    board.runner.cast(board.dack).resolve();
    for (ordinal, pick) in [(1, P1), (2, P2), (3, P3)] {
        expect_opponent_prompt(&board.runner, ordinal);
        for isamaru in [first, second] {
            assert_eq!(zone_of(&board.runner, isamaru), Zone::Battlefield);
            assert_eq!(
                controller(&board.runner, isamaru),
                P0,
                "prompt {ordinal}: both copies are P0's"
            );
        }
        answer(&mut board.runner, pick);
    }
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "no legend-rule choice: {:?}",
        board.runner.state().waiting_for
    );
    for isamaru in [first, second] {
        assert_eq!(zone_of(&board.runner, isamaru), Zone::Battlefield);
    }
    assert_ne!(
        controller(&board.runner, first),
        controller(&board.runner, second)
    );
    assert!([P1, P2, P3].contains(&controller(&board.runner, first)));
    assert!([P1, P2, P3].contains(&controller(&board.runner, second)));
}

/// A3.7(c) + C3.4 event legs: CR 608.2c + CR 608.2f + CR 117.5 — after each
/// answer but the last the next decision is the next opponent prompt (never
/// priority) and the stack gains no entry; no control change and no handoff
/// resolution is emitted before the last answer, whose apply carries all three
/// control changes and the one handoff resolution.
#[test]
fn dack_repetitions_open_no_priority_window_and_hand_off_once() {
    let mut board = dack_board(4, DACK_LIBRARY);
    let cast_events = board.runner.cast(board.dack).resolve().events().to_vec();
    assert!(control_changes(&cast_events).is_empty());
    assert!(handoff_resolutions(&cast_events).is_empty());
    expect_opponent_prompt(&board.runner, 1);
    let population = iterated_population(&board.runner);
    let stack_len = board.runner.state().stack.len();
    let mut remaining = vec![P1, P2, P3];
    let mut last_events = Vec::new();
    for (ordinal, pick) in [(1, P3), (2, P1), (3, P2)] {
        let p = expect_opponent_prompt(&board.runner, ordinal);
        assert_eq!(p.options, option_set(&remaining));
        assert_eq!(board.runner.state().stack.len(), stack_len);
        let events = answer(&mut board.runner, pick);
        remaining.retain(|player| *player != pick);
        if ordinal < 3 {
            assert!(
                matches!(
                    board.runner.state().waiting_for,
                    WaitingFor::NamedChoice { .. }
                ),
                "no priority between repetitions: {:?}",
                board.runner.state().waiting_for
            );
            assert!(
                control_changes(&events).is_empty(),
                "answer {ordinal}: no control change yet: {events:?}"
            );
            assert!(
                handoff_resolutions(&events).is_empty(),
                "answer {ordinal}: no handoff yet"
            );
        } else {
            last_events = events;
        }
    }
    assert!(remaining.is_empty());
    let mut changes = control_changes(&last_events);
    changes.sort();
    let mut expected: Vec<_> = population
        .iter()
        .zip([P3, P1, P2])
        .map(|(member, recipient)| (*member, P0, recipient))
        .collect();
    expected.sort();
    assert_eq!(changes, expected, "CR 608.2f: every handoff in one apply");
    assert_eq!(handoff_resolutions(&last_events), vec![board.dack]);
    // Reach-guard: the kept creatures entered and were handed off.
    for name in ["Creature A", "Creature B", "Creature C"] {
        let id = board.card(name);
        assert_eq!(zone_of(&board.runner, id), Zone::Battlefield);
        assert_ne!(controller(&board.runner, id), P0);
    }
}

/// A3.10 Dack completion leg: a `GameState` serde round trip and a
/// resolution-state wire round trip, each taken while prompt 2 is pending with
/// one entry recorded, keep the assignment; the resolution completed after the
/// round trip hands each permanent to exactly its chosen opponent.
#[test]
fn dack_assignment_survives_a_mid_resolution_round_trip() {
    for wire in [false, true] {
        let mut board = dack_board(4, DACK_LIBRARY);
        board.runner.cast(board.dack).resolve();
        expect_opponent_prompt(&board.runner, 1);
        let population = iterated_population(&board.runner);
        answer(&mut board.runner, P2);
        expect_opponent_prompt(&board.runner, 2);

        let restored = if wire {
            let value = serde_json::to_value(ResolutionStateWire::from_game_state(
                board.runner.state().clone(),
            ))
            .expect("paused resolution serializes through the current wire");
            assert_eq!(
                value["resolution_state_version"],
                RESOLUTION_STATE_WIRE_VERSION
            );
            serde_json::from_value::<ResolutionStateWire>(value)
                .expect("paused resolution round-trips")
                .into_game_state()
        } else {
            let value = serde_json::to_value(board.runner.state()).expect("state serializes");
            serde_json::from_value::<GameState>(value).expect("state deserializes")
        };
        *board.runner.state_mut() = restored;

        // Reach-guard: the restored state is at prompt 2, nothing handed off.
        let second = expect_opponent_prompt(&board.runner, 2);
        assert_eq!(second.options, option_set(&[P1, P3]), "wire: {wire}");
        for id in &population {
            assert_eq!(controller(&board.runner, *id), P0, "wire: {wire}");
        }
        answer(&mut board.runner, P3);
        expect_opponent_prompt(&board.runner, 3);
        answer(&mut board.runner, P1);
        assert!(prompt(&board.runner).is_none());
        for (member, recipient) in population.iter().zip([P2, P3, P1]) {
            assert_eq!(
                controller(&board.runner, *member),
                recipient,
                "wire: {wire}: the entry recorded before the round trip is handed off"
            );
        }
    }
}
