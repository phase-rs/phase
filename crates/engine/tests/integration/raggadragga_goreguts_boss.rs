//! Raggadragga, Goreguts Boss (issue #1184) — per-clause runtime coverage,
//! driven through the real parse → event → layer pipeline from the card's
//! verbatim Oracle text (a paraphrase could take a different parser branch
//! than the real card).
//!
//! * Static anthem (CR 605.1 + CR 613.4c): each creature you control with a
//!   mana ability gets +2/+2.
//! * Attack trigger (CR 508.1 + CR 603.2): whenever such a creature attacks,
//!   untap it.
//! * Spell trigger (CR 603.4 + CR 601.2h): whenever you cast a spell with at
//!   least seven mana spent, untap target creature; it gets +7/+7 and gains
//!   trample until end of turn.

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Oracle text of Raggadragga, Goreguts Boss (fixture `r.jsonl`).
const RAGGADRAGGA_ORACLE: &str = "Each creature you control with a mana ability gets +2/+2.\nWhenever a creature you control with a mana ability attacks, untap it.\nWhenever you cast a spell, if at least seven mana was spent to cast it, untap target creature. It gets +7/+7 and gains trample until end of turn.";
/// Verbatim Oracle text of Llanowar Elves (fixture `l.jsonl`).
const LLANOWAR_ELVES_ORACLE: &str = "{T}: Add {G}.";
/// Verbatim Oracle text of Pelakka Wurm (fixture `p.jsonl`): 4GGG, seven
/// mana for Raggadragga's spell trigger to count.
const PELAKKA_WURM_ORACLE: &str = "Trample (This creature can deal excess combat damage to the player or planeswalker it's attacking.)\nWhen this creature enters, you gain 7 life.\nWhen this creature dies, draw a card.";
/// Verbatim Oracle text of Kinnan, Bonder Prodigy (2/2): the positive witness —
/// its trigger meets CR 605.1b (untargeted; fires from a {T}-mana-ability
/// resolution per CR 106.12a; adds mana).
const KINNAN_ORACLE: &str = "Whenever you tap a nonland permanent for mana, add one mana of any type that permanent produced.\n{5}{G}{U}: Look at the top five cards of your library. You may put a non-Human creature card from among them onto the battlefield. Put the rest on the bottom of your library in a random order.";
/// Verbatim Oracle text of Lotus Cobra (2/1): the flag trap — its landfall
/// trigger carries a mana body (and the derived `is_mana_ability` JSON flag)
/// but its ChangesZone mode observes no mana event, so it must NOT count.
const LOTUS_COBRA_ORACLE: &str =
    "Landfall — Whenever a land you control enters, add one mana of any color.";
/// Verbatim Oracle text of Treasure Nabber (3/2): the mode-without-body trap —
/// its TapsForMana trigger adds no mana, so it must NOT count (R-MB5 logic).
const TREASURE_NABBER_ORACLE: &str =
    "Whenever an opponent taps an artifact for mana, gain control of that artifact until the end of your next turn.";
/// Verbatim Oracle text of Solemn Simulacrum (2/2): the strict-driver
/// witness — its ETB "you may search" prompts mid-drive.
const SOLEMN_SIMULACRUM_ORACLE: &str = "When this creature enters, you may search your library for a basic land card, put that card onto the battlefield tapped, then shuffle.\nWhen this creature dies, you may draw a card.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

fn green_pool(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

fn add_raggadragga(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Raggadragga, Goreguts Boss", 4, 4, RAGGADRAGGA_ORACLE)
        .with_subtypes(vec!["Human", "Boar"])
        .id()
}

fn add_elves(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, LLANOWAR_ELVES_ORACLE)
        .with_subtypes(vec!["Elf", "Druid"])
        .id()
}

/// Drive to an empty stack, failing on any player prompt (proves no trigger).
/// CR 500.2: the drive passes priority until the stack is empty; an unexpected
/// prompt stops the drive with a non-empty stack or a non-Priority halt, and
/// the completion asserts below fail instead of silently returning.
fn drive_quiet(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "CR 500.2: no prompt expected, so the stack must be empty, got {:?} with {:?}",
        runner.state().stack.len(),
        runner.state().waiting_for
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "CR 500.2: no prompt expected, got {:?}",
        runner.state().waiting_for
    );
}

/// Advance from main-phase priority to `DeclareAttackers`, answering
/// beginning-of-combat triggers in identity order.
fn advance_to_declare_attackers(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering combat triggers should succeed");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance toward declare attackers");
            }
            other => panic!("unexpected waiting_for advancing to declare attackers: {other:?}"),
        }
    }
    panic!("expected DeclareAttackers");
}

