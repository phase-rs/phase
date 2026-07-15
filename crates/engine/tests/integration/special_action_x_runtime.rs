//! t104 — RUNTIME WITNESSES for the SPECIAL-ACTION X channel (CR 107.3d / CR 702.37f).
//!
//! Turning a face-down permanent face up is a SPECIAL ACTION (CR 116.2b): it uses no stack and
//! never passes through `push_ability_entry`. So neither pre-existing X channel could reach it,
//! BY CONSTRUCTION:
//!   * `GameObject::cost_x_paid` is the CR 107.3m *cast* channel — and a face-down permanent was
//!     cast for {3} (CR 702.37c), so its `cost_x_paid` is 0.
//!   * t97's source-keyed carrier published only for an *activated ability*.
//!
//! CR 107.3d: "If a cost associated with a special action, such as a suspend cost or a morph
//! cost, has an {X} or an X in it, the value of X is chosen by the player taking the special
//! action immediately before they pay that cost."
//! CR 702.37f (morph) / CR 702.168e (disguise): "If a permanent's morph cost includes X, other
//! abilities of that permanent may also refer to X. The value of X in those abilities is equal to
//! the value of X chosen as the morph special action was taken."
//!
//! The three live faces put X in three DIFFERENT AST slots, consumed at three different times —
//! and all three bind through the SAME field, `ResolvedAbility::chosen_x`, stamped at trigger
//! INSTANTIATION by `triggers::build_triggered_ability`:
//!
//!   | face                | slot                                  | consumed at      |
//!   |---------------------|---------------------------------------|------------------|
//!   | Warbreak Trumpeter  | `Token.count`                         | resolution       |
//!   | Aurelia's Vindicator| `multi_target.max`                    | TARGET SELECTION |
//!   | Bane of the Living  | `PumpAll.power/toughness = Var("-X")` | resolution       |
//!
//! MEASURED ON PRISTINE MAIN, through this same path (before the channel existed): flipping
//! Warbreak Trumpeter (Morph {X}{X}{R}) cost **1 mana** — the {X} shards were dropped entirely,
//! not merely unbound — and created **0 goblins**. Both halves are fixed here and pinned below.
//!
//! HARNESS NOTE (inherited from t96/t97, learned the hard way): `add_card_to_hand` builds a
//! name-only object "without rules text" — a probe built on it is VACUOUS and reads 0 for
//! everything, which looks exactly like a fabrication. Every card below is therefore synthesized
//! from its VERBATIM Oracle text (pool export), and the Hooded Hydra control exists precisely to
//! catch a regression back into that vacuum.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// ─────────────────────────────────────────────────────────────────────────────
// VERBATIM Oracle text (pool export — NOT paraphrased, NOT from memory).
//
// NOTE: Warbreak Trumpeter's trigger is bare "create X 1/1 red Goblin creature tokens". There is
// NO "where X is the amount of mana spent" clause on the card — the X binds implicitly via
// CR 702.37f. This matters: the morph cost is {X}{X}{R}, so "the amount of mana spent" would be
// 2X+1, not X. A fix built to that (mis)quote would be wrong by a factor of two.
// ─────────────────────────────────────────────────────────────────────────────

const WARBREAK_TRUMPETER: &str = "Morph {X}{X}{R} (You may cast this card face down as a 2/2 \
                                  creature for {3}. Turn it face up any time for its morph \
                                  cost.)\nWhen this creature is turned face up, create X 1/1 red \
                                  Goblin creature tokens.";

const BANE_OF_THE_LIVING: &str = "Morph {X}{B}{B} (You may cast this card face down as a 2/2 \
                                  creature for {3}. Turn it face up any time for its morph \
                                  cost.)\nWhen this creature is turned face up, all creatures get \
                                  -X/-X until end of turn.";

const AURELIAS_VINDICATOR: &str = "Flying, lifelink, ward {2}\nDisguise {X}{3}{W}\nWhen this \
                                   creature is turned face up, exile up to X other target \
                                   creatures from the battlefield and/or creature cards from \
                                   graveyards.\nWhen this creature leaves the battlefield, return \
                                   the exiled cards to their owners' hands.";

