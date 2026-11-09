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
//!   a save written before `ManaUnit::source_incarnation` existed carries no pin, so
//!   the shared restore chokepoint asks what the save still records about THAT unit
//!   (`GameState::saved_producer_epoch`): the journal's own production record for the
//!   pip, the pool's producer ledger for the same pip, or — only where the storage id
//!   answers for no object — the sole epoch its exit LKI names. When the save records
//!   an epoch, admission binds it and the payment answers from it; when it records
//!   nothing, the pool is refused, because a live read would price the unit from
//!   whatever object occupies the storage id now (CR 400.7). That is the Minimus
//!   Containment shape: **Minimus Containment** `{2}{W}` Aura — "Enchanted permanent
//!   is a Treasure artifact with \"{T}, Sacrifice this artifact: Add one mana of any
//!   color,\" and it loses all other abilities." The test's three legs separate the
//!   pinned control, the pin-less save whose record survives, and the same save with
//!   the record removed.
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
//! * **a live successor** (`legacy_pinless_pool_is_not_attributed_to_a_battlefield_successor`)
//!   — the same board as the stacked-trigger shape, saved without pins. The storage
//!   id holds the Treasure Cloudshift returned, which a live read would price the
//!   trigger's mana from; the journal's own copy of each unit names the epoch that
//!   generated the trigger, so the pool is admitted and bound to it, and the same
//!   save with that record removed is refused rather than priced from the successor.
//! * **the retention effect has already left**
//!   (`a_retention_effect_that_has_already_left_is_not_evidence`) and **a departed
//!   producer** (`a_departed_producer_is_admitted_only_when_the_save_names_its_epoch`) —
//!   the two shapes that decided the rule. Turn advancement clears the journal, so
//!   a scan of the board at RESTORE describes the wrong moment: Upwelling can be
//!   destroyed in the very step the pool is restored into (its ruling still gives
//!   the mana until the phase ends), and a Treasure that paid for its own mana
//!   ceases to exist under CR 704.5d, leaving its storage id naming no object at
//!   all. Both are answered from the record the boundary itself wrote —
//!   `GameState::pooled_mana_producer_snapshots`, one entry per still-pooled unit,
//!   keyed by that unit's pip — and the shape that carries neither that record nor
//!   a pin nor a journal account is refused.
//! * **three witnesses** (section 10) on the rule the whole file lands: a save with
//!   neither the pin nor the ledger is refused instead of being bound to the live
//!   successor; a mixed-tenure pool binds EACH unit to its own producer (the ledger
//!   names the older tenure for its own pips, the journal names the newer one for
//!   its pip) and pays exactly like the pinned control; and a restored legacy pool
//!   round trips through a payment, because the boundary stamps the pool's copy AND
//!   the journal's copies of the same unit.
//! * **two pin-validation witnesses** (section 10, after the three above), on the
//!   other half of the same rule: an explicit `ManaUnit::source_incarnation` pin is
//!   a CLAIM, not proof. A retained pool whose pins are re-pointed at the live
//!   successor epoch — the ledger beside them left naming the copy that generated
//!   the trigger — is refused instead of being priced as that successor; and a pin
//!   that would overwrite the journal's own production record for the same unit is
//!   refused rather than stamped over it, because the journal's copies are
//!   independent evidence the save carried and recovery is the only case that
//!   writes them.

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
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
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

/// Remove the resolved-rules journal from a save, reproducing one written before
/// the engine recorded production there at all. The field is `#[serde(default)]`,
/// so the stripped save still decodes; what it can no longer prove is the
/// producer of a unit whose pin is also gone.
fn strip_resolved_rules_journal(json: &mut serde_json::Value) -> bool {
    json.get_mut("state")
        .and_then(|state| state.as_object_mut())
        .and_then(|state| state.remove("resolved_rules_journal"))
        .is_some()
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

/// Re-point every pooled unit's producing-incarnation PIN at `incarnation`,
/// leaving the rest of the save — the pool's per-pip producer ledger, the
/// resolved-rules journal, the object table — exactly as the engine wrote it.
/// That is the shape the admission boundary must catch: one claim about a unit's
/// producer, contradicted by the independent per-pip records beside it.
fn pin_pool_units(json: &mut serde_json::Value, incarnation: u64) -> usize {
    let mut pinned = 0;
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
                object.insert(
                    "source_incarnation".to_string(),
                    serde_json::json!(incarnation),
                );
                pinned += 1;
            }
        }
    }
    pinned
}

/// Strip the pool-carried producer facts (CR 106.3's retained `(source_id,
/// source_incarnation)` ledger), reproducing a save written before that field
/// existed. `#[serde(default)]` keeps the stripped save decodable, so again it is
/// the admission decision — not serde — that must catch what it cannot prove.
fn strip_pool_producer_facts(json: &mut serde_json::Value) -> usize {
    json.get_mut("state")
        .and_then(|state| state.as_object_mut())
        .and_then(|state| state.remove("pooled_mana_producer_snapshots"))
        .and_then(|facts| facts.as_array().map(Vec::len))
        .unwrap_or(0)
}

/// Strip the producing-incarnation pin from every copy the resolved-rules journal
/// keeps of a mana unit — the `produced_mana` record, any `spent_mana` record, and
/// the unit inside the insert/spend command entries — reproducing a save whose
/// journal was written before `ManaUnit::source_incarnation` existed. Those copies
/// are all the same unit, so a genuine pre-pin journal is pin-less in all of them;
/// the producer NODE is deliberately left alone, because it is the record
/// production wrote (`RulesExecutionNodeKind::ActivatedMana` / `TriggeredMana`),
/// not the pin, and a pre-pin journal already carried it.
fn strip_journal_unit_pins(json: &mut serde_json::Value) -> usize {
    /// A serialized `ManaUnit` is the only object in the journal carrying both a
    /// `source_id` and a `pip_id`; strip its pin wherever it appears.
    fn strip(value: &mut serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(object) => {
                let mut stripped = 0;
                if object.contains_key("source_id")
                    && object.contains_key("pip_id")
                    && object.remove("source_incarnation").is_some()
                {
                    stripped += 1;
                }
                for (_, child) in object.iter_mut() {
                    stripped += strip(child);
                }
                stripped
            }
            serde_json::Value::Array(items) => items.iter_mut().map(strip).sum(),
            _ => 0,
        }
    }
    json.get_mut("state")
        .and_then(|state| state.get_mut("resolved_rules_journal"))
        .map(strip)
        .unwrap_or(0)
}

/// Decode a save through the production restore chokepoint, keeping the failure
/// as a value: a save the engine wrote must decode AND be admitted, so a witness
/// that only checked admission would miss a save its own journal rejects at the
/// decoder.
fn decode_and_admit(json: serde_json::Value) -> Result<GameState, String> {
    let persisted = serde_json::from_value::<PersistedGameState>(json)
        .map_err(|error| format!("the save must decode: {error}"))?;
    persisted
        .into_game_state()
        .map_err(|error| format!("the save must be admitted: {error:?}"))
}

