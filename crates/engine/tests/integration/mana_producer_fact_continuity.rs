//! CR 106.3 + CR 106.4 + CR 400.7: a mana unit's producing-source binding lasts
//! as long as the unit does, and it never outlives it.
//!
//! Three shapes, all driven through the real pipeline (`GameScenario` /
//! `GameRunner`, real activations, the real restore chokepoint) and all read
//! through Rain of Riches' `FilterProp::ManaSpentFromSource` gate plus the
//! CR 601.2i cast-history record, and two more that pin the SEAM the fact crosses:
//!
//! * **phase boundary** (`retained_pip_keeps_its_producer_across_the_phase_boundary`) —
//!   Oracle verified against Scryfall: **Upwelling** `{3}{G}` Enchantment —
//!   "Players don't lose unspent mana as steps and phases end." Mana produced by
//!   sacrificing a **Treasure** token survives precombat main → postcombat main,
//!   where it pays for the turn's first qualifying spell. CR 106.3 makes the
//!   mana's source the source of the ability that produced it, so the fact must
//!   survive the boundary that ends generic exit LKI — but only for the units a
//!   pool still holds, which is why the generic cache is NOT lengthened here.
//! * **legacy restore** (`legacy_pinless_pool_is_refused_and_pinned_pool_still_pays`) —
//!   a save written before `ManaUnit::source_incarnation` existed carries no way
//!   to prove which object produced its floating mana. The shared restore
//!   chokepoint refuses the pool when the save PROVES the legacy fallback would
//!   read the wrong object — a source storage id that no longer holds a
//!   battlefield permanent, so a live read returns a card in a graveyard
//!   (CR 400.7) — which is the Minimus Containment shape: **Minimus Containment**
//!   `{2}{W}` Aura — "Enchanted permanent is a Treasure artifact with \"{T},
//!   Sacrifice this artifact: Add one mana of any color,\" and it loses all other
//!   abilities." The same test carries the two controls that keep the refusal
//!   honest: a current-format pinned pip must be admitted and still record the
//!   Treasure, and a pin-less pip whose storage id still names a battlefield
//!   permanent must stay admitted, because the save proves no wrong read there.
//! * **stacked trigger** (`trigger_source_mana_is_not_the_returned_copy`) —
//!   **Burning-Tree Emissary** `{R/G}{R/G}` Creature — "When this creature
//!   enters, add {R}{G}." (a triggered ability, NOT a mana ability per CR 605.1b:
//!   it does not trigger from an activated mana ability or from mana being
//!   added, so it uses the stack), **Cloudshift** `{W}` Instant — "Exile target
//!   creature you control, then return that card to the battlefield under your
//!   control.", and **Clever Impersonator** `{2}{U}{U}` Creature — "You may have
//!   this creature enter as a copy of any nonland permanent on the
//!   battlefield." The copy's trigger waits while Cloudshift returns the card as
//!   a Treasure; the trigger's mana is the ORIGINAL object's (CR 400.7), so it
//!   must not qualify for Rain of Riches even though a Treasure now shares the
//!   storage id.
//! * **viewer projection** (`retained_producer_fact_is_stripped_from_the_opponent_view`) —
//!   **Chaos Warp** shuffles the tapped **Llanowar Elves** into its owner's
//!   library while Upwelling holds the pip it made, so the retained producer fact
//!   names a card that is now in a hidden zone (CR 400.2). CR 401.2 forbids
//!   looking at, or reordering, the cards in a library, so the fact must not reach
//!   any viewer's serialized state — while the engine keeps using it.
//! * **pre-pin restore with a live successor**
//!   (`legacy_pinless_pool_is_not_attributed_to_a_battlefield_successor`) — the
//!   same board as the stacked-trigger shape, saved without pins. The guard's old
//!   "is the current object off the battlefield" discriminator admitted it and the
//!   live read priced the trigger's mana as the returned Treasure's; the journal
//!   proves an earlier producing tenure, so the unit's producer is unprovable and
//!   the pool is refused.

use engine::game::derived_views::ClientGameStateRef;
use engine::game::effects::token::predefined_token_abilities;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{
    CastPaymentMode, GameState, ManaChoice, PersistedGameState, PersistedRestoreError,
    StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const UPWELLING: &str = "Players don't lose unspent mana as steps and phases end.";
const RAIN_OF_RICHES: &str = "When this enchantment enters, create two Treasure tokens.\nThe first spell you cast each turn that mana from a Treasure was spent to cast has cascade.";
const MINIMUS_TYPE_CHANGE: &str = "Enchanted permanent is a Treasure artifact with \"{T}, Sacrifice this artifact: Add one mana of any color,\" and it loses all other abilities.";
const ENVIRONMENTAL_SCIENCES: &str = "Search your library for a basic land card, reveal it, put it into your hand, then shuffle. You gain 2 life.";
const BURNING_TREE_EMISSARY: &str = "When this creature enters, add {R}{G}.";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";
const CLEVER_IMPERSONATOR: &str =
    "You may have this creature enter as a copy of any nonland permanent on the battlefield.";
const BEARS_NAME: &str = "Grizzly Bears";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One battlefield permanent, created the way the engine creates a predefined
/// token: real card types plus the registry's own ability list
/// (`predefined_token_abilities`), so its mana ability is the printed one and
/// not a hand-built approximation.
fn treasure_token(state: &mut GameState, card_id: u64) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Treasure".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Treasure".to_string());
    obj.base_card_types = obj.card_types.clone();
    let abilities = predefined_token_abilities("Treasure");
    *std::sync::Arc::make_mut(&mut obj.abilities) = abilities.clone();
    *std::sync::Arc::make_mut(&mut obj.base_abilities) = abilities;
    id
}

/// A plain land object, so setup mana carries a real (and non-Treasure) source.
fn land(state: &mut GameState, card_id: u64, name: &str, subtype: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.subtypes.push(subtype.to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Pool mana naming a real object as its source (the payment block snapshots a
/// per-unit producer fact only when `source_id` names one).
fn add_mana_from_source(
    state: &mut GameState,
    player: PlayerId,
    mana_type: ManaType,
    amount: u32,
    source: ObjectId,
) {
    let pool = &mut state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(mana_type, source, false, vec![]));
    }
}

fn pool_units(runner: &GameRunner) -> Vec<ManaUnit> {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .units()
        .collect()
}

/// The `{2}` sorcery the qualifying cast is measured on.
fn sciences_in_hand(scenario: &mut GameScenario) -> ObjectId {
    let mut builder = scenario.add_spell_to_hand_from_oracle(
        P0,
        "Environmental Sciences",
        false,
        ENVIRONMENTAL_SCIENCES,
    );
    builder.with_mana_cost(ManaCost::generic(2));
    builder.id()
}

/// Activate a mana ability through the real driver, answering the "one mana of
/// any color" prompt it opens (CR 605.3a: mana abilities do not use the stack).
fn activate_mana_ability(runner: &mut GameRunner, source: ObjectId, color: ManaType) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activating the mana ability must be accepted");
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseManaColor { .. } => {
            runner
                .act(GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(color),
                    count: 1,
                })
                .expect("the colour answer must be accepted");
        }
        WaitingFor::Priority { .. } => {}
        other => panic!("unexpected state after activating a mana ability: {other:?}"),
    }
}

