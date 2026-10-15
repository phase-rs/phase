//! Giada, Font of Hope + Metallic Mimic (chosen type: Angel).
//!
//! Giada, Font of Hope (FDN) Oracle: "Flying, vigilance\nEach other Angel you control
//! enters with an additional +1/+1 counter on it for each Angel you already control.\n
//! {T}: Add {W}. Spend this mana only to cast an Angel spell."
//! Metallic Mimic Oracle: "As this creature enters, choose a creature type.\nThis
//! creature is the chosen type in addition to its other types.\nEach other creature
//! you control of the chosen type enters with an additional +1/+1 counter on it."
//!
//! Expected: Mimic cast with Giada out, choosing Angel, enters with 1 +1/+1 counter.
//! Actual:   Mimic enters with 0 counters; Giada's `valid_card` (Subtype Angel) is
//!           tested against the pre-choice Shapeshifter, because the as-enters
//!           choice is a post-replacement continuation run after the ZoneChange.
//! CR 614.12 (check the entering permanent "as it would exist on the battlefield",
//! including replacements already applied and its own static abilities) +
//! CR 614.12a (an as-enters choice is made before the permanent enters).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ChosenAttribute, ContinuousModification, Effect, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    CastPaymentMode, NamedChoiceSourceBinding, PersistedGameState, PersistedRestoreFinalization,
    WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const GIADA: &str = "Flying, vigilance\nEach other Angel you control enters with \
an additional +1/+1 counter on it for each Angel you already control.\n{T}: Add \
{W}. Spend this mana only to cast an Angel spell.";

const METALLIC_MIMIC: &str = "As this creature enters, choose a creature type.\n\
This creature is the chosen type in addition to its other types.\n\
Each other creature you control of the chosen type enters with an additional \
+1/+1 counter on it.";

/// Cast `id` from hand (zero-cost scenario card), resolve it, and answer every
/// as-enters creature-type prompt with `chosen`. Returns how many such prompts
/// the engine raised.
fn cast_and_choose(runner: &mut GameRunner, id: ObjectId, chosen: &str) -> usize {
    let card_id = runner.state().objects[&id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from hand");
    let mut prompts = 0;
    for _ in 0..12 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => {
                prompts += 1;
                runner
                    .act(GameAction::ChooseOption {
                        choice: chosen.to_string(),
                    })
                    .expect("answer the creature-type choice");
            }
            // CR 616.1: if a fix makes both replacements apply, order them.
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("order replacements");
            }
            _ if !runner.state().stack.is_empty() => runner.advance_until_stack_empty(),
            _ => break,
        }
    }
    prompts
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn giada_board() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Giada, Font of Hope", 2, 2, GIADA)
        .with_subtypes(vec!["Angel"]);
    scenario
}

#[test]
fn metallic_mimic_choosing_angel_gets_giadas_counter() {
    let mut scenario = giada_board();
    let mimic = scenario
        .add_creature_to_hand_from_oracle(P0, "Metallic Mimic", 2, 1, METALLIC_MIMIC)
        .as_artifact_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Angel".to_string(),
        "Shapeshifter".to_string(),
        "Elf".to_string(),
    ];

    let prompts = cast_and_choose(&mut runner, mimic, "Angel");

    let obj = &runner.state().objects[&mimic];
    assert!(
        prompts >= 1
            && obj.zone == Zone::Battlefield
            && obj.chosen_creature_type() == Some("Angel"),
        "reach: Mimic must enter after the engine-raised creature-type prompt with Angel chosen \
         (prompts={prompts}, zone={:?}, chosen={:?})",
        obj.zone,
        obj.chosen_creature_type()
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Angel"),
        "reach: Mimic's own static must make it an Angel on the battlefield, subtypes={:?}",
        obj.card_types.subtypes
    );
    assert_eq!(
        p1p1(&runner, mimic),
        1,
        "defect: CR 614.12 — Mimic is an Angel as it would exist on the battlefield, so \
         Giada's replacement applies (1 Angel already controlled: Giada)"
    );
}

/// Control: a Changeling is an Angel with no choice involved. Expected GREEN at
/// baseline; isolates the defect to the choice/self-static projection, not Giada's
/// filter or count.
#[test]
fn control_changeling_gets_giadas_counter() {
    let mut scenario = giada_board();
    let shifter = scenario
        .add_creature_to_hand(P0, "Test Changeling", 1, 1)
        .with_subtypes(vec!["Shapeshifter"])
        .with_keyword(Keyword::Changeling)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec![
        "Angel".to_string(),
        "Shapeshifter".to_string(),
        "Elf".to_string(),
    ];

    let _ = cast_and_choose(&mut runner, shifter, "Angel");

    assert_eq!(
        runner.state().objects[&shifter].zone,
        Zone::Battlefield,
        "reach: the Changeling resolved onto the battlefield"
    );
    assert_eq!(
        p1p1(&runner, shifter),
        1,
        "control: a Changeling Angel gets Giada's counter (CR 702.73a)"
    );
}

// ---------------------------------------------------------------------------
// The class: CR 614.12a pre-entry as-enters choices + the CR 614.12 projection
// ---------------------------------------------------------------------------

const ADAPTIVE_AUTOMATON: &str = "As this creature enters, choose a creature type.\n\
This creature is the chosen type in addition to its other types.\n\
Other creatures you control of the chosen type get +1/+1.";

const PHYREXIAN_CENSOR: &str = "Each player can't cast more than one non-Phyrexian spell \
each turn.\nNon-Phyrexian creatures enter tapped.";

