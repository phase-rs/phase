//! Printed-card regressions for the count and entry riders of token append replacements.

use engine::game::combat::AttackTarget;
use engine::game::game_object::GameObject;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::parse_counter_type;
use engine::types::game_state::{ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ALLENAL: &str = "Queen Allenal of Ruadach";
const CHATTERFANG: &str = "Chatterfang, Squirrel General";
const DOUBLING_SEASON: &str = "Doubling Season";
const RAISE: &str = "Raise the Alarm";
const BESTIAL: &str = "Bestial Menace";
const BESTIAL_ORACLE: &str =
    "Create a 1/1 green Snake creature token, a 2/2 green Wolf creature token, and a 3/3 green Elephant creature token.";
const FAE_OFFERING_ORACLE: &str =
    "At the beginning of each end step, if you've cast both a creature spell and a noncreature spell this turn, create a Clue token, a Food token, and a Treasure token.";
const BANKBUSTER_ORACLE: &str = concat!(
    "This Vehicle enters with three charge counters on it.\n",
    "{2}, {T}, Remove a charge counter from this Vehicle: Draw a card. Then if there are no charge counters on this Vehicle, create a Treasure token and a 1/1 colorless Pilot creature token with \"This token crews Vehicles as though its power were 2 greater.\"\n",
    "Crew 3",
);
const FARMER_COTTON: &str = "Farmer Cotton";
const FARMER_COTTON_ORACLE: &str = r#"When this creature enters, create X 1/1 white Halfling creature tokens and X Food tokens. (They're artifacts with "{2}, {T}, Sacrifice this token: You gain 3 life.")"#;
const MANUFACTOR: &str = "Academy Manufactor";
const VISITATION: &str = "Divine Visitation";
const HALVING: &str = "Halving Season";
const BESTIAL_MANA: &[ManaType] = &[
    ManaType::Green,
    ManaType::Green,
    ManaType::Colorless,
    ManaType::Colorless,
    ManaType::Colorless,
];

fn tokens(runner: &GameRunner, owner: PlayerId) -> Vec<&GameObject> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .map(|id| &state.objects[id])
        .filter(|object| object.is_token && object.controller == owner)
        .collect()
}

fn assert_tokens(runner: &GameRunner, owner: PlayerId, counts: &[(&str, usize)]) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let tokens = tokens(runner, owner);
    assert_eq!(
        tokens.len(),
        counts.iter().map(|(_, count)| count).sum::<usize>()
    );
    for (subtype, count) in counts {
        assert_eq!(
            tokens
                .iter()
                .filter(|object| { object.card_types.subtypes.iter().any(|ty| ty == subtype) })
                .count(),
            *count,
            "count of {subtype} tokens"
        );
    }
}

fn add_bestial_menace(scenario: &mut GameScenario, caster: PlayerId, oracle: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(caster, BESTIAL, false, oracle)
        .with_mana_cost(parse_mtgjson_mana_cost("{3}{G}{G}"))
        .id()
}