/// Drive priority (answering single-slot prompt waits with the identity/first
/// option) until `done` holds.
fn pass_until(runner: &mut GameRunner, mut done: impl FnMut(&GameRunner) -> bool, what: &str) {
    for _ in 0..400 {
        if done(runner) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority must be accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("identity OrderTriggers must succeed");
            }
            WaitingFor::ReplacementChoice { .. } => {
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("the first replacement option must be accepted");
            }
            // CR 508.1: a board that happens to hold a creature stops at combat.
            // Declaring no attackers is the empty declaration, which keeps the
            // drive to the phases the callers are waiting for.
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: Vec::new(),
                        bands: Vec::new(),
                    })
                    .expect("declaring no attackers must be accepted");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: Vec::new(),
                    })
                    .expect("declaring no blockers must be accepted");
            }
            other => panic!("unhandled waiting state while {what}: {other:?}"),
        }
    }
    panic!("drive loop exhausted while {what}");
}

/// `{2}` paid from two retained pips; the Treasure facts must be intact.
fn assert_treasure_record(state: &GameState, treasure_ids: &[ObjectId]) {
    let record = state.spells_cast_this_turn_by_player[&P0]
        .back()
        .expect("the qualifying cast must be recorded");
    let snapshots = record.mana_source_snapshots();
    assert_eq!(
        snapshots.len(),
        2,
        "both spent units must carry a producer fact; got {snapshots:?}"
    );
    for snapshot in snapshots {
        assert!(
            treasure_ids.contains(&snapshot.source_id),
            "every spent unit came from a Treasure; got {snapshots:?}"
        );
        assert!(
            snapshot.lki.subtypes.iter().any(|s| s == "Treasure"),
            "the retained fact must describe the Treasure artifact, got {:?}",
            snapshot.lki.subtypes
        );
    }
}

// ---------------------------------------------------------------------------
// 1 — the pip survives a phase boundary, so its producer fact must too
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 106.4 + CR 400.7: mana held across a step/phase boundary by
/// Upwelling still answers "mana from a Treasure was spent to cast".
///
/// The Treasure is sacrificed for its own mana in precombat main, so the unit's
/// producing incarnation is already gone when the boundary clears generic exit
/// LKI; the pip survives (Upwelling retains it), the fact survives with the pip,
/// and Rain of Riches' first-qualifying-spell gate therefore grants cascade in
/// postcombat main.
#[test]
fn retained_pip_keeps_its_producer_across_the_phase_boundary() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let rain = scenario
        .add_enchantment_from_oracle(P0, "Rain of Riches", RAIN_OF_RICHES)
        .id();
    let upwelling = scenario
        .add_enchantment_from_oracle(P0, "Upwelling", UPWELLING)
        .id();
    let sciences = sciences_in_hand(&mut scenario);
    let mut runner = scenario.build();
    let first_treasure = treasure_token(runner.state_mut(), 9001);
    let second_treasure = treasure_token(runner.state_mut(), 9002);

    // --- Reach guards: both statics are the real parsed modes. ---
    {
        let state = runner.state();
        assert!(
            state.objects[&upwelling]
                .static_definitions
                .iter_all()
                .any(|static_def| matches!(static_def.mode, StaticMode::StepEndUnspentMana { .. })),
            "reach: Upwelling must parse to a step-end unspent-mana static, got {:?}",
            state.objects[&upwelling]
                .static_definitions
                .iter_all()
                .map(|static_def| &static_def.mode)
                .collect::<Vec<_>>()
        );
        assert!(
            state.objects[&rain]
                .static_definitions
                .iter_all()
                .any(|static_def| {
                    matches!(static_def.mode, StaticMode::CastWithKeyword { .. })
                        && static_def.condition.is_some()
                }),
            "reach: Rain of Riches' static must be a conditioned CastWithKeyword grant"
        );
    }

    // --- Act 1: sacrifice both Treasures for their own mana, in precombat main.
    let first_incarnation = runner.state().objects[&first_treasure].incarnation;
    let second_incarnation = runner.state().objects[&second_treasure].incarnation;
    activate_mana_ability(&mut runner, first_treasure, ManaType::Red);
    activate_mana_ability(&mut runner, second_treasure, ManaType::Green);

    {
        let state = runner.state();
        for (treasure, incarnation) in [
            (first_treasure, first_incarnation),
            (second_treasure, second_incarnation),
        ] {
            assert_eq!(
                state.objects[&treasure].zone,
                Zone::Graveyard,
                "CR 701.21a: the self-sacrifice cost moved the Treasure to the graveyard"
            );
            assert_ne!(
                state.objects[&treasure].incarnation, incarnation,
                "CR 400.7: the graveyard card is a new object"
            );
            assert!(
                state
                    .lki_by_incarnation
                    .get(&treasure)
                    .is_some_and(|history| history.contains_key(&incarnation)),
                "reach: the sacrificed Treasure's exit LKI exists inside this step"
            );
        }
        let units = pool_units(&runner);
        assert_eq!(units.len(), 2, "reach: two units in the pool");
        for (unit, (treasure, incarnation)) in units.iter().zip([
            (first_treasure, first_incarnation),
            (second_treasure, second_incarnation),
        ]) {
            assert_eq!(unit.source_id, treasure, "CR 106.3: the mana's source");
            assert_eq!(
                unit.source_incarnation,
                Some(incarnation),
                "the unit is pinned to the epoch that produced it"
            );
            assert_eq!(
                unit.expiry, None,
                "Upwelling retains ordinary units; it does not restamp their expiry"
            );
        }
    }

    // --- Act 2: cross the real phase boundary into postcombat main.
    pass_until(
        &mut runner,
        |r| r.state().phase == Phase::PostCombatMain,
        "crossing combat into postcombat main",
    );

    // Reach guards: the pips survived, the sources did not, and the GENERIC exit
    // LKI was cleared — so anything the payment reads now is retained per-pip,
    // not by lengthening the statewide cache.
    {
        let state = runner.state();
        assert_eq!(
            pool_units(&runner).len(),
            2,
            "Upwelling must have retained both units across the boundary"
        );
        assert_eq!(state.phase, Phase::PostCombatMain);
        assert!(
            state.lki_by_incarnation.is_empty(),
            "CR 400.7: generic exit LKI is step-scoped and was cleared at the boundary"
        );
        assert!(
            state.lki_cache.is_empty(),
            "CR 400.7: the broad LKI cache was cleared at the boundary"
        );
        assert_eq!(
            state.pooled_mana_producer_snapshots.len(),
            2,
            "the producer facts of the two floating units must be retained with them"
        );
        for (treasure, incarnation) in [
            (first_treasure, first_incarnation),
            (second_treasure, second_incarnation),
        ] {
            assert!(
                state.pooled_mana_producer_snapshots.iter().any(|snapshot| {
                    snapshot.source_id == treasure && snapshot.source_incarnation == incarnation
                }),
                "the retained fact must name the exact producing epoch"
            );
        }
    }

    // --- Act 3: the retained mana pays for the turn's first qualifying spell.
    {
        let commit = runner.cast(sciences).commit();
        let state = commit.state();
        assert_treasure_record(state, &[first_treasure, second_treasure]);
        let keywords = &state.objects[&sciences].cast_spell_keywords;
        assert!(
            keywords.iter().any(|k| matches!(k, Keyword::Cascade)),
            "CR 702.85a + CR 611.3b: the first spell cast with Treasure mana must gain \
             cascade even though the Treasure died a phase ago; got {keywords:?}"
        );
        let cascade_triggers = state
            .stack
            .iter()
            .filter(|entry| {
                entry.source_id == sciences
                    && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                    && entry
                        .ability()
                        .is_some_and(|ability| matches!(ability.effect, Effect::Cascade))
            })
            .count();
        assert_eq!(
            cascade_triggers, 1,
            "the granted cascade must enqueue exactly one Cascade trigger"
        );
    }
}

