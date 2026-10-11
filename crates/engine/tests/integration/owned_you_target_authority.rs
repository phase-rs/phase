//! Explicit ownership retains the declaring ability's authority at announcement.
//! Grammar and typed building-block fixtures are labelled separately from cards.

#[cfg(feature = "test-support")]
use engine::game::ability_utils::build_resolved_from_def;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CardTypeSetSource, ChosenAttribute, Comparator, ControllerRef, Effect,
    FilterProp, MultiTargetSpec, PlayerFilter, QuantityExpr, QuantityRef, TargetChoiceTiming,
    TargetFilter, TargetRef, TypedFilter, UNION_DEPTH_BUDGET,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const GRAMMAR: &str = "Target player draws a card. Exile target creature card from your graveyard.";
const WRETCHED: &str = "Choose three. You may choose the same mode more than once.\n• Target player draws a card and loses 1 life.\n• Target creature gets -2/-2 until end of turn.\n• Return target creature card from your graveyard to your hand.";
const REGROWTH: &str = "Return target card from your graveyard to your hand.";
const SEPARATOR: &str = "{5}, {T}, Sacrifice this artifact: Exile target creature card from your graveyard. Create a token that's a copy of that card, except it's 1/1, it's a Spirit in addition to its other types, and it has flying. Create a black Zombie creature token with power equal to that card's power and toughness equal to that card's toughness.";
const PROXY: &str = "Prototype {1}{U}{U} — 2/1 (You may cast this spell with different mana cost, color, and size. It keeps its abilities and types.)\nWhen this creature enters, if you cast it, exile target instant or sorcery card with mana value less than or equal to this creature's power from your graveyard. Copy that card. You may cast the copy without paying its mana cost.";
const OOZE: &str = "{G}: Exile target card from a graveyard. If it was a creature card, put a +1/+1 counter on this creature and you gain 1 life.";

fn board() -> GameScenario {
    let mut s = GameScenario::new_n_player(3, 71);
    s.at_phase(Phase::PreCombatMain);
    s.with_mana_pool(
        P0,
        (0..12)
            .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
            .collect(),
    );
    for p in [P0, P1, P2] {
        s.add_card_to_library_top(p, "Draw control");
    }
    s
}

fn graves(s: &mut GameScenario) -> [Vec<ObjectId>; 3] {
    [P0, P1, P2].map(|p| {
        (0..3)
            .map(|i| {
                s.add_creature_to_graveyard(p, &format!("Grave {} {i}", p.0), 2, 2)
                    .id()
            })
            .collect()
    })
}

fn begin(r: &mut GameRunner, spell: ObjectId) -> Result<(), engine::game::engine::EngineError> {
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id: r.state().objects[&spell].card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .map(|_| ())
}

fn choose(r: &mut GameRunner, target: Option<TargetRef>) {
    r.act(GameAction::ChooseTarget { target })
        .expect("public target choice must succeed");
}

fn menu(r: &GameRunner) -> Vec<TargetRef> {
    match &r.state().waiting_for {
        WaitingFor::TargetSelection { selection, .. }
        | WaitingFor::TriggerTargetSelection { selection, .. } => {
            selection.current_legal_targets.clone()
        }
        other => panic!("expected real target selection, got {other:?}"),
    }
}

fn objects(ids: &[ObjectId]) -> Vec<TargetRef> {
    ids.iter().copied().map(TargetRef::Object).collect()
}

fn exact(actual: &[TargetRef], expected: &[TargetRef]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "exact menu: {actual:?} versus {expected:?}"
    );
    for e in expected {
        assert!(actual.contains(e), "missing {e:?} in {actual:?}");
    }
}

// CR 115.10a: the engine auto-announces the paired opponent binder before the real object target.
fn fanout_menu(r: &GameRunner, player: PlayerId, ids: &[ObjectId]) {
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        pending_cast,
        ..
    } = &r.state().waiting_for
    else {
        panic!("actual paired object prompt")
    };
    assert_eq!(
        selection.current_slot % 2,
        1,
        "engine auto-announces each forced player binder"
    );
    assert_eq!(
        selection.selected_slots.last(),
        Some(&Some(TargetRef::Player(player))),
        "paired authority at slot {} for {:?}: {:?}",
        selection.current_slot,
        player,
        pending_cast.ability.effect,
    );
    exact(
        &target_slots[selection.current_slot - 1].legal_targets,
        &[TargetRef::Player(player)],
    );
    exact(&selection.current_legal_targets, &objects(ids));
}

fn assert_zones(r: &GameRunner, ids: &[ObjectId], zone: Zone) {
    for id in ids {
        assert_eq!(r.state().objects[id].zone, zone, "object {id:?}");
    }
}

fn exile_definition(filter: TargetFilter) -> AbilityDefinition {
    let mut def = parse_oracle_text(
        "Exile target creature.",
        "Typed fixture",
        &[],
        &["Instant".into()],
        &[],
    )
    .abilities
    .remove(0);
    let Effect::ChangeZone { target, .. } = def.effect.as_mut() else {
        panic!("exile parser reach guard")
    };
    *target = filter;
    def
}

fn own_creature(owner: ControllerRef, zone: Zone) -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().properties(vec![
        FilterProp::InZone { zone },
        FilterProp::Owned { controller: owner },
    ]))
}

fn typed_spell(s: &mut GameScenario, def: AbilityDefinition) -> ObjectId {
    s.add_spell_to_hand(P0, "Typed ownership fixture", true)
        .with_ability_definition(def)
        .id()
}