/// CR 605.1 + CR 613.4c: the anthem buffs the mana dork (+2/+2), leaves a
/// vanilla creature alone, and does not buff Raggadragga itself (its own
/// abilities are a static plus two triggers — no mana ability).
#[test]
fn raggadragga_anthem_buffs_only_creatures_with_mana_abilities() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ragga = add_raggadragga(&mut scenario);
    let elves = add_elves(&mut scenario);
    let bears = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let mut runner = scenario.build();

    assert_eq!(
        effective_pt(&mut runner, elves),
        (3, 3),
        "Llanowar Elves ({{T}}: Add {{G}}) gets +2/+2: 1/1 -> 3/3"
    );
    assert_eq!(
        effective_pt(&mut runner, bears),
        (2, 2),
        "a vanilla creature must NOT be buffed"
    );
    assert_eq!(
        effective_pt(&mut runner, ragga),
        (4, 4),
        "Raggadragga has no mana ability, so it must NOT buff itself"
    );
}

/// CR 508.1 + CR 603.2: the attacking mana dork untaps; a vanilla attacker
/// stays tapped (which also proves both creatures attacked — the negative's
/// reach-guard).
#[test]
fn raggadragga_untaps_attacking_mana_dork_but_not_vanilla_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_raggadragga(&mut scenario);
    let elves = add_elves(&mut scenario);
    let bears = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[
            (elves, AttackTarget::Player(P1)),
            (bears, AttackTarget::Player(P1)),
        ])
        .expect("both creatures should be legal attackers");
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects[&elves].tapped,
        "the attacking mana dork must be untapped by the trigger"
    );
    assert!(
        runner.state().objects[&bears].tapped,
        "the vanilla attacker stays tapped (and proves it attacked)"
    );
}

/// CR 605.1b + CR 508.1 + CR 603.2: a TRIGGERED mana ability (Kinnan:
/// "whenever you tap a nonland permanent for mana, add one mana ...") counts
/// as "with a mana ability" — Kinnan untaps on attack and gets the anthem.
/// Lotus Cobra (ChangesZone mode + mana body: wrong trigger source) and
/// Treasure Nabber (TapsForMana mode + non-mana body) do NOT count — both stay
/// tapped with unchanged P/T, and each tapped body proves it attacked (the
/// negatives' reach-guards). Raggadragga itself attacks and stays tapped (its
/// own Attacks/SpellCast triggers are not mana abilities).
#[test]
fn raggadragga_counts_triggered_mana_abilities_but_not_lookalikes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ragga = add_raggadragga(&mut scenario);
    let kinnan = scenario
        .add_creature_from_oracle(P0, "Kinnan, Bonder Prodigy", 2, 2, KINNAN_ORACLE)
        .with_subtypes(vec!["Human", "Druid"])
        .id();
    let cobra = scenario
        .add_creature_from_oracle(P0, "Lotus Cobra", 2, 1, LOTUS_COBRA_ORACLE)
        .with_subtypes(vec!["Snake"])
        .id();
    let nabber = scenario
        .add_creature_from_oracle(P0, "Treasure Nabber", 3, 2, TREASURE_NABBER_ORACLE)
        .with_subtypes(vec!["Goblin", "Rogue"])
        .id();
    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner, P0);
    runner
        .declare_attackers(&[
            (ragga, AttackTarget::Player(P1)),
            (kinnan, AttackTarget::Player(P1)),
            (cobra, AttackTarget::Player(P1)),
            (nabber, AttackTarget::Player(P1)),
        ])
        .expect("all four creatures should be legal attackers");
    runner.advance_until_stack_empty();

    assert!(
        !runner.state().objects[&kinnan].tapped,
        "Kinnan's triggered mana ability counts: the attack trigger must untap it"
    );
    assert_eq!(
        effective_pt(&mut runner, kinnan),
        (4, 4),
        "Kinnan 2/2 + 2/2 anthem (same eval arm, static path)"
    );
    assert!(
        runner.state().objects[&cobra].tapped,
        "Lotus Cobra's landfall trigger is not a mana ability (ChangesZone mode \
         observes no mana event); stays tapped (and proves it attacked)"
    );
    assert_eq!(
        effective_pt(&mut runner, cobra),
        (2, 1),
        "no anthem for Lotus Cobra"
    );
    assert!(
        runner.state().objects[&nabber].tapped,
        "Treasure Nabber's trigger adds no mana; stays tapped (and proves it attacked)"
    );
    assert_eq!(
        effective_pt(&mut runner, nabber),
        (3, 2),
        "no anthem for Treasure Nabber"
    );
    assert!(
        runner.state().objects[&ragga].tapped,
        "Raggadragga's own triggers are not mana abilities (and proves it attacked)"
    );
    assert!(
        runner.state().stack.is_empty(),
        "completion: the stack drained with no stray prompt"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "completion: Priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 603.4 + CR 601.2h: casting a seven-mana spell fires the trigger; the
/// targeted tapped dork untaps and becomes 10/10 with trample until end of
/// turn (1/1 base + 2/2 anthem + 7/7 pump). The wurm's own ETB life gain
/// proves the stack drained fully through the production pipeline.
#[test]
fn raggadragga_seven_mana_spell_untaps_and_pumps_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, green_pool(6));
    add_raggadragga(&mut scenario);
    let elves = add_elves(&mut scenario);
    let wurm = scenario
        .add_creature_to_hand(P0, "Pelakka Wurm", 7, 7)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::Green,
                ManaCostShard::Green,
                ManaCostShard::Green,
            ],
            generic: 4,
        })
        .from_oracle_text_with_keywords(&["Trample"], PELAKKA_WURM_ORACLE)
        .id();
    let mut runner = scenario.build();

    // Tap the dork through its own mana ability (production activation path);
    // the seventh green joins the pool-funded six for the 4GGG cast.
    runner.activate(elves, 0).resolve();
    assert!(
        runner.state().objects[&elves].tapped,
        "reach-guard: the dork is tapped before the trigger resolves"
    );

    let life_before = runner.state().players[0].life;
    // The wurm itself takes no targets, so the declared object intent flows
    // to the spell trigger's "target creature" slot (CR 601.2c order).
    let outcome = runner.cast(wurm).target_objects(&[elves]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the trigger, the wurm, and its ETB all resolved, got {:?}",
        outcome.final_waiting_for()
    );

    assert!(
        !runner.state().objects[&elves].tapped,
        "the seven-mana trigger must untap its target"
    );
    assert_eq!(
        effective_pt(&mut runner, elves),
        (10, 10),
        "1/1 base + 2/2 anthem + 7/7 pump"
    );
    assert!(
        runner.state().objects[&elves].has_keyword(&Keyword::Trample),
        "the target must gain trample until end of turn"
    );
    assert!(
        runner.state().battlefield.contains(&wurm),
        "reach-guard: the seven-mana spell resolved"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before + 7,
        "reach-guard: the wurm's ETB resolved, so the stack fully drained"
    );
}

