//! Blocker-3 regression: each parked batch member resumes with its OWN damage
//! source (CR 120.1), never the parent trigger's latch (CR 608.2h).
//!
//! Sarkhan the Masterless's static trigger ("Whenever a creature attacks you or
//! a planeswalker you control, each Dragon you control deals 1 damage to that
//! creature") resolves through the shared noncombat batch. When the first
//! Dragon's damage pauses for a replacement ordering choice, the remaining
//! members park as raw `DealDamage` continuations. Those nodes must rebuild
//! their `DamageContext` from their own source id: Dragonlord Dromoka's hit
//! must carry lifelink (CR 702.15b) even though the trigger latch names the
//! attacking creature.
//!
//! Reverting the fix (parked members inherit the parent latch) rebuilds
//! Dromoka's hit with the attacker's characteristics — no lifelink, no life
//! gain — and the final life assertion fails.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;

// Verbatim Oracle texts (Scryfall named-card API).
const SARKHAN: &str = "Whenever a creature attacks you or a planeswalker you control, each Dragon you control deals 1 damage to that creature.\n+1: Until end of turn, each planeswalker you control becomes a 4/4 red Dragon creature and gains flying.\n\u{2212}3: Create a 4/4 red Dragon creature token with flying.";
const SHIVAN: &str = "Flying\n{R}: This creature gets +1/+0 until end of turn.";
const DROMOKA: &str = "This spell can't be countered.\nFlying, lifelink\nYour opponents can't cast spells during your turn.";
const JADE: &str = "{1}: The next time a source of your choice would deal damage to target creature this turn, that source deals that damage to you instead.";

fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    for _ in 0..count {
        runner
            .state_mut()
            .add_mana_to_pool(player, ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
}

fn priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..runner.state().players.len() {
        match runner.state().waiting_for {
            WaitingFor::Priority { player: current } if current == player => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            ref other => panic!("expected priority, got {other:?}"),
        }
    }
    panic!("priority did not reach {player:?}");
}

fn life_of(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .life
}

/// Activate one Jade Monolith naming `source`, shielding `protected`: mirrors
/// the `latched_redirect_controller` driving (announce + DamageSourceChoice).
fn install_monolith_redirect(
    runner: &mut GameRunner,
    jade: ObjectId,
    protected: ObjectId,
    source: ObjectId,
    shields_before: usize,
) {
    priority(runner, P0);
    runner
        .act(GameAction::ActivateAbility {
            source_id: jade,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("announce the Monolith activation");
    let WaitingFor::TargetSelection { .. } = runner.state().waiting_for else {
        panic!(
            "expected Monolith target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(protected)),
        })
        .expect("target the attacker");
    for _ in 0..16 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DamageSourceChoice { .. }
        ) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "Monolith must reach its source choice"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("resolve Monolith through priority passes");
    }
    let WaitingFor::DamageSourceChoice {
        player, options, ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected Monolith source choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert!(
        options.contains(&source),
        "the first Dragon must be a legal chosen source"
    );
    runner
        .act(GameAction::ChooseDamageSource { source })
        .expect("resume Monolith with the selected source");
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().objects[&protected]
            .replacement_definitions
            .len(),
        shields_before + 1,
        "one redirect shield installs per activation"
    );
}