#[test]
fn owner_only_static_and_sequential_menus_keep_spell_controller() {
    let mut s = board();
    let gy = graves(&mut s);
    let battlefield = [
        s.add_creature(P0, "Battlefield A", 2, 2).id(),
        s.add_creature(P1, "Battlefield B", 2, 2).id(),
    ];
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Grammar fixture", true, GRAMMAR)
        .id();
    let mut r = s.build();
    let declared = &r.state().objects[&spell].abilities[0];
    let TargetFilter::Typed(tf) = declared
        .sub_ability
        .as_ref()
        .unwrap()
        .effect
        .target_filter()
        .unwrap()
    else {
        panic!("canonical Your-zone shape")
    };
    assert_eq!(tf.controller, Some(ControllerRef::You));
    assert!(tf.properties.contains(&FilterProp::InZone {
        zone: Zone::Graveyard
    }));
    assert!(!tf
        .properties
        .iter()
        .any(|p| matches!(p, FilterProp::Owned { .. })));
    begin(&mut r, spell).unwrap();
    let WaitingFor::TargetSelection { target_slots, .. } = &r.state().waiting_for else {
        panic!("two-clause target prompt")
    };
    assert_eq!(target_slots.len(), 2);
    exact(
        &target_slots[0].legal_targets,
        &[
            TargetRef::Player(P0),
            TargetRef::Player(P1),
            TargetRef::Player(P2),
        ],
    );
    // CR 109.5: an independent draw recipient never redefines "your" graveyard.
    exact(&target_slots[1].legal_targets, &objects(&gy[0]));
    choose(&mut r, Some(TargetRef::Player(P1)));
    exact(&menu(&r), &objects(&gy[0]));
    choose(&mut r, Some(TargetRef::Object(gy[0][2])));
    r.advance_until_stack_empty();
    assert_zones(&r, &[gy[0][2]], Zone::Exile);
    assert_zones(&r, &gy[0][..2], Zone::Graveyard);
    assert_zones(&r, &gy[1], Zone::Graveyard);
    assert_zones(&r, &battlefield, Zone::Battlefield);
    assert_eq!(r.state().players[1].hand.len(), 1);
}

// CR 109.5 + CR 608.2c: Your ownership and the independent draw instruction retain their respective authorities.
#[test]
fn owner_only_canonical_driver_moves_nonfirst_card_and_draws_for_other_player() {
    let mut s = board();
    let gy = graves(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Grammar fixture", true, GRAMMAR)
        .id();
    let mut r = s.build();
    let out = r
        .cast(spell)
        .target_player(P1)
        .target_object(gy[0][2])
        .resolve();
    out.assert_zone(&[gy[0][2]], Zone::Exile);
    out.assert_zone(&gy[1], Zone::Graveyard);
    out.assert_hand_drawn(P1, 1);
}

fn stolen_board(s: &mut GameScenario) -> [Vec<ObjectId>; 2] {
    [P1, P2].map(|p| {
        let own: Vec<_> = (0..3)
            .map(|i| {
                s.add_creature(P0, &format!("Stolen {} {i}", p.0), 2, 2)
                    .controlled_by(p)
                    .with_mana_cost(ManaCost::generic(3))
                    .id()
            })
            .collect();
        s.add_creature(p, "Opponent native", 2, 2);
        own
    })
}

// CR 108.3 + CR 110.2: permanent ownership is independent of the selected controller.
#[test]
fn mixed_public_grammar_keeps_owner_distinct_from_target_player_controller() {
    let mut s = board();
    let own = stolen_board(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Mixed grammar fixture",
            true,
            "Exile target creature you own that target player controls.",
        )
        .id();
    let mut r = s.build();
    begin(&mut r, spell).unwrap();
    assert!(
        menu(&r).contains(&TargetRef::Player(P1)),
        "companion player reach guard"
    );
    choose(&mut r, Some(TargetRef::Player(P1)));
    // CR 108.3 + CR 110.2: ownership survives the independent control change.
    exact(&menu(&r), &objects(&own[0]));
    choose(&mut r, Some(TargetRef::Object(own[0][2])));
    r.advance_until_stack_empty();
    assert_zones(&r, &[own[0][2]], Zone::Exile);
    assert_zones(&r, &own[1], Zone::Battlefield);
}

// CR 109.5 + CR 110.2: the explicit typed contract uses genuine TargetPlayer
// control and declaring-P0 ownership. Original TypedYou would require P0 control;
// its separate legacy announcement substitution is not a movement reference.
#[test]
fn typed_target_player_controller_and_nested_owner_keep_separate_authorities() {
    let mut s = board();
    let own = stolen_board(&mut s);
    let wrong_type = s
        .add_creature(P0, "Stolen artifact", 2, 2)
        .as_artifact()
        .controlled_by(P1)
        .id();
    let wrong_zone = s
        .add_creature_to_graveyard(P0, "Graveyard decoy", 2, 2)
        .id();
    let mut def = parse_oracle_text(GRAMMAR, "Typed chain", &[], &["Instant".into()], &[])
        .abilities
        .remove(0);
    let Effect::ChangeZone { target, origin, .. } =
        def.sub_ability.as_mut().unwrap().effect.as_mut()
    else {
        panic!("chain reach guard")
    };
    *origin = Some(Zone::Battlefield);
    *target = TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::TargetPlayer),
        properties: vec![FilterProp::AnyOf {
            props: vec![
                FilterProp::Owned {
                    controller: ControllerRef::You,
                },
                FilterProp::Not {
                    prop: Box::new(FilterProp::Not {
                        prop: Box::new(FilterProp::Owned {
                            controller: ControllerRef::You,
                        }),
                    }),
                },
            ],
        }],
        ..TypedFilter::creature()
    });
    let original = def.clone();
    let spell = typed_spell(&mut s, def);
    let mut r = s.build();
    begin(&mut r, spell).unwrap();
    let WaitingFor::TargetSelection { target_slots, .. } = &r.state().waiting_for else {
        panic!("independent player, child companion and object prompt")
    };
    assert_eq!(target_slots.len(), 3);
    choose(&mut r, Some(TargetRef::Player(P1)));
    assert!(
        menu(&r).contains(&TargetRef::Player(P1)),
        "child companion reach"
    );
    choose(&mut r, Some(TargetRef::Player(P1)));
    exact(&menu(&r), &objects(&own[0]));
    let WaitingFor::TargetSelection { selection, .. } = &r.state().waiting_for else {
        panic!("object prompt")
    };
    assert_eq!(
        selection.selected_slots,
        vec![Some(TargetRef::Player(P1)), Some(TargetRef::Player(P1))]
    );
    choose(&mut r, Some(TargetRef::Object(own[0][2])));
    r.advance_until_stack_empty();
    assert_zones(&r, &[own[0][2]], Zone::Exile);
    assert_zones(&r, &own[0][..2], Zone::Battlefield);
    assert_zones(&r, &own[1], Zone::Battlefield);
    assert_zones(&r, &[wrong_type], Zone::Battlefield);
    assert_zones(&r, &[wrong_zone], Zone::Graveyard);
    assert_eq!(r.state().objects[&spell].abilities[0], original);
}