fn give_bestial_mana(scenario: &mut GameScenario, casts: usize) {
    scenario.with_mana_pool(
        P0,
        BESTIAL_MANA
            .iter()
            .cycle()
            .take(BESTIAL_MANA.len() * casts)
            .map(|&color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
}

fn cast_board(
    replacement_sources: &[&str],
    caster: PlayerId,
    spell: &str,
    mana: &[ManaType],
) -> (GameRunner, ObjectId) {
    let db = crate::support::shared_card_db().expect("curated card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in replacement_sources {
        scenario.add_real_card(P0, name, Zone::Battlefield, db);
    }
    let spell = match spell {
        BESTIAL => add_bestial_menace(&mut scenario, caster, BESTIAL_ORACLE),
        FARMER_COTTON => scenario
            .add_creature_to_hand_from_oracle(caster, FARMER_COTTON, 1, 1, FARMER_COTTON_ORACLE)
            .with_mana_cost(parse_mtgjson_mana_cost("{X}{G}{W}"))
            .id(),
        _ => scenario.add_real_card(caster, spell, Zone::Hand, db),
    };
    scenario.with_mana_pool(
        caster,
        mana.iter()
            .map(|color| ManaUnit::new(*color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    if caster == P1 {
        runner
            .act(GameAction::PassPriority)
            .expect("P1 gains priority");
    }
    (runner, spell)
}

#[test]
fn printed_appenders_apply_their_count_and_owner_or_type_gates() {
    use ManaType::{Colorless, Green, Red, White};
    // CR 614.1a + CR 614.5: one replacement opportunity modifies this creation.
    let rows: &[(_, _, _, &[_], &[_])] = &[
        (
            ALLENAL,
            P0,
            "Sprout",
            &[Green],
            &[("Saproling", 1), ("Soldier", 1)],
        ),
        (ALLENAL, P0, RAISE, &[White, Colorless], &[("Soldier", 3)]),
        (ALLENAL, P0, "Strike It Rich", &[Red], &[("Treasure", 1)]),
        (ALLENAL, P1, RAISE, &[White, Colorless], &[("Soldier", 2)]),
        (
            CHATTERFANG,
            P0,
            RAISE,
            &[White, Colorless],
            &[("Soldier", 2), ("Squirrel", 2)],
        ),
        (
            "Peregrin Took",
            P0,
            RAISE,
            &[White, Colorless],
            &[("Soldier", 2), ("Food", 1)],
        ),
        (
            "Worldwalker Helm",
            P0,
            "Strike It Rich",
            &[Red],
            &[("Treasure", 1), ("Map", 1)],
        ),
        (
            "Worldwalker Helm",
            P0,
            RAISE,
            &[White, Colorless],
            &[("Soldier", 2)],
        ),
    ];
    for &(source, caster, spell, mana, expected) in rows {
        let (mut runner, spell) = cast_board(&[source], caster, spell, mana);
        runner.cast(spell).resolve();
        assert_tokens(&runner, caster, expected);
        if caster == P1 {
            assert_tokens(&runner, P0, &[]);
        }
    }
}

fn choose_append_order(other: &str, first: &str) -> GameRunner {
    let (mut runner, spell) = cast_board(
        &[ALLENAL, other],
        P0,
        RAISE,
        &[ManaType::White, ManaType::Colorless],
    );
    runner.cast(spell).resolve();
    answer_append_order(&mut runner, other, first);
    runner
}

fn answer_append_order(runner: &mut GameRunner, other: &str, first: &str) {
    answer_replacement_order(runner, [ALLENAL, other], first);
}

fn answer_replacement_order(runner: &mut GameRunner, sources: [&str; 2], first: &str) {
    let WaitingFor::ReplacementChoice {
        player,
        kind,
        candidates,
        ..
    } = &runner.state().waiting_for
    else {
        let token_names: Vec<_> = tokens(runner, P0).iter().map(|obj| &obj.name).collect();
        panic!(
            "expected replacement ordering for {sources:?}; waiting for {:?}, stack has {} item(s), tokens {token_names:?}",
            runner.state().waiting_for,
            runner.state().stack.len()
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*kind, ReplacementChoiceKind::Order);
    assert_eq!(candidates.len(), 2);
    assert!(sources
        .iter()
        .all(|name| candidates.iter().any(|c| c.source_name == *name)));
    let index = candidates
        .iter()
        .position(|c| c.source_name == first)
        .unwrap();
    runner
        .act(GameAction::ChooseReplacement { index })
        .expect("apply the chosen replacement");
    runner.advance_until_stack_empty();
}

#[test]
fn allenal_and_doubling_season_order_changes_the_soldier_count() {
    // CR 616.1e-f: double then append is 2 × 2 + 1; append then double is (2 + 1) × 2.
    assert_tokens(
        &choose_append_order(DOUBLING_SEASON, DOUBLING_SEASON),
        P0,
        &[("Soldier", 5)],
    );
    assert_tokens(
        &choose_append_order(DOUBLING_SEASON, ALLENAL),
        P0,
        &[("Soldier", 6)],
    );
}

#[test]
fn allenal_and_chatterfang_order_changes_the_admitted_count() {
    // CR 616.1f: Chatterfang reads the current event, which can already include
    // Allenal's Soldier. Allenal still adds one if Chatterfang has gone first.
    assert_tokens(
        &choose_append_order(CHATTERFANG, ALLENAL),
        P0,
        &[("Soldier", 3), ("Squirrel", 3)],
    );
    assert_tokens(
        &choose_append_order(CHATTERFANG, CHATTERFANG),
        P0,
        &[("Soldier", 3), ("Squirrel", 2)],
    );
}

#[test]
fn allenal_adds_one_soldier_to_bestial_menace() {
    let (mut runner, spell) = cast_board(&[ALLENAL], P0, BESTIAL, BESTIAL_MANA);
    runner.cast(spell).resolve();
    assert_tokens(
        &runner,
        P0,
        &[("Snake", 1), ("Wolf", 1), ("Elephant", 1), ("Soldier", 1)],
    );
}

#[test]
fn bankbuster_appenders_use_the_admitted_batches() {
    let db = crate::support::shared_card_db().expect("curated card fixture");
    for (source, extra, count) in [(ALLENAL, "Soldier", 1), (CHATTERFANG, "Squirrel", 2)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, source, Zone::Battlefield, db);
        let bankbuster = scenario
            .add_artifact_from_oracle(P0, "Reckoner Bankbuster", BANKBUSTER_ORACLE)
            .with_subtypes(vec!["Vehicle"])
            .id();
        scenario.add_real_card(P0, "Island", Zone::Library, db);
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ],
        );
        let mut runner = scenario.build();
        // Use the last-charge state to exercise the conditional list.
        runner
            .state_mut()
            .objects
            .get_mut(&bankbuster)
            .unwrap()
            .counters
            .insert(parse_counter_type("charge"), 1);
        runner.activate(bankbuster, 0).resolve();
        assert_tokens(
            &runner,
            P0,
            &[("Treasure", 1), ("Pilot", 1), (extra, count)],
        );
        assert_eq!(runner.state().last_created_token_ids.len(), 2 + count);
    }
}

#[test]
fn bestial_menace_order_survives_serialization_and_publishes_all_created_ids() {
    for (first, soldiers) in [(DOUBLING_SEASON, 1), (ALLENAL, 2)] {
        let db = crate::support::shared_card_db().expect("curated card fixture");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, ALLENAL, Zone::Battlefield, db);
        scenario.add_real_card(P0, DOUBLING_SEASON, Zone::Battlefield, db);
        // Use Scout for Survivors' printed counter instruction as a test-local
        // follow-up to Bestial Menace's list.
        let oracle = format!("{BESTIAL_ORACLE} Put a +1/+1 counter on each of them.");
        let spell = add_bestial_menace(&mut scenario, P0, &oracle);
        give_bestial_mana(&mut scenario, 1);
        let mut runner = scenario.build();
        runner.cast(spell).resolve();
        let pending = &runner
            .state()
            .pending_replacement
            .as_ref()
            .expect("creation is still pending")
            .proposed;
        let views = pending.clone().into_token_batch_views();
        assert_eq!(views.len(), 3);
        assert!(views
            .iter()
            .all(|view| view.create_token_count() == Some(1) && view.applied_set().is_empty()));
        assert!(tokens(&runner, P0).is_empty());

        let bytes = serde_json::to_vec(runner.state()).expect("serialize parked state");
        let mut runner =
            GameRunner::from_state(serde_json::from_slice(&bytes).expect("restore parked state"));
        let restored = &runner
            .state()
            .pending_replacement
            .as_ref()
            .unwrap()
            .proposed;
        assert_eq!(restored, pending);
        answer_append_order(&mut runner, DOUBLING_SEASON, first);

        // CR 616.1e-f: double first gives 3 × 2 + 1. Append first gives (3 + 1) × 2.
        assert_tokens(
            &runner,
            P0,
            &[
                ("Snake", 2),
                ("Wolf", 2),
                ("Elephant", 2),
                ("Soldier", soldiers),
            ],
        );
        let mut published = runner.state().last_created_token_ids.clone();
        published.sort_unstable_by_key(|id| id.0);
        let mut actual: Vec<_> = tokens(&runner, P0).iter().map(|obj| obj.id).collect();
        actual.sort_unstable_by_key(|id| id.0);
        assert_eq!(published, actual);
        for id in actual {
            assert_eq!(
                runner.state().objects[&id]
                    .counters
                    .get(&parse_counter_type("+1/+1")),
                Some(&2),
                "the follow-up puts one counter on {id:?}, doubled by Doubling Season",
            );
        }
    }
}

#[test]
fn farmer_cotton_resolves_x_for_both_members() {
    use ManaType::{Colorless, Green, White};
    let (mut runner, spell) = cast_board(
        &[],
        P0,
        FARMER_COTTON,
        &[Green, White, Colorless, Colorless, Colorless],
    );
    runner.cast(spell).x(3).resolve();
    runner.advance_until_stack_empty();
    assert_tokens(&runner, P0, &[("Halfling", 3), ("Food", 3)]);
    assert_eq!(runner.state().last_created_token_ids.len(), 6);
}

#[test]
fn fae_offering_and_manufactor_create_three_of_each_in_one_batch() {
    let runner = resolve_fae_offering(None);
    assert_tokens(&runner, P0, &[("Clue", 3), ("Food", 3), ("Treasure", 3)]);
    assert_eq!(runner.state().last_created_token_ids.len(), 9);
}

#[test]
fn manufactor_and_chatterfang_order_changes_the_squirrel_count() {
    for (first, squirrels) in [(MANUFACTOR, 9), (CHATTERFANG, 3)] {
        let runner = resolve_fae_offering(Some(first));
        assert_tokens(
            &runner,
            P0,
            &[
                ("Clue", 3),
                ("Food", 3),
                ("Treasure", 3),
                ("Squirrel", squirrels),
            ],
        );
    }
}

fn resolve_fae_offering(first: Option<&str>) -> GameRunner {
    use ManaType::{Colorless, Red};
    let db = crate::support::shared_card_db().expect("curated card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Fae Offering", FAE_OFFERING_ORACLE);
    let manufactor = scenario.add_real_card(P0, MANUFACTOR, Zone::Hand, db);
    if first.is_some() {
        scenario.add_real_card(P0, CHATTERFANG, Zone::Battlefield, db);
    }
    let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    scenario.with_mana_pool(
        P0,
        [Colorless, Colorless, Colorless, Red]
            .into_iter()
            .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut runner = scenario.build();
    runner.cast(manufactor).resolve();
    runner.cast(bolt).target_player(P1).resolve();
    runner.advance_to_phase(Phase::End);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner.declare_attackers(&[]).expect("skip combat");
        runner.advance_to_phase(Phase::End);
    }
    assert_eq!(runner.state().phase, Phase::End);
    while !runner.state().stack.is_empty()
        && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
    {
        runner
            .act(GameAction::PassPriority)
            .expect("resolve Fae Offering's end step trigger");
    }
    if let Some(first) = first {
        answer_replacement_order(&mut runner, [MANUFACTOR, CHATTERFANG], first);
    }
    runner
}

#[test]
fn halving_season_counts_angels_after_divine_visitation() {
    let db = crate::support::shared_card_db().expect("curated card fixture");
    for (first, angels) in [(VISITATION, 1), (HALVING, 0)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, VISITATION, Zone::Battlefield, db);
        scenario.add_real_card(P1, HALVING, Zone::Battlefield, db);
        let spell = add_bestial_menace(&mut scenario, P0, BESTIAL_ORACLE);
        give_bestial_mana(&mut scenario, 1);
        let mut runner = scenario.build();
        runner.cast(spell).resolve();
        answer_replacement_order(&mut runner, [VISITATION, HALVING], first);
        assert_tokens(&runner, P0, &[("Angel", angels)]);
    }
}

#[test]
fn esix_offers_one_substitution_for_all_of_bestial_menace() {
    let db = crate::support::shared_card_db().expect("curated card fixture");
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Esix, Fractal Bloom", Zone::Battlefield, db);
        let ox = scenario.add_creature(P0, "Decoy Ox", 5, 4).id();
        let first = add_bestial_menace(&mut scenario, P0, BESTIAL_ORACLE);
        let second = add_bestial_menace(&mut scenario, P0, BESTIAL_ORACLE);
        give_bestial_mana(&mut scenario, 2);
        let mut runner = scenario.build();
        runner.cast(first).commit().stop_at_optional().resolve();
        assert!(
            matches!(&runner.state().waiting_for, WaitingFor::ReplacementChoice { player, candidates, .. }
            if *player == P0 && candidates.iter().any(|c| c.source_name == "Esix, Fractal Bloom"))
        );
        runner
            .act(GameAction::ChooseReplacement {
                index: usize::from(!accept),
            })
            .unwrap();
        if accept {
            assert!(
                matches!(&runner.state().waiting_for, WaitingFor::CopyTargetChoice { valid_targets, .. }
                if valid_targets.contains(&ox))
            );
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(ox)),
                })
                .unwrap();
            runner.advance_until_stack_empty();
            assert_eq!(tokens(&runner, P0).len(), 3);
            assert!(tokens(&runner, P0).iter().all(|obj| obj.name == "Decoy Ox"));
        } else {
            assert_tokens(&runner, P0, &[("Snake", 1), ("Wolf", 1), ("Elephant", 1)]);
        }

        runner.cast(second).commit().stop_at_optional().resolve();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        let originals = if accept { 1 } else { 2 };
        for subtype in ["Snake", "Wolf", "Elephant"] {
            assert_eq!(
                tokens(&runner, P0)
                    .iter()
                    .filter(|obj| obj.card_types.subtypes.iter().any(|ty| ty == subtype))
                    .count(),
                originals
            );
        }
    }
}

