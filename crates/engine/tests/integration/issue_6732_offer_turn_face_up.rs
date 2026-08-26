//! CR 116.2b + CR 702.37e: the turn-face-up special action must be OFFERED, not
//! merely accepted (#6732, #4381).
//!
//! The engine implemented the action and its Priority preflight counted it as
//! progress, but `ai_support::candidates::priority_actions_with_probe` — the
//! list the client renders — never emitted it. Nothing could send an action the
//! engine never advertised, so the whole morph / megamorph / disguise / manifest
//! / cloak class was unturnable in play. #7342 wired the client's dispatch and
//! closed both reports; its test supplies the action to itself, so it proves the
//! client's half and cannot observe the engine's.
//!
//! Reported from a real game state: the controller had priority, a face-down
//! Coral Trickster (morph `{U}`) and thirty untapped Islands, and `legalActions`
//! held six casts, four land plays and a pass.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    ReplacementDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{
    ManaAbilityResume, PendingCostMoveResume, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};

fn morph_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 0,
    }
}

fn pool(kinds: &[ManaType]) -> Vec<ManaUnit> {
    kinds
        .iter()
        .copied()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

/// A face-down permanent with a morph cost, put onto the battlefield through the
/// engine's own face-down play so `back_face` carries the real card.
fn face_down_morph_board(controller: PlayerId, mana: &[ManaType]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_to_hand(controller, "Coral Trickster", 2, 1)
        .with_keyword(Keyword::Morph(morph_cost()))
        .id();
    if !mana.is_empty() {
        scenario.with_mana_pool(controller, pool(mana));
    }
    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::morph::play_face_down(runner.state_mut(), controller, id, &mut events)
        .expect("the card is played face down");
    assert!(
        runner.state().objects[&id].face_down,
        "setup: the permanent is face down"
    );

    (runner, id)
}

fn offered_turn_face_ups(runner: &GameRunner) -> Vec<ObjectId> {
    engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .filter_map(|action| match action {
            GameAction::TurnFaceUp { object_id, .. } => Some(object_id),
            _ => None,
        })
        .collect()
}

/// The defect, end to end: the action is offered, and taking the offer flips the
/// permanent to its real face.
#[test]
fn a_face_down_morph_permanent_is_offered_and_flips() {
    let (mut runner, id) = face_down_morph_board(P0, &[ManaType::Blue]);

    assert_eq!(
        offered_turn_face_ups(&runner),
        vec![id],
        "CR 116.2b: the controller has priority and can pay {{U}}, so the special \
         action must be on the list the client renders"
    );

    runner
        .act(GameAction::TurnFaceUp {
            object_id: id,
            x: 0,
        })
        .expect("the offered action must be accepted");

    let obj = &runner.state().objects[&id];
    assert!(!obj.face_down, "CR 702.37e: the permanent is now face up");
    assert_eq!(obj.name, "Coral Trickster", "and shows its real face");
}

/// The affordability half of the same authority: an unpayable cost is not
/// offered, so the list never advertises an action the reducer would reject.
///
/// This is also what keeps the row above from passing for the wrong reason — if
/// the offer were unconditional, this row would fail.
#[test]
fn an_unpayable_turn_face_up_is_not_offered() {
    let (runner, _) = face_down_morph_board(P0, &[]);

    assert!(
        offered_turn_face_ups(&runner).is_empty(),
        "with no mana the morph cost cannot be paid, so nothing is offered"
    );
}

/// CR 702.37e: "you may turn a face-down permanent YOU CONTROL face up". The
/// offer is per-holder, and `legal_actions` speaks for the priority holder.
#[test]
fn an_opponents_face_down_permanent_is_not_offered() {
    let (mut runner, _) = face_down_morph_board(P1, &[ManaType::Blue]);
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    assert!(
        offered_turn_face_ups(&runner).is_empty(),
        "a face-down permanent an opponent controls is not this player's to turn up"
    );
}

// ── The paused mana-source payment (#4538's blocker) ────────────────────────

fn redirect_exile_to_graveyard() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Exile)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Graveyard,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        ))
}