fn fanout_definition(
    optional: bool,
    depth: Option<usize>,
    owner: ControllerRef,
    zone: Zone,
) -> AbilityDefinition {
    let mut tf = TypedFilter::creature();
    tf.controller = Some(ControllerRef::TargetPlayer);
    tf.properties = vec![
        FilterProp::InZone { zone },
        FilterProp::Owned { controller: owner },
    ];
    if let Some(depth) = depth {
        let visible = own_creature(ControllerRef::You, Zone::Battlefield);
        let mut source = CardTypeSetSource::Objects {
            filter: visible.clone(),
        };
        for _ in 0..depth {
            source = CardTypeSetSource::any_of(vec![
                source,
                CardTypeSetSource::Objects {
                    filter: visible.clone(),
                },
            ])
            .unwrap();
        }
        tf.properties.push(FilterProp::Cmc {
            comparator: Comparator::GE,
            value: QuantityExpr::Ref {
                qty: QuantityRef::DistinctCardTypes { source },
            },
        });
    }
    let mut def = exile_definition(TargetFilter::Typed(tf));
    def.optional_targeting = optional;
    def.multi_target = Some(MultiTargetSpec::bounded(
        usize::from(!optional),
        QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::Opponent,
            },
        },
    ));
    def
}

// CR 109.5 + CR 108.3: each opponent narrows control while the declaring owner remains fixed.
#[test]
fn per_opponent_owner_binding_keeps_each_opponents_stolen_pool() {
    let mut s = board();
    let own = stolen_board(&mut s);
    let spell = typed_spell(
        &mut s,
        fanout_definition(false, None, ControllerRef::You, Zone::Battlefield),
    );
    let mut r = s.build();
    begin(&mut r, spell).unwrap();
    let WaitingFor::TargetSelection { target_slots, .. } = &r.state().waiting_for else {
        panic!("fanout target prompt")
    };
    assert_eq!(
        target_slots.len(),
        4,
        "fanout must produce two real player/object pairs"
    );
    for (p, ids) in [(P1, &own[0]), (P2, &own[1])] {
        fanout_menu(&r, p, ids);
        choose(&mut r, Some(TargetRef::Object(ids[2])));
    }
    r.advance_until_stack_empty();
    assert_zones(&r, &[own[0][2], own[1][2]], Zone::Exile);
    assert_zones(&r, &own[0][..2], Zone::Battlefield);
    assert_zones(&r, &own[1][..2], Zone::Battlefield);
}

// CR 601.2c: an unknown owner-qualified pool cannot authorize announcement.
#[test]
fn per_opponent_incomplete_binding_refuses_mandatory_and_optional_casts() {
    for optional in [false, true] {
        let mut s = board();
        let own = stolen_board(&mut s);
        let def = fanout_definition(
            optional,
            Some(usize::try_from(UNION_DEPTH_BUDGET).expect("union budget fits usize")),
            ControllerRef::You,
            Zone::Battlefield,
        );
        let before = def.clone();
        let spell = typed_spell(&mut s, def);
        let mut r = s.build();
        let result = begin(&mut r, spell);
        assert!(
            matches!(result, Err(engine::game::engine::EngineError::ActionNotAllowed(ref message)) if message == "No legal targets available"),
            "incomplete owner binding must refuse even optional announcement"
        );
        assert_eq!(
            r.state().objects[&spell].abilities[0],
            before,
            "definition is immutable"
        );
        assert_zones(&r, &[spell], Zone::Hand);
        assert!(r.state().stack.is_empty());
        assert!(
            matches!(r.state().waiting_for, WaitingFor::Priority { .. }),
            "no partial menu published"
        );
        assert_eq!(r.state().players[0].mana_pool.mana.len(), 12);
        assert_zones(&r, &own[0], Zone::Battlefield);
    }
}