const CURATOR_BEASTIE: &str = "Reach\nColorless creatures you control enter with two \
additional +1/+1 counters on them.\nWhenever this creature enters or attacks, manifest \
dread. (Look at the top two cards of your library. Put one onto the battlefield face down \
as a 2/2 creature and the other into your graveyard. Turn it face up any time for its mana \
cost if it's a creature card.)";

const OPEN_THE_VAULTS: &str = "Return all artifact and enchantment cards from all \
graveyards to the battlefield under their owners' control. (Auras with nothing to enchant \
remain in graveyards.)";

const REANIMATE: &str = "Put target creature card from a graveyard onto the battlefield \
under your control. You lose life equal to that card's mana value.";

const RECKLESS_FIREWEAVER: &str = "Whenever an artifact you control enters, this creature \
deals 1 damage to each opponent.";

const THRIVING_GROVE: &str = "This land enters tapped. As it enters, choose a color other \
than green.\n{T}: Add {G} or one mana of the chosen color.";

/// A test permanent with the Thunderous Velocipede `EntersWithAdditionalCounters` static
/// shape, narrowed to Angels, so the static (non-replacement) entry path reads the
/// entrant's chosen type.
const ANGEL_FORGE: &str = "Each other Angel you control enters with an additional +1/+1 \
counter on it if its mana value is 4 or less. Otherwise, it enters with three additional \
+1/+1 counters on it.";

const CREATURE_TYPES: [&str; 7] = [
    "Angel",
    "Shapeshifter",
    "Elf",
    "Phyrexian",
    "Wizard",
    "Beast",
    "Wall",
];

/// A prompt the engine raised while driving an action to quiescence.
#[derive(Debug, Clone, PartialEq)]
enum Prompt {
    /// `NamedChoice`: its chooser, its source binding, and the zone its source
    /// object was in when the prompt was raised.
    Named {
        player: PlayerId,
        binding: Option<NamedChoiceSourceBinding>,
        source_zone: Option<Zone>,
    },
    /// A CR 616.1 `ReplacementChoice`, by its candidates' source names.
    Order { sources: Vec<String> },
}

/// Drive to quiescence. Named choices take `answers` in order; an ordering prompt
/// applies the candidate whose source is named `first` (else the first candidate).
fn drive(runner: &mut GameRunner, answers: &[&str], first: Option<&str>) -> Vec<Prompt> {
    let mut prompts = Vec::new();
    let mut answers = answers.iter();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { player, source, .. } => {
                let source_zone = source.as_ref().and_then(|source| {
                    runner
                        .state()
                        .objects
                        .get(&source.prompt.identity.reference.object_id)
                        .map(|object| object.zone)
                });
                prompts.push(Prompt::Named {
                    player,
                    binding: source.as_ref().map(|source| source.binding),
                    source_zone,
                });
                let answer = answers
                    .next()
                    .expect("a scripted answer for every NamedChoice");
                runner
                    .act(GameAction::ChooseOption {
                        choice: answer.to_string(),
                    })
                    .expect("answer the named choice");
            }
            WaitingFor::ReplacementChoice { candidates, .. } => {
                let sources: Vec<String> =
                    candidates.iter().map(|c| c.source_name.clone()).collect();
                let index = first
                    .and_then(|name| sources.iter().position(|source| source == name))
                    .unwrap_or(0);
                prompts.push(Prompt::Order { sources });
                runner
                    .act(GameAction::ChooseReplacement { index })
                    .expect("order the replacements");
            }
            _ if !runner.state().stack.is_empty() => runner.advance_until_stack_empty(),
            _ => break,
        }
    }
    prompts
}

fn cast(runner: &mut GameRunner, id: ObjectId) {
    let card_id = runner.state().objects[&id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast from hand");
}

fn set_creature_types(runner: &mut GameRunner) {
    runner.state_mut().all_creature_types = CREATURE_TYPES.iter().map(|t| t.to_string()).collect();
}

/// A Metallic Mimic already on the battlefield that chose `chosen` as it entered.
fn mimic_on_battlefield(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, name, 2, 1, METALLIC_MIMIC)
        .as_artifact_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .id()
}

fn choose_on_battlefield(runner: &mut GameRunner, id: ObjectId, chosen: &str) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::CreatureType(chosen.to_string()));
    engine::game::layers::mark_layers_full(runner.state_mut());
    engine::game::layers::flush_layers(runner.state_mut());
}

fn mimic_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Metallic Mimic", 2, 1, METALLIC_MIMIC)
        .as_artifact_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .id()
}

fn is_named(prompt: &Prompt) -> bool {
    matches!(prompt, Prompt::Named { .. })
}

/// T3 ? the negative of the issue row: choosing a non-Angel type gets nothing from
/// Giada (vacuity guard: Giada is present and the prompt was raised).
#[test]
fn metallic_mimic_choosing_elf_gets_no_giada_counter() {
    let mut scenario = giada_board();
    let mimic = mimic_in_hand(&mut scenario);
    let mut runner = scenario.build();
    set_creature_types(&mut runner);

    cast(&mut runner, mimic);
    let prompts = drive(&mut runner, &["Elf"], None);

    assert!(
        prompts.iter().any(is_named)
            && runner.state().objects[&mimic].chosen_creature_type() == Some("Elf"),
        "reach: Elf chosen through the engine prompt, prompts={prompts:?}"
    );
    assert_eq!(p1p1(&runner, mimic), 0, "an Elf Mimic is not an Angel");
}