/// CR 106.3 + CR 400.7: a save that records production's own account of a
/// floating unit pays exactly like a pinned one, and the same save with that
/// account removed is refused.
///
/// The board is the Minimus Containment shape. An aura grants the enchanted Bears
/// "{T}, Sacrifice this artifact: Add one mana of any color", so activating it
/// produces the mana AND moves the source to the graveyard (CR 400.7 makes the
/// graveyard card a different object from the permanent whose ability made it).
///
/// Three legs, and together they are the whole decision:
///
/// * the save this build writes (pin intact) is admitted and records the Treasure;
/// * the same save with the PIN removed is admitted just the same, because the
///   journal still carries the account production wrote for that pip — the unit
///   and the exact source epoch its ability was activated against — so the answer
///   is proven, not guessed, and this leg pays exactly like the pinned control;
/// * the same save with the pin AND the journal account removed has nothing left
///   but the graveyard card the storage id now names, and CR 400.7 says that card
///   never made this mana. That pool is refused
///   ([`PersistedRestoreError::AmbiguousFloatingManaProvenance`]) — the refusal is
///   about missing evidence, never about the live board being inconvenient.
///
/// The last leg is also what stops the rule from being self-satisfying: the epoch
/// the two admitted legs agree on is the one the ability was activated at, not the
/// incarnation of the card in the graveyard.
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
    assert_eq!(
        runner
            .state()
            .resolved_rules_journal
            .produced_unit(units[0].pip_id)
            .map(|unit| unit.source_id),
        Some(bears),
        "reach: production journaled the unit against its own pip"
    );

    // --- Leg 1: the pin is gone, production's account survives. The save proves
    // the epoch, so it is admitted and must pay exactly like the pinned control.
    {
        let mut legacy = persisted_json(runner.state());
        assert_eq!(
            strip_pool_pins(&mut legacy),
            1,
            "exactly one unit must lose its pin to reproduce the legacy save"
        );
        let mut restored = GameRunner::from_state(admitted(legacy).unwrap_or_else(|error| {
            panic!(
                "the journal records the epoch the granting permanent's ability was \
                 activated at, so the pin-less pool must be admitted and bound, got {error:?}"
            )
        }));
        let unit = pool_units(&restored)
            .into_iter()
            .next()
            .expect("the floating unit survives");
        assert_eq!(
            unit.source_incarnation,
            Some(producing_incarnation),
            "CR 106.3: admission binds the epoch production recorded for this pip"
        );
        // A second, non-Treasure mana source so `{2}` is payable without weakening
        // the assertion: only the Treasure unit may qualify.
        let forest = land(restored.state_mut(), 9200, "Forest", "Forest");
        add_mana_from_source(restored.state_mut(), P0, ManaType::Green, 1, forest);
        let commit = restored.cast(sciences).commit();
        let state = commit.state();
        let snapshots = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the cast must be recorded")
            .mana_source_snapshots();
        let treasure = snapshots
            .iter()
            .find(|snapshot| snapshot.source_id == bears)
            .unwrap_or_else(|| {
                panic!("the snapshot list must name the sacrificed Bears: {snapshots:?}")
            });
        assert!(
            treasure.lki.subtypes.iter().any(|s| s == "Treasure"),
            "CR 106.3 + CR 400.7: the bound unit records the Treasure artifact, not the \
             graveyard card; got {:?}",
            treasure.lki.subtypes
        );
        assert!(
            state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade)),
            "the recovered Treasure unit must still qualify for Rain of Riches"
        );
    }

    // --- Leg 2: the pin AND production's account are gone. Nothing in the save
    // names this unit's producer any more, so the pool is refused.
    {
        let mut unresolvable = persisted_json(runner.state());
        assert_eq!(strip_pool_pins(&mut unresolvable), 1);
        assert!(
            strip_resolved_rules_journal(&mut unresolvable),
            "reach: the journal account must be removed for this leg"
        );
        match admitted(unresolvable) {
            Err(PersistedRestoreError::AmbiguousFloatingManaProvenance { player, source_id }) => {
                assert_eq!(player, P0);
                assert_eq!(
                    source_id, bears,
                    "the refused unit is the one whose producer cannot be proven"
                );
            }
            Err(other) => panic!("expected an ambiguous-provenance refusal, got {other:?}"),
            Ok(_) => panic!(
                "a pin-less unit whose source storage id now names a graveyard card, and \
                 whose save carries no production record for it, must be refused: a live \
                 read would record that card as the producer (CR 400.7)"
            ),
        }
    }

    // --- Leg 3: the live battlefield occupant is never evidence. A pin-less unit
    // whose storage id names a LIVE battlefield permanent, in a save that records
    // no epoch for it, is refused too: CR 400.7 makes the occupant a different
    // object whenever the id changed tenures, and the save does not say whether it
    // did.
    {
        // The permanent exists BEFORE the save is taken, so the save really
        // carries a live battlefield object at that storage id — a save
        // serialized first and re-pointed afterwards would only prove what the
        // guard does with an id no object answers for.
        let live_land = land(runner.state_mut(), 9300, "Forest", "Forest");
        let live_incarnation = runner.state().objects[&live_land].incarnation;
        let mut legacy_with_live_source = persisted_json(runner.state());
        assert_eq!(strip_pool_pins(&mut legacy_with_live_source), 1);
        assert!(strip_resolved_rules_journal(&mut legacy_with_live_source));
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
        assert!(
            matches!(
                admitted(legacy_with_live_source),
                Err(PersistedRestoreError::AmbiguousFloatingManaProvenance { source_id, .. })
                    if source_id == live_land
            ),
            "a pin-less unit the save cannot attribute must be refused even when its storage \
             id names a live battlefield permanent {live_land:?} (incarnation \
             {live_incarnation}): the occupant is a different object whenever the id changed \
             tenures, and nothing in this save rules that out (CR 400.7)"
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
    clever_impersonator_trigger_pool_prepared(retention, |_| {})
}

/// [`clever_impersonator_trigger_pool`] with one hook into the staged scenario,
/// so a witness can add the cards its own step of the story needs (an opponent's
/// removal spell, say) without restating the board.
fn clever_impersonator_trigger_pool_prepared(
    retention: ManaRetention,
    prepare: impl FnOnce(&mut GameScenario),
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
    prepare(&mut scenario);

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
// 5 — a live battlefield successor is not evidence either
// ---------------------------------------------------------------------------
/// CR 106.3 + CR 400.7: the storage id may hold a LIVE battlefield object that is
/// not the producer — the Clever Impersonator has returned as a Treasure — so a
/// pin-less pool unit's only recorded incarnation under that id (the CURRENT
/// object's) describes the returned copy, and reading it would price the ORIGINAL
/// trigger's mana with the returned copy's characteristics.
///
/// Two legs, and the pair is the rule:
///
/// * the save records production's own account of the units (the journal's
///   per-pip producer node), so admission binds the epoch that generated the
///   trigger — even with the storage id holding a live successor — and the `{2}`
///   is priced from the Emissary copy: no Treasure, no cascade. That is the
///   assertion this test exists for, reached by proof instead of refusal;
/// * the same save with that account removed has nothing left but the successor,
///   and the successor is not evidence, so the pool is refused. A live object at
///   the storage id is exactly the shape a guard must NOT answer from.
///
/// The current-format control (the save the engine writes today, pins intact) is
/// admitted on both.
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
            "reach: the returned card is on the battlefield, so an off-battlefield \
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

    // --- Leg 1: the same pool with the pins gone and production's account kept.
    {
        let mut legacy = persisted_json(runner.state());
        assert_eq!(
            strip_pool_pins(&mut legacy),
            2,
            "both trigger units must lose their pin to reproduce the legacy save"
        );
        let mut restored = GameRunner::from_state(admitted(legacy).unwrap_or_else(|error| {
            panic!(
                "the journal records the epoch each unit's trigger was instantiated with, so \
                 the pin-less pool must be admitted and bound, got {error:?}"
            )
        }));
        for unit in pool_units(&restored) {
            assert_eq!(
                unit.source_incarnation,
                Some(copied_incarnation),
                "CR 106.3 + CR 400.7: admission must bind the epoch that generated the \
                 trigger, never the incarnation of the Treasure now holding the storage id"
            );
        }
        let commit = restored.cast(sciences).commit();
        let state = commit.state();
        let named: Vec<&str> = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the restored cast must be recorded")
            .mana_source_snapshots()
            .iter()
            .map(|snapshot| snapshot.lki.name.as_str())
            .collect();
        assert!(
            named.iter().all(|name| *name == "Burning-Tree Emissary"),
            "CR 400.7: the trigger's mana must be priced from the copy that generated it, \
             not the returned Treasure; got {named:?}"
        );
        assert!(
            !state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade)),
            "no Treasure produced this mana, so Rain of Riches must not grant cascade"
        );
    }

    // --- Leg 2: the same pool with the pins AND production's account gone. The
    // only object left is the successor, which is not evidence.
    {
        let mut unresolvable = persisted_json(runner.state());
        assert_eq!(strip_pool_pins(&mut unresolvable), 2);
        assert!(
            strip_resolved_rules_journal(&mut unresolvable),
            "reach: the journal account must be removed for this leg"
        );
        match admitted(unresolvable) {
            Err(PersistedRestoreError::AmbiguousFloatingManaProvenance { player, source_id }) => {
                assert_eq!(player, P0);
                assert_eq!(
                    source_id, impersonator,
                    "the refused unit is the one whose producer the save cannot prove"
                );
            }
            Err(other) => panic!("expected an ambiguous-provenance refusal, got {other:?}"),
            Ok(state) => {
                // The adjudication a live-object fallback permits: the pool is
                // admitted, and the live read prices the ORIGINAL trigger's mana from
                // the returned Treasure. Shown, not asserted, so a reinstated fallback
                // fails here with its consequence.
                let mut restored = GameRunner::from_state(state);
                let commit = restored.cast(sciences).commit();
                let state = commit.state();
                let named: Vec<&str> = state.spells_cast_this_turn_by_player[&P0]
                    .back()
                    .expect("the admitted cast must be recorded")
                    .mana_source_snapshots()
                    .iter()
                    .map(|snapshot| snapshot.lki.name.as_str())
                    .collect();
                let cascade = state.objects[&sciences]
                    .cast_spell_keywords
                    .iter()
                    .any(|keyword| matches!(keyword, Keyword::Cascade));
                panic!(
                    "CR 106.3 + CR 400.7: a pin-less unit whose producer the save cannot \
                     name must be refused even when its storage id holds a live battlefield \
                     object; this save was admitted and attributed the trigger's mana to \
                     {named:?} (cascade granted: {cascade})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 6 — the same board one TURN later: the pool's own producer fact answers
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 106.4 + CR 400.7: the turn journal is not an evidence base once a
/// step-end retention effect is in play, and the pool's own producer fact is.
///
/// Test 5's proof is this turn's `zone_changes_this_turn`: it records the
/// pristine copy leaving the battlefield, so the storage id provably had an
/// earlier producing tenure. Turn advancement clears that journal
/// (`turns::prune_turn_scoped_state`), and Upwelling — "Players don't lose unspent
/// mana as steps and phases end" — is exactly what lets the `{R}{G}` the copied
/// Emissary's trigger made outlive the arm of the game that recorded it. A rule
/// keyed on the journal is blind precisely where the mana is still alive.
///
/// The record that DOES survive is the one the engine writes as the boundary
/// drains: `retain_pooled_mana_producer_snapshots` copies each still-pooled unit's
/// producing epoch onto the pool's own fact ledger before the step-scoped LKI
/// caches are cleared, because CR 106.3's answer must last as long as the unit
/// does. So the pin-less restore of this save BINDS `copied_incarnation` — the
/// epoch that generated the trigger, not the Treasure now holding the storage id —
/// and the `{2}` it pays for earns no cascade and records no Treasure.
///
/// The current-format control (the save the engine writes today, pins intact) is
/// admitted to the same answer, and test 2's third leg keeps the no-retention
/// pin-less shape admitted and bound, so this rule decides only on saved evidence.
#[test]
fn pool_producer_fact_keeps_the_trigger_epoch_across_the_turn_boundary() {
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
        assert!(
            state.pooled_mana_producer_snapshots.iter().any(|snapshot| {
                snapshot.source_id == impersonator
                    && snapshot.source_incarnation == copied_incarnation
            }),
            "reach: the boundary that carried the units wrote their producing epoch onto \
             the pool's own fact ledger; got {:?}",
            state.pooled_mana_producer_snapshots
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

    // The fact ledger is the evidence that survives the cleared journal, so the
    // restore BINDS the epoch it names instead of reading the live board.
    let restored_state = admitted(legacy).unwrap_or_else(|error| {
        panic!(
            "the save carries the producing epoch on the pool's own fact ledger, so the \
             pin-less pool must be admitted and bound, got {error:?}"
        )
    });
    for unit in restored_state.players[P0.0 as usize].mana_pool.units() {
        assert_eq!(unit.source_id, impersonator);
        assert_eq!(
            unit.source_incarnation,
            Some(copied_incarnation),
            "CR 106.3 + CR 400.7: admission must bind the epoch the pool's producer fact \
             names, not the incarnation of the Treasure now holding the storage id"
        );
    }

    let mut restored = GameRunner::from_state(restored_state);
    let commit = restored.cast(sciences).commit();
    let state = commit.state();
    let record = state.spells_cast_this_turn_by_player[&P0]
        .back()
        .expect("the restored cast must be recorded");
    let snapshots = record.mana_source_snapshots();
    assert_eq!(
        snapshots.len(),
        2,
        "the bound epoch's producer fact must answer the payment; got {snapshots:?}"
    );
    for snapshot in snapshots {
        assert!(
            !snapshot.lki.subtypes.iter().any(|s| s == "Treasure"),
            "CR 400.7: the trigger's mana must not be attributed to the returned \
             Treasure; got {:?}",
            snapshot.lki.subtypes
        );
    }
    let cascade = state.objects[&sciences]
        .cast_spell_keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Cascade));
    assert!(
        !cascade,
        "Rain of Riches must not grant cascade: no Treasure produced this mana"
    );
}

// ---------------------------------------------------------------------------
// 7 — the over-rejection edge of that rule: a Stack → Battlefield arrival
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 400.7: the rule above must convict only on evidence about the
/// UNIT, and a `Stack → Battlefield` arrival is never that evidence.
///
/// A permanent cast this turn arrives through such a record, which is the current
/// permanent's own beginning (CR 400.7 makes the permanent the object the spell
/// became) — every creature cast this turn carries one. Treating it as a second
/// tenure would refuse a pin-less save whose mana came from the very permanent on
/// the battlefield now, and that is the over-rejection this control guards.
///
/// The producer here is real production, which is the only shape a saved pool can
/// hold: **Burning-Tree Emissary** `{R/G}{R/G}` — "When this creature enters, add
/// {R}{G}." — is cast, its enters trigger adds the mana (`add_mana_to_pool` stamps
/// a pip and journals the unit against it), and the save then loses the POOL pins.
/// Admission must bind each unit to the epoch that generated the trigger — the
/// permanent that is there now — from the journal's own copy of the unit, and must
/// not convict it on the arrival.
#[test]
fn a_stack_arrival_this_turn_is_not_an_earlier_producing_tenure() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let dork = {
        let mut builder = scenario.add_creature_to_hand_from_oracle(
            P0,
            "Burning-Tree Emissary",
            2,
            2,
            BURNING_TREE_EMISSARY,
        );
        builder.with_mana_cost(ManaCost::generic(2));
        builder.id()
    };

    let mut runner = scenario.build();
    let mountain = land(runner.state_mut(), 9500, "Mountain", "Mountain");
    add_mana_from_source(runner.state_mut(), P0, ManaType::Colorless, 2, mountain);
    runner.cast(dork).resolve();
    // CR 603.3: the enters trigger uses the stack; let it resolve so its mana is in
    // the pool.
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the Emissary's enters trigger",
    );

    // Reach guards: the cast left exactly the record this control is about, and it is
    // the arrival of the tenure the Emissary is in now; the floating units are the
    // ones its trigger made.
    let produced_incarnation = {
        let state = runner.state();
        assert_eq!(
            state.objects[&dork].zone,
            Zone::Battlefield,
            "reach: the Emissary resolved onto the battlefield"
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
            "reach: the Emissary's journal is the cast and its Stack → Battlefield arrival"
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
        let units = pool_units(&runner);
        assert_eq!(
            units.len(),
            2,
            "reach: the trigger's {{R}}{{G}} is floating, got {units:?}"
        );
        for unit in &units {
            assert_eq!(unit.source_id, dork);
        }
        state.objects[&dork].incarnation
    };

    // A save of that pool whose POOL pins are gone. The journal's own copy of each
    // unit still carries the epoch production stamped, and that copy — not the board
    // — is what the guard reads: this trigger resolved OFF the stack rather than as
    // an inline mana ability, so the journal records the unit without a mana node,
    // and the unit's own pin is the only carrier of the epoch.
    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_pool_pins(&mut legacy),
        2,
        "both trigger units must lose their pin to reproduce the legacy save"
    );
    let admitted_state = admitted(legacy).unwrap_or_else(|error| {
        panic!(
            "a pin-less unit whose producer is the permanent this turn's Stack → Battlefield \
             arrival names must stay admitted: the save records the trigger's own source, and \
             the arrival is not a second tenure, got {error:?}"
        )
    });
    let units: Vec<ManaUnit> = admitted_state.players[P0.0 as usize]
        .mana_pool
        .units()
        .collect();
    assert_eq!(units.len(), 2);
    for unit in units {
        assert_eq!(unit.source_id, dork);
        assert_eq!(
            unit.source_incarnation,
            Some(produced_incarnation),
            "CR 106.3: the admitted unit is bound to the epoch the save proved — the \
             permanent that is there now — so a later blink cannot re-point it"
        );
    }
}

// ---------------------------------------------------------------------------
// 8 — the retention effect has ALREADY LEFT when the save is restored
// ---------------------------------------------------------------------------

/// Naturalize, verbatim (Scryfall).
const NATURALIZE: &str = "Destroy target artifact or enchantment.";

/// The id of the first object `name`s in `zone`, which is how a witness names a
/// card the shared fixture helper staged for it.
fn object_named(state: &GameState, name: &str, zone: Zone) -> ObjectId {
    state
        .objects
        .values()
        .find(|object| object.name == name && object.zone == zone)
        .map(|object| object.id)
        .unwrap_or_else(|| panic!("reach: no {name} in {zone:?}"))
}

/// CR 106.3 + CR 106.4 + CR 400.7: a retention effect that has already left is
/// not evidence, and neither is the object that now occupies the producer's
/// storage id.
///
/// This is test 6's board taken one step further: after the `{R}{G}` the copied
/// Emissary's trigger made has been carried past a turn boundary by Upwelling, the
/// OPPONENT destroys Upwelling with Naturalize in the new main phase, and the game
/// is saved before that phase ends. Upwelling's own ruling gives the player "until
/// the end of the current step (or phase) to use the mana before it disappears", so
/// the mana is still legally floating with no retention handler in play (CR 106.4
/// empties a pool at a boundary; this pool has not reached one), and the current
/// turn's `zone_changes_this_turn` holds nothing about the copied Emissary because
/// turn advancement cleared it.
///
/// A guard that asks the live board answers "no handler retains this, so the pool
/// emptied at every boundary, so the unit was produced this turn, so the object at
/// its storage id (the Treasure Cloudshift returned) is its producer" — and stamps
/// the Treasure's epoch onto mana CR 106.3 gave to the object that generated the
/// trigger (CR 400.7: the returned card is a new object). The exact-incarnation read
/// then prices the `{2}` from the Treasure and Rain of Riches grants cascade.
///
/// What the save DOES hold is the pool's own producer fact, written by the boundary
/// that carried the units: the epoch that generated the trigger. Admission binds it,
/// so the payment records the Emissary copy and no cascade is earned.
#[test]
fn a_retention_effect_that_has_already_left_is_not_evidence() {
    let (mut runner, impersonator, copied_incarnation, sciences) =
        clever_impersonator_trigger_pool_prepared(ManaRetention::Upwelling, |scenario| {
            let mut builder =
                scenario.add_spell_to_hand_from_oracle(P1, "Naturalize", true, NATURALIZE);
            builder.with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            });
        });
    let naturalize = object_named(runner.state(), "Naturalize", Zone::Hand);

    // --- Cross a turn so the journal that recorded the earlier tenure is gone.
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
    let upwelling = object_named(runner.state(), "Upwelling", Zone::Battlefield);
    {
        let state = runner.state();
        assert_eq!(
            pool_units(&runner).len(),
            2,
            "reach: Upwelling carried both trigger units into this turn"
        );
        assert!(
            state
                .zone_changes_this_turn
                .iter()
                .all(|record| record.object_id != impersonator),
            "reach: turn advancement cleared the journal that convicted this id"
        );
        assert_ne!(
            state.objects[&impersonator].incarnation, copied_incarnation,
            "reach: the Treasure now holding the storage id is not the producer"
        );
    }

    // --- The OPPONENT destroys Upwelling. It must be the opponent's mana: a
    // payment from P0's pool would spend the very pips under test, and an empty
    // pool would make the witness vacuous.
    {
        let p1_forest = create_object(
            runner.state_mut(),
            CardId(9600),
            P1,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let object = runner.state_mut().objects.get_mut(&p1_forest).unwrap();
            object.card_types.core_types.push(CoreType::Land);
            object.card_types.subtypes.push("Forest".to_string());
            object.base_card_types = object.card_types.clone();
        }
        add_mana_from_source(runner.state_mut(), P1, ManaType::Green, 1, p1_forest);
        add_mana_from_source(runner.state_mut(), P1, ManaType::Colorless, 1, p1_forest);
    }
    pass_until(
        &mut runner,
        |r| matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "handing priority to the opponent in the new main phase",
    );
    runner.cast(naturalize).target_object(upwelling).commit();
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the opponent's Naturalize",
    );
    pass_until(
        &mut runner,
        |r| matches!(r.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "returning priority to the active player",
    );

    // Reach guards: the retention effect is GONE, the mana is still floating, no
    // step-end handler is in play, and the pool's fact ledger names the trigger.
    {
        let state = runner.state();
        assert_eq!(
            state.objects[&upwelling].zone,
            Zone::Graveyard,
            "reach: Naturalize destroyed Upwelling"
        );
        assert!(
            !state.objects.values().any(|object| {
                object.zone == Zone::Battlefield
                    && object
                        .static_definitions
                        .iter_all()
                        .any(|def| matches!(def.mode, StaticMode::StepEndUnspentMana { .. }))
            }),
            "reach: no step-end retention static is live at restore — this is the whole \
             point of the witness"
        );
        assert_eq!(state.phase, Phase::PreCombatMain);
        let units = pool_units(&runner);
        assert_eq!(
            units.len(),
            2,
            "reach: the mana is still floating in the pool, got {units:?}"
        );
        for unit in &units {
            assert_eq!(unit.source_id, impersonator);
            assert_eq!(
                unit.source_incarnation,
                Some(copied_incarnation),
                "reach: the current-format save pins the epoch that generated the trigger"
            );
        }
        assert!(
            state.pooled_mana_producer_snapshots.iter().any(|snapshot| {
                snapshot.source_id == impersonator
                    && snapshot.source_incarnation == copied_incarnation
            }),
            "reach: the boundary that retained the units wrote their epoch onto the pool's \
             own fact ledger; got {:?}",
            state.pooled_mana_producer_snapshots
        );
    }

    // The intact-pin control: the save the engine writes today is admitted, keeps
    // the trigger's epoch, and earns nothing.
    {
        let mut pinned = GameRunner::from_state(
            admitted(persisted_json(runner.state())).expect("a current-format save is admitted"),
        );
        let unit = pool_units(&pinned)
            .into_iter()
            .next()
            .expect("the pinned unit survives");
        assert_eq!(
            unit.source_incarnation,
            Some(copied_incarnation),
            "reach: the intact pin still names the producing epoch"
        );
        let commit = pinned.cast(sciences).commit();
        let state = commit.state();
        assert!(
            !state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade)),
            "the pinned control must earn no cascade: no Treasure produced this mana"
        );
    }

    // --- The legacy shape: the same pool, saved before the pin existed.
    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_pool_pins(&mut legacy),
        2,
        "both floating units must lose their pin to reproduce the legacy save"
    );
    let restored_state = admitted(legacy).unwrap_or_else(|error| {
        panic!(
            "the pool's fact ledger names the producing epoch, so the pin-less pool must be \
             admitted and bound to it, got {error:?}"
        )
    });
    for unit in restored_state.players[P0.0 as usize].mana_pool.units() {
        assert_eq!(
            unit.source_incarnation,
            Some(copied_incarnation),
            "CR 106.3 + CR 400.7: admission must bind the epoch the pool's producer fact \
             names — never the incarnation of the Treasure now holding the storage id"
        );
    }

    let mut restored = GameRunner::from_state(restored_state);
    let commit = restored.cast(sciences).commit();
    let state = commit.state();
    let record = state.spells_cast_this_turn_by_player[&P0]
        .back()
        .expect("the restored cast must be recorded");
    let snapshots = record.mana_source_snapshots();
    assert_eq!(
        snapshots.len(),
        2,
        "the bound epoch's producer fact must answer the payment; got {snapshots:?}"
    );
    for snapshot in snapshots {
        assert_eq!(
            snapshot.source_id, impersonator,
            "the spent mana came from the storage id the trigger named"
        );
        assert!(
            !snapshot.lki.subtypes.iter().any(|s| s == "Treasure"),
            "CR 106.3 + CR 400.7: a retention effect that has already left must not hand \
             the trigger's mana to the Treasure successor; got {:?}",
            snapshot.lki.subtypes
        );
    }
    let cascade = state.objects[&sciences]
        .cast_spell_keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Cascade));
    assert!(
        !cascade,
        "Rain of Riches must not grant cascade: no Treasure produced this mana"
    );
    assert_eq!(
        state
            .stack
            .iter()
            .filter(|entry| {
                matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                    && entry
                        .ability()
                        .is_some_and(|ability| matches!(ability.effect, Effect::Cascade))
            })
            .count(),
        0,
        "no cascade trigger may be enqueued"
    );
}

