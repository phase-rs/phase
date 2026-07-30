//! Sothera, the Supervoid — the two previously-`Unimplemented` triggered
//! abilities (parser round-5 fix).
//!
//! Oracle (verbatim, Scryfall):
//!   - "Whenever a creature you control dies, each opponent chooses a creature
//!     they control and exiles it."
//!   - "At the beginning of your end step, if a player controls no creatures,
//!     sacrifice Sothera, then put a creature card exiled with it onto the
//!     battlefield under your control with two additional +1/+1 counters on it."
//!
//! CR anchors:
//!   - CR 700.4 (dies) + CR 109.5 (each opponent, "they/you" = the iterating
//!     actor): the per-opponent edict. `set_controller_recursive` rebinds the
//!     ability controller per iterated opponent, so the reparsed edict "exile a
//!     creature you control" makes each opponent exile from their OWN board.
//!   - CR 607.2 + CR 406.6: Sothera's exile of each opponent's creature links to
//!     Sothera via the `ExiledBySource` interlock (Ability 2's ExiledBySource
//!     consumer marks Sothera a tracked exile source).
//!   - CR 122.1: reanimation with two additional +1/+1 counters.
//!   - CR 701.21a (sacrifice), CR 603.4 (intervening-if).
//!
//! These tests drive the REAL resolution path — `resolve_ability_chain` on the
//! PARSED trigger bodies wired onto the source object (mirrors
//! `plaguecrafter_etb_class.rs`), not hand-built ASTs.

use std::sync::Arc;

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::{parse_oracle_text, ParsedAbilities};
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const SOTHERA_ORACLE: &str = "Whenever a creature you control dies, each opponent chooses a creature they control and exiles it.\n\
At the beginning of your end step, if a player controls no creatures, sacrifice Sothera, then put a creature card exiled with it onto the battlefield under your control with two additional +1/+1 counters on it.";

fn parsed_sothera() -> ParsedAbilities {
    parse_oracle_text(
        SOTHERA_ORACLE,
        "Sothera, the Supervoid",
        &[],
        &["Legendary".to_string(), "Enchantment".to_string()],
        &[],
    )
}

/// The DIES trigger's execute body (each-opponent exile edict). Disambiguated
/// from the end-step reanimation by the ABSENCE of an `ExiledBySource` consumer:
/// the edict exiles creatures the opponents control (no linked-exile reference),
/// whereas the reanimation targets `ExiledBySource`.
fn dies_edict_body(parsed: &ParsedAbilities) -> AbilityDefinition {
    parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find(|exec| !mentions_exiled_by_source(exec))
        .cloned()
        .expect("Sothera must parse a dies-trigger exile edict body")
}

/// The end-step trigger's execute body (sacrifice + reanimate). The reanimation
/// lives in the `sub_ability` after the `Sacrifice` clause, so we serialize the
/// whole `AbilityDefinition` (not just `.effect`) to see the linked-exile
/// consumer.
fn end_step_body(parsed: &ParsedAbilities) -> AbilityDefinition {
    parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find(|exec| mentions_exiled_by_source(exec))
        .cloned()
        .expect("Sothera must parse an end-step reanimation body")
}

/// True when the serialized ability definition (effect + sub_ability tree)
/// contains an `ExiledBySource` linked-exile reference.
fn mentions_exiled_by_source(def: &AbilityDefinition) -> bool {
    serde_json::to_string(def)
        .map(|s| s.contains("ExiledBySource"))
        .unwrap_or(false)
}

/// Number of creature-card names a player controls on the battlefield.
fn battlefield_creatures(state: &GameState, player: PlayerId) -> Vec<String> {
    let mut names: Vec<String> = state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == player
                && o.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|o| o.name.clone())
        .collect();
    names.sort();
    names
}

/// Names of creature cards currently in the Exile zone owned by `player`.
fn exiled_owned_by(state: &GameState, player: PlayerId) -> Vec<String> {
    let mut names: Vec<String> = state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Exile && o.owner == player)
        .map(|o| o.name.clone())
        .collect();
    names.sort();
    names
}

/// Wire Sothera as a Legendary Enchantment carrying its two parsed triggers so
/// the `ExiledBySource` interlock (`source_contains_linked_exile_consumer`) sees
/// Ability 2's consumer and marks Sothera a tracked exile source.
fn wire_sothera(state: &mut GameState, sothera: ObjectId, parsed: &ParsedAbilities) {
    let obj = state.objects.get_mut(&sothera).expect("sothera");
    obj.card_types.core_types = vec![CoreType::Enchantment];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    for trig in &parsed.triggers {
        obj.trigger_definitions.push(trig.clone());
    }
    obj.base_trigger_definitions = Arc::new(parsed.triggers.clone());
}

// ---------------------------------------------------------------------------
// Ability 1 — the per-opponent exile edict (S2 controller-scoping fix).
// ---------------------------------------------------------------------------