/// CONTROL card — its morph cost has NO X ({3}{G}{G}). Its X is the CAST-X channel (CR 107.3m),
/// and its turn-face-up rider is a REPLACEMENT, not a trigger.
const HOODED_HYDRA: &str = "This creature enters with X +1/+1 counters on it.\nWhen this creature \
                            dies, create a 1/1 green Snake creature token for each +1/+1 counter \
                            on it.\nMorph {3}{G}{G}\nAs this creature is turned face up, put five \
                            +1/+1 counters on it.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, ty: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(ty, ObjectId(0), false, vec![]);
        runner.state_mut().players[0].mana_pool.add(unit);
    }
}

fn pool_total(runner: &engine::game::scenario::GameRunner) -> usize {
    runner.state().players[0].mana_pool.total()
}

/// Battlefield objects whose name contains `needle`, as (name, power, toughness).
fn named_on_battlefield(
    runner: &engine::game::scenario::GameRunner,
    needle: &str,
) -> Vec<(String, i32, i32)> {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.name.contains(needle))
        .map(|o| {
            (
                o.name.clone(),
                o.power.unwrap_or(0),
                o.toughness.unwrap_or(0),
            )
        })
        .collect()
}

fn zone_size(runner: &engine::game::scenario::GameRunner, zone: Zone) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == zone)
        .count()
}

/// CR 702.37c: cast `card` face down as a 2/2, landing it on the battlefield.
fn play_face_down(runner: &mut engine::game::scenario::GameRunner, card: ObjectId) {
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::PlayFaceDown {
            object_id: card,
            card_id,
        })
        .expect("play face down");
    assert!(
        runner.state().objects[&card].face_down,
        "reach-guard: the card must actually be FACE DOWN on the battlefield, or nothing below \
         is a test of the turn-face-up path"
    );
}

/// CR 116.2b + CR 107.3d: take the turn-face-up special action, announcing `x`, then drive any
/// resulting trigger (including its target selection) to resolution.
fn turn_face_up_for_x(runner: &mut engine::game::scenario::GameRunner, card: ObjectId, x: u32) {
    runner
        .act(GameAction::TurnFaceUp { object_id: card, x })
        .expect("turn face up for the announced X");
    drain_trigger_targets(runner);
    runner.advance_until_stack_empty();
}

/// CR 603.3d: a turn-face-up trigger picks its targets as it goes on the stack.
fn drain_trigger_targets(runner: &mut engine::game::scenario::GameRunner) {
    let mut chosen: Vec<engine::types::ability::TargetRef> = Vec::new();
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                // CR 601.2c: each slot takes a DISTINCT object, so track the picks.
                let slot = &target_slots[selection.current_slot];
                let pick = slot
                    .legal_targets
                    .iter()
                    .find(|t| !chosen.contains(t))
                    .cloned();
                if let Some(target) = pick.clone() {
                    chosen.push(target);
                }
                runner
                    .act(GameAction::ChooseTarget { target: pick })
                    .expect("choose a legal target for the turn-face-up trigger");
            }
            _ => break,
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// THE THREE FACES — one per X slot.
// ═════════════════════════════════════════════════════════════════════════════

/// CR 107.3d + CR 702.37f — Warbreak Trumpeter, Morph `{X}{X}{R}`:
/// "When this creature is turned face up, create X 1/1 red Goblin creature tokens."
///
/// The X lives in `Token.count` (an *effect* slot, consumed at resolution). Turned face up for
/// X=3, it must create 3 Goblins. On pristine main this created **0** — the announced X had
/// nowhere to live.
#[test]
fn warbreak_trumpeter_turned_face_up_for_x_creates_x_goblins() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trumpeter = scenario
        .add_creature_to_hand_from_oracle(P0, "Warbreak Trumpeter", 1, 1, WARBREAK_TRUMPETER)
        .id();
    let mut runner = scenario.build();

    play_face_down(&mut runner, trumpeter);
    add_mana(&mut runner, ManaType::Red, 12);

    turn_face_up_for_x(&mut runner, trumpeter, 3);

    assert!(
        !runner.state().objects[&trumpeter].face_down,
        "reach-guard: the permanent must actually be face up"
    );
    let goblins = named_on_battlefield(&runner, "Goblin");
    assert_eq!(
        goblins.len(),
        3,
        "CR 702.37f: turned face up for X=3, Warbreak Trumpeter creates X = 3 Goblins. 0 here \
         means the special action's announced X was DROPPED (the pre-t104 behaviour). \
         MEASURED: {goblins:?}"
    );
    assert!(
        goblins.iter().all(|(_, p, t)| (*p, *t) == (1, 1)),
        "the tokens are 1/1 red Goblins. MEASURED: {goblins:?}"
    );
}