// ---------------------------------------------------------------------------
// 9 — a departed producer needs saved evidence too
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 106.4 + CR 400.7 + CR 704.5d: a Treasure's mana crossing a step
/// boundary outlives the Treasure, and the save that names its producer is the
/// only source of the answer.
///
/// The board is test 1's — Upwelling holds two Treasure pips from precombat main
/// into postcombat main — with REAL tokens, so CR 704.5d removes them from the game
/// once they leave the battlefield. Both step-scoped LKI caches are empty by then
/// (a step transition clears them), so nothing about those producers survives in
/// the caches; what survives is what the save itself recorded.
///
/// Three legs, and they are the three answers the rule has to tell apart:
///
/// * the pin is stripped and the ledger survives → the save RECOVERS the epoch and
///   the producer's characteristics with it, so the `{2}` still qualifies for Rain
///   of Riches. Refusing here would throw away a fact the save carries;
/// * the pin AND the ledger are stripped, but production's journal account of the
///   same turn survives → the EPOCH is still proven (each unit's producer node
///   names the exact object and incarnation that made it), so the pool is admitted
///   and bound to it. What is gone with the ledger is the producer's last-known
///   information — the tokens ceased to exist under CR 704.5d, so no live read can
///   supply their characteristics — and the payment therefore records no source
///   fact. That loss is asserted, not hidden: it is the cost of the field this save
///   predates, not a guess the guard made;
/// * the pin, the ledger AND the journal account are stripped → nothing in the save
///   names the producer and the storage ids answer for no object at all, so the pool
///   is REFUSED. This is the refusal witness 1 relies on.
#[test]
fn a_departed_producer_is_admitted_only_when_the_save_names_its_epoch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.add_enchantment_from_oracle(P0, "Rain of Riches", RAIN_OF_RICHES);
    scenario.add_enchantment_from_oracle(P0, "Upwelling", UPWELLING);
    let sciences = sciences_in_hand(&mut scenario);
    let mut runner = scenario.build();
    let first_treasure = treasure_token(runner.state_mut(), 9001);
    let second_treasure = treasure_token(runner.state_mut(), 9002);
    // CR 704.5d: these are TOKENS, so leaving the battlefield ends their existence
    // and the save cannot answer for their storage ids at all.
    for treasure in [first_treasure, second_treasure] {
        runner
            .state_mut()
            .objects
            .get_mut(&treasure)
            .unwrap()
            .is_token = true;
    }
    let first_incarnation = runner.state().objects[&first_treasure].incarnation;
    let second_incarnation = runner.state().objects[&second_treasure].incarnation;

    // --- Act 1: sacrifice both Treasures for their own mana, in precombat main.
    activate_mana_ability(&mut runner, first_treasure, ManaType::Red);
    activate_mana_ability(&mut runner, second_treasure, ManaType::Green);

    // --- Act 2: cross the real phase boundary into postcombat main.
    pass_until(
        &mut runner,
        |r| r.state().phase == Phase::PostCombatMain,
        "crossing combat into postcombat main",
    );

    // Reach guards: the pips survived, the producers ceased to exist, the generic
    // LKI is gone, and the only surviving record of the producers is the pool's.
    {
        let state = runner.state();
        assert_eq!(
            pool_units(&runner).len(),
            2,
            "Upwelling must have retained both units across the boundary"
        );
        for treasure in [first_treasure, second_treasure] {
            assert!(
                !state.objects.contains_key(&treasure),
                "CR 704.5d: the sacrificed Treasure token ceases to exist, so the save \
                 answers for no object at {treasure:?}"
            );
        }
        assert!(
            state.lki_by_incarnation.is_empty() && state.lki_cache.is_empty(),
            "CR 400.7: both generic LKI caches were cleared at the boundary"
        );
        for (treasure, incarnation) in [
            (first_treasure, first_incarnation),
            (second_treasure, second_incarnation),
        ] {
            assert!(
                state.pooled_mana_producer_snapshots.iter().any(|snapshot| {
                    snapshot.source_id == treasure && snapshot.source_incarnation == incarnation
                }),
                "reach: the retained fact names the exact producing epoch"
            );
        }
    }

    // --- Leg 1: pins AND facts stripped. Production's journal account of the same
    // turn still names both epochs, so the pool is admitted and bound — and the
    // producer's characteristics are gone with the ledger this save predates.
    {
        let mut pre_pin = persisted_json(runner.state());
        assert_eq!(strip_pool_pins(&mut pre_pin), 2);
        assert_eq!(
            strip_pool_producer_facts(&mut pre_pin),
            2,
            "both retained facts must be removed to reproduce the pre-fact save format"
        );
        let recovered = admitted(pre_pin).unwrap_or_else(|error| {
            panic!(
                "the journal records both units against the epoch that made them, so the \
                 pin-less pool must be admitted and bound, got {error:?}"
            )
        });
        let recovered_units: Vec<ManaUnit> =
            recovered.players[P0.0 as usize].mana_pool.units().collect();
        assert_eq!(recovered_units.len(), 2);
        for (unit, (treasure, incarnation)) in recovered_units.iter().zip([
            (first_treasure, first_incarnation),
            (second_treasure, second_incarnation),
        ]) {
            assert_eq!(unit.source_id, treasure);
            assert_eq!(
                unit.source_incarnation,
                Some(incarnation),
                "CR 106.3: admission binds the epoch production recorded for this pip"
            );
        }

        // What the missing ledger costs, asserted rather than glossed over: CR
        // 704.5d ended both tokens, so their characteristics exist in no live read,
        // and the only record of them was the ledger. The payment records nothing —
        // which is a fact about the records this save predates, not a guess.
        let mut restored = GameRunner::from_state(recovered);
        let commit = restored.cast(sciences).commit();
        let state = commit.state();
        let record = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the admitted cast must be recorded");
        let snapshots = record.mana_source_snapshots();
        assert!(
            snapshots.is_empty(),
            "CR 704.5d: with the ledger gone there is no last-known information left to \
             price these units from, so the payment records none; got {snapshots:?}"
        );
        assert!(
            !state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade)),
            "no source fact survives, so Rain of Riches cannot qualify this cast"
        );
    }

    // --- Leg 2: pins, facts AND the journal account stripped — nothing in the save
    // names a producer, and the storage ids answer for no object at all.
    {
        let mut nameless = persisted_json(runner.state());
        assert_eq!(strip_pool_pins(&mut nameless), 2);
        assert_eq!(strip_pool_producer_facts(&mut nameless), 2);
        assert!(
            strip_resolved_rules_journal(&mut nameless),
            "reach: the journal account must be removed for this leg"
        );
        match admitted(nameless) {
            Err(PersistedRestoreError::AmbiguousFloatingManaProvenance { player, source_id }) => {
                assert_eq!(player, P0);
                assert!(
                    source_id == first_treasure || source_id == second_treasure,
                    "the refused unit is one of the tokens that ceased to exist; got {source_id:?}"
                );
            }
            Err(other) => panic!("expected an ambiguous-provenance refusal, got {other:?}"),
            Ok(state) => {
                // The adjudication a permissive guard permits: the pool is admitted even
                // though nothing the save retains names its producers. Demonstrated
                // rather than asserted, so a loosened rule fails here with its own
                // consequence.
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
                    "CR 106.3 + CR 400.7 + CR 704.5d: a save that names no producer for its \
                     floating mana must be refused. This one was admitted and the payment \
                     recorded {named:?} (cascade granted: {cascade})"
                );
            }
        }
    }

    // --- Leg 2: the pin is gone, the fact survives — the producer is recoverable.
    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_pool_pins(&mut legacy),
        2,
        "both retained units must lose their pin to reproduce the legacy save"
    );
    let recovered = admitted(legacy).unwrap_or_else(|error| {
        panic!(
            "the save's producer facts name both epochs, so the pin-less pool must be \
             admitted and bound, got {error:?}"
        )
    });
    let recovered_units: Vec<ManaUnit> =
        recovered.players[P0.0 as usize].mana_pool.units().collect();
    assert_eq!(recovered_units.len(), 2);
    for (unit, (treasure, incarnation)) in recovered_units.iter().zip([
        (first_treasure, first_incarnation),
        (second_treasure, second_incarnation),
    ]) {
        assert_eq!(unit.source_id, treasure);
        assert_eq!(
            unit.source_incarnation,
            Some(incarnation),
            "CR 106.3: admission must bind the epoch the retained fact names"
        );
    }

    let mut restored = GameRunner::from_state(recovered);
    let commit = restored.cast(sciences).commit();
    let state = commit.state();
    assert_treasure_record(state, &[first_treasure, second_treasure]);
    assert!(
        state.objects[&sciences]
            .cast_spell_keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Cascade)),
        "a producer the save recovers must still earn Rain of Riches cascade"
    );
}