/// P1 ? CR 614.12a: the as-enters choice is made BEFORE the permanent enters. The
/// prompt is raised while Mimic is still on the stack, bound to the parked entry.
#[test]
fn as_enters_choice_is_raised_before_the_permanent_enters() {
    let mut scenario = giada_board();
    let mimic = mimic_in_hand(&mut scenario);
    let mut runner = scenario.build();
    set_creature_types(&mut runner);

    cast(&mut runner, mimic);
    runner.advance_until_stack_empty();

    let WaitingFor::NamedChoice { source, player, .. } = &runner.state().waiting_for else {
        panic!(
            "the as-enters choice must be pending, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(
        source.as_ref().map(|source| source.binding),
        Some(NamedChoiceSourceBinding::PendingEntry),
        "the prompt answers the parked entry"
    );
    assert_eq!(
        runner.state().objects[&mimic].zone,
        Zone::Stack,
        "CR 614.12a: Mimic has not entered yet"
    );
    assert!(!runner.state().battlefield.contains(&mimic));

    let prompts = drive(&mut runner, &["Angel"], None);
    assert_eq!(prompts.len(), 1, "one prompt, answered: {prompts:?}");
    assert_eq!(runner.state().objects[&mimic].zone, Zone::Battlefield);
    assert_eq!(p1p1(&runner, mimic), 1);
}

/// CR 616.1e/f after the choice: Giada (Giada + Mimic A already controlled ? +2) and
/// Mimic A (chose Angel ? +1) both apply once Mimic B is an Angel: 3 counters. Neither
/// applied before the choice, so the first prompt is the choice.
#[test]
fn second_mimic_choosing_angel_gets_giada_and_first_mimic_counters() {
    let mut scenario = giada_board();
    let first = mimic_on_battlefield(&mut scenario, "Metallic Mimic A");
    let second = mimic_in_hand(&mut scenario);
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    choose_on_battlefield(&mut runner, first, "Angel");
    assert!(
        runner.state().objects[&first]
            .card_types
            .subtypes
            .iter()
            .any(|s| s == "Angel"),
        "reach: Mimic A is an Angel"
    );

    cast(&mut runner, second);
    let prompts = drive(&mut runner, &["Angel"], None);

    assert!(
        prompts.first().is_some_and(is_named),
        "the choice comes first: {prompts:?}"
    );
    assert_eq!(p1p1(&runner, second), 3, "Giada +2, Mimic A +1");
    assert_eq!(p1p1(&runner, first), 0, "Mimic A is not re-entering");
}

fn censor_board() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P1, "Phyrexian Censor", 3, 3, PHYREXIAN_CENSOR)
        .with_subtypes(vec!["Phyrexian", "Wizard"]);
    let mimic = mimic_in_hand(&mut scenario);
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    (runner, mimic)
}

fn assert_both_ordered(prompts: &[Prompt]) {
    let Some(Prompt::Order { sources }) = prompts.first() else {
        panic!("CR 616.1: the first prompt must order the two effects, got {prompts:?}");
    };
    assert!(
        sources.iter().any(|s| s == "Phyrexian Censor")
            && sources.iter().any(|s| s == "Metallic Mimic"),
        "reach: both Censor's and Mimic's replacements apply, got {sources:?}"
    );
}

/// P2 ? CR 616.1e: Censor applied first taps the non-Phyrexian entrant; the choice of
/// Phyrexian made afterwards doesn't untap it.
#[test]
fn censor_first_then_choosing_phyrexian_enters_tapped() {
    let (mut runner, mimic) = censor_board();
    cast(&mut runner, mimic);
    let prompts = drive(&mut runner, &["Phyrexian"], Some("Phyrexian Censor"));

    assert_both_ordered(&prompts);
    assert!(
        prompts.iter().any(is_named),
        "the choice is still made: {prompts:?}"
    );
    let obj = &runner.state().objects[&mimic];
    assert_eq!(obj.chosen_creature_type(), Some("Phyrexian"));
    assert!(obj.tapped, "Censor applied while Mimic was non-Phyrexian");
}

/// P2 ? CR 616.1e + CR 614.12: the choice applied first makes Mimic a Phyrexian as it
/// would exist, so Censor no longer applies (CR 616.1f re-check).
#[test]
fn choosing_phyrexian_first_enters_untapped_under_censor() {
    let (mut runner, mimic) = censor_board();
    cast(&mut runner, mimic);
    let prompts = drive(&mut runner, &["Phyrexian"], Some("Metallic Mimic"));

    assert_both_ordered(&prompts);
    let obj = &runner.state().objects[&mimic];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.chosen_creature_type(), Some("Phyrexian"));
    assert!(!obj.tapped, "a Phyrexian creature isn't tapped by Censor");
}

/// T5 ? Mimic A's `IsChosenCreatureType` replacement reads the entering Mimic B as it
/// would exist with its own choice.
#[test]
fn mimic_counter_reads_the_entering_mimics_chosen_type() {
    for (chosen, expected) in [("Angel", 1), ("Elf", 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let first = mimic_on_battlefield(&mut scenario, "Metallic Mimic A");
        let second = mimic_in_hand(&mut scenario);
        let mut runner = scenario.build();
        set_creature_types(&mut runner);
        choose_on_battlefield(&mut runner, first, "Angel");

        cast(&mut runner, second);
        let prompts = drive(&mut runner, &[chosen], None);

        assert!(prompts.iter().any(is_named), "reach: prompt raised");
        assert_eq!(
            p1p1(&runner, second),
            expected,
            "Mimic B choosing {chosen} against Mimic A's Angel"
        );
    }
}

/// T6 ? a second producer of the class: Adaptive Automaton choosing Angel is an Angel
/// as it enters, so Giada applies.
#[test]
fn adaptive_automaton_choosing_angel_gets_giadas_counter() {
    let mut scenario = giada_board();
    let automaton = scenario
        .add_creature_to_hand_from_oracle(P0, "Adaptive Automaton", 2, 2, ADAPTIVE_AUTOMATON)
        .as_artifact_creature()
        .with_subtypes(vec!["Construct"])
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);

    cast(&mut runner, automaton);
    let prompts = drive(&mut runner, &["Angel"], None);

    assert!(prompts.first().is_some_and(is_named), "{prompts:?}");
    assert_eq!(p1p1(&runner, automaton), 1);
}

