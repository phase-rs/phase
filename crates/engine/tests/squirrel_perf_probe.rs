//! One-off perf probe for the turn-40 squirrel board (Cryptolith Rite + ~700
//! Squirrel tokens). Loads a saved frontend game-state JSON and measures where
//! the `legal_actions` clone-storm bites. Run with:
//!   SQUIRREL_STATE=/path/to/state.json cargo test -p engine --test squirrel_perf_probe -- --nocapture --ignored
//! Diagnostic only; ignored by default so it never runs in CI.

use std::time::Instant;

use engine::ai_support::legal_actions_full;
use engine::game::combat::{
    get_valid_block_targets_for_player, AttackTarget, AttackerInfo, CombatState,
};
use engine::game::functioning_abilities::game_functioning_statics;
use engine::game::perf_counters;
use engine::types::ability::TargetFilter;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

fn load_state() -> GameState {
    let path = std::env::var("SQUIRREL_STATE").expect("set SQUIRREL_STATE=/path/to/state.json");
    let raw = std::fs::read_to_string(&path).expect("read state json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("parse json");
    let gs = v.get("gameState").cloned().unwrap_or(v);
    serde_json::from_value(gs).expect("deserialize GameState")
}

fn measure(label: &str, state: &GameState) {
    perf_counters::reset();
    let start = Instant::now();
    let (actions, costs, grouped) = legal_actions_full(state);
    let elapsed = start.elapsed();
    let c = perf_counters::snapshot();
    println!(
        "[{label}] elapsed={:?} actions={} spell_costs={} grouped={} \
         clones_for_legality={} static_full_scans={} mana_sweeps={} swept_objs={} \
         spell_cost_sweeps={}",
        elapsed,
        actions.len(),
        costs.len(),
        grouped.len(),
        c.state_clone_for_legality,
        c.static_full_scans,
        c.mana_display_sweeps,
        c.mana_display_swept_objects,
        c.legal_actions_spell_cost_sweeps,
    );
}

#[test]
#[ignore]
fn probe_squirrel_board() {
    let state = load_state();
    println!(
        "loaded: turn={} active={:?} priority={:?} battlefield={} objects={} waiting_for={:?}",
        state.turn_number,
        state.active_player,
        state.priority_player,
        state.battlefield.len(),
        state.objects.len(),
        std::mem::discriminant(&state.waiting_for),
    );

    // (a) current state as exported (Priority on some player)
    measure("current-priority", &state);

    // (b) synthesize the DeclareAttackers step for the active player: valid
    // attackers = that player's untapped, non-summoning-sick creatures; targets
    // = every other non-eliminated player. This reproduces what the frontend
    // asks for when rendering the declare-attackers UI.
    let active = state.active_player;
    let valid_attacker_ids: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.controller == active
                    && o.card_types.core_types.contains(&CoreType::Creature)
                    && !o.tapped
                    && !o.has_summoning_sickness
            })
        })
        .collect();
    let valid_attack_targets: Vec<_> = (0..state.players.len())
        .map(|i| PlayerId(i as u8))
        .filter(|p| *p != active && !state.eliminated_players.contains(p))
        .map(AttackTarget::Player)
        .collect();

    println!(
        "declare-attackers synth: active={:?} valid_attackers={} targets={}",
        active,
        valid_attacker_ids.len(),
        valid_attack_targets.len(),
    );

    let mut da = state.clone();
    da.waiting_for = WaitingFor::DeclareAttackers {
        player: active,
        valid_attacker_ids,
        valid_attack_targets,
    };
    measure("declare-attackers", &da);
}