// ---------------------------------------------------------------------------
// 10 — the save must PROVE the producer: three witnesses on one rule
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 400.7: a pool the save cannot attribute is refused, never
/// stamped with the object that happens to occupy the storage id now.
///
/// Test 6's board one field further back in time: the same retained `{R}{G}`,
/// saved by an engine that wrote NEITHER the per-unit pin NOR the pool's
/// producer ledger (`pooled_mana_producer_snapshots`). Nothing in that save
/// names the epoch that produced the mana — turn advancement reset the
/// journal (`reset_resolved_rules_journal`), and the ledger that outlives it is
/// exactly the field this save predates. The only object the save still offers
/// is the Treasure Cloudshift returned, and CR 400.7 makes it a different
/// object from the copy whose trigger made the mana.
///
/// The old fallback took that object's epoch. This witness pins the refusal, and
/// demonstrates the adjudication a permissive guard permits rather than asserting
/// it: admitted, the `{2}` is priced from the Treasure and Rain of Riches grants
/// cascade for mana a copy produced (CR 106.3). The intact-pin control is the
/// other half — the same board, the save this build writes — and it must restore
/// and cast WITHOUT cascade.
#[test]
fn a_pre_ledger_retained_pool_is_refused_rather_than_bound_to_the_successor() {
    let (mut runner, impersonator, copied_incarnation, sciences) =
        clever_impersonator_trigger_pool(ManaRetention::Upwelling);

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

    // Reach guards: the mana is retained and pinned, the journal that could have
    // named its producer is empty, the ledger is the only record left, and the
    // storage id holds a LIVE successor that a live read would price as a
    // Treasure.
    {
        let state = runner.state();
        let units = pool_units(&runner);
        assert_eq!(
            units.len(),
            2,
            "reach: Upwelling kept both units, got {units:?}"
        );
        for unit in &units {
            assert_eq!(unit.source_id, impersonator);
            assert_eq!(unit.source_incarnation, Some(copied_incarnation));
        }
        assert!(
            state.resolved_rules_journal.produced_mana().is_empty(),
            "reach: turn advancement reset the journal, so no per-unit production \
             record survives; got {:?}",
            state.resolved_rules_journal.produced_mana()
        );
        assert!(
            state.pooled_mana_producer_snapshots.iter().any(|snapshot| {
                snapshot.source_id == impersonator
                    && snapshot.source_incarnation == copied_incarnation
            }),
            "reach: the boundary that carried the units wrote their epoch onto the \
             pool's own fact ledger; got {:?}",
            state.pooled_mana_producer_snapshots
        );
        assert_eq!(state.objects[&impersonator].zone, Zone::Battlefield);
        assert_ne!(state.objects[&impersonator].incarnation, copied_incarnation);
        assert!(
            state.objects[&impersonator]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Treasure"),
            "reach: the successor is a Treasure, so a live read would earn cascade"
        );
    }

    // The intact-pin control: the save this build writes restores and casts with no
    // cascade, because the pin still names the copy that generated the trigger.
    {
        let mut pinned = GameRunner::from_state(
            admitted(persisted_json(runner.state())).expect("a current-format save is admitted"),
        );
        let unit = pool_units(&pinned)
            .into_iter()
            .next()
            .expect("the pinned unit survives");
        assert_eq!(
            unit.source_incarnation,
            Some(copied_incarnation),
            "reach: the intact pin still names the producing epoch"
        );
        let commit = pinned.cast(sciences).commit();
        let state = commit.state();
        assert!(
            !state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade)),
            "the intact-pin control must earn no cascade: no Treasure produced this mana"
        );
    }

    // --- The pre-ledger, pre-pin shape: BOTH records the save could have carried
    // are absent, which is what a save written before either field existed holds.
    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_pool_pins(&mut legacy),
        2,
        "both retained units must lose their pin to reproduce the legacy save"
    );
    assert!(
        strip_pool_producer_facts(&mut legacy) > 0,
        "the pool's fact ledger must be gone too: it is the other field this save predates"
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
            // The adjudication a live-object fallback permits: the pool is admitted
            // and the returned Treasure's epoch prices the trigger's mana. Shown, not
            // asserted, so a reinstated fallback fails here with its consequence.
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
                "CR 106.3 + CR 400.7: a save that names no producing epoch for its retained \
                 mana must be refused. This one was admitted and priced the trigger's mana as \
                 {named:?} (cascade granted: {cascade})"
            );
        }
    }
}

