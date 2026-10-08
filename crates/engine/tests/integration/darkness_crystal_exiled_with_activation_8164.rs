//! Discriminating regression for issue #8164 — **The Darkness Crystal**:
//!
//! > Black spells you cast cost {1} less to cast.
//! > If a nontoken creature an opponent controls would die, instead exile it
//! > and you gain 2 life.
//! > {4}{B}{B}, {T}: Put target creature card exiled with The Darkness Crystal
//! > onto the battlefield tapped under your control with two additional
//! > +1/+1 counters on it.
//!
//! Both the replacement effect and the activated ability already parse to the
//! fully-correct AST shape (see `parser/oracle_replacement.rs`'s
//! `the_darkness_crystal_prefix_instead_exile_it` and `parser/oracle_target.rs`'s
//! `target creature card exiled with ~` coverage) — this is a *runtime* bug,
//! not a parser gap. The replacement redirects the dying creature to Exile
//! correctly, but the engine never recorded an `ExileLink` from the exiled
//! card to The Darkness Crystal, so the activated ability's
//! `TargetFilter::ExiledBySource` had nothing to find: no card was ever a
//! legal target.
//!
//! ROOT CAUSE: `game/sba.rs`'s replacement-consulted death delivery (and the
//! sibling paths in `game/effects/destroy.rs` / `game/sacrifice.rs`) has no
//! `ResolvedAbility` in scope, so it cannot call
//! `exile_links::should_track_exiled_by_source` the way the ability-chain
//! path (`game/effects/change_zone.rs`) does. It relied entirely on
//! `ProposedEvent::ZoneChange.cause` to attribute the redirect to a source,
//! but `replacement::apply_single_replacement`'s zone-redirect application
//! never stamped `cause` with the redirecting replacement's own source
//! (`rid.source`) — so the shared delivery tail's `cause.or(source_id)`
//! always resolved to the replacement's own `None`/self-attributed
//! pre-redirect value, never to The Darkness Crystal.
//!
//! FIX: (1) `apply_single_replacement` now stamps
//! `cause = Some(rid.source)` whenever it applies a `modifiers.redirect_zone`
//! (CR 607.2b: the redirecting replacement's own source is the "exiled with
//! [this object]" linked-ability authority). (2)
//! `zone_pipeline::apply_zone_delivery_tail`'s exile-link-kind determination
//! now also auto-detects a linked-exile consumer on the resolved source
//! (`exile_links::source_is_linked_exile_consumer`), so callers with no
//! ability chain (SBA death, `Effect::Destroy`, `Effect::Sacrifice`) get
//! exactly the same CR 607.2b linkage the ability-chain path already had.
//!
//! DISCRIMINATOR: with the fix reverted, `state.exile_links` stays empty
//! after the exile (confirmed live via a throwaway probe before landing this
//! fix), and `AbilityActivation::target_object` cannot even find the exiled
//! card as a legal target — the whole activation cannot be driven. With the
//! fix, the link exists and the ability resolves fully.
//!
//! CR 607.2b: "If an object has an ability printed on it that generates a
//! replacement effect which causes one or more cards to be exiled and an
//! ability printed on it that refers... to cards 'exiled with [this
//! object],' these abilities are linked."
//! CR 616.1: when two or more replacement effects could apply to the same
//! event, the affected player chooses which applies (mirrored below in
//! `darkness_crystal_multi_authority_exile_link_binds_to_chosen_source`).

use engine::game::combat::AttackTarget;
use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DARKNESS_CRYSTAL: &str = "Black spells you cast cost {1} less to cast.\nIf a nontoken creature an opponent controls would die, instead exile it and you gain 2 life.\n{4}{B}{B}, {T}: Put target creature card exiled with The Darkness Crystal onto the battlefield tapped under your control with two additional +1/+1 counters on it.";

const BARE_DIE_EXILE_WATCHER: &str =
    "If a nontoken creature an opponent controls would die, exile it instead.";

fn mana(kind: ManaType, n: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(kind, ObjectId(0), false, vec![]); n]
}

fn crystal_activation_cost_pool() -> Vec<ManaUnit> {
    // {4}{B}{B}
    let mut pool = mana(ManaType::Colorless, 4);
    pool.extend(mana(ManaType::Black, 2));
    pool
}