#[test]
fn sarkhan_second_dragon_keeps_own_lifelink_through_replacement_pause() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let sarkhan = scenario
        .add_planeswalker_from_oracle(P0, "Sarkhan the Masterless", "Sarkhan", 5, SARKHAN)
        .id();
    // Battlefield order decides batch order: the plain Dragon is member 0 (it
    // pauses), Dromoka is member 1 (it resumes through a parked raw node).
    let plain = {
        // `from_oracle` parses rules text only: the Dragon subtype comes from
        // the type line, so it is stamped explicitly (Shivan Dragon is a
        // Creature — Dragon).
        let mut b = scenario.add_creature_from_oracle(P0, "Shivan Dragon", 5, 5, SHIVAN);
        b.with_subtypes(vec!["Dragon"]);
        b.id()
    };
    let mut dromoka_builder = scenario.add_creature(P0, "Dragonlord Dromoka", 5, 7);
    dromoka_builder.with_subtypes(vec!["Dragon"]);
    dromoka_builder.from_oracle_text_with_keywords(&["Flying", "Lifelink"], DROMOKA);
    let dromoka = dromoka_builder.id();
    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let jade_a = scenario
        .add_artifact_from_oracle(P0, "Jade Monolith", JADE)
        .id();
    let jade_b = scenario
        .add_artifact_from_oracle(P0, "Jade Monolith", JADE)
        .id();

    let mut runner = scenario.build();

    // SHAPE reach guards: the static trigger, both Dragons, and both
    // activations must exist before combat proves anything about resume.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&sarkhan)
            .unwrap()
            .trigger_definitions
            .iter_all()
            .count(),
        1,
        "Sarkhan's attack trigger must parse"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&plain)
            .unwrap()
            .card_types
            .subtypes
            .contains(&"Dragon".to_string()),
        "Shivan Dragon must be a Dragon"
    );
    assert!(
        engine::game::keywords::object_has_effective_keyword_kind(
            runner.state(),
            dromoka,
            KeywordKind::Lifelink,
        ),
        "Dromoka must have lifelink before combat"
    );
    assert!(
        !engine::game::keywords::object_has_effective_keyword_kind(
            runner.state(),
            plain,
            KeywordKind::Lifelink,
        ),
        "the plain Dragon must not have lifelink (discriminating member)"
    );
    for jade in [jade_a, jade_b] {
        let abilities = &runner.state().objects.get(&jade).unwrap().abilities;
        assert_eq!(abilities.len(), 1);
        assert_eq!(abilities[0].kind, AbilityKind::Activated);
        assert!(matches!(
            abilities[0].effect.as_ref(),
            Effect::CreateDamageReplacement { .. }
        ));
    }

    let p0_life = life_of(&runner, P0);
    runner.state_mut().active_player = P1;

    // Two redirects on the FIRST Dragon's damage force a CR 616.1 ordering
    // choice mid-batch: the real pause the parked member must survive.
    add_mana(&mut runner, P0, ManaType::Colorless, 2);
    install_monolith_redirect(&mut runner, jade_a, attacker, plain, 0);
    install_monolith_redirect(&mut runner, jade_b, attacker, plain, 1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("declare attackers");

    // Drive the Sarkhan trigger to resolution; it must pause for the ordering
    // choice (never sail through: that would skip the parked resume path).
    for _ in 0..32 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        assert!(
            !runner.state().stack.is_empty(),
            "the trigger must pause for its replacement choice"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward the pause");
    }
    let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for else {
        panic!(
            "expected the batch ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player, P1,
        "CR 616.1: the damaged creature's controller orders"
    );
    // Reach guard: the pause belongs to member 0 (the plain Dragon) with both
    // redirects competing — Dromoka is the unprocessed member this test is about.
    let pending = runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("a pending replacement backs the prompt");
    let ProposedEvent::Damage { source_id, .. } = &pending.proposed else {
        panic!(
            "expected the paused damage event, got {:?}",
            pending.proposed
        );
    };
    assert_eq!(*source_id, plain);
    assert_eq!(
        pending.candidates.len(),
        2,
        "both Monolith redirects must compete (ordering, not optional)"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("order the first redirect first");
    runner.advance_until_stack_empty();

    // Pass the post-trigger priority round into the declare-blockers step.
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass into declare blockers");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ),
        "combat waits for blockers after the trigger, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the trigger fully resolves"
    );

    // CR 702.15b: Dromoka's resumed hit gains 1 life, cancelling the redirected
    // 1 the first Dragon dealt to P0. Without per-member source identity the
    // hit rebuilds from the trigger latch (the attacker: no lifelink) and P0
    // ends one life short.
    assert_eq!(life_of(&runner, P0), p0_life);
    assert_eq!(
        runner.state().objects.get(&attacker).unwrap().damage_marked,
        1,
        "only Dromoka's 1 reaches the attacker; member 0 redirected to P0"
    );
}