// ---------------------------------------------------------------------------
// 2 — a legacy pin-less pool is refused; a pinned pool is admitted and pays
// ---------------------------------------------------------------------------

/// The Minimus Containment board: Rain of Riches and a Bears enchanted by the
/// Aura, so the Bears is a Treasure artifact with exactly the granted mana
/// ability. Returns the runner and the ids the assertions need.
fn minimus_board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.add_enchantment_from_oracle(P0, "Rain of Riches", RAIN_OF_RICHES);
    let bears = scenario.add_creature(P0, BEARS_NAME, 2, 2).id();
    let aura = scenario.add_creature(P0, "Minimus Containment", 0, 0).id();
    let sciences = sciences_in_hand(&mut scenario);

    let mut runner = scenario.build();
    let type_change = parse_oracle_text(
        MINIMUS_TYPE_CHANGE,
        "Minimus Containment",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    )
    .statics
    .into_iter()
    .find(|static_def| {
        static_def.modifications.iter().any(|modification| {
            matches!(
                modification,
                engine::types::ability::ContinuousModification::GrantAbility { .. }
            )
        })
    })
    .expect("Minimus Containment must parse to a GrantAbility type-change static");
    {
        let state = runner.state_mut();
        let aura_obj = state.objects.get_mut(&aura).unwrap();
        aura_obj.card_types.core_types = vec![CoreType::Enchantment];
        aura_obj.card_types.subtypes = vec!["Aura".to_string()];
        aura_obj.base_card_types = aura_obj.card_types.clone();
        aura_obj.power = None;
        aura_obj.toughness = None;
        aura_obj.base_power = None;
        aura_obj.base_toughness = None;
        aura_obj.attached_to = Some(engine::game::game_object::AttachTarget::Object(bears));
        aura_obj.static_definitions.push(type_change.clone());
        std::sync::Arc::make_mut(&mut aura_obj.base_static_definitions).push(type_change);
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    (runner, bears, aura, sciences)
}

/// The persisted JSON a save of `state` produces.
fn persisted_json(state: &GameState) -> serde_json::Value {
    serde_json::to_value(PersistedGameState::capture(state.clone())).expect("the save serializes")
}

fn admitted(json: serde_json::Value) -> Result<GameState, PersistedRestoreError> {
    serde_json::from_value::<PersistedGameState>(json)
        .expect("the save decodes")
        .into_game_state()
}

/// Strip the producing-incarnation pin from every pooled unit, reproducing a
/// save written before `ManaUnit::source_incarnation` existed. The field is
/// `Option` with `#[serde(default)]`, so the stripped save still decodes — it is
/// the admission decision, not serde, that must catch it.
fn strip_pool_pins(json: &mut serde_json::Value) -> usize {
    let mut stripped = 0;
    let Some(players) = json
        .get_mut("state")
        .and_then(|state| state.get_mut("players"))
        .and_then(serde_json::Value::as_array_mut)
    else {
        panic!("the save carries players");
    };
    for player in players {
        let Some(mana) = player
            .get_mut("mana_pool")
            .and_then(|pool| pool.get_mut("mana"))
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for unit in mana {
            if let Some(object) = unit.as_object_mut() {
                if object.remove("source_incarnation").is_some() {
                    stripped += 1;
                }
            }
        }
    }
    stripped
}