/// T7 ? the static `EntersWithAdditionalCounters` path (CR 614.1c) reads the entrant as
/// it exists with its own statics applied, before the layer pass has run on it.
#[test]
fn static_enters_with_counters_reads_the_mimics_chosen_type() {
    for (chosen, expected) in [("Angel", 1), ("Elf", 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let forge = scenario
            .add_creature_from_oracle(P0, "Angel Forge", 1, 1, ANGEL_FORGE)
            .id();
        let mimic = mimic_in_hand(&mut scenario);
        let mut runner = scenario.build();
        set_creature_types(&mut runner);
        assert!(
            runner.state().objects[&forge]
                .static_definitions
                .iter_unchecked()
                .any(|def| matches!(def.mode, StaticMode::EntersWithAdditionalCounters { .. })),
            "reach: the source carries the static entry-counter shape"
        );

        cast(&mut runner, mimic);
        drive(&mut runner, &[chosen], None);

        assert_eq!(p1p1(&runner, mimic), expected, "choosing {chosen}");
    }
}

/// T8 ? a self-only pair (Thriving Grove's own "enters tapped" + its colour choice)
/// commutes: no ordering prompt, the land enters tapped with its colour chosen.
#[test]
fn thriving_land_choice_needs_no_ordering_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let grove = scenario
        .add_land_to_hand(P0, "Thriving Grove")
        .from_oracle_text(THRIVING_GROVE)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&grove].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: grove,
            card_id,
        })
        .expect("play the land");
    let prompts = drive(&mut runner, &["Blue"], None);

    assert_eq!(prompts.len(), 1, "only the colour choice: {prompts:?}");
    assert!(is_named(&prompts[0]));
    let obj = &runner.state().objects[&grove];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.tapped, "the land's own enters-tapped applied");
    assert_eq!(obj.chosen_color(), Some(ManaColor::Blue));
}

fn mimic_in_graveyard(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_graveyard(owner, "Metallic Mimic", 2, 1)
        .from_oracle_text(METALLIC_MIMIC)
        .as_artifact_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .id()
}

/// P3 ? CR 614.12a in a batch: Open the Vaults returns two Mimics at once. Each entry
/// parks on its own choice and resumes the same owner, which settles; both enter with
/// their choices, and the first to be delivered gets only Giada's +1 (Giada is the one
/// Angel already controlled). The owner records both deliveries, so Reckless Fireweaver
/// sees both artifacts enter (CR 603.2): 2 damage. Without the parked boundary the
/// answered delivery is not attributed to its owner and one trigger is lost.
///
/// Not asserted: the second delivery's count. Batch entries are delivered one at a time,
/// so the second entrant sees the first as already controlled ? a pre-existing
/// simultaneity defect reproduced with two plain artifact Angels at the base (counts
/// 2 and 1), independent of the as-enters choice.
#[test]
fn open_the_vaults_returns_two_mimics_each_choosing_before_entry() {
    let mut scenario = giada_board();
    scenario
        .add_creature_from_oracle(P0, "Reckless Fireweaver", 1, 3, RECKLESS_FIREWEAVER)
        .with_subtypes(vec!["Human", "Artificer"]);
    let first = mimic_in_graveyard(&mut scenario, P0);
    let second = mimic_in_graveyard(&mut scenario, P0);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    let opponent_life = runner.state().players[1].life;

    cast(&mut runner, vaults);
    let prompts = drive(&mut runner, &["Angel", "Angel"], None);

    assert_eq!(
        prompts.iter().filter(|prompt| is_named(prompt)).count(),
        2,
        "reach: one choice per entering Mimic, {prompts:?}"
    );
    for (mimic, label) in [(first, "first"), (second, "second")] {
        let obj = &runner.state().objects[&mimic];
        assert_eq!(obj.zone, Zone::Battlefield, "{label} Mimic delivered");
        assert_eq!(
            obj.chosen_creature_type(),
            Some("Angel"),
            "{label} choice kept"
        );
    }
    assert_eq!(
        p1p1(&runner, first).min(p1p1(&runner, second)),
        1,
        "the first-delivered Mimic is an Angel as it enters: Giada's +1"
    );
    assert!(runner.state().pending_replacement.is_none());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the batch settled, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        opponent_life - runner.state().players[1].life,
        2,
        "Reckless Fireweaver triggers once per entering artifact"
    );
}

/// P4 ? CR 400.7: a Mimic that chose Angel, died and is reanimated is a new object; its
/// old choice is not its own. Giada isn't offered before the new choice, and choosing
/// Elf gets nothing.
#[test]
fn reanimated_mimic_does_not_keep_its_old_choice() {
    let mut scenario = giada_board();
    let mimic = mimic_in_graveyard(&mut scenario, P0);
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    runner
        .state_mut()
        .objects
        .get_mut(&mimic)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::CreatureType("Angel".to_string()));

    runner.cast(reanimate).target_object(mimic).commit();
    let prompts = drive(&mut runner, &["Elf"], None);

    assert!(
        prompts.first().is_some_and(is_named),
        "the new choice comes first ? Giada isn't offered for the old Angel: {prompts:?}"
    );
    let obj = &runner.state().objects[&mimic];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.chosen_creature_type(), Some("Elf"));
    assert_eq!(p1p1(&runner, mimic), 0);
}