/// CR 106.3 + CR 400.7 + CR 118.3a: one retained fact belongs to ONE unit.
///
/// Test 6's board, one producing tenure later. The retained `{R}{G}` crossed the
/// turn boundary with Upwelling, and the returned Clever Impersonator — a live
/// Treasure copy — is then sacrificed for red in the new main phase. The pool now
/// holds two tenures of one storage id: the copy's trigger units (pinned to the
/// epoch that generated the trigger, named by the pool's fact ledger) and the
/// Treasure's red unit (pinned to the epoch that activated its own Treasure
/// ability, named by the journal record production wrote for that pip).
///
/// The saved evidence is asymmetric on purpose: the ledger holds ONLY the older
/// producer, because a boundary rebuilds it from the pool and the Treasure was
/// still live when the last one ran (`retain_pooled_mana_producer_snapshots`),
/// while the newer unit's producer fact lives in the journal record for its own
/// pip. A guard that answers an id-keyed question cannot tell the two apart and
/// binds the Treasure's red mana to the old Emissary, so the first spell of the
/// turn loses the cascade the Treasure mana had earned. The rule must ask what
/// the save records about THIS unit: the pinned control's answer and the restored
/// answer must agree, unit for unit and cascade for cascade.
#[test]
fn a_mixed_tenure_pool_binds_each_unit_to_its_own_producer() {
    const TWO_RED: &str = "Search your library for a basic land card, reveal it, put it \
                           into your hand, then shuffle. You gain 2 life.";
    let (mut runner, impersonator, copied_incarnation, _sciences) =
        clever_impersonator_trigger_pool_prepared(ManaRetention::Upwelling, |scenario| {
            let mut builder =
                scenario.add_spell_to_hand_from_oracle(P0, "Red Pair", false, TWO_RED);
            builder.with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::Red],
                generic: 0,
            });
        });
    let red_pair = object_named(runner.state(), "Red Pair", Zone::Hand);

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

    // --- Act 5: sacrifice the returned copy, now a Treasure, for red.
    assert_eq!(
        runner.state().objects[&impersonator].zone,
        Zone::Battlefield,
        "reach: the returned copy is still on the battlefield"
    );
    let treasure_incarnation = runner.state().objects[&impersonator].incarnation;
    assert_ne!(
        treasure_incarnation, copied_incarnation,
        "CR 400.7: the returned copy is a new object, so its tenure is distinct"
    );
    activate_mana_ability(&mut runner, impersonator, ManaType::Red);

    // Reach guards: three units from two tenures, each pinned to its own, and the
    // ledger still naming only the older one.
    {
        let state = runner.state();
        let units = pool_units(&runner);
        assert_eq!(
            units.len(),
            3,
            "reach: two trigger units plus the Treasure's red, got {units:?}"
        );
        let older = units
            .iter()
            .filter(|unit| unit.source_incarnation == Some(copied_incarnation))
            .count();
        let newer = units
            .iter()
            .filter(|unit| unit.source_incarnation == Some(treasure_incarnation))
            .count();
        assert_eq!(
            (older, newer),
            (2, 1),
            "reach: the pool holds both tenures, each pinned at production; got {units:?}"
        );
        assert!(
            units.iter().all(|unit| unit.source_id == impersonator),
            "CR 106.3: both tenures name the same storage id"
        );
        assert!(
            state.pooled_mana_producer_snapshots.iter().all(|snapshot| {
                snapshot.source_id != impersonator
                    || snapshot.source_incarnation == copied_incarnation
            }),
            "reach: the ledger names ONLY the older producer, because the Treasure was \
             live when the last boundary rebuilt it; got {:?}",
            state.pooled_mana_producer_snapshots
        );
        assert!(
            state
                .resolved_rules_journal
                .produced_mana()
                .iter()
                .any(|record| {
                    record.unit.source_id == impersonator
                        && record.unit.source_incarnation == Some(treasure_incarnation)
                }),
            "reach: the journal records the Treasure's red unit against its own pip; got {:?}",
            state.resolved_rules_journal.produced_mana()
        );
        assert_eq!(
            state.objects[&impersonator].zone,
            Zone::Graveyard,
            "reach: sacrificing the copy for its own mana took it off the battlefield"
        );
    }

    // The pinned control: the save this build writes, paid on the same pool.
    let pinned_cascade = {
        let mut pinned = GameRunner::from_state(
            admitted(persisted_json(runner.state())).expect("a current-format save is admitted"),
        );
        let commit = pinned.cast(red_pair).commit();
        let state = commit.state();
        let snapshots = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the control's cast must be recorded")
            .mana_source_snapshots();
        assert!(
            snapshots.iter().any(|snapshot| snapshot
                .lki
                .subtypes
                .iter()
                .any(|subtype| subtype == "Treasure")),
            "reach: {{R}}{{R}} must spend the Treasure's red unit, so the control prices a \
             Treasure; got {snapshots:?}"
        );
        state.objects[&red_pair]
            .cast_spell_keywords
            .iter()
            .any(|keyword| matches!(keyword, Keyword::Cascade))
    };
    assert!(
        pinned_cascade,
        "reach: the pinned control's Treasure mana must earn Rain of Riches cascade"
    );

    // --- The legacy shape: the same pool, pins stripped, the ledger kept.
    let mut legacy = persisted_json(runner.state());
    assert_eq!(
        strip_pool_pins(&mut legacy),
        3,
        "all three units must lose their pin to reproduce the legacy save"
    );
    let restored_state = admitted(legacy).unwrap_or_else(|error| {
        panic!(
            "CR 118.3a + CR 106.3: both tenures are named by the save — the older by the \
             pool's fact ledger for its own pips, the newer by the journal record for its \
             own pip — so the pin-less pool must be admitted and bound, got {error:?}"
        )
    });
    let mut bound: Vec<(Option<u64>, ManaType)> = restored_state.players[P0.0 as usize]
        .mana_pool
        .units()
        .map(|unit| (unit.source_incarnation, unit.color))
        .collect();
    bound.sort_by_key(|(epoch, color)| (*epoch, format!("{color:?}")));
    let older = Some(copied_incarnation);
    let newer = Some(treasure_incarnation);
    assert!(
        bound.contains(&(older, ManaType::Green)) && bound.contains(&(older, ManaType::Red)),
        "CR 106.3: the trigger's {{R}}{{G}} binds to the epoch that generated the trigger; \
         got {bound:?}"
    );
    assert!(
        bound.contains(&(newer, ManaType::Red)),
        "CR 118.3a + CR 400.7: the Treasure's red unit binds to the epoch that activated \
         ITS ability, never to the older Emissary tenure; got {bound:?}"
    );

    // Paying with that pool must not lose the cascade the pinned control earned.
    let mut restored = GameRunner::from_state(restored_state);
    let (legacy_cascade, legacy_priced_a_treasure) = {
        let commit = restored.cast(red_pair).commit();
        let state = commit.state();
        let snapshots = state.spells_cast_this_turn_by_player[&P0]
            .back()
            .expect("the restored cast must be recorded")
            .mana_source_snapshots();
        (
            state.objects[&red_pair]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade)),
            snapshots.iter().any(|snapshot| {
                snapshot
                    .lki
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Treasure")
            }),
        )
    };
    assert!(
        legacy_priced_a_treasure,
        "CR 106.3: the restored Treasure unit must still price as a Treasure"
    );
    assert_eq!(
        legacy_cascade, pinned_cascade,
        "CR 106.3 + CR 400.7: the restored pool must answer the payment exactly as the \
         pinned control does; binding the Treasure's red mana to the old Emissary tenure \
         silently loses the cascade it earned"
    );
}