/// CR 106.3 + CR 400.7: an older save reaches priority with a Minimus
/// Containment permanent's floating mana and no pin. The producing epoch is not
/// recoverable from the save (the only incarnation it records is the CURRENT
/// object's, and the graveyard card is a different object), and the save does
/// prove the spend would read that card — so the shared restore chokepoint
/// refuses the pool. Two controls pin the decision in both directions: the same
/// board with the pin intact (what the current engine writes) is admitted and
/// records the Treasure, and a pin-less unit still sourced from a battlefield
/// permanent keeps loading.
#[test]
fn legacy_pinless_pool_is_refused_and_pinned_pool_still_pays() {
    let (mut runner, bears, _aura, sciences) = minimus_board();

    // Sacrifice the enchanted Bears for its granted mana, exactly as a real game
    // would: the unit is stamped with the epoch that generated the ability.
    let producing_incarnation = runner.state().objects[&bears].incarnation;
    activate_mana_ability(&mut runner, bears, ManaType::Green);
    let units = pool_units(&runner);
    assert_eq!(units.len(), 1, "reach: one floating unit");
    assert_eq!(
        units[0].source_incarnation,
        Some(producing_incarnation),
        "reach: the current engine pins the producing epoch"
    );
    assert_eq!(
        runner.state().objects[&bears].zone,
        Zone::Graveyard,
        "reach: the source card is now in the graveyard"
    );

    // --- The legacy shape: the same pool, saved before the pin existed. ---
    {
        let mut legacy = persisted_json(runner.state());
        assert_eq!(
            strip_pool_pins(&mut legacy),
            1,
            "exactly one unit must lose its pin to reproduce the legacy save"
        );
        match admitted(legacy) {
            Err(PersistedRestoreError::AmbiguousFloatingManaProvenance { player, source_id }) => {
                assert_eq!(player, P0);
                assert_eq!(
                    source_id, bears,
                    "the refused unit is the one whose producer cannot be proven"
                );
            }
            Err(other) => panic!("expected an ambiguous-provenance refusal, got {other:?}"),
            Ok(_) => panic!(
                "a pin-less unit whose source storage id now names a graveyard card must be \
                 refused: the legacy spend fallback would record that card as the producer \
                 (CR 400.7)"
            ),
        }
    }

    // --- The over-rejection guard: a pin-less unit whose storage id still names a
    // battlefield permanent keeps loading. The save does not prove a wrong read
    // (the live object is the same one the pre-pin engine read), so refusing it
    // would reject historical snapshots for an ambiguity nothing establishes.
    {
        // The permanent exists BEFORE the save is taken, so the save really
        // carries a live battlefield object at that storage id — a save
        // serialized first and re-pointed afterwards would only prove what the
        // guard does with an id no object answers for.
        let live_land = land(runner.state_mut(), 9300, "Forest", "Forest");
        let live_incarnation = runner.state().objects[&live_land].incarnation;
        let mut legacy_with_live_source = persisted_json(runner.state());
        assert_eq!(strip_pool_pins(&mut legacy_with_live_source), 1);
        // Replace the departed Treasure's unit with one whose source is that live
        // permanent, leaving the pin stripped.
        for player in legacy_with_live_source["state"]["players"]
            .as_array_mut()
            .expect("players")
        {
            for unit in player["mana_pool"]["mana"]
                .as_array_mut()
                .expect("pool units")
            {
                unit["source_id"] = serde_json::json!(live_land.0);
            }
        }
        let admitted_state = admitted(legacy_with_live_source).unwrap_or_else(|error| {
            panic!(
                "a pin-less unit whose storage id still names battlefield permanent \
                 {live_land:?} (incarnation {live_incarnation}) must stay admitted, got {error:?}"
            )
        });
        let source = admitted_state.objects.get(&live_land).unwrap_or_else(|| {
            panic!(
                "the restored save must carry the live source object {live_land:?}; a save \
                 without it would only exercise an id no object answers for"
            )
        });
        assert_eq!(
            (source.zone, source.incarnation),
            (Zone::Battlefield, live_incarnation),
            "reach: the restored unit's storage id answers for a live battlefield permanent"
        );
        let unit = admitted_state.players[P0.0 as usize]
            .mana_pool
            .units()
            .next()
            .expect("the unit survives");
        assert_eq!(
            unit.source_incarnation,
            Some(live_incarnation),
            "CR 106.3 + CR 400.7: admission BINDS the proved epoch. The save proves \
             the live object is the producer, so leaving the unit pin-less would let \
             a later blink of that same storage id re-point the read at its successor"
        );
    }

    // --- The current-format control: the pin survives the round trip. ---
    let mut restored = GameRunner::from_state(
        admitted(persisted_json(runner.state())).expect("a current-format save is admitted"),
    );
    let restored_unit = pool_units(&restored)
        .into_iter()
        .next()
        .expect("the floating unit survives the round trip");
    assert_eq!(
        restored_unit.source_incarnation,
        Some(producing_incarnation),
        "reach: the restored unit still carries the producing pin"
    );

    // A second, non-Treasure mana source so `{2}` is payable without weakening
    // the assertion: only the Treasure unit may qualify.
    let forest = land(restored.state_mut(), 9100, "Forest", "Forest");
    add_mana_from_source(restored.state_mut(), P0, ManaType::Green, 1, forest);

    {
        let commit = restored.cast(sciences).commit();
        let state = commit.state();
        let record = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the cast must be recorded");
        let snapshots = record.mana_source_snapshots();
        assert_eq!(snapshots.len(), 2, "both units stamp a producer fact");
        let treasure = snapshots
            .iter()
            .find(|snapshot| snapshot.source_id == bears)
            .unwrap_or_else(|| {
                panic!("the snapshot list must name the sacrificed Bears: {snapshots:?}")
            });
        assert!(
            treasure.lki.subtypes.iter().any(|s| s == "Treasure"),
            "CR 106.3 + CR 400.7: the pinned unit records the Treasure artifact, not the \
             graveyard card; got {:?}",
            treasure.lki.subtypes
        );
        let keywords = &state.objects[&sciences].cast_spell_keywords;
        assert!(
            keywords.iter().any(|k| matches!(k, Keyword::Cascade)),
            "the restored pinned pip must still qualify for Rain of Riches; got {keywords:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3 — a triggered ability's mana comes from the trigger's own source
// ---------------------------------------------------------------------------

/// CR 106.4 + CR 500.5 + CR 703.4q: whether the shared board is under a step-end
/// retention effect — the only way a floating unit outlives the turn whose
/// zone-change journal a save carries. A typed choice rather than a `bool`
/// because "no retention" is a claim about what the pool does at every boundary,
/// not a switch on one card.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ManaRetention {
    /// Nothing keeps the pool: CR 106.4 empties it at the end of every step and
    /// phase, so every floating unit was produced during the turn in play.
    Empties,
    /// Upwelling — "Players don't lose unspent mana as steps and phases end."
    /// The pool survives every boundary, the turn's last included.
    Upwelling,
}

/// The shared board of tests 3 and 5, driven to the moment the original
/// Emissary trigger has resolved and its two pinned units are floating.
///
/// A Clever Impersonator entering as a copy of Burning-Tree Emissary puts
/// "add {R}{G}" on the stack (CR 605.1b does not apply); Cloudshift returns the
/// card to the battlefield as a Treasure copy while that trigger waits, so the
/// storage id holds a LIVE battlefield object that is not the producer. Returns
/// the runner, that storage id, the epoch that generated the trigger, and the
/// `{2}` spell the floating mana is spent on.
fn clever_impersonator_trigger_pool(
    retention: ManaRetention,
) -> (GameRunner, ObjectId, u64, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    // CR 704.5b: the opponent must be able to draw, or crossing a turn ends the
    // game before the pool this test is about can be saved.
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest"]);
    scenario.add_enchantment_from_oracle(P0, "Rain of Riches", RAIN_OF_RICHES);
    if retention == ManaRetention::Upwelling {
        scenario.add_enchantment_from_oracle(P0, "Upwelling", UPWELLING);
    }
    let donor = scenario
        .add_creature_from_oracle(P0, "Burning-Tree Emissary", 2, 2, BURNING_TREE_EMISSARY)
        .id();
    let impersonator = {
        let mut builder = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Clever Impersonator",
            0,
            0,
            CLEVER_IMPERSONATOR,
        );
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 2,
        });
        builder.id()
    };
    let cloudshift = {
        let mut builder =
            scenario.add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        });
        builder.id()
    };
    let sciences = sciences_in_hand(&mut scenario);

    let mut runner = scenario.build();
    // The copy donor for the RETURNED card: a Treasure token it can become.
    let treasure = treasure_token(runner.state_mut(), 9200);
    let island = land(runner.state_mut(), 9201, "Island", "Island");
    let plains = land(runner.state_mut(), 9202, "Plains", "Plains");
    // `{2}{U}{U}` for the Impersonator, `{W}` for Cloudshift. Every unit names a
    // real, non-Treasure source so a stray qualification is visible.
    add_mana_from_source(runner.state_mut(), P0, ManaType::Blue, 2, island);
    add_mana_from_source(runner.state_mut(), P0, ManaType::Colorless, 2, island);
    add_mana_from_source(runner.state_mut(), P0, ManaType::White, 1, plains);

    // --- Act 1: the Impersonator enters as a copy of the Emissary.
    runner
        .act(GameAction::CastSpell {
            object_id: impersonator,
            card_id: runner.state().objects[&impersonator].card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Clever Impersonator must be accepted");
    runner.advance_until_stack_empty();
    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { candidates, .. } => {
            assert_eq!(
                candidates.len(),
                2,
                "the optional enter-as-a-copy replacement offers accept and decline"
            );
        }
        other => panic!("expected the enter-as-a-copy offer, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the copy replacement");
    match runner.state().waiting_for.clone() {
        WaitingFor::CopyTargetChoice { valid_targets, .. } => {
            assert!(
                valid_targets.contains(&donor),
                "the donor Emissary must be offered"
            );
        }
        other => panic!("expected the copy target prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        })
        .expect("copy the Burning-Tree Emissary");

    // Reach guard: it really is a copy of the Emissary, and its trigger is on the
    // stack (a triggered ability, not an inline mana ability).
    let copied_incarnation = {
        let state = runner.state();
        let object = &state.objects[&impersonator];
        assert_eq!(
            object.zone,
            Zone::Battlefield,
            "the copy is on the battlefield"
        );
        assert_eq!(
            object.name, "Burning-Tree Emissary",
            "reach: the copy took the Emissary's name"
        );
        let triggers: Vec<_> = state
            .stack
            .iter()
            .filter(|entry| {
                entry.source_id == impersonator
                    && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
            })
            .collect();
        assert_eq!(
            triggers.len(),
            1,
            "the copied 'when this creature enters' trigger must be on the stack, got {:?}",
            state.stack
        );
        object.incarnation
    };

    // --- Act 2: Cloudshift the copy in response; it returns as a Treasure.
    runner.cast(cloudshift).target_object(impersonator).commit();
    // Cloudshift resolves (exile, then return), the returned card re-enters as a
    // copy of the Treasure token.
    pass_until(
        &mut runner,
        |r| {
            r.state()
                .objects
                .get(&impersonator)
                .is_some_and(|object| object.zone == Zone::Battlefield)
                && r.state().objects[&impersonator].incarnation != copied_incarnation
        },
        "Cloudshift returning the card",
    );
    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { .. } => {
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("accept the returned card's copy replacement");
            match runner.state().waiting_for.clone() {
                WaitingFor::CopyTargetChoice { valid_targets, .. } => {
                    assert!(
                        valid_targets.contains(&treasure),
                        "the Treasure token must be an allowed copy target"
                    );
                }
                other => panic!("expected the return copy target prompt, got {other:?}"),
            }
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(treasure)),
                })
                .expect("return the card as a Treasure copy");
        }
        WaitingFor::CopyTargetChoice { .. } => {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(treasure)),
                })
                .expect("return the card as a Treasure copy");
        }
        WaitingFor::Priority { .. } => {}
        other => panic!("expected the returned card's entry prompt, got {other:?}"),
    }

    // Reach guard: the returned object shares the storage id and IS a Treasure,
    // with a new incarnation (CR 400.7).
    {
        let state = runner.state();
        let returned = &state.objects[&impersonator];
        assert_eq!(returned.zone, Zone::Battlefield);
        assert_ne!(
            returned.incarnation, copied_incarnation,
            "CR 400.7: the returned card is a new object"
        );
        assert!(
            returned.card_types.subtypes.iter().any(|s| s == "Treasure"),
            "reach: the returned copy must be a Treasure, got {:?}",
            returned.card_types.subtypes
        );
    }

    // --- Act 3: the original trigger resolves and adds its {R}{G}.
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving Cloudshift and the copied Emissary trigger",
    );
    let trigger_units = pool_units(&runner);
    assert_eq!(
        trigger_units.len(),
        2,
        "reach: the trigger added {{R}}{{G}}, got {trigger_units:?}"
    );
    for unit in &trigger_units {
        assert_eq!(
            unit.source_id, impersonator,
            "CR 106.3: the trigger's mana names the storage id it came from"
        );
        assert_eq!(
            unit.source_incarnation,
            Some(copied_incarnation),
            "CR 113.7 + CR 400.7: the mana's source is the object that generated the \
             trigger, not the Treasure now holding the storage id"
        );
    }

    (runner, impersonator, copied_incarnation, sciences)
}