// CR 601.2c: complete binding distinguishes actual legal targets from an empty optional pool.
#[test]
fn per_opponent_within_budget_binding_and_optional_empty_are_valid() {
    let mut s = board();
    let own = stolen_board(&mut s);
    let spell = typed_spell(
        &mut s,
        fanout_definition(false, Some(2), ControllerRef::You, Zone::Battlefield),
    );
    let mut r = s.build();
    begin(&mut r, spell).unwrap();
    for (p, ids) in [(P1, &own[0]), (P2, &own[1])] {
        fanout_menu(&r, p, ids);
        choose(&mut r, Some(TargetRef::Object(ids[2])));
    }
    r.advance_until_stack_empty();
    assert_zones(&r, &[own[0][2], own[1][2]], Zone::Exile);
    let mut s = board();
    s.add_creature(P1, "Native only", 2, 2);
    let spell = typed_spell(
        &mut s,
        fanout_definition(true, None, ControllerRef::You, Zone::Battlefield),
    );
    let mut r = s.build();
    let out = r.cast(spell).resolve();
    out.assert_zone(&[spell], Zone::Graveyard);
}

// CR 601.2c: a declared target-player owner restriction follows that selected player.
#[test]
fn declared_target_player_owner_still_means_the_selected_players_graveyard() {
    let mut s = board();
    let gy = graves(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Target-player grammar",
            true,
            "Exile target creature card from target player's graveyard.",
        )
        .id();
    let mut r = s.build();
    begin(&mut r, spell).unwrap();
    choose(&mut r, Some(TargetRef::Player(P1)));
    exact(&menu(&r), &objects(&gy[1]));
    choose(&mut r, Some(TargetRef::Object(gy[1][2])));
    r.advance_until_stack_empty();
    assert_zones(&r, &[gy[1][2]], Zone::Exile);
    assert_zones(&r, &gy[0], Zone::Graveyard);
}

// CR 109.5: an independent target-player choice does not replace an explicit owner authority.
#[test]
fn literal_and_source_chosen_owner_do_not_follow_prior_player() {
    for owner in [
        ControllerRef::SpecificPlayer { id: P2 },
        ControllerRef::SourceChosenPlayer,
    ] {
        let mut s = board();
        let gy = graves(&mut s);
        let mut def = parse_oracle_text(
            GRAMMAR,
            "Alternate-authority fixture",
            &[],
            &["Instant".into()],
            &[],
        )
        .abilities
        .remove(0);
        let Effect::ChangeZone { target, .. } = def.sub_ability.as_mut().unwrap().effect.as_mut()
        else {
            panic!("chain reach guard")
        };
        *target = own_creature(owner, Zone::Graveyard);
        let spell = typed_spell(&mut s, def);
        let mut r = s.build();
        r.state_mut()
            .objects
            .get_mut(&spell)
            .unwrap()
            .chosen_attributes
            .push(ChosenAttribute::Player(P2));
        begin(&mut r, spell).unwrap();
        choose(&mut r, Some(TargetRef::Player(P1)));
        exact(&menu(&r), &objects(&gy[2]));
        choose(&mut r, Some(TargetRef::Object(gy[2][2])));
        r.advance_until_stack_empty();
        assert_zones(&r, &[gy[2][2]], Zone::Exile);
    }
}

// CR 109.5 + CR 608.2c: chosen draw and return modes keep separate player and owner authorities.
#[test]
fn wretched_full_modes_keep_your_graveyard_independent_from_draw_recipient() {
    let mut s = board();
    let gy = graves(&mut s);
    let unused = s.add_creature(P1, "Unused second mode", 3, 3).id();
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Wretched Confluence", true, WRETCHED)
        .id();
    let mut r = s.build();
    begin(&mut r, spell).unwrap();
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::ModeChoice { .. }
    ));
    r.act(GameAction::SelectModes {
        indices: vec![0, 2, 2],
    })
    .unwrap();
    choose(&mut r, Some(TargetRef::Player(P1)));
    exact(&menu(&r), &objects(&gy[0]));
    choose(&mut r, Some(TargetRef::Object(gy[0][1])));
    exact(&menu(&r), &objects(&gy[0]));
    choose(&mut r, Some(TargetRef::Object(gy[0][2])));
    r.advance_until_stack_empty();
    assert_zones(&r, &gy[0][1..], Zone::Hand);
    assert_zones(&r, &[unused], Zone::Battlefield);
    assert_eq!(r.state().players[1].life, 19);
    assert_eq!(r.state().players[1].hand.len(), 1);
    let mut s = board();
    let gy = graves(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Wretched Confluence", true, WRETCHED)
        .id();
    let mut r = s.build();
    let out = r
        .cast(spell)
        .modes(&[0, 2, 2])
        .target_player(P1)
        .target_objects(&gy[0][1..])
        .resolve();
    out.assert_zone(&gy[0][1..], Zone::Hand);
    out.assert_hand_drawn(P1, 1);
    out.assert_life_delta(P1, -1);
}