/// CR 107.3d + CR 702.37f — Bane of the Living, Morph `{X}{B}{B}`:
/// "When this creature is turned face up, all creatures get -X/-X until end of turn."
///
/// The X lives in `PumpAll.power/toughness` as `PtValue::Variable("-X")` — a NEGATED placeholder
/// resolved by `pump::resolve_variable_pt`, which reads `ability.chosen_x` and negates it. With
/// `chosen_x` unset that helper returns `None` and the wrath is a silent 0/0 — the board does not
/// move at all. Turned face up for X=2 it must be a -2/-2 sweep.
#[test]
fn bane_of_the_living_turned_face_up_for_x_wraths_for_minus_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bane = scenario
        .add_creature_to_hand_from_oracle(P0, "Bane of the Living", 4, 3, BANE_OF_THE_LIVING)
        .id();
    let victim = scenario.add_creature(P1, "Grizzly Bears", 3, 3).id();
    let mut runner = scenario.build();

    play_face_down(&mut runner, bane);
    add_mana(&mut runner, ManaType::Black, 12);

    turn_face_up_for_x(&mut runner, bane, 2);

    let bears = runner.state().objects.get(&victim);
    assert_eq!(
        bears.map(|o| (o.power.unwrap_or(0), o.toughness.unwrap_or(0))),
        Some((1, 1)),
        "CR 702.37f: turned face up for X=2, ALL creatures get -2/-2 — the 3/3 becomes a 1/1. \
         A 3/3 here means the negated X (`PtValue::Variable(\"-X\")`) resolved to 0 and the wrath \
         did nothing. MEASURED: {:?}",
        bears.map(|o| (o.power, o.toughness))
    );
    // The sweep hits Bane itself too (CR 611.2c — "all creatures", no exclusion): 4/3 -> 2/1.
    let self_pt = runner
        .state()
        .objects
        .get(&bane)
        .map(|o| (o.power, o.toughness));
    assert_eq!(
        self_pt,
        Some((Some(2), Some(1))),
        "\"all creatures\" includes Bane of the Living itself: 4/3 - 2/2 = 2/1. MEASURED: \
         {self_pt:?}"
    );
}

/// CR 107.3d + CR 702.168e — Aurelia's Vindicator, Disguise `{X}{3}{W}`:
/// "When this creature is turned face up, exile up to X other target creatures ..."
///
/// THIS is the face that forces the stamp to land at trigger INSTANTIATION rather than at
/// resolution: its X lives in `multi_target.max`, which is consumed during TARGET SELECTION —
/// before the trigger ever resolves. With X unbound the trigger offers "up to 0" targets and
/// exiles nothing, the same silent zero this campaign exists to kill.
#[test]
fn aurelias_vindicator_turned_face_up_for_x_exiles_up_to_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vindicator = scenario
        .add_creature_to_hand_from_oracle(P0, "Aurelia's Vindicator", 4, 2, AURELIAS_VINDICATOR)
        .id();
    scenario.add_creature(P1, "Doomed Traveler", 1, 1);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    let mut runner = scenario.build();

    play_face_down(&mut runner, vindicator);
    add_mana(&mut runner, ManaType::White, 12);

    let exiled_before = zone_size(&runner, Zone::Exile);
    turn_face_up_for_x(&mut runner, vindicator, 2);

    let exiled = zone_size(&runner, Zone::Exile) - exiled_before;
    assert_eq!(
        exiled, 2,
        "CR 702.168e: turned face up for X=2, the trigger exiles up to X = 2 other target \
         creatures. 0 here means `multi_target.max` read an unbound X and offered 'up to 0' \
         targets — the target-count slot is consumed at SELECTION, so a stamp applied at \
         resolution would arrive too late. MEASURED: {exiled}"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// DEFECT B — the COST. Announcing X must actually charge 2X + {R}.
// ═════════════════════════════════════════════════════════════════════════════

/// CR 107.1b + CR 601.2f + CR 702.37e: the morph cost's `{X}` shards must be CONCRETIZED before
/// payment. Warbreak Trumpeter is `{X}{X}{R}` — TWO X shards — so X=3 costs 2*3 + {R} = 7 mana.
///
/// MEASURED on pristine main: the flip cost **1 mana**. The unconcretized `ManaCostShard::X`
/// reached mana payment, where `ShardRequirement::X` is not payable and the shard was dropped, so
/// the permanent flipped for its non-X remainder ({R}) alone. That is a strictly worse bug than
/// the missing carrier: without this, binding X would let a player pay {R} and collect X goblins.
#[test]
fn turn_face_up_charges_the_announced_x_for_every_x_shard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trumpeter = scenario
        .add_creature_to_hand_from_oracle(P0, "Warbreak Trumpeter", 1, 1, WARBREAK_TRUMPETER)
        .id();
    let mut runner = scenario.build();

    play_face_down(&mut runner, trumpeter);
    add_mana(&mut runner, ManaType::Red, 12);
    let before = pool_total(&runner);

    turn_face_up_for_x(&mut runner, trumpeter, 3);

    let spent = before - pool_total(&runner);
    assert_eq!(
        spent, 7,
        "CR 107.1b: EACH `{{X}}` shard costs the announced X, so Morph {{X}}{{X}}{{R}} at X=3 is \
         2*3 + {{R}} = 7 mana. A 1 here is the pre-t104 behaviour: both X shards silently dropped \
         at payment. MEASURED: {spent}"
    );
}