/// CR 106.3 + CR 113.7 + CR 400.7: the trigger keeps the object that generated
/// it, so paying for the turn's first spell with the trigger's mana must NOT
/// qualify for Rain of Riches — no cascade, and no Treasure fact in the cast
/// history — even though a Treasure now occupies the storage id.
#[test]
fn trigger_source_mana_is_not_the_returned_copy() {
    let (mut runner, impersonator, _copied_incarnation, sciences) =
        clever_impersonator_trigger_pool(ManaRetention::Empties);

    // --- Act 4: spend that mana on the turn's first spell.
    {
        let commit = runner.cast(sciences).commit();
        let state = commit.state();
        let record = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the cast must be recorded");
        let snapshots = record.mana_source_snapshots();
        assert_eq!(
            snapshots.len(),
            2,
            "both spent units must stamp a producer fact; got {snapshots:?}"
        );
        for snapshot in snapshots {
            assert_eq!(
                snapshot.source_id, impersonator,
                "the spent mana came from the storage id the trigger named"
            );
            assert!(
                !snapshot.lki.subtypes.iter().any(|s| s == "Treasure"),
                "CR 400.7: the trigger's mana must not be attributed to the returned \
                 Treasure; got {:?}",
                snapshot.lki.subtypes
            );
        }
        let keywords = &state.objects[&sciences].cast_spell_keywords;
        assert!(
            !keywords.iter().any(|k| matches!(k, Keyword::Cascade)),
            "Rain of Riches must not grant cascade: no Treasure produced this mana; \
             got {keywords:?}"
        );
        let cascade_triggers = state
            .stack
            .iter()
            .filter(|entry| {
                matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                    && entry
                        .ability()
                        .is_some_and(|ability| matches!(ability.effect, Effect::Cascade))
            })
            .count();
        assert_eq!(cascade_triggers, 0, "no cascade trigger may be enqueued");
    }
}