#[test]
#[ignore]
fn probe_declare_blockers() {
    let mut state = load_state();
    let active = state.active_player;

    // P0's valid attackers (untapped, non-summoning-sick creatures).
    let attacker_ids: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|o| {
                o.controller == active
                    && o.card_types.core_types.contains(&CoreType::Creature)
                    && !o.tapped
                    && !o.has_summoning_sickness
            })
        })
        .collect();

    // Defender = non-active, non-eliminated player controlling the most untapped
    // creatures (the worst-case blocker set for this synth).
    let creature_count = |p: PlayerId| {
        state
            .battlefield
            .iter()
            .filter(|id| {
                state.objects.get(id).is_some_and(|o| {
                    o.controller == p
                        && o.card_types.core_types.contains(&CoreType::Creature)
                        && !o.tapped
                })
            })
            .count()
    };
    let defender = (0..state.players.len())
        .map(|i| PlayerId(i as u8))
        .filter(|p| *p != active && !state.eliminated_players.contains(p))
        .max_by_key(|p| creature_count(*p))
        .expect("a defending player");
    let blocker_creatures = creature_count(defender);

    // Register all attackers against the chosen defender.
    let attackers: Vec<AttackerInfo> = attacker_ids
        .iter()
        .map(|&id| AttackerInfo::new(id, AttackTarget::Player(defender), defender))
        .collect();
    state.combat = Some(CombatState {
        attackers,
        ..Default::default()
    });

    // Poison-relevant functioning-static scan.
    let mut modes: std::collections::BTreeSet<String> = Default::default();
    let mut any_specific_object = false;
    let mut blocker_poison: std::collections::BTreeSet<String> = Default::default();
    for (_, def) in game_functioning_statics(&state) {
        let dbg = format!("{:?}", def.mode);
        let short = dbg
            .split(['(', '{', ' '])
            .next()
            .unwrap_or(&dbg)
            .to_string();
        modes.insert(short.clone());
        if matches!(def.affected, Some(TargetFilter::SpecificObject { .. })) {
            any_specific_object = true;
        }
        if matches!(
            def.mode,
            StaticMode::CantBlock
                | StaticMode::CantAttackOrBlock
                | StaticMode::CantBeBlocked
                | StaticMode::CantBeBlockedBy { .. }
                | StaticMode::CantBeBlockedExceptBy { .. }
                | StaticMode::CantBeBlockedByMoreThan { .. }
                | StaticMode::BlockRestriction { .. }
                | StaticMode::MustBlock
                | StaticMode::MustBlockAttacker { .. }
                | StaticMode::MustBeBlocked
                | StaticMode::MustBeBlockedByAll
                | StaticMode::MaxBlockersEachCombat { .. }
                | StaticMode::ExtraBlockers { .. }
                | StaticMode::CanBlockShadow
                | StaticMode::IgnoreLandwalkForBlocking { .. }
                | StaticMode::Menace
        ) {
            blocker_poison.insert(short);
        }
    }

    // Time the O(blockers × attackers) valid-block-targets precompute alone.
    let t0 = Instant::now();
    let vbt = get_valid_block_targets_for_player(&state, defender);
    let vbt_elapsed = t0.elapsed();
    let valid_blocker_ids: Vec<_> = vbt.keys().copied().collect();
    let total_pairs: usize = vbt.values().map(|v| v.len()).sum();

    println!(
        "attackers={} defender={:?} untapped_creatures={} valid_blocker_ids={} total_block_pairs={}",
        attacker_ids.len(),
        defender,
        blocker_creatures,
        valid_blocker_ids.len(),
        total_pairs,
    );
    println!("functioning_static_modes={:?}", modes);
    println!(
        "any_specific_object_affected={} blocker_poison_modes_present={:?}",
        any_specific_object, blocker_poison
    );
    println!(
        "get_valid_block_targets_for_player elapsed={:?}",
        vbt_elapsed
    );

    state.waiting_for = WaitingFor::DeclareBlockers {
        player: defender,
        valid_blocker_ids,
        valid_block_targets: vbt,
        block_requirements: std::collections::HashMap::new(),
    };

    perf_counters::reset();
    let t1 = Instant::now();
    let (actions, _costs, _grouped) = legal_actions_full(&state);
    let elapsed = t1.elapsed();
    let c = perf_counters::snapshot();
    println!(
        "[declare-blockers] elapsed={:?} actions={} clones_for_legality={} static_full_scans={}",
        elapsed,
        actions.len(),
        c.state_clone_for_legality,
        c.static_full_scans,
    );
}