/// The strict-driver proof: resolving Solemn Simulacrum raises its ETB "you may
/// search" OptionalEffectChoice mid-drive, so `drive_quiet` must PANIC on its
/// completion assert instead of silently returning. Guards against
/// reintroducing a wildcard/early-return into the driver. (A TargetSelection
/// prompt cannot serve here: `commit()` announces pending triggers with a
/// first-legal default, so trigger targets never survive to the drive.)
#[test]
#[should_panic(expected = "CR 500.2: no prompt expected")]
fn drive_quiet_panics_on_an_unexpected_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, green_pool(4));
    add_raggadragga(&mut scenario);
    let solemn = scenario
        .add_creature_to_hand(P0, "Solemn Simulacrum", 2, 2)
        .as_artifact_creature()
        .with_subtypes(vec!["Golem"])
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text(SOLEMN_SIMULACRUM_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.cast(solemn).commit();
    drive_quiet(&mut runner);
}

/// CR 603.4: six mana spent is one short of the "at least seven" threshold,
/// so the spell trigger stays silent — the boundary pin from below (seven
/// fires, six does not). Reach-guards mirror the cheap-spell negative.
#[test]
fn raggadragga_silent_for_six_mana_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, green_pool(6));
    add_raggadragga(&mut scenario);
    let elves = add_elves(&mut scenario);
    let six_drop = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(6))
        .id();
    let mut runner = scenario.build();

    runner.cast(six_drop).commit();
    drive_quiet(&mut runner);

    {
        let state = runner.state();
        assert!(state.stack.is_empty());
        assert!(
            state.battlefield.contains(&six_drop),
            "reach-guard: the six-mana spell resolved"
        );
    }
    assert_eq!(
        effective_pt(&mut runner, elves),
        (3, 3),
        "reach-guard: the dork keeps exactly its anthem P/T (no pump)"
    );
}

/// A two-mana spell resolves with no trigger prompt at all (the negative is
/// the over-fire discriminator). Reach-guards: the spell resolved onto the
/// battlefield and the dork keeps exactly its anthem P/T.
#[test]
fn raggadragga_silent_for_cheap_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, green_pool(2));
    add_raggadragga(&mut scenario);
    let elves = add_elves(&mut scenario);
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();

    runner.cast(bears).commit();
    drive_quiet(&mut runner);

    {
        let state = runner.state();
        assert!(state.stack.is_empty());
        assert!(
            state.battlefield.contains(&bears),
            "reach-guard: the two-mana spell resolved"
        );
    }
    assert_eq!(
        effective_pt(&mut runner, elves),
        (3, 3),
        "reach-guard: the dork keeps exactly its anthem P/T (no pump)"
    );
}
