//! Production-path regression for issue #6009 / PR #6058: Sakashima of a
//! Thousand Faces ("You may have Sakashima enter as a copy of another
//! creature you control, except it has Sakashima's other abilities.") must
//! retain Sakashima's ENTIRE other-ability surface (CR 707.9a) — here,
//! Partner and the controller-scoped "legend rule doesn't apply" static —
//! on the copy, not just a single indexed ability.
//!
//! Unlike `become_copy.rs::become_copy_retains_all_other_abilities_from_source`
//! (a hand-built `ResolvedAbility`) this drives the real production pipeline:
//! Sakashima's Oracle text is parsed by the actual parser (via
//! `CardDatabase::from_mtgjson`), cast from hand, and resolved through the
//! `AsPermanentEnters` replacement-choice/copy-target-choice flow so Layer 1
//! (`RetainAllOtherAbilitiesFromSource`) is exercised end to end. It then
//! proves the retained legend-rule exemption FUNCTIONS (not merely that the
//! static is present): a second legendary permanent under the same
//! controller is not forced through CR 704.5j's `ChooseLegend` prompt while
//! the copy's retained static is active.

use std::path::Path;
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use engine::game::sba::{check_state_based_actions, legend_rule_exempt};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::process_triggers;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::keywords::{Keyword, PartnerType};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn sakashima_db() -> &'static CardDatabase {
    static DB: OnceLock<CardDatabase> = OnceLock::new();
    DB.get_or_init(|| {
        CardDatabase::from_mtgjson(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json"),
        )
        .expect("parser fixture must contain Sakashima of a Thousand Faces")
    })
}

fn add_mana(runner: &mut GameRunner, mana: &[ManaType]) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for m in mana {
        pool.add(ManaUnit::new(*m, dummy, false, vec![]));
    }
}

fn drive_until_stack_empty(runner: &mut GameRunner) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("accept enter-as-copy replacement");
            }
            WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                let target = valid_targets[0];
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose copy target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional copy");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner.choose_first_legal_target().expect("choose target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pay mana");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting_for while resolving: {other:?}"),
        }
    }
    panic!("resolution loop exhausted");
}

#[test]
fn sakashima_becomes_copy_and_retains_partner_and_legend_rule_exemption() {
    let db = sakashima_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let jace_incumbent =
        scenario.add_real_card(P0, "Jace, the Mind Sculptor", Zone::Battlefield, db);
    let sakashima = scenario.add_real_card(P0, "Sakashima of a Thousand Faces", Zone::Hand, db);
    let jace_challenger = scenario.add_real_card(P0, "Jace, the Mind Sculptor", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // {3}{U} for Sakashima.
    add_mana(
        &mut runner,
        &[
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    let sakashima_card_id = runner.state().objects[&sakashima].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: sakashima,
            card_id: sakashima_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Sakashima of a Thousand Faces");

    drive_until_stack_empty(&mut runner);

    let copy_id = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            *id != bears && {
                let obj = &runner.state().objects[id];
                obj.name == "Grizzly Bears" && obj.controller == P0
            }
        })
        .expect("Sakashima should enter as a copy of Grizzly Bears");

    let copy = runner.state().objects.get(&copy_id).unwrap();
    // The copy target's characteristics (name, P/T) are copied normally.
    assert_eq!(copy.name, "Grizzly Bears");
    assert_eq!(copy.power, Some(2));
    assert_eq!(copy.toughness, Some(2));
    // Sakashima's own other abilities survive the copy (CR 707.9a).
    assert!(
        copy.keywords
            .contains(&Keyword::Partner(PartnerType::Generic)),
        "Partner keyword must be retained on the copy; got {:?}",
        copy.keywords
    );
    assert!(
        legend_rule_exempt(runner.state(), copy_id),
        "the copy's own legend-rule exemption static must report itself exempt"
    );

    // Functional proof, not just structural: the retained static must exempt
    // *other* legendary permanents this controller controls (CR 704.5j), even
    // though the exemption's source object is no longer named "Sakashima".
    assert!(
        legend_rule_exempt(runner.state(), jace_incumbent),
        "the retained controller-scoped exemption must cover this player's other legendaries"
    );

    // {2}{U}{U} for the second Jace.
    add_mana(
        &mut runner,
        &[
            ManaType::Blue,
            ManaType::Blue,
            ManaType::Colorless,
            ManaType::Colorless,
        ],
    );
    let jace_card_id = runner.state().objects[&jace_challenger].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: jace_challenger,
            card_id: jace_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the second Jace, the Mind Sculptor");

    drive_until_stack_empty(&mut runner);

    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::ChooseLegend { .. }),
        "the retained legend-rule exemption must suppress CR 704.5j's choose-legend prompt"
    );
    assert_eq!(
        runner.state().objects[&jace_incumbent].zone,
        Zone::Battlefield,
        "the incumbent Jace must survive the legend-rule SBA sweep"
    );
    assert_eq!(
        runner.state().objects[&jace_challenger].zone,
        Zone::Battlefield,
        "the second Jace must survive the legend-rule SBA sweep"
    );
}