/// The full end-to-end repro: a nontoken opponent creature dies to lethal
/// combat damage while The Darkness Crystal is on the battlefield, and its
/// own activated ability is then used to reclaim that exact card.
#[test]
fn darkness_crystal_activation_puts_exiled_creature_onto_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crystal = scenario
        .add_artifact_from_oracle(P0, "The Darkness Crystal", DARKNESS_CRYSTAL)
        .id();
    let attacker = scenario.add_creature(P0, "Big Attacker", 3, 3).id();
    let victim = scenario.add_creature(P1, "Doomed Blocker", 1, 1).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("the attacker must be able to attack");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    runner
        .declare_blockers(&[(victim, attacker)])
        .expect("the blocker must be able to block the attacker");
    runner.combat_damage();

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "CR 614.1a: the replacement must redirect the dying creature to exile"
    );

    // DISCRIMINATOR 1: the exile must be linked to The Darkness Crystal
    // (CR 607.2b). Before the fix this vec is empty.
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == victim && link.source_id == crystal),
        "victim must be recorded as exiled with The Darkness Crystal, got exile_links={:?}",
        runner.state().exile_links
    );

    // CR 500.4: the mana pool empties at the end of each step/phase, so fund
    // it AFTER combat resolves rather than at scenario-build time.
    for unit in crystal_activation_cost_pool() {
        let _ = runner.state_mut().add_mana_to_pool(P0, unit);
    }

    // DISCRIMINATOR 2: the activated ability can now target and resolve
    // against the exiled card through the full CR 602 activation pipeline.
    // Before the fix, `TargetFilter::ExiledBySource` matches nothing and
    // `target_object` cannot even be satisfied — this whole block cannot be
    // driven at all pre-fix.
    let outcome = runner.activate(crystal, 0).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Battlefield);
    outcome.assert_tapped(victim, true);
    assert_eq!(
        outcome.state().objects[&victim].controller,
        P0,
        "CR 110.2a: the ability puts the card onto the battlefield under its controller"
    );
    assert_eq!(
        outcome.counters(victim, CounterType::Plus1Plus1),
        2,
        "the ability adds two additional +1/+1 counters"
    );
}

/// SIBLING PATH: CR 700.4 "dying" also covers a creature reduced to 0
/// toughness (CR 704.5f), a DIFFERENT state-based-action mechanism than the
/// lethal-damage/Destroy path exercised above (`game/sba.rs`'s
/// `move_to_graveyard_via_pipeline`, not the `Destroy`-classified batch). The
/// pre-redirect `cause` on that path is the dying object itself, not `None` —
/// this test proves the fix's unconditional overwrite handles that shape too.
#[test]
fn darkness_crystal_zero_toughness_death_links_exile_to_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crystal = scenario
        .add_artifact_from_oracle(P0, "The Darkness Crystal", DARKNESS_CRYSTAL)
        .id();
    let victim = scenario
        .add_creature(P1, "Zero Toughness Victim", 1, 0)
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    check_state_based_actions(runner.state_mut(), &mut events);

    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "CR 704.5f + CR 614.1a: a 0-toughness opponent creature must still be redirected to exile"
    );
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == victim && link.source_id == crystal),
        "the zero-toughness death path must also attribute the exile to The Darkness Crystal, got exile_links={:?}",
        runner.state().exile_links
    );
}

/// MULTI-AUTHORITY HOSTILE FIXTURE (CR 616.1): two DIFFERENT sources both
/// carry "would die, exile it instead" replacements for the same dying
/// creature — only The Darkness Crystal also carries a companion "exiled
/// with [this object]" reading ability. When the affected player resolves
/// the CR 616.1 ordering choice by picking The Darkness Crystal's
/// replacement, the exile must link to The Darkness Crystal specifically —
/// never to the other (non-linked) source, and never to both.
#[test]
fn darkness_crystal_multi_authority_exile_link_binds_to_chosen_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let crystal = scenario
        .add_artifact_from_oracle(P0, "The Darkness Crystal", DARKNESS_CRYSTAL)
        .id();
    let watcher = scenario
        .add_creature_from_oracle(P0, "Bare Die-Exile Watcher", 2, 2, BARE_DIE_EXILE_WATCHER)
        .id();
    let victim = scenario
        .add_creature(P1, "Doomed Blocker", 1, 1)
        .with_damage_marked(1)
        .id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    check_state_based_actions(runner.state_mut(), &mut events);

    let WaitingFor::ReplacementChoice { ref candidates, .. } = runner.state().waiting_for else {
        panic!(
            "two applicable die-exile replacements from different sources must pause on a CR 616.1 ordering choice, got {:?}",
            runner.state().waiting_for
        );
    };
    let crystal_index = candidates
        .iter()
        .position(|candidate| candidate.source_id == crystal)
        .expect("The Darkness Crystal must be one of the two ordering candidates");

    runner
        .act(GameAction::ChooseReplacement {
            index: crystal_index,
        })
        .expect("answer the CR 616.1 ordering choice by picking The Darkness Crystal");

    assert_eq!(runner.state().objects[&victim].zone, Zone::Exile);
    assert!(
        runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == victim && link.source_id == crystal),
        "the chosen replacement's source (The Darkness Crystal) must be the exile-with authority, got exile_links={:?}",
        runner.state().exile_links
    );
    assert!(
        !runner
            .state()
            .exile_links
            .iter()
            .any(|link| link.exiled_id == victim && link.source_id == watcher),
        "the NON-chosen source (Bare Die-Exile Watcher) must never receive the exile link, got exile_links={:?}",
        runner.state().exile_links
    );
}