const WARBREAK_TRUMPETER: &str = "Morph {X}{X}{R} (You may cast this card face down as a 2/2 \\
                                  creature for {3}. Turn it face up any time for its morph \\
                                  cost.)\nWhen this creature is turned face up, create X 1/1 red \\
                                  Goblin creature tokens.";

/// CR 605.3b + CR 616.1: the offer is only honest if the action can FINISH.
///
/// A mana source whose own cost exiles it, plus two exile→graveyard
/// replacements, forces a CR 616.1 ordering choice while the turn-face-up cost
/// is being auto-tapped. `casting.rs` deliberately reports such a source as
/// payable, so the offer above is right to include it — but the compatibility
/// wrapper `pay_special_action_mana_cost` converts the resulting `Paused` into
/// an error. That is why #4538 was asked to build a typed resume before the
/// action could be offered.
///
/// The permanent must still be face down while the choice is open, and must flip
/// exactly once the choice is answered.
#[test]
fn a_paused_mana_source_resumes_the_locked_turn_face_up() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let id = scenario
        .add_creature_to_hand_from_oracle(P0, "Warbreak Trumpeter", 1, 1, WARBREAK_TRUMPETER)
        .id();
    let source = scenario
        .add_creature(P0, "Self-Exiling Mana Source", 1, 1)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![ManaColor::Red],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Exile {
                        count: 1,
                        zone: None,
                        filter: Some(TargetFilter::SelfRef),
                    },
                ],
            }),
        )
        .id();
    for name in ["First Pause Replacement", "Second Pause Replacement"] {
        scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_replacement_definition(redirect_exile_to_graveyard());
    }
    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::morph::play_face_down(runner.state_mut(), P0, id, &mut events)
        .expect("the card is played face down");

    let paused = runner
        .act(GameAction::TurnFaceUp {
            object_id: id,
            x: 0,
        })
        .expect("the source's own cost pauses the payment rather than failing it");
    assert!(
        matches!(paused.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "the mana source's exile replacement owns the window, got {:?}",
        paused.waiting_for
    );
    assert!(
        matches!(
            runner.state().pending_cost_move_resume.as_ref(),
            Some(PendingCostMoveResume::ManaAbilityPayment { pending, .. })
                if matches!(
                    &pending.resume,
                    ManaAbilityResume::TurnFaceUp {
                        player,
                        object_id,
                        announced_x: Some(0),
                        ..
                    } if *player == P0 && *object_id == id
                )
        ),
        "the typed continuation names the action to finish"
    );
    assert!(
        runner.state().objects[&id].face_down,
        "nothing is committed while the payment is open"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("the replacement choice is answered");

    let obj = &runner.state().objects[&id];
    assert!(
        !obj.face_down,
        "CR 605.3b: the locked payment completed and the flip committed"
    );
    assert_eq!(obj.name, "Warbreak Trumpeter");
    let bound_x = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } if *source_id == id => Some(ability.chosen_x),
            _ => None,
        })
        .expect("the turned-face-up trigger must be on the stack after the paused payment");
    assert_eq!(
        bound_x,
        Some(0),
        "a paused X=0 payment must preserve the real zero announcement, not collapse it to no X"
    );
    assert_eq!(
        runner.state().objects[&source].zone,
        Zone::Graveyard,
        "the mana source's own cost still resolved through its replacement"
    );
}

// ── The interactive turn-up replacement (review round 2) ────────────────────

/// CR 614.1e + CR 708.11: "As ~ is turned face up, put five +1/+1 counters on
/// it" — the parsed Hooded Hydra class. Its `AddCounter`, modified by two
/// materially-ordered replacements, raises a CR 616.1 ordering prompt DURING
/// the flip. The completion must hand that prompt back, not overwrite it with
/// `Priority`.
const COUNTER_MORPH: &str = "Morph {1} (You may cast this card face down as a \
                             2/2 creature for {3}. Turn it face up any time for \
                             its morph cost.)\nAs this creature is turned face \
                             up, put five +1/+1 counters on it.\nWhen this \
                             creature is turned face up, draw a card.";

fn add_counter_modifier(
    scenario: &mut GameScenario,
    name: &str,
    modification: engine::types::ability::QuantityModification,
) {
    scenario
        .add_creature(P0, name, 0, 4)
        .with_replacement_definition(
            ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(modification)
                .counter_match(engine::types::counter::CounterMatch::OfType(
                    engine::types::counter::CounterType::Plus1Plus1,
                )),
        );
}