// ---------------------------------------------------------------------------
// 4 — a retained producer fact is engine-private, never viewer-facing
// ---------------------------------------------------------------------------

/// Chaos Warp, verbatim (Scryfall).
const CHAOS_WARP: &str = "The owner of target permanent shuffles it into their \
                          library, then reveals the top card of their library. If \
                          it's a permanent card, they put it onto the battlefield.";
/// Llanowar Elves' printed mana ability, verbatim (Scryfall).
const LLANOWAR_ELVES: &str = "{T}: Add {G}.";
/// The viewer-facing wire of `state` for `viewer`, exactly as `phase-server`
/// sends it (`ClientGameStateRef::wrap_filtered` over that viewer's filter
/// output), rendered as text so an assertion can search the whole payload.
fn viewer_wire_text(state: &GameState, viewer: PlayerId) -> String {
    let projection = filter_state_for_viewer(state, viewer);
    let wrapped = ClientGameStateRef::wrap_filtered(state, &projection, Some(viewer));
    serde_json::to_string(&wrapped).expect("the viewer wire serializes")
}

/// CR 106.3 + CR 106.4 + CR 400.2 + CR 401.2: a floating unit's retained
/// producer fact is engine-private. Upwelling keeps a Llanowar Elves pip in the
/// pool across the step boundary that ends the generic LKI window, so the fact
/// CR 106.3 owes that unit's eventual payment is copied onto the pip. Chaos Warp
/// then shuffles the Elves into its owner's library, where CR 400.2 makes it a
/// card in a hidden zone: CR 401.2 keeps "the order of cards in a library"
/// unlookable, and a payload that still carried the shuffled card's name would
/// tell the opponent exactly which card sits in that library — the one thing
/// 401.2 withholds. The projection must therefore strip the fact for every
/// viewer the object is hidden from, while the authoritative state keeps it for
/// the payment that will spend the unit.
#[test]
fn retained_producer_fact_is_stripped_from_the_opponent_view() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A library deep enough that the Chaos Warp shuffle leaves the Elves in it;
    // the reach guards below verify that rather than assume it.
    scenario.with_library_top(
        P0,
        &[
            "Forest", "Forest", "Forest", "Forest", "Forest", "Forest", "Forest", "Forest",
            "Forest", "Forest", "Forest", "Forest",
        ],
    );
    let _upwelling = scenario
        .add_enchantment_from_oracle(P0, "Upwelling", UPWELLING)
        .id();
    let elves = scenario
        .add_creature_from_oracle(P0, "Llanowar Elves", 1, 1, LLANOWAR_ELVES)
        .id();
    let warp = {
        let mut builder =
            scenario.add_spell_to_hand_from_oracle(P0, "Chaos Warp", true, CHAOS_WARP);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        });
        builder.id()
    };
    let sciences = sciences_in_hand(&mut scenario);

    let mut runner = scenario.build();
    let mountain = land(runner.state_mut(), 9400, "Mountain", "Mountain");
    add_mana_from_source(runner.state_mut(), P0, ManaType::Colorless, 2, mountain);
    add_mana_from_source(runner.state_mut(), P0, ManaType::Red, 1, mountain);

    // --- Act 1: Chaos Warp targets the Elves, and the Elves tap in response.
    let producing_incarnation = runner.state().objects[&elves].incarnation;
    runner.cast(warp).target_object(elves).commit();
    activate_mana_ability(&mut runner, elves, ManaType::Green);

    // --- Act 2: Chaos Warp resolves — the Elves is shuffled into the library.
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving Chaos Warp",
    );
    {
        let state = runner.state();
        assert_eq!(
            state.objects[&elves].zone,
            Zone::Library,
            "reach: Chaos Warp must shuffle the Elves into its owner's library"
        );
        assert!(
            !state.last_revealed_ids.contains(&elves),
            "reach: the Elves must stay UNREVEALED for the hidden-identity projection to \
             apply; the shuffle revealed it"
        );
        assert!(
            state
                .lki_by_incarnation
                .get(&elves)
                .is_some_and(|history| history.contains_key(&producing_incarnation)),
            "reach: the battlefield exit that hid the Elves recorded exit LKI inside this step"
        );
        let units = pool_units(&runner);
        assert_eq!(units.len(), 1, "reach: one floating unit, got {units:?}");
        assert_eq!(
            units[0].source_incarnation,
            Some(producing_incarnation),
            "reach: the unit is pinned to the epoch that produced it"
        );
    }

    // --- Act 3: cross the step boundary, where the fact moves onto the pip.
    pass_until(
        &mut runner,
        |r| r.state().phase == Phase::PostCombatMain,
        "crossing combat into postcombat main",
    );
    {
        let state = runner.state();
        assert_eq!(
            pool_units(&runner).len(),
            1,
            "Upwelling must have retained the unit across the boundary"
        );
        assert!(
            state.lki_by_incarnation.is_empty() && state.lki_cache.is_empty(),
            "CR 400.7: the generic exit LKI was cleared at the boundary"
        );
        let retained = state
            .pooled_mana_producer_snapshots
            .iter()
            .find(|snapshot| {
                snapshot.source_id == elves && snapshot.source_incarnation == producing_incarnation
            })
            .unwrap_or_else(|| {
                panic!(
                    "the retained fact must name the exact producing epoch; got {:?}",
                    state.pooled_mana_producer_snapshots
                )
            });
        assert_eq!(
            retained.lki.name, "Llanowar Elves",
            "reach: the retained fact is last-known information about the Elves"
        );
        assert_eq!(state.objects[&elves].zone, Zone::Library);
    }

    // --- Act 4: the OPPONENT's serialized state must not name the hidden card.
    {
        let projection = filter_state_for_viewer(runner.state(), P1);
        assert_eq!(
            projection.objects[&elves].name, "Hidden Card",
            "reach: the projection already hides the library card's own identity"
        );
        let wire = viewer_wire_text(runner.state(), P1);
        if let Some(at) = wire.find("Llanowar Elves") {
            let from = at.saturating_sub(200);
            let to = (at + 200).min(wire.len());
            panic!(
                "CR 400.2 + CR 401.2: the opponent's state still names the card Chaos Warp \
                 shuffled into the library, so the retained producer fact identifies a hidden \
                 library card: …{}…",
                &wire[from..to]
            );
        }
    }

    // --- Act 5: authority still holds and uses the retained fact.
    {
        let state = runner.state();
        assert!(
            state.pooled_mana_producer_snapshots.iter().any(|snapshot| {
                snapshot.source_id == elves && snapshot.source_incarnation == producing_incarnation
            }),
            "projecting a viewer's state must not mutate the authoritative retained fact"
        );
        let forest = land(runner.state_mut(), 9401, "Forest", "Forest");
        add_mana_from_source(runner.state_mut(), P0, ManaType::Colorless, 1, forest);
        let commit = runner.cast(sciences).commit();
        let state = commit.state();
        let record = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the cast must be recorded");
        let snapshots = record.mana_source_snapshots();
        let producer = snapshots
            .iter()
            .find(|snapshot| snapshot.source_id == elves)
            .unwrap_or_else(|| {
                panic!(
                    "CR 106.3: the retained fact must still answer the payment; got {snapshots:?}"
                )
            });
        assert_eq!(
            producer.lki.name, "Llanowar Elves",
            "the engine itself must keep using the retained producer after the projection \
             stripped it for the viewer"
        );
    }
}