/// CR 118.3: a player can't announce an X they cannot pay for. With only 3 mana available, X=3 on
/// `{X}{X}{R}` (a 7-mana cost) must be REJECTED — and the permanent must stay face down.
#[test]
fn turn_face_up_rejects_an_unpayable_announced_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trumpeter = scenario
        .add_creature_to_hand_from_oracle(P0, "Warbreak Trumpeter", 1, 1, WARBREAK_TRUMPETER)
        .id();
    let mut runner = scenario.build();

    play_face_down(&mut runner, trumpeter);
    add_mana(&mut runner, ManaType::Red, 3);

    let result = runner.act(GameAction::TurnFaceUp {
        object_id: trumpeter,
        x: 3,
    });
    assert!(
        result.is_err(),
        "X=3 on Morph {{X}}{{X}}{{R}} needs 7 mana; with 3 available the announcement is illegal \
         (CR 118.3). Accepting it would flip the permanent for free."
    );
    assert!(
        runner.state().objects[&trumpeter].face_down,
        "a rejected announcement must leave the permanent FACE DOWN"
    );
    assert!(
        named_on_battlefield(&runner, "Goblin").is_empty(),
        "a rejected announcement must create no tokens"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// CONTROLS — each must be able to FAIL, and each pins a different way to get this wrong.
// ═════════════════════════════════════════════════════════════════════════════

/// CR 107.3d — the ANNOUNCED ZERO. X=0 is a legal announcement, and it is the one value where the
/// correct board and the FABRICATED board coincide (an unbound X also resolves to 0). A
/// board-only assertion here would therefore be VACUOUS.
///
/// The discriminating observation is taken while the trigger is ON THE STACK: it must carry
/// `chosen_x == Some(0)` — an announcement of zero — and NOT `None`. That is what pins
/// `Option<u32>` as the right carrier type: a carrier that collapsed 0 into "nothing announced"
/// would leave `None` here and pass a board-only test by luck. The cost is checked too: at X=0,
/// `{X}{X}{R}` costs exactly {R} — 1 mana.
#[test]
fn turn_face_up_for_x_zero_announces_a_real_zero() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let trumpeter = scenario
        .add_creature_to_hand_from_oracle(P0, "Warbreak Trumpeter", 1, 1, WARBREAK_TRUMPETER)
        .id();
    let mut runner = scenario.build();

    play_face_down(&mut runner, trumpeter);
    add_mana(&mut runner, ManaType::Red, 12);
    let before = pool_total(&runner);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: trumpeter,
            x: 0,
        })
        .expect("X=0 is a legal announcement (CR 107.3d)");

    let spent = before - pool_total(&runner);
    assert_eq!(
        spent, 1,
        "at X=0, Morph {{X}}{{X}}{{R}} costs just {{R}} = 1 mana. MEASURED: {spent}"
    );

    let bound_x = runner
        .state()
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id, ability, ..
            } if *source_id == trumpeter => Some(ability.chosen_x),
            _ => None,
        })
        .expect("the turn-face-up trigger must be on the stack (if not, nothing here is a test)");
    assert_eq!(
        bound_x,
        Some(0),
        "X=0 is an ANNOUNCED zero, not 'no X announced'. `None` here means the carrier collapsed \
         0 into absence — the board (0 goblins) would then be correct only by coincidence. \
         MEASURED: {bound_x:?}"
    );

    runner.advance_until_stack_empty();
    let goblins = named_on_battlefield(&runner, "Goblin");
    assert!(
        goblins.is_empty(),
        "announced X=0 creates zero Goblins. Any token here means a non-zero X was FABRICATED — a \
         value the player never announced. MEASURED: {goblins:?}"
    );
}