/// P4 cross-owner ? CR 110.2a: P0 reanimates P1's Mimic under P0's control; P0 makes
/// the choice, and P0's Giada applies.
#[test]
fn reanimating_an_opponents_mimic_lets_the_new_controller_choose() {
    let mut scenario = giada_board();
    let mimic = mimic_in_graveyard(&mut scenario, P1);
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);

    runner.cast(reanimate).target_object(mimic).commit();
    let prompts = drive(&mut runner, &["Angel"], None);

    assert!(
        matches!(
            prompts.first(),
            Some(Prompt::Named {
                player: P0,
                source_zone: Some(Zone::Graveyard),
                ..
            })
        ),
        "P0 chooses while the card is still in P1's graveyard: {prompts:?}"
    );
    let obj = &runner.state().objects[&mimic];
    assert_eq!(obj.controller, P0);
    assert_eq!(p1p1(&runner, mimic), 1);
}

/// CR 800.4a ? the chooser leaves the game while the pre-entry choice is pending: the
/// prompt and the parked entry are abandoned and the game continues.
#[test]
fn leaving_the_game_during_the_pre_entry_choice_abandons_it() {
    let mut scenario = GameScenario::new_n_player(3, 9783);
    scenario.at_phase(Phase::PreCombatMain);
    let mimic = mimic_in_hand(&mut scenario);
    let mut runner = scenario.build();
    set_creature_types(&mut runner);

    cast(&mut runner, mimic);
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "reach: the pre-entry choice is live"
    );
    runner
        .act(GameAction::Concede { player_id: P0 })
        .expect("concede mid-choice");

    assert!(runner.state().pending_replacement.is_none());
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::NamedChoice { .. }),
        "the prompt is abandoned, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().players[0].is_eliminated);
}

fn round_trip(runner: &GameRunner) -> GameRunner {
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    let state = serde_json::from_str::<PersistedGameState>(&json)
        .expect("persisted state decodes")
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("persisted state is admissible")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restored state is publishable");
    GameRunner::from_state(state)
}

/// Save/restore while the pre-entry choice is pending: the restored game answers it the
/// same way.
#[test]
fn pre_entry_choice_survives_save_and_restore() {
    let mut scenario = giada_board();
    let mimic = mimic_in_hand(&mut scenario);
    let mut runner = scenario.build();
    set_creature_types(&mut runner);

    cast(&mut runner, mimic);
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    let mut restored = round_trip(&runner);
    assert!(
        matches!(
            &restored.state().waiting_for,
            WaitingFor::NamedChoice { source: Some(source), .. }
                if source.binding == NamedChoiceSourceBinding::PendingEntry
        ),
        "reach: the restored prompt is still bound to the parked entry"
    );

    drive(&mut restored, &["Angel"], None);
    assert_eq!(p1p1(&restored, mimic), 1);
}

/// A save taken before any choice is answered carries none of the new fields, so a
/// pre-feature save shape restores, and the restored game makes the choice pre-entry.
#[test]
fn save_without_entry_choices_restores_and_continues() {
    let (mut runner, mimic) = censor_board();
    cast(&mut runner, mimic);
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    let json = serde_json::to_string(&PersistedGameState::capture(runner.state().clone()))
        .expect("state serializes");
    for key in ["enter_with_choices", "delivery_boundary", "PendingEntry"] {
        assert!(!json.contains(key), "{key} absent from a pre-answer save");
    }

    let mut restored = round_trip(&runner);
    let prompts = drive(&mut restored, &["Phyrexian"], Some("Metallic Mimic"));
    assert!(prompts.iter().any(is_named), "{prompts:?}");
    assert!(!restored.state().objects[&mimic].tapped);
}

/// CR 614.12 allowlist over the real card pool, failed closed: a face whose as-enters
/// choice is made before entry and which carries an own static off the allowlist that
/// writes a type, colour, ability or controller (the kinds entry consumers read) is
/// marked unsupported — the Primal Clay class (label-gated types), Painter's Servant (a
/// global chosen colour) and True-Name Nemesis (a self keyword). Metallic Mimic and
/// Adaptive Automaton (allowlisted) are supported. The whole-pool set is pinned by
/// `entry_projection_coverage_over_the_export`.
#[test]
fn label_gated_type_statics_are_marked_unsupported_for_the_entry_projection() {
    let Some(db) = crate::support::shared_card_db() else {
        return;
    };
    let gap_of = |name: &str| {
        let face = db
            .get_face_by_name(name)
            .unwrap_or_else(|| panic!("{name} is in the card database"));
        engine::game::coverage::card_face_gaps(face)
            .iter()
            .any(|gap| gap == engine::game::coverage::ENTRY_PROJECTION_GAP)
    };
    for name in [
        "Primal Clay",
        "Primal Plasma",
        "Corrupted Shapeshifter",
        "Painter's Servant",
        "True-Name Nemesis",
    ] {
        assert!(gap_of(name), "{name} must be marked unsupported");
    }
    // Reach guards for the negatives: each face still carries its persisting
    // as-enters choice and the static that reads it, so "no gap" can't pass
    // because a parse lost either.
    for (name, reads_choice) in [
        ("Metallic Mimic", "AddChosenSubtype"),
        ("Adaptive Automaton", "AddChosenSubtype"),
    ] {
        let face = db.get_face_by_name(name).expect("in the card database");
        assert!(
            face.replacements.iter().any(|def| {
                def.event == ReplacementEvent::Moved
                    && def.valid_card == Some(TargetFilter::SelfRef)
                    && def.execute.as_deref().is_some_and(|execute| {
                        matches!(*execute.effect, Effect::Choose { persist: true, .. })
                    })
            }),
            "reach: {name} keeps its persisting as-enters choice"
        );
        assert!(
            face.static_abilities.iter().any(|def| def
                .modifications
                .iter()
                .any(|modification| format!("{modification:?}").starts_with(reads_choice))),
            "reach: {name} keeps its {reads_choice} static"
        );
        assert!(!gap_of(name), "{name} is fully projected");
    }
}