// CR 109.5 + CR 113.7a: the selected own-card instruction survives the activated source cost.
#[test]
fn natural_regrowth_and_soul_separator_keep_selected_own_card() {
    let mut s = board();
    let gy = graves(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(P0, "Regrowth", false, REGROWTH)
        .id();
    let mut r = s.build();
    let out = r.cast(spell).target_object(gy[0][2]).resolve();
    out.assert_zone(&[gy[0][2]], Zone::Hand);
    out.assert_zone(&gy[1], Zone::Graveyard);
    let mut s = board();
    let gy = graves(&mut s);
    let source = s
        .add_artifact_from_oracle(P0, "Soul Separator", SEPARATOR)
        .id();
    let mut r = s.build();
    let out = r.activate(source, 0).target_object(gy[0][2]).resolve();
    // CR 113.7a: the activated ownership instruction survives its sacrifice cost.
    out.assert_zone(&[source], Zone::Graveyard);
    out.assert_zone(&[gy[0][2]], Zone::Exile);
    out.assert_zone(&gy[1], Zone::Graveyard);
    // Only the selected-card and paid-source slice is asserted; the Zombie tail is outside this repair.
}

// CR 601.2c: the unqualified graveyard target admits an opponent-owned creature card.
#[test]
fn natural_ooze_can_select_opponents_creature_from_a_graveyard() {
    let mut s = board();
    let gy = graves(&mut s);
    let source = s
        .add_creature_from_oracle(P0, "Scavenging Ooze", 2, 2, OOZE)
        .id();
    let mut r = s.build();
    let out = r.activate(source, 0).target_object(gy[1][2]).resolve();
    out.assert_zone(&[gy[1][2]], Zone::Exile);
    out.assert_life_delta(P0, 1);
    assert_eq!(
        out.state().objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(1)
    );
}

// CR 109.5 + CR 202.3: the printed Your and mana-value restrictions qualify the acquisition pool.
#[test]
fn natural_proxy_normal_cast_etb_uses_own_instant_or_sorcery_pool() {
    let mut s = board();
    let own = [
        s.add_spell_to_graveyard(P0, "Own instant A", true)
            .with_mana_cost(ManaCost::generic(1))
            .id(),
        s.add_spell_to_graveyard(P0, "Own sorcery at power", false)
            .with_mana_cost(ManaCost::generic(4))
            .id(),
    ];
    let opponent = s.add_spell_to_graveyard(P1, "Opponent instant", true).id();
    let wrong_type = s.add_creature_to_graveyard(P0, "Wrong type", 2, 2).id();
    let too_large = s
        .add_spell_to_graveyard(P0, "Above power", true)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let wrong_zone = s
        .add_spell_to_hand(P0, "Hand lookalike", true)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let proxy = s
        .add_creature_to_hand_from_oracle(P0, "Arcane Proxy", 4, 3, PROXY)
        .id();
    let mut r = s.build();
    r.cast(proxy).commit();
    while r.state().objects[&proxy].zone == Zone::Stack {
        r.act(GameAction::PassPriority).unwrap();
    }
    exact(&menu(&r), &objects(&own));
    choose(&mut r, Some(TargetRef::Object(own[1])));
    // The copy/cast tail is outside this repair; inspect the acquired card at its first committed movement.
    for _ in 0..12 {
        if r.state().objects[&own[1]].zone == Zone::Exile {
            break;
        }
        if !matches!(r.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        r.act(GameAction::PassPriority).unwrap();
    }
    assert_zones(&r, &[own[1]], Zone::Exile);
    assert_zones(
        &r,
        &[own[0], opponent, wrong_type, too_large],
        Zone::Graveyard,
    );
    assert_zones(&r, &[wrong_zone], Zone::Hand);
}

#[cfg(feature = "test-support")]
// CR 115.3 + CR 601.2c: one multi-target instance selects distinct owner-qualified cards.
#[test]
fn homogeneous_owner_run_advances_and_optional_sibling_uses_ordinary_selection() {
    for (optional, canonical) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut s = board();
        let gy = graves(&mut s);
        let filter = if canonical {
            TargetFilter::Typed(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .properties(vec![FilterProp::InZone {
                        zone: Zone::Graveyard,
                    }]),
            )
        } else {
            own_creature(ControllerRef::You, Zone::Graveyard)
        };
        let mut def = exile_definition(filter);
        def.multi_target = Some(MultiTargetSpec::fixed(if optional { 0 } else { 2 }, 2));
        let spell = typed_spell(&mut s, def);
        let mut r = s.build();
        engine::game::perf_counters::reset();
        begin(&mut r, spell).unwrap();
        let WaitingFor::TargetSelection {
            target_slots,
            pending_cast,
            ..
        } = &r.state().waiting_for
        else {
            panic!("multi-slot reach guard")
        };
        assert_eq!(target_slots.len(), 2);
        let resolved = build_resolved_from_def(&r.state().objects[&spell].abilities[0], spell, P0);
        assert_eq!(pending_cast.ability.effect, resolved.effect);
        exact(&menu(&r), &objects(&gy[0]));
        choose(&mut r, Some(TargetRef::Object(gy[0][1])));
        exact(&menu(&r), &objects(&[gy[0][0], gy[0][2]]));
        choose(&mut r, Some(TargetRef::Object(gy[0][2])));
        let counts = engine::game::perf_counters::homogeneous_target_walk_cache_snapshot();
        if optional {
            assert_eq!(
                counts.advances, 0,
                "optional sibling rejects cache despite successful selection"
            );
        } else {
            assert!(counts.initializations > 0);
            assert!(
                counts.advances > 0,
                "required second-slot cached advance is reached"
            );
        }
        r.advance_until_stack_empty();
        assert_zones(&r, &gy[0][1..], Zone::Exile);
        assert_zones(&r, &gy[1], Zone::Graveyard);
    }
}

// CR 601.2c: required and optional announcement use the actual owner-qualified pool.
#[test]
fn owner_singleton_missing_and_optional_decline_have_nonempty_controls() {
    for count in [0, 1, 3] {
        let mut s = board();
        let mut own = Vec::new();
        for _ in 0..count {
            own.push(s.add_creature_to_graveyard(P0, "Own", 2, 2).id());
        }
        let opponent = s.add_creature_to_graveyard(P1, "Opponent decoy", 2, 2).id();
        let spell = typed_spell(
            &mut s,
            exile_definition(own_creature(ControllerRef::You, Zone::Graveyard)),
        );
        let mut r = s.build();
        if count == 0 {
            assert!(begin(&mut r, spell).is_err());
            assert_zones(&r, &[spell], Zone::Hand);
        } else {
            let out = r.cast(spell).target_object(*own.last().unwrap()).resolve();
            out.assert_zone(&[*own.last().unwrap()], Zone::Exile);
        }
        assert_zones(&r, &[opponent], Zone::Graveyard);
    }
    for select in [false, true] {
        let mut s = board();
        let gy = graves(&mut s);
        let mut def = exile_definition(own_creature(ControllerRef::You, Zone::Graveyard));
        def.optional_targeting = true;
        let spell = typed_spell(&mut s, def);
        let mut r = s.build();
        let cast = r.cast(spell);
        let out = if select {
            cast.target_object(gy[0][2]).resolve()
        } else {
            cast.resolve()
        };
        out.assert_zone(
            &[gy[0][2]],
            if select { Zone::Exile } else { Zone::Graveyard },
        );
        out.assert_zone(&[spell], Zone::Graveyard);
    }
}

// CR 608.2c: each opponent instruction resumes with that iteration's bound scope.
#[test]
fn public_scoped_opponent_choices_preserve_resumed_scope_and_skip_empty_iteration() {
    // Typed building block: "Each opponent exiles a creature card from their graveyard."
    for empty_first in [false, true] {
        let mut s = board();
        let p0 = s
            .add_creature_to_graveyard(P0, "Controller decoy", 2, 2)
            .id();
        let p1: Vec<_> = if empty_first {
            vec![]
        } else {
            (0..2)
                .map(|i| {
                    s.add_creature_to_graveyard(P1, &format!("P1 {i}"), 2, 2)
                        .id()
                })
                .collect()
        };
        let p2: Vec<_> = (0..2)
            .map(|i| {
                s.add_creature_to_graveyard(P2, &format!("P2 {i}"), 2, 2)
                    .id()
            })
            .collect();
        let wrong_type = s.add_spell_to_graveyard(P1, "Wrong type", true).id();
        let wrong_zone = s.add_creature(P2, "Wrong zone", 2, 2).id();
        let mut def = exile_definition(own_creature(ControllerRef::ScopedPlayer, Zone::Graveyard));
        let Effect::ChangeZone { origin, .. } = def.effect.as_mut() else {
            panic!("typed ChangeZone input")
        };
        *origin = Some(Zone::Graveyard);
        def.player_scope = Some(PlayerFilter::Opponent);
        def.target_choice_timing = TargetChoiceTiming::Resolution;
        let spell = typed_spell(&mut s, def);
        let mut r = s.build();
        let out = r.cast(spell).resolve();
        assert!(matches!(
            out.final_waiting_for(),
            WaitingFor::EffectZoneChoice { .. }
        ));
        if !empty_first {
            let WaitingFor::EffectZoneChoice { player, cards, .. } = &r.state().waiting_for else {
                panic!("P1 choice")
            };
            assert_eq!(*player, P1);
            exact(&objects(cards), &objects(&p1));
            let continuation = r
                .state()
                .active_ability_continuation()
                .expect("P2 scoped continuation exists at the P1 pause");
            let mut node = Some(continuation.chain.as_ref());
            let scoped = loop {
                let current = node.expect("matching P2 ChangeZone continuation");
                if current.source_id == spell
                    && matches!(
                        current.effect,
                        Effect::ChangeZone {
                            origin: Some(Zone::Graveyard),
                            destination: Zone::Exile,
                            ..
                        }
                    )
                {
                    break current;
                }
                node = current.sub_ability.as_deref();
            };
            assert_eq!(scoped.scoped_player, Some(P2));
            assert_eq!(scoped.controller, P2);
            assert_eq!(scoped.original_controller, Some(P0));
            r.act(GameAction::SelectCards { cards: vec![p1[1]] })
                .unwrap();
            assert_zones(&r, &[p1[1]], Zone::Exile);
            assert_zones(&r, &[p1[0]], Zone::Graveyard);
        }
        let WaitingFor::EffectZoneChoice { player, cards, .. } = &r.state().waiting_for else {
            panic!("resumed P2 choice")
        };
        assert_eq!(*player, P2);
        exact(&objects(cards), &objects(&p2));
        r.act(GameAction::SelectCards { cards: vec![p2[1]] })
            .unwrap();
        assert_zones(&r, &[p2[1]], Zone::Exile);
        assert_zones(&r, &[p2[0], p0, wrong_type], Zone::Graveyard);
        assert_zones(&r, &[wrong_zone], Zone::Battlefield);
        assert!(r.state().stack.is_empty());
        assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
    }
}

// CR 601.2c: declared owner authority requires its actual binding; an unqualified graveyard remains unrestricted by owner.
#[test]
fn missing_source_choice_refuses_and_unqualified_graveyard_keeps_all_owners() {
    let mut s = board();
    let gy = graves(&mut s);
    let spell = typed_spell(
        &mut s,
        exile_definition(own_creature(
            ControllerRef::SourceChosenPlayer,
            Zone::Graveyard,
        )),
    );
    let mut r = s.build();
    assert!(begin(&mut r, spell).is_err());
    assert_zones(&r, &[spell], Zone::Hand);
    assert_zones(&r, &gy[2], Zone::Graveyard);
    let mut s = board();
    let gy = graves(&mut s);
    let spell = typed_spell(
        &mut s,
        exile_definition(own_creature(
            ControllerRef::SourceChosenPlayer,
            Zone::Graveyard,
        )),
    );
    let mut r = s.build();
    r.state_mut()
        .objects
        .get_mut(&spell)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Player(P2));
    begin(&mut r, spell).unwrap();
    exact(&menu(&r), &objects(&gy[2]));
    choose(&mut r, Some(TargetRef::Object(gy[2][2])));
    r.advance_until_stack_empty();
    assert_zones(&r, &[gy[2][2]], Zone::Exile);
    let mut s = board();
    let gy = graves(&mut s);
    let spell = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Unqualified grammar",
            true,
            "Target player draws a card. Exile target creature card from a graveyard.",
        )
        .id();
    let mut r = s.build();
    begin(&mut r, spell).unwrap();
    choose(&mut r, Some(TargetRef::Player(P1)));
    let all: Vec<_> = gy.iter().flatten().copied().collect();
    exact(&menu(&r), &objects(&all));
    choose(&mut r, Some(TargetRef::Object(gy[2][2])));
    r.advance_until_stack_empty();
    assert_zones(&r, &[gy[2][2]], Zone::Exile);
    assert_zones(&r, &gy[0], Zone::Graveyard);
}