// ---------------------------------------------------------------------------
// 5 — a live battlefield successor is not admissible evidence either
// ---------------------------------------------------------------------------
/// CR 106.3 + CR 400.7: the pre-pin guard keys on "the current object is off the
/// battlefield", which is the wrong discriminator. The stored id may hold a
/// LIVE battlefield object that is not the producer: the Clever Impersonator has
/// returned as a Treasure, so a pin-less pool unit's only recorded incarnation
/// (the CURRENT object's) describes the Treasure, and a live read would price
/// the ORIGINAL trigger's mana with the returned copy's characteristics. The
/// journal proves the same id has already had a producing tenure, so the epoch
/// is not recoverable from the save — the shared restore chokepoint refuses it,
/// exactly as it refuses the graveyard shape. The current-format control (the
/// save the engine writes today, pins intact) stays admitted.
#[test]
fn legacy_pinless_pool_is_not_attributed_to_a_battlefield_successor() {
    let (runner, impersonator, copied_incarnation, sciences) =
        clever_impersonator_trigger_pool(ManaRetention::Empties);

    // Reach guards: the storage id holds a live battlefield Treasure that is a
    // different object from the epoch that produced the mana (CR 400.7).
    {
        let state = runner.state();
        let returned = &state.objects[&impersonator];
        assert_eq!(
            returned.zone,
            Zone::Battlefield,
            "reach: the returned card is on the battlefield, so the off-battlefield \
             discriminator does not fire"
        );
        assert!(
            returned.card_types.subtypes.iter().any(|s| s == "Treasure"),
            "reach: the returned copy must be a Treasure, got {:?}",
            returned.card_types.subtypes
        );
        assert_ne!(
            returned.incarnation, copied_incarnation,
            "CR 400.7: the returned card is a new object"
        );
    }

    // The current-format control: the save the engine writes today is admitted.
    assert!(
        admitted(persisted_json(runner.state())).is_ok(),
        "a current-format save (pins intact) must stay admitted"
    );

    // --- The legacy shape: the same pool, saved before the pin existed. ---
    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_pool_pins(&mut legacy),
        2,
        "both trigger units must lose their pin to reproduce the legacy save"
    );

    match admitted(legacy) {
        Err(PersistedRestoreError::AmbiguousFloatingManaProvenance { player, source_id }) => {
            assert_eq!(player, P0);
            assert_eq!(
                source_id, impersonator,
                "the refused unit is the one whose producer the save cannot prove"
            );
        }
        Err(other) => panic!("expected an ambiguous-provenance refusal, got {other:?}"),
        Ok(state) => {
            // The adjudication a battlefield-only guard permits: the pool is
            // admitted, and the legacy live read prices the ORIGINAL trigger's
            // mana from the returned Treasure. Demonstrated rather than
            // asserted, so a loosened rule fails here with its own consequence.
            let mut restored = GameRunner::from_state(state);
            let commit = restored.cast(sciences).commit();
            let state = commit.state();
            let record = state.spells_cast_this_turn_by_player[&P0]
                .back()
                .expect("the admitted cast must be recorded");
            let named: Vec<&str> = record
                .mana_source_snapshots()
                .iter()
                .map(|snapshot| snapshot.lki.name.as_str())
                .collect();
            let cascade = state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade));
            panic!(
                "CR 106.3 + CR 400.7: a pin-less unit whose storage id names a battlefield \
                 permanent must be refused when the journal proves an earlier producing \
                 tenure; this save was admitted and attributed the trigger's mana to \
                 {named:?} (cascade granted: {cascade})"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 6 — the same board one TURN later: Upwelling outlives the journal
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 106.4 + CR 400.7 + CR 500.5 + CR 703.4q: the turn journal is not
/// an evidence base once a step-end retention effect is in play.
///
/// Test 5's proof is this turn's `zone_changes_this_turn`: it records the
/// pristine copy leaving the battlefield, so the storage id provably had an
/// earlier producing tenure. Turn advancement clears that journal
/// (`turns::prune_turn_scoped_state`), and Upwelling — "Players don't lose
/// unspent mana as steps and phases end" — is exactly what lets the `{R}{G}` the
/// copied Emissary's trigger made outlive the arm of the game that recorded it.
/// With the journal gone, a guard keyed on it is blind precisely where the mana
/// is still alive: the pin-less restore admits the units and the live read prices
/// them from the Treasure now holding the storage id, which is CR 106.3's source
/// question answered about the wrong object and earns Rain of Riches cascade.
///
/// CR 106.4 makes the pool empty at the end of every step and phase, so a unit
/// still floating while no step-end handler keeps it MUST have been produced in
/// the turn whose journal the save carries — that is what makes the journal a
/// complete evidence base, and it is a claim about retention, not about the
/// journal's length. With a handler in play the claim fails, the producing tenure
/// is unprovable, and the shared chokepoint refuses the pool.
///
/// The current-format control (the save the engine writes today, pins intact)
/// stays admitted, and test 2's third leg keeps the no-retention pin-less shape
/// admitted — and now BOUND — so this rule convicts only the ambiguous save.
#[test]
fn legacy_pinless_pool_is_refused_when_upwelling_carries_it_past_the_turn_boundary() {
    let (mut runner, impersonator, copied_incarnation, sciences) =
        clever_impersonator_trigger_pool(ManaRetention::Upwelling);

    // Reach guard: the retention effect is the real parsed static the boundary
    // drains, not a hand-set marker.
    assert!(
        runner
            .state()
            .objects
            .values()
            .any(|object| object.zone == Zone::Battlefield
                && object.name == "Upwelling"
                && object
                    .static_definitions
                    .iter_all()
                    .any(|def| matches!(def.mode, StaticMode::StepEndUnspentMana { .. }))),
        "reach: Upwelling must be on the battlefield as a step-end unspent-mana static"
    );

    // --- Cross a turn so the journal that proved the earlier tenure is gone.
    // Land on P0's next precombat main, where the retained mana can still pay for
    // a spell (CR 106.4: only a retention effect lets it live this long).
    let opening_turn = runner.state().turn_number;
    pass_until(
        &mut runner,
        |r| {
            r.state().turn_number > opening_turn
                && r.state().phase == Phase::PreCombatMain
                && r.state().active_player == P0
        },
        "crossing a full turn boundary with Upwelling holding the pool",
    );

    // Reach guards: the units survived with their pins, and the journal proof is
    // gone — this is the shape the old guard could not see.
    {
        let state = runner.state();
        let units = pool_units(&runner);
        assert_eq!(
            units.len(),
            2,
            "CR 106.4: Upwelling must keep both trigger units in the pool, got {units:?}"
        );
        for unit in &units {
            assert_eq!(unit.source_id, impersonator);
            assert_eq!(
                unit.source_incarnation,
                Some(copied_incarnation),
                "reach: the current-format save still pins the producing epoch"
            );
        }
        assert!(
            state
                .zone_changes_this_turn
                .iter()
                .all(|record| record.object_id != impersonator),
            "reach: turn advancement cleared the journal that convicted this id"
        );
        assert_eq!(
            state.objects[&impersonator].zone,
            Zone::Battlefield,
            "reach: the storage id still holds a LIVE battlefield object"
        );
        assert_ne!(
            state.objects[&impersonator].incarnation, copied_incarnation,
            "CR 400.7: the Treasure now holding the id is not the producer"
        );
        assert!(
            state.objects[&impersonator]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Treasure"),
            "reach: the successor is a Treasure, so a live read would earn cascade"
        );
    }

    // The current-format control: the save the engine writes today is admitted.
    assert!(
        admitted(persisted_json(runner.state())).is_ok(),
        "a current-format save (pins intact) must stay admitted"
    );

    // --- The legacy shape: the same pool, saved before the pin existed. ---
    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_pool_pins(&mut legacy),
        2,
        "both retained units must lose their pin to reproduce the legacy save"
    );

    match admitted(legacy) {
        Err(PersistedRestoreError::AmbiguousFloatingManaProvenance { player, source_id }) => {
            assert_eq!(player, P0);
            assert_eq!(
                source_id, impersonator,
                "the refused unit is the one whose producer the save cannot prove"
            );
        }
        Err(other) => panic!("expected an ambiguous-provenance refusal, got {other:?}"),
        Ok(state) => {
            // The adjudication a turn-scoped guard permits: with the journal
            // cleared, the pool is admitted and the legacy live read prices the
            // ORIGINAL trigger's mana from the returned Treasure. Demonstrated
            // rather than asserted, so a rule that stays keyed on the journal
            // fails here with its own consequence.
            let mut restored = GameRunner::from_state(state);
            let commit = restored.cast(sciences).commit();
            let state = commit.state();
            let record = state.spells_cast_this_turn_by_player[&P0]
                .back()
                .expect("the admitted cast must be recorded");
            let named: Vec<&str> = record
                .mana_source_snapshots()
                .iter()
                .map(|snapshot| snapshot.lki.name.as_str())
                .collect();
            let cascade = state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade));
            panic!(
                "CR 106.3 + CR 106.4 + CR 400.7: a pin-less unit Upwelling carried past \
                 the turn boundary must be refused — the turn journal it was keyed on no \
                 longer covers the producing tenure. This save was admitted and attributed \
                 the trigger's mana to {named:?} (cascade granted: {cascade})"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 7 — the over-rejection edge of that rule: a Stack → Battlefield arrival
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 400.7: the refusal above must convict only on an EARLIER producing
/// tenure. A permanent cast this turn arrives through a `Stack → Battlefield`
/// record, which is the current permanent's own beginning (CR 400.7 makes the
/// permanent the object the spell became) — every creature cast this turn carries
/// one. Convicting on it would refuse a pin-less save whose mana came from the very
/// permanent that is still on the battlefield, so the unit stays admitted.
#[test]
fn a_stack_arrival_this_turn_is_not_an_earlier_producing_tenure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let dork = {
        let mut builder =
            scenario.add_creature_to_hand_from_oracle(P0, "Llanowar Elves", 1, 1, LLANOWAR_ELVES);
        builder.with_mana_cost(ManaCost::generic(1));
        builder.id()
    };

    let mut runner = scenario.build();
    let mountain = land(runner.state_mut(), 9500, "Mountain", "Mountain");
    add_mana_from_source(runner.state_mut(), P0, ManaType::Colorless, 1, mountain);
    runner.cast(dork).resolve();

    // Reach guard: the cast left exactly the record this control is about, and it is
    // the arrival of the tenure the Elves is in now.
    {
        let state = runner.state();
        assert_eq!(
            state.objects[&dork].zone,
            Zone::Battlefield,
            "reach: the Elves resolved onto the battlefield"
        );
        let journal: Vec<(Option<Zone>, Zone)> = state
            .zone_changes_this_turn
            .iter()
            .filter(|record| record.object_id == dork)
            .map(|record| (record.from_zone, record.to_zone))
            .collect();
        assert_eq!(
            journal,
            vec![
                (Some(Zone::Hand), Zone::Stack),
                (Some(Zone::Stack), Zone::Battlefield),
            ],
            "reach: the Elves' journal is the cast and its Stack → Battlefield arrival"
        );
        let arrival = state
            .zone_changes_this_turn
            .iter()
            .find(|record| record.object_id == dork && record.from_zone == Some(Zone::Stack))
            .expect("reach: the arrival record exists");
        assert_eq!(
            arrival.entered_incarnation,
            Some(state.objects[&dork].incarnation),
            "CR 400.7: the arrival record names the epoch now on the battlefield"
        );
    }

    // A pin-less unit sourced from that permanent — the shape a pre-pin save writes
    // for any creature cast this turn.
    add_mana_from_source(runner.state_mut(), P0, ManaType::Green, 1, dork);
    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_pool_pins(&mut legacy),
        0,
        "reach: the hand-added unit already carries no pin"
    );
    let admitted_state = admitted(legacy).unwrap_or_else(|error| {
        panic!(
            "a pin-less unit sourced from a permanent whose only journal entry is its own \
             Stack → Battlefield arrival must stay admitted, got {error:?}"
        )
    });
    let unit = admitted_state.players[P0.0 as usize]
        .mana_pool
        .units()
        .next()
        .expect("the unit survives");
    assert_eq!(
        unit.source_incarnation,
        Some(admitted_state.objects[&dork].incarnation),
        "CR 106.3: the admitted unit is bound to the epoch the save proved — the \
         permanent that is there now — so a later blink cannot re-point it"
    );
    assert_eq!(unit.source_id, dork);
}