/// CR 106.3 + CR 118.3a + CR 400.7: a restored legacy pool must produce a save
/// that itself restores.
///
/// The ordinary same-turn shape, with no retention effect anywhere: a Forest
/// tapped for `{G}` in precombat main, saved before the mana is spent. The save
/// predates the pin in BOTH places it is written — the pooled unit and the
/// journal's own copy of that unit — so admission has to bind the epoch from what
/// production recorded for that pip (`RulesExecutionNodeKind::ActivatedMana`'s
/// exact source), and it must bind BOTH copies: the pool's, and the journal's.
///
/// If it stamps only the pool's copy, the next payment is recorded against a
/// produced unit that still carries `None` while the spent unit carries the epoch
/// — and `ResolvedRulesJournal::validate_serialized_authority` compares the whole
/// unit at load (CR 400.7's epoch included), so the very next save of the game
/// cannot be reloaded. That is why the round trip, not the first restore, is the
/// assertion: a save the engine wrote but cannot read back is the worst outcome
/// available, and the boundary is the only place that can prevent it.
#[test]
fn a_restored_legacy_pool_round_trips_after_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let elves = {
        let mut builder =
            scenario.add_creature_to_hand_from_oracle(P0, "Llanowar Elves", 1, 1, LLANOWAR_ELVES);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        });
        builder.id()
    };

    let mut runner = scenario.build();
    let forest_incarnation = runner.state().objects[&forest].incarnation;
    activate_mana_ability(&mut runner, forest, ManaType::Green);

    // Reach guards: one floating unit, pinned at production, and journaled against
    // its own pip by the activation that made it.
    let pip = {
        let state = runner.state();
        let units = pool_units(&runner);
        assert_eq!(units.len(), 1, "reach: one unit of {{G}}, got {units:?}");
        assert_eq!(units[0].source_id, forest);
        assert_eq!(units[0].source_incarnation, Some(forest_incarnation));
        let produced = state
            .resolved_rules_journal
            .produced_mana()
            .iter()
            .find(|record| record.unit.pip_id == units[0].pip_id)
            .unwrap_or_else(|| {
                panic!(
                    "reach: production must journal the unit against its own pip; got {:?}",
                    state.resolved_rules_journal.produced_mana()
                )
            });
        assert_eq!(
            produced.unit.source_incarnation,
            Some(forest_incarnation),
            "reach: the journal's copy carries the same pin the pool's does"
        );
        units[0].pip_id
    };

    // --- The legacy shape: the pin is absent from the pool AND from the journal's
    // copy of the same unit. Only the producer record survives.
    let mut legacy = persisted_json(runner.state());
    assert_eq!(strip_pool_pins(&mut legacy), 1);
    assert!(
        strip_journal_unit_pins(&mut legacy) >= 1,
        "the journal's copies of the same unit must lose their pin too"
    );
    let restored_state = admitted(legacy).unwrap_or_else(|error| {
        panic!(
            "the journal's producer record names the epoch that activated the Forest's \
             ability, so the pin-less unit must be admitted and bound, got {error:?}"
        )
    });
    {
        let unit = restored_state.players[P0.0 as usize]
            .mana_pool
            .units()
            .next()
            .expect("the unit survives");
        assert_eq!(unit.pip_id, pip, "reach: the same unit, by pip identity");
        assert_eq!(
            unit.source_incarnation,
            Some(forest_incarnation),
            "CR 106.3: admission binds the epoch the journal's producer record names"
        );
    }

    // --- Spend the unit on Llanowar Elves, then save and restore AGAIN.
    let mut restored = GameRunner::from_state(restored_state);
    {
        let commit = restored.cast(elves).commit();
        assert_eq!(
            commit.state().objects[&elves].zone,
            Zone::Stack,
            "reach: {{G}} from the Forest paid for the Elves"
        );
    }
    assert!(
        pool_units(&restored).is_empty(),
        "reach: the payment spent the only floating unit"
    );

    let second_save = persisted_json(restored.state());
    let reread = decode_and_admit(second_save).unwrap_or_else(|error| {
        panic!(
            "CR 400.7 + CR 118.3a: the save taken AFTER paying with a restored pool must \
             restore again. The journal compares each spent unit against the produced unit \
             it came from, epochs included, so an unstamped journal copy makes the engine's \
             own save unreadable: {error}"
        )
    });
    assert!(
        reread.players[P0.0 as usize]
            .mana_pool
            .units()
            .next()
            .is_none(),
        "reach: the re-read state is the post-payment state"
    );
    assert_eq!(
        reread
            .resolved_rules_journal
            .produced_mana()
            .iter()
            .find(|record| record.unit.pip_id == pip)
            .and_then(|record| record.unit.source_incarnation),
        Some(forest_incarnation),
        "CR 118.3a: the journal's own copy of the spent unit still names the epoch the \
         Forest had when it made the mana, across the whole round trip"
    );
}

/// CR 106.3 + CR 118.3a + CR 400.7: the two copies of one floating unit must name
/// the same producing epoch, and the engine enforces that where it can be enforced
/// — at the decoder.
///
/// The unit exists twice: in its owner's pool, and in the resolved-rules journal
/// that recorded its production and its payment. CR 400.7's epoch is part of that
/// unit's identity, so `ResolvedRulesJournal::validate_serialized_authority`
/// compares the copies whole when the save is decoded, and a save whose copies
/// disagree does not decode at all. That is what makes the round trip above
/// possible: the boundary there has only to bring an UNRECORDED copy up to the
/// epoch the save proves, because a save whose copies already disagreed never
/// reaches it. This test pins that division — the decoder is the authority on
/// agreement, the restore boundary on the epoch.
#[test]
fn a_pool_save_whose_journal_copies_disagree_never_decodes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);

    let mut runner = scenario.build();
    let forest_incarnation = runner.state().objects[&forest].incarnation;
    activate_mana_ability(&mut runner, forest, ManaType::Green);

    let mut tampered = persisted_json(runner.state());
    // The pool's copy keeps the true epoch; the journal's produced copy is re-pointed
    // at another tenure of the same storage id.
    let records = tampered["state"]["resolved_rules_journal"]["produced_mana"]
        .as_array_mut()
        .expect("the journal records the produced unit");
    assert_eq!(records.len(), 1, "reach: one produced record");
    records[0]["unit"]["source_incarnation"] = serde_json::json!(forest_incarnation + 7);

    match serde_json::from_value::<PersistedGameState>(tampered) {
        Ok(_) => panic!(
            "CR 118.3a + CR 400.7: a save whose journal copy of the unit names a different \
             producing epoch than the pool's own must not decode; admitting it would carry a \
             unit whose two identities disagree into the state a payment then writes"
        ),
        Err(error) => assert!(
            error
                .to_string()
                .contains("mana-insert command disagrees with produced mana"),
            "the refusal must come from the journal's own copy comparison, got {error}"
        ),
    }
}

/// CR 106.3 + CR 400.7 + CR 118.3a: an explicit `source_incarnation` pin is a
/// CLAIM about one unit's producer, so admission validates it against the save's
/// own INDEPENDENT per-pip records before trusting it.
///
/// The board is section 10's printed-card witness
/// (`a_pre_ledger_retained_pool_is_refused_rather_than_bound_to_the_successor`)
/// carried across its turn boundary: Upwelling held the Clever Impersonator copy's
/// triggered `{R}{G}`, turn advancement cleared the journal
/// (`reset_resolved_rules_journal`), the pool's producer ledger
/// (`pooled_mana_producer_snapshots`) is the record that outlived it, and the
/// Treasure Cloudshift returned now occupies the storage id with a LATER
/// incarnation.
///
/// The tampering is the one field this rule is about: every retained unit's pin is
/// re-pointed at the LIVE successor's epoch, and nothing else is touched — the
/// per-pip ledger still names the copy that generated the trigger, and the journal
/// is still empty. A boundary that trusts the pin admits this save; the payment
/// then prices the copy's mana as the Treasure's and Rain of Riches grants cascade
/// for mana no Treasure made (CR 106.3). The boundary must compare the pin with the
/// ledger and REFUSE
/// ([`PersistedRestoreError::ContradictoryFloatingManaProvenance`]) — not prefer
/// either side, and not rewrite the ledger to match the pin.
#[test]
fn a_retained_pool_pin_that_contradicts_its_own_per_pip_ledger_is_refused() {
    let (mut runner, impersonator, copied_incarnation, sciences) =
        clever_impersonator_trigger_pool(ManaRetention::Upwelling);

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

    // Reach guards: the pool is retained and pinned to the epoch that generated
    // the trigger, the journal that could have named that producer is empty, and
    // the ledger beside each unit still names the same epoch.
    let successor_incarnation = {
        let state = runner.state();
        let units = pool_units(&runner);
        assert_eq!(
            units.len(),
            2,
            "reach: Upwelling kept both units, got {units:?}"
        );
        for unit in &units {
            assert_eq!(unit.source_id, impersonator);
            assert_eq!(unit.source_incarnation, Some(copied_incarnation));
            assert_eq!(
                state
                    .pooled_mana_producer_snapshots
                    .iter()
                    .filter(|snapshot| snapshot.pip_id == unit.pip_id)
                    .map(|snapshot| (snapshot.source_id, snapshot.source_incarnation))
                    .collect::<Vec<_>>(),
                vec![(impersonator, copied_incarnation)],
                "reach: each retained unit's own per-pip ledger entry names the copy that \
                 generated the trigger"
            );
        }
        assert!(
            state.resolved_rules_journal.produced_mana().is_empty(),
            "reach: turn advancement reset the journal, so the ledger is the only per-pip \
             record left; got {:?}",
            state.resolved_rules_journal.produced_mana()
        );
        let successor = state.objects[&impersonator].incarnation;
        assert_ne!(
            successor, copied_incarnation,
            "reach: the live occupant of the storage id is a LATER tenure"
        );
        assert!(
            state.objects[&impersonator]
                .card_types
                .subtypes
                .iter()
                .any(|subtype| subtype == "Treasure"),
            "reach: the successor is a Treasure, so pinning to it earns cascade"
        );
        successor
    };

    // The contradiction: the pins move to the successor; the ledger does not.
    let mut tampered = persisted_json(runner.state());
    assert_eq!(
        pin_pool_units(&mut tampered, successor_incarnation),
        2,
        "both retained units must be re-pinned to the successor"
    );

    match admitted(tampered) {
        Err(PersistedRestoreError::ContradictoryFloatingManaProvenance {
            player,
            source_id,
            pinned,
            recorded,
        }) => {
            assert_eq!(player, P0);
            assert_eq!(
                source_id, impersonator,
                "the refused unit is the one whose pin the ledger contradicts"
            );
            assert_eq!(
                pinned, successor_incarnation,
                "the pin under test is the successor's epoch"
            );
            assert_eq!(
                recorded,
                vec![copied_incarnation],
                "the record the pin is checked against is the unit's own per-pip ledger entry"
            );
        }
        Err(other) => panic!("expected the pin/ledger contradiction to be refused, got {other:?}"),
        Ok(state) => {
            // The adjudication a trusting guard permits: the copy's retained mana is
            // priced as the Treasure and Rain of Riches grants cascade for it. Shown,
            // not asserted, so a reinstated trust-the-pin path fails here with its
            // consequence.
            let mut restored = GameRunner::from_state(state);
            let commit = restored.cast(sciences).commit();
            let state = commit.state();
            let named: Vec<&str> = state.spells_cast_this_turn_by_player[&P0]
                .back()
                .expect("the admitted cast must be recorded")
                .mana_source_snapshots()
                .iter()
                .map(|snapshot| snapshot.lki.name.as_str())
                .collect();
            let cascade = state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade));
            panic!(
                "CR 106.3 + CR 400.7: a pin that contradicts the save's own per-pip ledger must \
                 be refused. This save was admitted and priced the copy's retained mana as \
                 {named:?} (cascade granted: {cascade})"
            );
        }
    }
}