// CR 601.2c: selected-player ownership and original Your ownership remain distinct in paired menus.
#[test]
fn per_opponent_declared_owner_and_bound_empty_siblings_keep_their_contracts() {
    let mut s = board();
    let gy = graves(&mut s);
    let spell = typed_spell(
        &mut s,
        fanout_definition(false, None, ControllerRef::TargetPlayer, Zone::Graveyard),
    );
    let mut r = s.build();
    begin(&mut r, spell).unwrap();
    for (p, ids) in [(P1, &gy[1]), (P2, &gy[2])] {
        fanout_menu(&r, p, ids);
        choose(&mut r, Some(TargetRef::Object(ids[2])));
    }
    r.advance_until_stack_empty();
    assert_zones(&r, &[gy[1][2], gy[2][2]], Zone::Exile);
    assert_zones(&r, &gy[0], Zone::Graveyard);
    eprintln!(
        "TargetPlayer-owned fanout positive reached both exact menus and selected movement before OwnedYou empty-sibling controls"
    );
    for optional in [false, true] {
        let mut s = board();
        let own: Vec<_> = (0..3)
            .map(|i| {
                s.add_creature(P0, &format!("Only P2 stolen {i}"), 2, 2)
                    .controlled_by(P2)
                    .id()
            })
            .collect();
        let decoy = s.add_creature(P1, "Native P1", 2, 2).id();
        let spell = typed_spell(
            &mut s,
            fanout_definition(optional, None, ControllerRef::You, Zone::Battlefield),
        );
        let mut r = s.build();
        if optional {
            begin(&mut r, spell).unwrap();
            let WaitingFor::TargetSelection { target_slots, .. } = &r.state().waiting_for else {
                panic!("nonempty P2 pair")
            };
            assert_eq!(target_slots.len(), 2);
            fanout_menu(&r, P2, &own);
            choose(&mut r, Some(TargetRef::Object(own[2])));
            r.advance_until_stack_empty();
            assert_zones(&r, &[own[2]], Zone::Exile);
        } else {
            assert!(begin(&mut r, spell).is_err());
            assert_zones(&r, &[spell], Zone::Hand);
            assert_zones(&r, &own, Zone::Battlefield);
        }
        assert_zones(&r, &[decoy], Zone::Battlefield);
    }
}