/// The review-round-2 blocker, fresh route: the flip commits, and the ordering
/// choice the execute raised stays live instead of being clobbered to
/// `Priority` (which stranded a live `pending_replacement` record and silently
/// dropped both modifiers).
///
/// Review round 3: one run per selectable order. CR 616.1 lets the affected
/// object's controller pick which applicable replacement applies first, so the
/// pick must be proven to determine the result — (5+1)*2 = 12 when the plus
/// applies first, 5*2+1 = 11 when the doubling applies first — and the prompt
/// must hold EXACTLY the two live modifiers, with no stale or extra candidates.
#[test]
fn an_interactive_turn_up_replacement_keeps_its_choice_live() {
    use engine::types::ability::QuantityModification;
    use engine::types::counter::CounterType;

    let run_with_first_choice = |first_choice: &str| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let id = scenario
            .add_creature_to_hand_from_oracle(P0, "Counter Morph", 2, 2, COUNTER_MORPH)
            .id();
        add_counter_modifier(
            &mut scenario,
            "Plus One Modifier",
            QuantityModification::Plus { value: 1 },
        );
        add_counter_modifier(
            &mut scenario,
            "Times Two Modifier",
            QuantityModification::Times { factor: 2 },
        );
        scenario.with_mana_pool(P0, pool(&[ManaType::Colorless]));
        let mut runner = scenario.build();

        let mut events = Vec::new();
        engine::game::morph::play_face_down(runner.state_mut(), P0, id, &mut events)
            .expect("the card is played face down");

        let paused = runner
            .act(GameAction::TurnFaceUp {
                object_id: id,
                x: 0,
            })
            .expect("the special action succeeds up to the replacement's own choice");
        let WaitingFor::ReplacementChoice { candidates, .. } = &paused.waiting_for else {
            panic!(
                "CR 616.1: the ordering choice the turn-up replacement raised must stay \
                 live, got {:?}",
                paused.waiting_for
            );
        };
        let names: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.source_name.as_str())
            .collect();
        assert_eq!(
            names.len(),
            2,
            "exactly the two live modifiers may compete — no stale or extra \
             candidates, got {names:?}"
        );
        assert!(
            names.contains(&"Plus One Modifier") && names.contains(&"Times Two Modifier"),
            "the candidate set is the two counter modifiers, got {names:?}"
        );
        assert!(
            runner.state().pending_replacement.is_some(),
            "the parked counter addition is still waiting for its order"
        );
        let obj = &runner.state().objects[&id];
        assert!(
            !obj.face_down,
            "CR 708.11: the turn-up itself is not prevented by the pending choice"
        );
        assert_eq!(
            obj.counters.get(&CounterType::Plus1Plus1),
            None,
            "no counters land before the order is chosen"
        );

        let index = names
            .iter()
            .position(|name| *name == first_choice)
            .expect("the requested first choice is among the candidates");
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("the ordering choice is answered");

        assert!(
            runner.state().pending_replacement.is_none(),
            "the counter addition settled; no ghost record remains"
        );
        assert!(
            runner
                .state()
                .stack
                .iter()
                .any(|entry| matches!(&entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == id)),
            "CR 603.2: the 'when turned face up' trigger still reaches the stack \
             after the interposed choice"
        );
        runner.state().objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    };

    assert_eq!(
        run_with_first_choice("Plus One Modifier"),
        12,
        "CR 616.1: the plus applies first, then the doubling — (5+1)*2"
    );
    assert_eq!(
        run_with_first_choice("Times Two Modifier"),
        11,
        "CR 616.1: the doubling applies first, then the plus — 5*2+1"
    );
}

const WARBREAK_WITH_COUNTERS: &str = "Morph {X}{X}{R} (You may cast this card face down as a \
                                      2/2 creature for {3}. Turn it face up any time for its \
                                      morph cost.)\nAs this creature is turned face up, put \
                                      five +1/+1 counters on it.\nWhen this creature is \
                                      turned face up, create X 1/1 red Goblin creature tokens.";