/// P3 re-park ? CR 616.1f in a batch: under Phyrexian Censor, each returning Mimic first
/// orders Censor against its choice; with Censor applied first the entry re-parks on the
/// choice. The re-parked record keeps the event its owner parked on, so both deliveries
/// settle and Reckless Fireweaver sees both artifacts enter.
#[test]
fn open_the_vaults_under_censor_reparks_each_choice_on_its_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P1, "Phyrexian Censor", 3, 3, PHYREXIAN_CENSOR)
        .with_subtypes(vec!["Phyrexian", "Wizard"]);
    scenario
        .add_creature_from_oracle(P0, "Reckless Fireweaver", 1, 3, RECKLESS_FIREWEAVER)
        .with_subtypes(vec!["Human", "Artificer"]);
    let first = mimic_in_graveyard(&mut scenario, P0);
    let second = mimic_in_graveyard(&mut scenario, P0);
    let vaults = scenario
        .add_spell_to_hand_from_oracle(P0, "Open the Vaults", false, OPEN_THE_VAULTS)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    let opponent_life = runner.state().players[1].life;

    cast(&mut runner, vaults);
    let prompts = drive(&mut runner, &["Angel", "Angel"], Some("Phyrexian Censor"));

    assert_eq!(
        prompts.iter().filter(|prompt| is_named(prompt)).count(),
        2,
        "reach: each entry re-parked on its choice after Censor, {prompts:?}"
    );
    assert!(
        prompts
            .iter()
            .any(|prompt| matches!(prompt, Prompt::Order { .. })),
        "reach: Censor was ordered against the choice"
    );
    for mimic in [first, second] {
        let obj = &runner.state().objects[&mimic];
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.chosen_creature_type(), Some("Angel"));
        assert!(obj.tapped, "Censor applied before the choice");
    }
    assert_eq!(
        opponent_life - runner.state().players[1].life,
        2,
        "both deliveries settled on their owner"
    );
}

/// F1 board — CR 800.4a: P2's Phyrexian Censor, P1's Metallic Mimic in P1's graveyard,
/// P0 casting Reanimate on it. Returns the runner parked at P0's pre-entry choice and
/// the Mimic. With `censor`, Censor was applied first, so the parked proposal no longer
/// equals the event the Reanimate owner parked on.
fn reanimate_opponents_mimic_to_its_choice(censor: bool) -> (GameRunner, ObjectId) {
    const P2: PlayerId = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 9783);
    scenario.at_phase(Phase::PreCombatMain);
    if censor {
        scenario
            .add_creature_from_oracle(P2, "Phyrexian Censor", 3, 3, PHYREXIAN_CENSOR)
            .with_subtypes(vec!["Phyrexian", "Wizard"]);
    }
    let mimic = mimic_in_graveyard(&mut scenario, P1);
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE)
        .id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);

    runner.cast(reanimate).target_object(mimic).commit();
    runner.advance_until_stack_empty();
    if censor {
        let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
        else {
            panic!(
                "reach: Censor is ordered against the choice, got {:?}",
                runner.state().waiting_for
            );
        };
        let sources: Vec<&str> = candidates.iter().map(|c| c.source_name.as_str()).collect();
        assert!(
            sources.contains(&"Phyrexian Censor") && sources.contains(&"Metallic Mimic"),
            "reach: both ordering sources, got {sources:?}"
        );
        let index = sources
            .iter()
            .position(|s| *s == "Phyrexian Censor")
            .unwrap();
        runner
            .act(GameAction::ChooseReplacement { index })
            .expect("apply Censor first");
    }
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::NamedChoice { player: P0, source: Some(source), .. }
                if source.binding == NamedChoiceSourceBinding::PendingEntry
        ),
        "reach: the surviving controller P0 holds the pre-entry choice, got {:?}",
        runner.state().waiting_for
    );
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("the parked entry");
    assert_eq!(
        pending.proposed != *pending.delivery_boundary_event(),
        censor,
        "reach: the parked proposal differs from the owner's boundary exactly when Censor applied"
    );
    (runner, mimic)
}

fn assert_owner_departure_retires_the_entry(mut runner: GameRunner, mimic: ObjectId) {
    runner
        .act(GameAction::Concede { player_id: P1 })
        .expect("the Mimic's owner concedes");

    assert!(
        runner.state().pending_replacement.is_none(),
        "CR 800.4a: the parked entry of the departed card is retired"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&mimic)
            .is_none_or(|obj| obj.zone != Zone::Battlefield),
        "the departed owner's card never enters"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty() && runner.state().pending_replacement.is_none(),
        "P0's Reanimate finishes resolving, stack={:?}",
        runner.state().stack
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the game continues, got {:?}",
        runner.state().waiting_for
    );
}

