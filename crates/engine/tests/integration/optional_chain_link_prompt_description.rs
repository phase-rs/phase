//! A `WaitingFor::OptionalEffectChoice` raised by a CHAINED link of an ability
//! must carry the ability's printed text (CR 608.2c: every link of a parsed
//! chain is a later instruction of the same printed ability).
//!
//! Isochron Scepter is the reference case. Its activated ability parses to
//! `CopySpell` with a chained `CastFromZone` sub-ability, and BOTH links are
//! optional, so activating it opens two consecutive prompts:
//!
//!   1. "You may copy the exiled card."            — carried the printed text
//!   2. "…you may cast the copy without paying…"   — carried NOTHING
//!
//! The client renders `description` as the dialog's subtitle, so prompt 2 was a
//! bare Yes/No titled only with the source's name — indistinguishable from a
//! stray re-ask of prompt 1. Declining it discards the copy after the {2} and
//! the tap are already spent, and the imprinted spell never asks for a target:
//! from the player's seat, Isochron Scepter taps for nothing.
//!
//! This is a class defect, not a card defect: the parser records the printed
//! text only on a chain's head, and 152 optional chain links in this crate's
//! 4k-card test fixture alone open a prompt from a non-head link.

use engine::game::rehydrate_game_from_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{ExileLink, ExileLinkKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn fund_generic(runner: &mut GameRunner, amount: u32) {
    let dummy = ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

/// Isochron Scepter on the battlefield with `imprinted` linked to it by the
/// CR 406.6 / CR 607.2a exile link its Imprint trigger creates, an opponent's
/// creature to target, and enough mana in pool to activate.
fn imprinted_scepter(
    imprinted: &str,
    db: &engine::database::card_db::CardDatabase,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let scepter = scenario.add_real_card(P0, "Isochron Scepter", Zone::Battlefield, db);
    let exiled = scenario.add_real_card(P0, imprinted, Zone::Exile, db);
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().exile_links.push(ExileLink {
        source_id: scepter,
        exiled_id: exiled,
        kind: ExileLinkKind::TrackedBySource,
    });
    fund_generic(&mut runner, 2);

    runner
        .act(GameAction::ActivateAbility {
            source_id: scepter,
            ability_index: 0,
        })
        .expect("Isochron activation is legal with an imprint and {2} available");

    (runner, scepter, victim)
}

/// Every optional prompt Isochron Scepter's activation raises, in order.
fn optional_prompt_descriptions(runner: &mut GameRunner) -> Vec<Option<String>> {
    let mut descriptions = Vec::new();
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice {
                player,
                description,
                ..
            } => {
                if player == P0 {
                    descriptions.push(description);
                }
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: player == P0,
                    })
                    .expect("answering an optional prompt is legal");
            }
            WaitingFor::CopyRetarget { .. } => return descriptions,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return descriptions,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt: {}", other.variant_name()),
        }
    }
    panic!("Isochron activation did not settle");
}

/// The discriminating assertion. Pre-fix the second description is `None`; the
/// dialog it drives has no subtitle and reads as a duplicate of the first.
#[test]
fn isochron_cast_the_copy_prompt_states_what_it_is_asking() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut runner, _scepter, _victim) = imprinted_scepter("Path to Exile", db);

    let descriptions = optional_prompt_descriptions(&mut runner);

    assert_eq!(
        descriptions.len(),
        2,
        "activation raises the copy prompt and the cast-the-copy prompt"
    );
    for (index, description) in descriptions.iter().enumerate() {
        assert!(
            description.as_deref().is_some_and(|text| !text.is_empty()),
            "optional prompt {index} must tell the player what it is asking, got {description:?}"
        );
    }
}

/// The player-visible consequence the missing text caused: declining the
/// textless second prompt leaves the activation cost spent, no target asked
/// for, and the imprinted spell unresolved. Pins the cost of getting prompt 2
/// wrong so a regression is legible as behavior, not just as a string.
#[test]
fn declining_the_cast_the_copy_prompt_still_spends_the_activation_cost() {
    let Some(db) = load_db() else {
        return;
    };
    let (mut runner, scepter, victim) = imprinted_scepter("Path to Exile", db);

    let mut answered = 0;
    let mut asked_for_a_target = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                // Accept the copy, decline the cast — what a player does when
                // the second dialog says nothing.
                let accept = player == P0 && answered == 0;
                if player == P0 {
                    answered += 1;
                }
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answering an optional prompt is legal");
            }
            WaitingFor::CopyRetarget { .. } => {
                asked_for_a_target = true;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(victim)),
                    })
                    .expect("choosing the copy's target is legal");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt: {}", other.variant_name()),
        }
    }

    assert!(
        !asked_for_a_target,
        "a declined cast never announces targets (CR 601.2c)"
    );
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "the imprinted Path to Exile never resolved"
    );
    assert!(
        runner.state().objects[&scepter].tapped,
        "the tap cost was still paid"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the two generic mana were still spent"
    );
}

/// Corpus invariant: no chained link that can raise its own prompt may resolve
/// with an empty description while its chain head has one.
///
/// This is the class the Isochron Scepter tests above are one member of. It
/// walks every card face in the fixture, builds each printed ability through the
/// production constructor (`build_resolved_from_def` — the same call the
/// activation, cast and trigger paths make), and checks every link of every
/// resolved chain. Before the backfill, 150+ faces here failed it.
#[test]
fn no_prompt_raising_chain_link_resolves_without_a_description() {
    let Some(db) = load_db() else {
        return;
    };

    let mut offenders: Vec<String> = Vec::new();
    for face in db.faces_in_scan_order() {
        // A trigger's printed line is the text for its execute chain: the
        // materialization sites in `triggers.rs` stamp it on the head and then
        // push it down. Reproduce that pairing so trigger chains are checked
        // under the same text they get at runtime.
        let definitions = face
            .abilities
            .iter()
            .map(|definition| (definition, None))
            .chain(face.triggers.iter().filter_map(|trigger| {
                Some((trigger.execute.as_deref()?, trigger.description.clone()))
            }));

        for (definition, trigger_line) in definitions {
            let mut resolved =
                engine::game::ability_utils::build_resolved_from_def(definition, ObjectId(1), P0);
            if resolved.description.is_none() {
                resolved.description = trigger_line;
                resolved.backfill_chain_description();
            }
            if resolved.description.is_none() {
                // Nothing printed to inherit — out of this invariant's scope.
                continue;
            }

            let mut links = Vec::new();
            if let Some(sub_ability) = resolved.sub_ability.as_deref() {
                links.push((sub_ability, 0));
            }
            if let Some(else_ability) = resolved.else_ability.as_deref() {
                links.push((else_ability, 0));
            }
            while let Some((current, depth)) = links.pop() {
                if current.optional && current.description.is_none() {
                    offenders.push(format!("{} (link depth {depth})", face.name));
                    break;
                }
                if let Some(sub_ability) = current.sub_ability.as_deref() {
                    links.push((sub_ability, depth + 1));
                }
                if let Some(else_ability) = current.else_ability.as_deref() {
                    links.push((else_ability, depth + 1));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "{} card faces raise an optional prompt from a chain link with no text: {:?}",
        offenders.len(),
        &offenders[..offenders.len().min(20)]
    );
}