/// NON-VACUITY + NO-SPURIOUS-PUBLICATION control — Hooded Hydra, Morph `{3}{G}{G}` (**no X**).
///
/// Two jobs, and it can fail at either:
///  1. **The harness really flips permanents.** Hooded Hydra's "As this creature is turned face
///     up, put five +1/+1 counters on it" is a REPLACEMENT (CR 614.1e), independent of any X. If
///     this reads 0 counters, the harness is not turning anything face up and every verdict in
///     this file is void (the `add_card_to_hand` vacuum trap that bit t96).
///  2. **A no-X flip publishes NOTHING.** The carrier must stay `None`: publishing `Some((id, 0))`
///     for a cost with no `{X}` would assert an X that CR 107.3d never granted, and could clobber
///     an unrelated activated ability's in-flight X on another object.
#[test]
fn no_x_morph_flip_publishes_nothing_and_still_turns_face_up() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = scenario
        .add_creature_to_hand_from_oracle(P0, "Hooded Hydra", 0, 0, HOODED_HYDRA)
        .id();
    let mut runner = scenario.build();

    play_face_down(&mut runner, hydra);
    add_mana(&mut runner, ManaType::Green, 12);

    runner
        .act(GameAction::TurnFaceUp {
            object_id: hydra,
            x: 0,
        })
        .expect("a no-X morph flips with X=0");
    runner.advance_until_stack_empty();

    // (1) NON-VACUITY: the flip really happened and its replacement really applied.
    assert!(
        !runner.state().objects[&hydra].face_down,
        "NON-VACUITY: the permanent must actually be face up"
    );
    let counters = runner.state().objects[&hydra]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        counters, 5,
        "NON-VACUITY (CR 614.1e): Hooded Hydra's 'As this creature is turned face up, put five \
         +1/+1 counters on it' must apply. A 0 here means the harness never really flipped it and \
         EVERY verdict in this file is void. MEASURED: {counters}"
    );

    // (2) NO SPURIOUS PUBLICATION: a cost with no {X} announces no X.
    assert_eq!(
        runner.state().announced_source_x,
        None,
        "CR 107.3d grants an X choice only 'if a cost ... has an {{X}} ... in it'. Hooded Hydra's \
         Morph {{3}}{{G}}{{G}} has none, so the carrier must stay None. A `Some((.., 0))` here \
         would assert an X the rules never granted and could clobber an unrelated activated \
         ability's in-flight X. MEASURED: {:?}",
        runner.state().announced_source_x
    );
}

/// CR 107.3d is scoped to costs that HAVE an {X}: "if a cost ... has an {X} ... in it". A client
/// announcing X != 0 on a no-X cost is a bug, and must be rejected rather than silently ignored —
/// silently ignoring it would let a malformed action look like a legal flip.
#[test]
fn turn_face_up_rejects_a_nonzero_x_on_a_cost_with_no_x() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = scenario
        .add_creature_to_hand_from_oracle(P0, "Hooded Hydra", 0, 0, HOODED_HYDRA)
        .id();
    let mut runner = scenario.build();

    play_face_down(&mut runner, hydra);
    add_mana(&mut runner, ManaType::Green, 12);

    let result = runner.act(GameAction::TurnFaceUp {
        object_id: hydra,
        x: 4,
    });
    assert!(
        result.is_err(),
        "Morph {{3}}{{G}}{{G}} has no {{X}}, so CR 107.3d grants no choice — X=4 is not a legal \
         announcement and must be rejected, not ignored."
    );
    assert!(
        runner.state().objects[&hydra].face_down,
        "a rejected announcement must leave the permanent FACE DOWN"
    );
}