/// F1 — CR 800.4a: the Mimic's owner leaves while P0 makes the pre-entry choice for a
/// proposal Censor already changed. The owner's paused delivery is matched through
/// the record's boundary, so the record is retired and Reanimate finishes.
#[test]
fn owner_leaving_during_a_reparked_choice_retires_the_entry() {
    let (runner, mimic) = reanimate_opponents_mimic_to_its_choice(true);
    assert_owner_departure_retires_the_entry(runner, mimic);
}

/// F1 control — the same departure when the proposal is unchanged.
#[test]
fn owner_leaving_during_an_unmodified_choice_retires_the_entry() {
    let (runner, mimic) = reanimate_opponents_mimic_to_its_choice(false);
    assert_owner_departure_retires_the_entry(runner, mimic);
}

fn face_of(runner: &GameRunner, id: ObjectId) -> CardFace {
    let object = &runner.state().objects[&id];
    CardFace {
        replacements: object
            .replacement_definitions
            .iter_unchecked()
            .cloned()
            .collect(),
        static_abilities: object
            .static_definitions
            .iter_unchecked()
            .cloned()
            .collect(),
        ..CardFace::default()
    }
}

fn has_gap(face: &CardFace, label: &str) -> bool {
    engine::game::coverage::card_face_gaps(face)
        .iter()
        .any(|gap| gap == label)
}

/// Metallic Mimic (Shapeshifter, colorless) in P0's hand with `extra` own statics, and
/// Curator Beastie ("colorless creatures you control enter with two additional +1/+1
/// counters") on P0's battlefield.
fn beastie_mimic_board(extra: Vec<StaticDefinition>) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Curator Beastie", 6, 6, CURATOR_BEASTIE)
        .with_subtypes(vec!["Beast"])
        .with_color(vec![ManaColor::Green]);
    let mut mimic =
        scenario.add_creature_to_hand_from_oracle(P0, "Metallic Mimic", 2, 1, METALLIC_MIMIC);
    mimic
        .as_artifact_creature()
        .with_subtypes(vec!["Shapeshifter"])
        .with_color(vec![]);
    for def in extra {
        mimic.with_static_definition(def);
    }
    let mimic = mimic.id();
    let mut runner = scenario.build();
    set_creature_types(&mut runner);
    (runner, mimic)
}

fn blue() -> ContinuousModification {
    ContinuousModification::AddColor {
        color: ManaColor::Blue,
    }
}

/// Q3 — CR 613.1: a choice-reading static with no affected set affects every object, as
/// an explicit "every object" static does; neither is on the CR 614.12 allowlist, so both
/// shapes carry the same honest gap marker (the Painter's Servant shape).
#[test]
fn absent_and_every_object_affected_sets_gap_alike() {
    use engine::types::ability::ColorChangeMode;
    for affected in [None, Some(TargetFilter::Any)] {
        let mut paint = StaticDefinition::continuous().modifications(vec![
            ContinuousModification::AddChosenColor {
                mode: ColorChangeMode::Add,
            },
        ]);
        paint.affected = affected.clone();
        let mut scenario = GameScenario::new();
        let servant = scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Painter's Servant",
                1,
                3,
                "As this creature enters, choose a color.",
            )
            .with_static_definition(paint)
            .id();
        let runner = scenario.build();
        let face = face_of(&runner, servant);
        assert!(
            face.replacements
                .iter()
                .any(|def| def.event == ReplacementEvent::Moved),
            "reach: the as-enters choice is parsed"
        );
        assert!(
            has_gap(&face, engine::game::coverage::ENTRY_PROJECTION_GAP),
            "affected {affected:?}"
        );
    }
}

/// N1 — CR 613.6 + CR 613.8: an own static whose affected set ("Angel creatures") is
/// decided by another own static (the chosen subtype) is off the CR 614.12 allowlist.
/// Contract fixture, not a printed interaction: Metallic Mimic with an added "Angel
/// creatures are blue in addition" static, Curator Beastie out, the choice ordered first,
/// Angel chosen. The face carries the gap marker; the runtime's per-consumer rejection is
/// `filter::entry_projection_tests::runtime_projection_is_all_or_nothing_per_consumer`.
#[test]
fn dependent_own_static_is_marked_unsupported_for_the_entry_projection() {
    use engine::game::coverage::ENTRY_PROJECTION_GAP;
    let angels_blue = StaticDefinition::continuous()
        .affected(TargetFilter::Typed(
            TypedFilter::creature().subtype("Angel".to_string()),
        ))
        .modifications(vec![blue()]);
    let (mut runner, mimic) = beastie_mimic_board(vec![angels_blue]);
    assert!(
        has_gap(&face_of(&runner, mimic), ENTRY_PROJECTION_GAP),
        "the unproven dependency is marked unsupported"
    );
    cast(&mut runner, mimic);
    let prompts = drive(&mut runner, &["Angel"], Some("Metallic Mimic"));
    let Some(Prompt::Order { sources }) = prompts.first() else {
        panic!("reach: both replacements apply, got {prompts:?}");
    };
    assert!(sources.iter().any(|s| s == "Curator Beastie"));
    let obj = &runner.state().objects[&mimic];
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Angel")
            && obj.color.contains(&ManaColor::Blue),
        "reach: on the battlefield it is an Angel and therefore blue"
    );

    // Control: a self colour static is off the allowlist too.
    let (runner, mimic) = beastie_mimic_board(vec![StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![blue()])]);
    assert!(has_gap(&face_of(&runner, mimic), ENTRY_PROJECTION_GAP));

    // Control: the allowlisted Mimic alone is projected; colorless, so Beastie applies.
    let (mut runner, mimic) = beastie_mimic_board(Vec::new());
    assert!(!has_gap(&face_of(&runner, mimic), ENTRY_PROJECTION_GAP));
    cast(&mut runner, mimic);
    drive(&mut runner, &["Angel"], None);
    assert_eq!(
        p1p1(&runner, mimic),
        2,
        "a colorless entrant gets Beastie's counters"
    );
}

