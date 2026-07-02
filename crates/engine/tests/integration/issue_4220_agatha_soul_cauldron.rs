//! Issue #4220 — Agatha's Soul Cauldron: spend mana as any color for activated
//! abilities of creatures you control.
//!
//! https://github.com/phase-rs/phase/issues/4220
//!
//! With Agatha in play, a creature with a +1/+1 counter that has gained Yawgmoth's
//! `{B}{B}` proliferate ability must be able to pay the black mana with green (or
//! any) mana from the pool.

use std::sync::Arc;

use engine::game::casting::{can_activate_ability_now, can_pay_ability_mana_cost_after_auto_tap};
use engine::game::layers::evaluate_layers;
use engine::game::scenario::GameRunner;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, StaticDefinition,
    TargetFilter,
};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::{ExileLink, ExileLinkKind, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

const AGATHA_SPEND_LINE: &str = "You may spend mana as though it were mana of any color to activate abilities of creatures you control.";

const AGATHA_GRANT_LINE: &str = "Creatures you control with +1/+1 counters on them have all activated abilities of all creature cards exiled with Agatha's Soul Cauldron.";

fn add_green_mana_source(state: &mut GameState, card_id: u64) -> ObjectId {
    let forest = create_object(
        state,
        CardId(card_id),
        P0,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&forest).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.supertypes.push(Supertype::Basic);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.base_card_types = obj.card_types.clone();
        obj.entered_battlefield_turn = obj.entered_battlefield_turn.or(Some(0));
        obj.summoning_sick = false;
    }
    forest
}

/// Exiled creature card carrying `{B}{B}: Draw a card` — the mana-color axis under test.
fn exiled_black_draw_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        },
    })
}

fn build_agatha_yawgmoth_state() -> (GameState, ObjectId, usize) {
    let mut state = GameState::new_two_player(20);
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let spend_parsed = parse_oracle_text(
        AGATHA_SPEND_LINE,
        "Agatha's Soul Cauldron",
        &[],
        &["Artifact".into()],
        &[],
    );
    assert!(
        spend_parsed.statics.iter().any(|s| matches!(
            s.mode,
            StaticMode::SpendManaAsAnyColor {
                spell_filter: None,
                activation_source_filter: Some(_),
            }
        )),
        "Agatha spend line must parse to activation-source-scoped SpendManaAsAnyColor; got {:?}",
        spend_parsed.statics
    );

    let grant_parsed = parse_oracle_text(
        AGATHA_GRANT_LINE,
        "Agatha's Soul Cauldron",
        &[],
        &["Artifact".into()],
        &[],
    );
    assert_eq!(
        grant_parsed.statics.len(),
        1,
        "Agatha grant line must parse to one static; got {:?}",
        grant_parsed.statics
    );

    let cauldron = create_object(
        &mut state,
        CardId(4220),
        P0,
        "Agatha's Soul Cauldron".to_string(),
        Zone::Battlefield,
    );
    {
        let statics: Vec<StaticDefinition> = spend_parsed
            .statics
            .into_iter()
            .chain(grant_parsed.statics)
            .collect();
        let obj = state.objects.get_mut(&cauldron).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.base_card_types = obj.card_types.clone();
        obj.static_definitions = statics.clone().into();
        obj.base_static_definitions = Arc::new(statics);
    }

    let exiled_yawgmoth = create_object(
        &mut state,
        CardId(4221),
        P0,
        "Yawgmoth, Thran Physician".to_string(),
        Zone::Exile,
    );
    {
        let donated = exiled_black_draw_ability();
        let obj = state.objects.get_mut(&exiled_yawgmoth).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.abilities = Arc::new(vec![donated.clone()]);
        obj.base_abilities = Arc::new(vec![donated]);
    }
    state.exile_links.push(ExileLink {
        exiled_id: exiled_yawgmoth,
        source_id: cauldron,
        kind: ExileLinkKind::TrackedBySource,
    });

    let host = create_object(
        &mut state,
        CardId(4222),
        P0,
        "Grafted Host".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&host).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.counters.insert(CounterType::Plus1Plus1, 1);
    }

    let library_card = create_object(
        &mut state,
        CardId(4223),
        P0,
        "Top of Library".to_string(),
        Zone::Library,
    );
    state.players[P0.0 as usize].library.push_back(library_card);

    evaluate_layers(&mut state);

    let ability_index = state.objects[&host]
        .abilities
        .iter()
        .position(|a| {
            matches!(a.kind, AbilityKind::Activated)
                && matches!(a.effect.as_ref(), Effect::Draw { .. })
        })
        .expect("host must gain the exiled creature card's {B}{B} draw ability");

    (state, host, ability_index)
}

#[test]
fn agatha_spend_line_parses_activation_source_filter() {
    let parsed = parse_oracle_text(
        AGATHA_SPEND_LINE,
        "Agatha's Soul Cauldron",
        &[],
        &["Artifact".into()],
        &[],
    );
    assert!(
        parsed.statics.iter().any(|s| matches!(
            s.mode,
            StaticMode::SpendManaAsAnyColor {
                spell_filter: None,
                activation_source_filter: Some(_),
            }
        )),
        "expected activation-source-scoped SpendManaAsAnyColor; got {:?}",
        parsed.statics
    );
}

/// CR 609.4b: Auto-tap / legality dry-runs must honor activation-source-scoped
/// spend-as-any-color, not only board-wide grants — green sources alone must
/// suffice before payment begins.
#[test]
fn agatha_granted_bb_ability_affordable_via_green_auto_tap_sources() {
    let (mut state, host, ability_index) = build_agatha_yawgmoth_state();
    add_green_mana_source(&mut state, 4224);
    add_green_mana_source(&mut state, 4225);

    let bb_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 0,
    };
    assert!(
        can_pay_ability_mana_cost_after_auto_tap(&state, P0, host, &bb_cost),
        "auto-tap planner must treat green sources as paying {{B}}{{B}} under Agatha"
    );
    assert!(
        can_activate_ability_now(&state, P0, host, ability_index),
        "can_activate_ability_now must agree before payment begins"
    );
}

fn fund_green(state: &mut GameState, count: u32) {
    let pool = &mut state.players[P0.0 as usize].mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(0),
            pip_id: engine::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });
    }
}

/// CR 609.4b: With Agatha's spend-as-any-color static active, a creature you
/// control that gained a `{B}{B}` activated ability may pay that cost using green
/// mana only.
#[test]
fn agatha_granted_bb_ability_pays_with_green_mana() {
    let (mut state, host, ability_index) = build_agatha_yawgmoth_state();
    fund_green(&mut state, 2);

    let mut runner = GameRunner::from_state(state);
    let outcome = runner.activate(host, ability_index).resolve();

    assert_eq!(
        outcome.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "both green mana must be spent paying the black cost"
    );
    outcome.assert_hand_drawn(P0, 1);
    assert_eq!(
        outcome.state().stack.len(),
        0,
        "ability must fully resolve off the stack"
    );
}