/// CR 118.3a + CR 106.3 + CR 400.7: a pin that ARRIVED in the save is not a
/// recovery, so admission never rewrites the journal's independent record of the
/// same unit.
///
/// The unit exists twice in a save: in the pool, carrying
/// `ManaUnit::source_incarnation`, and in the resolved-rules journal that recorded
/// its production. When the pool's pin is present there is nothing to recover, and
/// `ResolvedRulesJournal::bind_unit_epoch` — the migration that stamps a
/// RECOVERED epoch onto the journal's copies — must not run: it writes one record
/// over another before the decoder that compares records ever sees them, which
/// launders a contradiction into a loadable save.
///
/// This witness states which answer this build gives and why: the save is
/// **refused**, not silently rewritten. The pool's pin is re-pointed at another
/// tenure of the same storage id while the journal's own production record is left
/// naming the Forest's epoch; a boundary that stamps from the pin would overwrite
/// that record with the pin and admit the save, and this build compares the two and
/// refuses. The pin-less leg then shows the record still answers for the unit —
/// admission binds exactly the epoch the refused leg was trying to overwrite.
#[test]
fn a_pin_that_arrived_in_the_save_never_rewrites_the_journals_production_record() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);

    let mut runner = scenario.build();
    let forest_incarnation = runner.state().objects[&forest].incarnation;
    activate_mana_ability(&mut runner, forest, ManaType::Green);

    // Reach guards: one pinned unit, and the journal's own production record for
    // that pip names the same epoch the pool's pin does.
    let pip = {
        let state = runner.state();
        let units = pool_units(&runner);
        assert_eq!(units.len(), 1, "reach: one unit of {{G}}, got {units:?}");
        assert_eq!(units[0].source_id, forest);
        assert_eq!(units[0].source_incarnation, Some(forest_incarnation));
        assert_eq!(
            state
                .resolved_rules_journal
                .produced_unit(units[0].pip_id)
                .and_then(|unit| unit.source_incarnation),
            Some(forest_incarnation),
            "reach: production recorded the unit's epoch against its own pip"
        );
        units[0].pip_id
    };
    let journal_epoch = |state: &GameState| {
        state
            .resolved_rules_journal
            .produced_unit(pip)
            .and_then(|unit| unit.source_incarnation)
    };

    // Control: an intact save is admitted, and admission leaves the journal's copy
    // exactly as the engine wrote it.
    {
        let state = runner.state();
        let intact = admitted(persisted_json(state)).expect("a current-format save is admitted");
        assert_eq!(
            journal_epoch(&intact),
            Some(forest_incarnation),
            "a pin that needs no recovery must leave the journal's record alone"
        );
    }

    // The contradiction: the pool's pin moves to another tenure of the same storage
    // id; the journal's independent production record is untouched.
    let other_incarnation = forest_incarnation + 7;
    let mut tampered = persisted_json(runner.state());
    assert_eq!(
        pin_pool_units(&mut tampered, other_incarnation),
        1,
        "exactly one pooled unit must be re-pinned"
    );
    assert_eq!(
        tampered["state"]["resolved_rules_journal"]["produced_mana"][0]["unit"]
            ["source_incarnation"],
        serde_json::json!(forest_incarnation),
        "reach: the tampering re-points the pool's pin only; the journal's record still \
         names the Forest's epoch"
    );

    match admitted(tampered) {
        Err(PersistedRestoreError::ContradictoryFloatingManaProvenance {
            player,
            source_id,
            pinned,
            recorded,
        }) => {
            assert_eq!(player, P0);
            assert_eq!(source_id, forest);
            assert_eq!(pinned, other_incarnation, "the pin under test");
            assert_eq!(
                recorded,
                vec![forest_incarnation],
                "production's own record is the independent fact the pin was checked \
                 against, and it was not rewritten to agree with the pin"
            );
        }
        Err(other) => panic!("expected the pin/journal contradiction to be refused, got {other:?}"),
        Ok(state) => panic!(
            "CR 118.3a: a pin that arrived in the save must never be stamped over the \
             journal's independently recorded epoch. This save was admitted and the \
             journal's copy now names {:?} instead of {forest_incarnation}",
            journal_epoch(&state)
        ),
    }

    // The evidence survived, which is what the refusal protected: with the pool's
    // pin gone, admission binds exactly the epoch the journal recorded.
    {
        let mut pinless = persisted_json(runner.state());
        assert_eq!(strip_pool_pins(&mut pinless), 1);
        let bound = admitted(pinless).expect(
            "the journal's production record proves the epoch, so the pin-less pool is admitted",
        );
        let unit = bound.players[P0.0 as usize]
            .mana_pool
            .units()
            .next()
            .expect("the unit survives");
        assert_eq!(
            unit.source_incarnation,
            Some(forest_incarnation),
            "CR 106.3: the recovered epoch is the one the journal recorded, proving the \
             refused leg never overwrote it"
        );
        assert_eq!(
            journal_epoch(&bound),
            Some(forest_incarnation),
            "a recovered pin stamps the journal's copies from the record it was recovered \
             from — unchanged, because it already said so"
        );
    }
}

// ---------------------------------------------------------------------------
// 12 — admission leaves ONE answer: every copy of an admitted unit agrees
// ---------------------------------------------------------------------------

/// Re-point the producing-incarnation pin on EVERY copy of every floating unit —
/// the pooled copies AND each copy the resolved-rules journal keeps — at
/// `incarnation`, leaving the producer node and the pool's per-pip ledger exactly
/// as the engine wrote them.
///
/// That is the shape where the unit's copies all agree with each other and only
/// the save's INDEPENDENT producer record is left to disagree: a guard that reads
/// the copies and stops has nothing to check the pin against.
fn pin_every_unit_copy(json: &mut serde_json::Value, incarnation: u64) -> usize {
    /// A serialized `ManaUnit` is the only object in the journal carrying both a
    /// `source_id` and a `pip_id`; unlike `strip_journal_unit_pins` this helper
    /// only needs the journal half, because the pooled half is `pin_pool_units`'.
    fn pin(value: &mut serde_json::Value, incarnation: u64) -> usize {
        match value {
            serde_json::Value::Object(object) => {
                let mut pinned = 0;
                if object.contains_key("source_id") && object.contains_key("pip_id") {
                    object.insert(
                        "source_incarnation".to_string(),
                        serde_json::json!(incarnation),
                    );
                    pinned += 1;
                }
                pinned
                    + object
                        .values_mut()
                        .map(|child| pin(child, incarnation))
                        .sum::<usize>()
            }
            serde_json::Value::Array(items) => {
                items.iter_mut().map(|child| pin(child, incarnation)).sum()
            }
            _ => 0,
        }
    }
    let pooled = pin_pool_units(json, incarnation);
    let journaled = json
        .get_mut("state")
        .and_then(|state| state.get_mut("resolved_rules_journal"))
        .map(|journal| pin(journal, incarnation))
        .unwrap_or(0);
    pooled + journaled
}