// CR 109.5 + CR 113.7a: triggered Your retains its triggering controller independently of its source.
#[test]
fn triggered_your_owner_survives_source_control_change_or_removal() {
    for remove_source in [false, true] {
        let mut s = board();
        let gy = graves(&mut s);
        let source = s.add_creature_to_hand_from_oracle(P0, "Triggered grammar fixture", 2, 2,
            "When this creature enters, target player draws a card. Exile target creature card from your graveyard.").id();
        let response = s
            .add_spell_to_hand_from_oracle(
                if remove_source { P0 } else { P1 },
                "Response grammar",
                true,
                if remove_source {
                    "Destroy target creature."
                } else {
                    "Gain control of target creature until end of turn."
                },
            )
            .id();
        let mut r = s.build();
        r.cast(source).commit();
        while r.state().objects[&source].zone == Zone::Stack {
            r.act(GameAction::PassPriority).unwrap();
        }
        choose(&mut r, Some(TargetRef::Player(P2)));
        exact(&menu(&r), &objects(&gy[0]));
        choose(&mut r, Some(TargetRef::Object(gy[0][2])));
        let triggered = r.state().stack.back().unwrap().ability().unwrap();
        assert_eq!(triggered.controller, P0);
        assert!(triggered
            .sub_ability
            .as_ref()
            .unwrap()
            .target_occurrence_is_current(0, r.state()));
        if !remove_source {
            r.act(GameAction::PassPriority).unwrap();
        }
        let out = r.cast(response).target_object(source).resolve();
        if remove_source {
            out.assert_zone(&[source], Zone::Graveyard);
        } else {
            assert_eq!(out.state().objects[&source].controller, P1);
        }
        out.assert_zone(&[gy[0][2]], Zone::Exile);
        out.assert_zone(&gy[1], Zone::Graveyard);
        out.assert_hand_drawn(P2, 1);
    }
}