/// The same blocker through the paused-payment route: the mana source's own
/// replacement choice settles FIRST, the resumed completion flips, and the
/// turn-up replacement's ordering prompt must then surface — not the stale
/// exile prompt, and not a premature `Priority` that strands the parked
/// counters. The X=0 announcement must still bind across BOTH pauses.
///
/// Review round 3: one run per selectable order (CR 616.1) — the resumed
/// prompt must hold EXACTLY the two counter modifiers (the settled exile
/// prompt must not resurface among them), and the chosen order must determine
/// the count: plus first (5+1)*2 = 12, doubling first 5*2+1 = 11.
#[test]
fn a_resumed_payment_still_surfaces_the_turn_up_replacement_choice() {
    use engine::types::ability::QuantityModification;
    use engine::types::counter::CounterType;

    let run_with_first_choice = |first_choice: &str| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let id = scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Warbreak Trumpeter",
                1,
                1,
                WARBREAK_WITH_COUNTERS,
            )
            .id();
        let source = scenario
            .add_creature(P0, "Self-Exiling Mana Source", 1, 1)
            .with_ability_definition(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Red],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::Exile {
                            count: 1,
                            zone: None,
                            filter: Some(TargetFilter::SelfRef),
                        },
                    ],
                }),
            )
            .id();
        for name in ["First Pause Replacement", "Second Pause Replacement"] {
            scenario
                .add_creature(P0, name, 0, 0)
                .as_enchantment()
                .with_replacement_definition(redirect_exile_to_graveyard());
        }
        add_counter_modifier(
            &mut scenario,
            "Plus One Modifier",
            QuantityModification::Plus { value: 1 },
        );
        add_counter_modifier(
            &mut scenario,
            "Times Two Modifier",
            QuantityModification::Times { factor: 2 },
        );
        let mut runner = scenario.build();

        let mut events = Vec::new();
        engine::game::morph::play_face_down(runner.state_mut(), P0, id, &mut events)
            .expect("the card is played face down");

        let paused = runner
            .act(GameAction::TurnFaceUp {
                object_id: id,
                x: 0,
            })
            .expect("the source's own cost pauses the payment rather than failing it");
        assert!(
            matches!(paused.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "first pause: the mana source's exile replacement owns the window"
        );

        let resumed = runner
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("the exile choice is answered and the payment resumes");
        let WaitingFor::ReplacementChoice { candidates, .. } = &resumed.waiting_for else {
            panic!(
                "second pause: the resumed completion must hand back the turn-up \
                 replacement's ordering choice, got {:?}",
                resumed.waiting_for
            );
        };
        let names: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.source_name.as_str())
            .collect();
        assert_eq!(
            names.len(),
            2,
            "exactly the two counter modifiers — the settled exile prompt must \
             not resurface among the candidates, got {names:?}"
        );
        assert!(
            names.contains(&"Plus One Modifier") && names.contains(&"Times Two Modifier"),
            "the live prompt is the COUNTER ordering choice, got {names:?}"
        );
        assert!(
            !runner.state().objects[&id].face_down,
            "CR 708.11: the flip itself committed before the counter order is chosen"
        );

        let index = names
            .iter()
            .position(|name| *name == first_choice)
            .expect("the requested first choice is among the candidates");
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("the ordering choice is answered");

        let bound_x = runner
            .state()
            .stack
            .iter()
            .find_map(|entry| match &entry.kind {
                StackEntryKind::TriggeredAbility {
                    source_id, ability, ..
                } if *source_id == id => Some(ability.chosen_x),
                _ => None,
            })
            .expect("the turned-face-up trigger must still reach the stack across both pauses");
        assert_eq!(
            bound_x,
            Some(0),
            "the X=0 announcement survives the payment pause AND the turn-up choice"
        );
        assert_eq!(
            runner.state().objects[&source].zone,
            Zone::Graveyard,
            "the mana source's own cost still resolved through its replacement"
        );

        runner.state().objects[&id]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
    };

    assert_eq!(
        run_with_first_choice("Plus One Modifier"),
        12,
        "CR 616.1 across both pauses: the plus applies first — (5+1)*2"
    );
    assert_eq!(
        run_with_first_choice("Times Two Modifier"),
        11,
        "CR 616.1 across both pauses: the doubling applies first — 5*2+1"
    );
}