/// CR 106.3 + CR 113.7 + CR 118.3a + CR 400.7: an admitted unit leaves admission
/// with ONE producer, in every copy — so the save the next payment writes is a
/// save the next load can read.
///
/// The board is the Forest → Llanowar Elves shape of
/// `a_restored_legacy_pool_round_trips_after_payment`, and the tampering is one
/// field narrower: only the journal's own copies of the unit lose the pin
/// (`strip_journal_unit_pins`). The pool's pin survives, and so does the producer
/// NODE production wrote (`RulesExecutionNodeKind::ActivatedMana`, which names the
/// Forest's exact epoch and is not a pin) — so the epoch is PROVEN by two
/// independent records that agree, and there is nothing for this save to
/// contradict.
///
/// What it must not do is admit the unit and leave the journal's copies pin-less.
/// A payment records the spent unit against the produced unit it came from, and
/// `ResolvedRulesJournal::validate_serialized_authority` compares those copies
/// whole (CR 400.7's epoch included) — so that admission writes a save the engine
/// cannot read back, which is the worst outcome available. Admission therefore
/// COMPLETES the missing copies from the epoch the save proves before admitting,
/// and the assertion is the round trip: admit, spend, save, load again.
#[test]
fn a_pool_pin_with_unpinned_journal_copies_is_completed_so_the_payment_round_trips() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    let forest = scenario.add_basic_land(P0, ManaColor::Green);
    let elves = {
        let mut builder =
            scenario.add_creature_to_hand_from_oracle(P0, "Llanowar Elves", 1, 1, LLANOWAR_ELVES);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        });
        builder.id()
    };

    let mut runner = scenario.build();
    let forest_incarnation = runner.state().objects[&forest].incarnation;
    activate_mana_ability(&mut runner, forest, ManaType::Green);

    // Reach guards: one pinned unit, pinned in the journal too, and the producer
    // node names the epoch the Forest's ability was activated at.
    let pip = {
        let state = runner.state();
        let units = pool_units(&runner);
        assert_eq!(units.len(), 1, "reach: one unit of {{G}}, got {units:?}");
        assert_eq!(units[0].source_id, forest);
        assert_eq!(units[0].source_incarnation, Some(forest_incarnation));
        assert_eq!(
            state
                .resolved_rules_journal
                .produced_unit(units[0].pip_id)
                .and_then(|unit| unit.source_incarnation),
            Some(forest_incarnation),
            "reach: the journal's own copy carries the same pin the pool's does"
        );
        assert_eq!(
            state
                .resolved_rules_journal
                .produced_source_epoch(units[0].pip_id)
                .map(|note| (note.object_id, note.incarnation)),
            Some((forest, forest_incarnation)),
            "reach: the producer node names the epoch that generated the ability"
        );
        units[0].pip_id
    };

    // --- The tampered shape: ONLY the journal's copies lose the pin.
    let mut stripped = persisted_json(runner.state());
    assert!(
        strip_journal_unit_pins(&mut stripped) >= 1,
        "the journal's copies of the unit must lose their pin"
    );
    let pooled_pin = |json: &serde_json::Value| -> Option<u64> {
        json["state"]["players"]
            .as_array()
            .expect("the save carries players")
            .iter()
            .flat_map(|player| {
                player["mana_pool"]["mana"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .find(|unit| unit["source_id"] == serde_json::json!(forest.0))
            .and_then(|unit| unit["source_incarnation"].as_u64())
    };
    assert_eq!(
        pooled_pin(&stripped),
        Some(forest_incarnation),
        "reach: the tampering strips only the journal's copies; the pool's pin stays"
    );
    let activated_nodes: Vec<(u64, u64)> = stripped["state"]["resolved_rules_journal"]["nodes"]
        .as_array()
        .expect("the journal carries its nodes")
        .iter()
        .filter_map(|node| {
            let source = node.get("kind")?.get("ActivatedMana")?.get("source")?;
            Some((
                source.get("object_id")?.as_u64()?,
                source.get("incarnation")?.as_u64()?,
            ))
        })
        .collect();
    assert!(
        activated_nodes.contains(&(forest.0, forest_incarnation)),
        "reach: the producer node still names the Forest's epoch, got {activated_nodes:?}"
    );
    assert_eq!(
        stripped["state"]["resolved_rules_journal"]["produced_mana"][0]["unit"]
            ["source_incarnation"],
        serde_json::Value::Null,
        "reach: the journal's produced copy is the thing that lost its pin"
    );

    let restored_state = admitted(stripped).unwrap_or_else(|error| {
        panic!(
            "CR 106.3 + CR 113.7: the producer node and the pool's pin agree on the epoch, so \
             this save proves the unit's producer and must be admitted with the journal's \
             copies completed, got {error:?}"
        )
    });

    // --- Admission completed the journal's copies from the proven epoch.
    {
        let unit = restored_state.players[P0.0 as usize]
            .mana_pool
            .units()
            .next()
            .expect("the unit survives");
        assert_eq!(unit.pip_id, pip, "reach: the same unit, by pip identity");
        assert_eq!(
            unit.source_incarnation,
            Some(forest_incarnation),
            "the pool's pin was already the proven epoch"
        );
    }

    // --- Spend it, save, and load the save the payment wrote.
    let mut restored = GameRunner::from_state(restored_state);
    {
        let commit = restored.cast(elves).commit();
        assert_eq!(
            commit.state().objects[&elves].zone,
            Zone::Stack,
            "reach: {{G}} from the Forest paid for the Elves"
        );
    }
    assert!(
        pool_units(&restored).is_empty(),
        "reach: the payment spent the only floating unit"
    );

    let reread = decode_and_admit(persisted_json(restored.state())).unwrap_or_else(|error| {
        panic!(
            "CR 118.3a + CR 400.7: the save taken AFTER paying with this pool must load again. \
             The journal compares each spent unit against the produced unit it came from, \
             epochs included, so leaving the journal's copies incomplete makes the engine's \
             own save unreadable: {error}"
        )
    });
    assert_eq!(
        reread
            .resolved_rules_journal
            .produced_unit(pip)
            .and_then(|unit| unit.source_incarnation),
        Some(forest_incarnation),
        "CR 118.3a: the produced and spent copies still name one epoch across the round trip"
    );
    assert_eq!(
        reread
            .resolved_rules_journal
            .spent_mana()
            .iter()
            .find(|record| record.unit.pip_id == pip)
            .and_then(|record| record.unit.source_incarnation),
        Some(forest_incarnation),
        "CR 118.3a: the spent copy names the same epoch the produced copy does"
    );
    assert_eq!(
        reread
            .resolved_rules_journal
            .produced_unit(pip)
            .and_then(|unit| unit.source_incarnation),
        Some(forest_incarnation),
        "CR 118.3a: admission completes the journal's missing copy with the epoch the \
         save proves, so the two copies of one unit agree before any payment reads them"
    );
}

/// CR 106.3 + CR 113.7 + CR 400.7: the producer NODE is compared even when every
/// unit copy carries the same pin, because the copies are one record and the node
/// is another.
///
/// The board is section 2's Minimus Containment shape: an Aura grants the Bears
/// "{T}, Sacrifice this artifact: Add one mana of any color", so activating it
/// makes the mana AND moves the source out of the battlefield (CR 400.7 makes the
/// graveyard card a different object). Production wrote two independent records of
/// that epoch: the pin on the unit's copies, and the producer node
/// (`RulesExecutionNodeKind::ActivatedMana`) naming the object whose ability was
/// activated — the record the payment itself resolves through
/// (`mana_spent_source_snapshot`).
///
/// The tampering re-points the pooled pin AND every journal copy at another epoch
/// of the same storage id, and touches nothing else: no retained side ledger
/// exists in this same-phase save, and the node is left naming the epoch the
/// ability was activated at. Every pin in the save therefore agrees with every
/// other pin, and only the node disagrees — so a guard that reads its pin set from
/// the unit copies alone admits it. The payment then resolves the pin against an
/// incarnation the storage id never had, records NO producing source for the spent
/// unit, and Rain of Riches' `ManaSpentFromSource` gate never sees the Treasure:
/// the cascade the mana earned is silently lost (CR 106.3 makes the mana's source
/// the source of the ability that produced it, CR 113.7 makes that the object that
/// generated the ability). Admission must refuse
/// ([`PersistedRestoreError::ContradictoryFloatingManaProvenance`]) instead.
#[test]
fn a_pin_every_unit_copy_agrees_on_is_still_checked_against_the_producer_node() {
    let (mut runner, bears, _aura, sciences) = minimus_board();

    let producing_incarnation = runner.state().objects[&bears].incarnation;
    activate_mana_ability(&mut runner, bears, ManaType::Green);

    // Reach guards: the producing epoch is pinned on the pooled unit, the journal's
    // copy records the same epoch, the producer node names it independently, and no
    // retained side ledger exists for it to be compared against instead.
    {
        let state = runner.state();
        let units = pool_units(&runner);
        assert_eq!(units.len(), 1, "reach: one floating unit");
        assert_eq!(units[0].source_id, bears);
        assert_eq!(units[0].source_incarnation, Some(producing_incarnation));
        assert_eq!(
            state
                .resolved_rules_journal
                .produced_unit(units[0].pip_id)
                .and_then(|unit| unit.source_incarnation),
            Some(producing_incarnation),
            "reach: the journal's copy of the unit carries the same pin"
        );
        assert_eq!(
            state
                .resolved_rules_journal
                .produced_source_epoch(units[0].pip_id)
                .map(|note| (note.object_id, note.incarnation)),
            Some((bears, producing_incarnation)),
            "reach: the producer node names the epoch the ability was activated at"
        );
        assert!(
            state
                .pooled_mana_producer_snapshots
                .iter()
                .all(|snapshot| snapshot.pip_id != units[0].pip_id),
            "reach: no retained side ledger entry exists for this unit; got {:?}",
            state.pooled_mana_producer_snapshots
        );
    }

    // --- The tampering: every PIN in the save agrees; the node is untouched.
    let wrong_incarnation = producing_incarnation + 7;
    let mut tampered = persisted_json(runner.state());
    assert!(
        pin_every_unit_copy(&mut tampered, wrong_incarnation) >= 2,
        "the pooled pin and the journal's copy must both be re-pointed"
    );
    assert_eq!(
        tampered["state"]["resolved_rules_journal"]["produced_mana"][0]["unit"]
            ["source_incarnation"],
        serde_json::json!(wrong_incarnation),
        "reach: the journal's copy now agrees with the pool's pin"
    );
    assert_eq!(
        tampered["state"]["resolved_rules_journal"]["nodes"]
            .as_array()
            .expect("nodes")
            .iter()
            .find_map(|node| {
                let source = node.get("kind")?.get("ActivatedMana")?.get("source")?;
                Some(source.get("incarnation")?.as_u64().unwrap_or_default())
            }),
        Some(producing_incarnation),
        "reach: production's own record is untouched"
    );

    match admitted(tampered) {
        Err(PersistedRestoreError::ContradictoryFloatingManaProvenance {
            player,
            source_id,
            pinned,
            recorded,
        }) => {
            assert_eq!(player, P0);
            assert_eq!(source_id, bears, "the refused unit is the one under test");
            assert_eq!(pinned, wrong_incarnation, "the pin under test");
            assert!(
                recorded.contains(&producing_incarnation),
                "the independent fact the pin was checked against is the producer node's own \
                 epoch, got {recorded:?}"
            );
            assert!(
                recorded.iter().any(|epoch| *epoch != pinned),
                "the refusal is about a record that disagrees with the pin, got {recorded:?}"
            );
        }
        Err(other) => {
            panic!("expected the pin/producer-node contradiction to be refused, got {other:?}")
        }
        Ok(state) => {
            // The adjudication a pin-only guard permits: the unit is priced from an
            // epoch its storage id never had, so the payment records no producing
            // source at all and the Treasure's cascade is lost. Shown, not asserted,
            // so a reinstated pin-only path fails here with its consequence.
            let mut restored = GameRunner::from_state(state);
            let forest = land(restored.state_mut(), 9601, "Forest", "Forest");
            add_mana_from_source(restored.state_mut(), P0, ManaType::Green, 1, forest);
            let commit = restored.cast(sciences).commit();
            let state = commit.state();
            let named: Vec<&str> = state.spells_cast_this_turn_by_player[&P0]
                .back()
                .expect("the admitted cast must be recorded")
                .mana_source_snapshots()
                .iter()
                .map(|snapshot| snapshot.lki.name.as_str())
                .collect();
            let cascade = state.objects[&sciences]
                .cast_spell_keywords
                .iter()
                .any(|keyword| matches!(keyword, Keyword::Cascade));
            panic!(
                "CR 106.3 + CR 113.7 + CR 400.7: a pin the unit's own copies agree on must still \
                 answer to the producer node, which is the record production wrote and the \
                 payment resolves through. This save was admitted and priced the pip's mana as \
                 {named:?} (cascade granted: {cascade})"
            );
        }
    }
}