/// CR 109.5: each opponent exiles a creature THEY control — not the caster's,
/// not the other opponent's. Hostile 3-player fixture: P0 (caster) keeps a
/// creature; P1 and P2 each have exactly one creature that must be exiled.
///
/// Revert-failing assertion: if the edict's controller scope regressed to the
/// caster (the pre-fix bug), both opponents would target P0's board — P0's Bear
/// would be exiled and the opponents' creatures would survive. The paired
/// "opponents lose their creatures / P0 keeps Bear" assertions flip on revert.
#[test]
fn sothera_each_opponent_exiles_own_creature() {
    let parsed = parsed_sothera();

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let sothera = scenario
        .add_creature(P0, "Sothera, the Supervoid", 0, 0)
        .id();
    scenario.add_creature(P0, "Aegis Bear", 2, 2);
    scenario.add_creature(P1, "Dire Wolf", 2, 2);
    scenario.add_creature(P2, "Crag Ogre", 2, 2);
    let mut runner = scenario.build();
    wire_sothera(runner.state_mut(), sothera, &parsed);

    let body = dies_edict_body(&parsed);
    // Anti-Unimplemented reach guard: the edict resolves to a real exile chain.
    assert!(
        !matches!(body.effect.as_ref(), Effect::Unimplemented { .. }),
        "Ability 1 edict must not be Effect::Unimplemented, got {:?}",
        body.effect
    );

    let ability = build_resolved_from_def(&body, sothera, P0);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0).expect("edict resolves");

    let state = runner.state();
    // Each opponent exiled their OWN creature.
    assert_eq!(
        battlefield_creatures(state, P1),
        Vec::<String>::new(),
        "P1 must exile its own Dire Wolf"
    );
    assert_eq!(
        battlefield_creatures(state, P2),
        Vec::<String>::new(),
        "P2 must exile its own Crag Ogre"
    );
    // The caster's creature is untouched (proves the edict is opponent-scoped,
    // NOT caster-scoped — the S2 revert-failing assertion).
    assert_eq!(
        battlefield_creatures(state, P0),
        vec!["Aegis Bear".to_string()],
        "P0's Aegis Bear must NOT be exiled — each opponent exiles from their \
         own board, not the caster's"
    );
    // The exiled cards are the opponents' own.
    assert_eq!(exiled_owned_by(state, P1), vec!["Dire Wolf".to_string()]);
    assert_eq!(exiled_owned_by(state, P2), vec!["Crag Ogre".to_string()]);

    // CR 607.2 + CR 406.6: both exiled creatures are linked to Sothera (the
    // ExiledBySource interlock), so Ability 2 can reanimate them.
    let linked = state
        .exile_links
        .iter()
        .filter(|l| l.source_id == sothera)
        .count();
    assert_eq!(
        linked, 2,
        "both opponent creatures must be linked to Sothera via exile_links"
    );
}

// ---------------------------------------------------------------------------
// Ability 2 — reanimate a Sothera-linked exiled creature with two counters.
// ---------------------------------------------------------------------------

/// CR 607.2 + CR 406.6 + CR 122.1: the end-step reanimation targets only cards
/// "exiled with it" (Sothera's linked pool) and enters with two +1/+1 counters.
///
/// Provenance guard: an UNRELATED creature card sitting in exile (never linked
/// to Sothera) must NOT be reanimated — the S3 `And{Typed(Creature),
/// ExiledBySource}` filter restricts the pool to Sothera's own exiles. If S3
/// regressed to a bare `Typed(Creature)`, the reanimation could pull the
/// unrelated card or a battlefield creature.
#[test]
fn sothera_reanimates_only_linked_creature_with_two_counters() {
    let parsed = parsed_sothera();

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let sothera = scenario
        .add_creature(P0, "Sothera, the Supervoid", 0, 0)
        .id();
    scenario.add_creature(P0, "Aegis Bear", 2, 2);
    // Single opponent creature so the reanimation's "a creature card" pick has
    // exactly one eligible candidate (forced, non-interactive). P2 controls no
    // creature, so its edict clause is a CR 101.3 no-op.
    scenario.add_creature(P1, "Dire Wolf", 2, 2);
    let unrelated = scenario.add_creature(P1, "Unlinked Specter", 3, 3).id();
    let mut runner = scenario.build();
    wire_sothera(runner.state_mut(), sothera, &parsed);
    // Move the unrelated creature into exile with NO link to Sothera.
    runner.state_mut().objects.get_mut(&unrelated).unwrap().zone = Zone::Exile;

    // Drive Ability 1 so the opponent's creature is exiled + linked.
    let edict = build_resolved_from_def(&dies_edict_body(&parsed), sothera, P0);
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &edict, &mut events, 0).expect("edict resolves");

    // Reach guard (non-vacuous): the exile actually happened and linked. Exactly
    // one Sothera-linked card (Dire Wolf) — the unrelated Specter is NOT linked.
    let linked_before = runner
        .state()
        .exile_links
        .iter()
        .filter(|l| l.source_id == sothera)
        .count();
    assert_eq!(
        linked_before, 1,
        "reach guard: the opponent's exile must be linked before reanimation"
    );

    // Drive Ability 2 (sacrifice Sothera, then reanimate a linked creature).
    let reanimate = build_resolved_from_def(&end_step_body(&parsed), sothera, P0);
    let mut events2 = Vec::<GameEvent>::new();
    resolve_ability_chain(runner.state_mut(), &reanimate, &mut events2, 0)
        .expect("reanimation resolves");

    let state = runner.state();
    // Exactly one Sothera-linked creature returned to the battlefield under P0's
    // control, carrying two +1/+1 counters.
    let reanimated: Vec<_> = state
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.controller == P0
                && o.card_types.core_types.contains(&CoreType::Creature)
                && o.name == "Dire Wolf"
        })
        .collect();
    assert_eq!(
        reanimated.len(),
        1,
        "the Sothera-linked creature (Dire Wolf) must be reanimated, got {:?}",
        reanimated.iter().map(|o| &o.name).collect::<Vec<_>>()
    );
    assert_eq!(
        reanimated[0]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2),
        "the reanimated creature must enter with two additional +1/+1 counters"
    );

    // Provenance guard: the unrelated exiled creature stays in exile — the
    // ExiledBySource filter excluded it.
    assert!(
        state
            .objects
            .get(&unrelated)
            .map(|o| o.zone == Zone::Exile)
            .unwrap_or(true),
        "the unrelated (non-linked) exiled creature must NOT be reanimated"
    );
}