#[test]
fn peregrin_appends_once_to_each_separate_create_instruction() {
    use ManaType::{Blue, Colorless, Red};

    // CR 608.2c + CR 614.5: Frost Fair Lure Fish instructs the controller
    // to create two Fish and separately create two tapped Treasures. Each
    // creation gives Peregrin an opportunity to add one Food.
    let (mut runner, spell) = cast_board(
        &["Peregrin Took"],
        P0,
        "Frost Fair Lure Fish",
        &[
            Blue, Red, Colorless, Colorless, Colorless, Colorless, Colorless,
        ],
    );
    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    assert_tokens(&runner, P0, &[("Fish", 2), ("Treasure", 2), ("Food", 2)]);
    assert!(tokens(&runner, P0)
        .iter()
        .filter(|object| object.card_types.subtypes.iter().any(|ty| ty == "Treasure"))
        .all(|object| object.tapped));
}

#[test]
fn allenals_extra_soldier_inherits_the_attacking_instruction() {
    let db = crate::support::shared_card_db().expect("curated card fixture");
    for (name, tapped) in [
        ("Skyknight Vanguard", true),
        ("Brimaz, King of Oreskos", false),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, ALLENAL, Zone::Battlefield, db);
        let attacker = scenario.add_real_card(P0, name, Zone::Battlefield, db);
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
            .expect("declare the printed attacker");
        runner.advance_until_stack_empty();

        // CR 508.4 + CR 110.5b + CR 614.6: both tokens enter attacking.
        // Skyknight Vanguard's instruction additionally specifies tapped.
        assert_tokens(&runner, P0, &[("Soldier", 2)]);
        let combat = runner.state().combat.as_ref().expect("live combat");
        for token in tokens(&runner, P0) {
            assert_eq!(token.tapped, tapped, "{name}: {}", token.name);
            assert!(
                combat.attackers.iter().any(|entry| {
                    entry.object_id == token.id && entry.attack_target == AttackTarget::Player(P1)
                }),
                "{name}: {} must be attacking P1",
                token.name
            );
        }
    }
}