/// N4 — CR 613.1c + CR 614.12: an own rename plus an own colour static keyed on the new
/// name. Neither is on the allowlist, so the face carries the gap marker and Curator
/// Beastie (a colour reader) sees no projection: the entrant reads as staged, colorless.
/// Without the rename the named filter never matches the Mimic.
#[test]
fn renamed_entrant_is_marked_unsupported_for_the_entry_projection() {
    use engine::game::coverage::ENTRY_PROJECTION_GAP;
    let rename = StaticDefinition::continuous()
        .affected(TargetFilter::SelfRef)
        .modifications(vec![ContinuousModification::SetTextName {
            name: "Projected Mimic".to_string(),
        }]);
    let named_blue = StaticDefinition::continuous()
        .affected(TargetFilter::Named {
            name: "Projected Mimic".to_string(),
        })
        .modifications(vec![blue()]);
    let (mut runner, mimic) = beastie_mimic_board(vec![rename, named_blue.clone()]);
    assert!(has_gap(&face_of(&runner, mimic), ENTRY_PROJECTION_GAP));
    cast(&mut runner, mimic);
    drive(&mut runner, &["Angel"], Some("Metallic Mimic"));
    let obj = &runner.state().objects[&mimic];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert_eq!(obj.name, "Projected Mimic", "reach: the rename applies");
    assert!(obj.color.contains(&ManaColor::Blue), "reach: so it's blue");
    assert_eq!(
        p1p1(&runner, mimic),
        2,
        "no projection for a colour reader: Beastie sees it as staged"
    );

    // Control: without the rename the named colour static never matches.
    let (mut runner, mimic) = beastie_mimic_board(vec![named_blue]);
    cast(&mut runner, mimic);
    drive(&mut runner, &["Angel"], None);
    let obj = &runner.state().objects[&mimic];
    assert_eq!(obj.zone, Zone::Battlefield, "reach: the control entered");
    assert_eq!(
        obj.chosen_creature_type(),
        Some("Angel"),
        "reach: it chose Angel"
    );
    assert_eq!(obj.name, "Metallic Mimic", "reach: no rename");
    assert!(!obj.color.contains(&ManaColor::Blue));
}

/// CR 614.12 over the whole export (self-skips when it isn't generated): no printed
/// battlefield-entry replacement or "enters with an additional counter" static reads an
/// entrant characteristic outside the consumer contract, and the faces the allowlist
/// demotes are exactly the census set. Chameleon Spirit and Plague Engineer (P/T writers
/// no consumer reads) stay supported.
#[test]
fn entry_projection_coverage_over_the_export() {
    use engine::game::coverage::{ENTRY_CONSUMER_GAP, ENTRY_PROJECTION_GAP};
    let Some(export) = crate::support::shared_card_export_json() else {
        return;
    };
    let faces: Vec<CardFace> = export
        .values()
        .filter(|value| {
            value
                .get("replacements")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|replacements| !replacements.is_empty())
                || value
                    .get("static_abilities")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|statics| {
                        statics.iter().any(|def| {
                            def.get("mode")
                                .and_then(|mode| mode.get("EntersWithAdditionalCounters"))
                                .is_some()
                        })
                    })
        })
        .map(|value| serde_json::from_value(value.clone()).expect("an export face deserializes"))
        .collect();
    assert!(
        faces.len() > 1_000,
        "reach guard: a truncated export would pass vacuously, got {}",
        faces.len()
    );
    let named = |label: &str| {
        let mut names: Vec<&str> = faces
            .iter()
            .filter(|face| has_gap(face, label))
            .map(|face| face.name.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    };
    assert_eq!(named(ENTRY_CONSUMER_GAP), Vec::<&str>::new());
    assert_eq!(
        named(ENTRY_PROJECTION_GAP),
        [
            "Arcane Adaptation",
            "Ashes of the Fallen",
            "Ashling's Prerogative",
            "Bicycle Elemental",
            "Chronicle of Victory",
            "Conspiracy",
            "Corrupted Shapeshifter",
            "Cover of Darkness",
            "Frostcliff Siege",
            "Glorious Dragon-Kin",
            "Greymond, Avacyn's Stalwart",
            "Kindred Boon",
            "Lavabrink Venturer",
            "Leyline of Transformation",
            "Lifecraft Engine",
            "Order of the Stars",
            "Painter's Servant",
            "Primal Clay",
            "Primal Plasma",
            "Realmwright",
            "Riders of Gavony",
            "Rukarumel, Biologist",
            "Serra's Emissary",
            "Shifting Sky",
            "Shimmer",
            "Steely Resolve",
            "True-Name Nemesis",
            "Voice of All",
            "Ward Sliver",
            "Xenograft",
        ]
    );
    for name in ["Chameleon Spirit", "Plague Engineer", "Giada, Font of Hope"] {
        assert!(
            faces.iter().any(|face| face.name == name),
            "reach: {name} has a replacement"
        );
    }
}
