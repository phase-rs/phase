//! Shared fixture helpers: typed mana for printed costs, the cast/pass driver, and counter reads.
#![allow(dead_code)]

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

pub fn cost(printed: &str) -> ManaCost {
    parse_mtgjson_mana_cost(printed)
}

/// Mana that pays every printed cost in `costs` (digits as colorless, one unit per colored symbol; `{X}`
/// contributes nothing) plus `extra` colorless units for an announced X.
pub fn mana_for(costs: &[&str], extra: usize) -> Vec<ManaUnit> {
    let mut out = Vec::new();
    for printed in costs {
        for sym in printed.split('{').filter_map(|s| s.strip_suffix('}')) {
            let kinds: Vec<ManaType> = match sym {
                "W" => vec![ManaType::White],
                "U" => vec![ManaType::Blue],
                "B" => vec![ManaType::Black],
                "R" => vec![ManaType::Red],
                "G" => vec![ManaType::Green],
                "X" => vec![],
                n => vec![ManaType::Colorless; n.parse::<usize>().expect("generic symbol")],
            };
            out.extend(kinds);
        }
    }
    out.extend(std::iter::repeat_n(ManaType::Colorless, extra));
    out.into_iter()
        .map(|t| ManaUnit::new(t, ObjectId(0), false, vec![]))
        .collect()
}

pub fn scenario() -> GameScenario {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    sc
}

/// Announce `object_id` through the production `CastSpell` action (targets chosen by the engine when only one is legal).
pub fn cast_spell(runner: &mut GameRunner, object_id: ObjectId) {
    let card_id = runner.state().objects[&object_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap_or_else(|e| panic!("cast {object_id:?}: {e:?}"));
}

/// Announce X for the spell just cast, when the engine asks for it (CR 107.3 + CR 601.2b).
pub fn choose_x_if_asked(runner: &mut GameRunner, x: u32) {
    if let WaitingFor::ChooseXValue { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseX { value: x })
            .expect("choose X");
    }
}

/// Pass priority until the stack is empty or a non-priority decision appears; returns the decision.
pub fn drain(runner: &mut GameRunner) -> WaitingFor {
    for _ in 0..60 {
        if runner.state().stack.is_empty() {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => return other,
        }
    }
    runner.state().waiting_for.clone()
}

pub fn is_proliferate(w: &WaitingFor) -> bool {
    matches!(w, WaitingFor::ProliferateChoice { .. })
}

pub fn plus_counters(state: &GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

pub fn life(state: &GameState, p: PlayerId) -> i32 {
    state.players[p.0 as usize].life
}

pub fn zone(state: &GameState, id: ObjectId) -> Zone {
    state.objects[&id].zone
}

pub const COUNTER_HOLDER: &str = "Counter Holder";

/// A textless filler permanent that carries a +1/+1 counter, so a proliferate prompt has something to offer
/// (reach guard for every "no proliferate" negative).
pub fn add_counter_holder(sc: &mut GameScenario) -> ObjectId {
    let mut b = sc.add_creature(P1, COUNTER_HOLDER, 1, 1);
    b.with_plus_counters(1);
    b.id()
}

pub fn opp(p: PlayerId) -> PlayerId {
    if p == P0 {
        P1
    } else {
        P0
    }
}