// CR 400.7 + CR 608.2b: the originally selected incarnation is revalidated at resolution.
#[test]
fn selected_owner_target_does_not_follow_a_new_incarnation() {
    for round_trip in [false, true] {
        let mut s = board();
        let gy = graves(&mut s);
        let spell = s
            .add_spell_to_hand_from_oracle(P0, "Grammar fixture", true, GRAMMAR)
            .id();
        let mut r = s.build();
        r.cast(spell)
            .target_player(P1)
            .target_object(gy[0][2])
            .commit();
        let selected = r
            .state()
            .stack
            .back()
            .unwrap()
            .ability()
            .unwrap()
            .sub_ability
            .as_ref()
            .unwrap();
        assert!(selected.target_occurrence_is_current(0, r.state()));
        let old = r.state().objects[&gy[0][2]].incarnation;
        if round_trip {
            // CR 400.7 + CR 608.2b: an independent public-zone round trip invalidates the announced target.
            // Hostile board setup uses the existing production zone primitive, after public commitment.
            let mut events = vec![];
            engine::game::zones::move_to_zone(r.state_mut(), gy[0][2], Zone::Exile, &mut events);
            assert_zones(&r, &[gy[0][2]], Zone::Exile);
            engine::game::zones::move_to_zone(
                r.state_mut(),
                gy[0][2],
                Zone::Graveyard,
                &mut events,
            );
            assert_ne!(r.state().objects[&gy[0][2]].incarnation, old);
            let selected = r
                .state()
                .stack
                .back()
                .unwrap()
                .ability()
                .unwrap()
                .sub_ability
                .as_ref()
                .unwrap();
            assert!(!selected.target_occurrence_is_current(0, r.state()));
        }
        r.advance_until_stack_empty();
        assert_zones(
            &r,
            &[gy[0][2]],
            if round_trip {
                Zone::Graveyard
            } else {
                Zone::Exile
            },
        );
        assert_zones(&r, &gy[0][..2], Zone::Graveyard);
        assert_eq!(
            r.state().players[1].hand.len(),
            1,
            "independent legal player instruction still resolves"
        );
    }
}

// CR 109.5 + CR 108.4a + CR 400.1: this labelled typed API fixture preserves
// the declaring owner across explicitly listed hand/graveyard zones. No new
// effect-chain grammar is claimed and no single-origin constraint is imposed.
#[test]
fn typed_multi_zone_your_pool_moves_selected_hand_and_graveyard_cards() {
    for from_hand in [false, true] {
        let mut s = board();
        let gy = graves(&mut s);
        let hands: [Vec<ObjectId>; 3] = [P0, P1, P2].map(|p| {
            (0..3)
                .map(|i| {
                    s.add_creature_to_hand(p, &format!("Hand {} {i}", p.0), 2, 2)
                        .id()
                })
                .collect()
        });
        let battlefield = s.add_creature(P0, "Battlefield exclusion", 2, 2).id();
        let wrong_type = s.add_spell_to_hand(P0, "Instant exclusion", true).id();
        let mut def = parse_oracle_text(GRAMMAR, "Typed multi-zone", &[], &["Instant".into()], &[])
            .abilities
            .remove(0);
        let Effect::ChangeZone { target, origin, .. } =
            def.sub_ability.as_mut().unwrap().effect.as_mut()
        else {
            panic!("chain reach")
        };
        *origin = None;
        *target = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::InAnyZone {
                    zones: vec![Zone::Graveyard, Zone::Hand],
                }]),
        );
        let original = def.clone();
        let spell = typed_spell(&mut s, def);
        let mut r = s.build();
        begin(&mut r, spell).unwrap();
        choose(&mut r, Some(TargetRef::Player(P1)));
        let expected: Vec<_> = gy[0].iter().chain(&hands[0]).copied().collect();
        exact(&menu(&r), &objects(&expected));
        let selected = if from_hand { hands[0][2] } else { gy[0][2] };
        choose(&mut r, Some(TargetRef::Object(selected)));
        r.advance_until_stack_empty();
        assert_zones(&r, &[selected], Zone::Exile);
        for id in &gy[0] {
            if *id != selected {
                assert_zones(&r, &[*id], Zone::Graveyard);
            }
        }
        for id in &hands[0] {
            if *id != selected {
                assert_zones(&r, &[*id], Zone::Hand);
            }
        }
        assert_zones(&r, &gy[1], Zone::Graveyard);
        assert_zones(&r, &gy[2], Zone::Graveyard);
        assert_zones(&r, &hands[1], Zone::Hand);
        assert_zones(&r, &hands[2], Zone::Hand);
        assert_zones(&r, &[battlefield], Zone::Battlefield);
        assert_zones(&r, &[wrong_type], Zone::Hand);
        assert_eq!(r.state().players[1].hand.len(), 4);
        assert_eq!(r.state().objects[&spell].abilities[0], original);
    }
}
