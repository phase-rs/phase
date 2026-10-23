//! Per-seat bandwidth budget for `StateUpdate` frames.
//!
//! A deterministic four-seat scripted game (no AI) is driven through the real
//! engine pipeline, one production submission per loop iteration. After every
//! submission each seat's `StateUpdate` is built exactly as the shared server
//! builds it (`build_state_update_message`, which runs
//! `server_core::filter_state_for_player` and the derived/interaction views),
//! wrapped in the `Envelope` the socket writer serializes, and passed through
//! the gzip wire envelope (`wire::encode_json_message_sync`). The test then
//! compares each seat's update count, serialized JSON bytes and wire bytes with
//! the checked-in budget in `bandwidth_budget.json`.
//!
//! What the budget covers: every byte of the per-recipient `StateUpdate` --
//! `GameState`/`GameObject`/`ServerMessage` field growth (including definitions
//! and `base_*` copies), per-viewer filtering and redaction, derived views,
//! legal-action and interaction payloads -- and the scripted engine-transition
//! count times seats. What it does not cover: how often production calls
//! `StateUpdatePublisher::publish`. This test publishes through its own seat
//! loop that mirrors `publish`, so a duplicated or suppressed production send
//! is not caught here.
//!
//! This is a regression budget, not a model of real games. Deliberate
//! differences from production: `rewind_targets` is empty (the shared server
//! sends none), `command_id` is `None`, only seat `StateUpdate`s are measured
//! (no `GameStarted`, no spectator frames), and no deck pools are attached.
//!
//! Tolerances are two-sided: `json_bytes` must stay within
//! `tolerance_pct.json_bytes` and `wire_bytes` within `tolerance_pct.wire_bytes`
//! of the budget in either direction, so a large reduction also forces a
//! re-baseline and the budget cannot silently go stale. `updates` is exact.
//!
//! # Updating the budget
//!
//! ```text
//! UPDATE_BANDWIDTH_BUDGET=1 cargo test -p phase-server --bin phase-server \
//!     bandwidth_budget -- --nocapture
//! ```
//!
//! rewrites `bandwidth_budget.json` (keeping `schema`, `scenario` and the
//! tolerances) from the measured table and skips the comparison; re-run without
//! the variable to confirm. Policy: a commit that updates the budget says why
//! the size moved and links the field or PR responsible. An unexplained
//! increase is a regression to be fixed, not a reason to re-baseline. A change
//! that alters the scripted game itself (cards, script, seats) is a new
//! scenario: bump `scenario` in the JSON by hand and re-baseline.
//!
//! # Manual release check
//!
//! This test pins per-update size on one small scripted round. The end-to-end
//! release metric is measured outside this repository by an external stress
//! harness: `StateUpdate` frames and wire bytes per seat-turn on paired fixed
//! games (same seeds, gzip envelope on), baseline versus candidate. Reference
//! values measured at upstream 6d285188f (after #10066): about 42
//! `StateUpdate`s and about 1.7 MB wire per seat-turn in four-player Commander
//! with the gzip envelope on.
//!
//! To compute the figure from that harness's per-stream recordings: each row is
//! one seat-stream and records its `max_turn` and, per message type, the
//! `[frames, wire_bytes, json_bytes]` triple (take the `StateUpdate` entry).
//! Sum `StateUpdate` frames and `StateUpdate` wire bytes over all seat streams,
//! and divide each by the sum of `max_turn` over the same streams. A report
//! that prints "per turn (all seats)" shows that per-seat-turn figure
//! multiplied by the seats per game, so compare like with like.
//!
//! The measuring copies below mirror production code and name their sources:
//! the `ActionResult` tuple (`server_core::session`, `handle_action_with_card_db_outcome`),
//! the per-seat publish (`StateUpdatePublisher::publish`; its seat-range and
//! closed-sender skips and the completion `command_id` are deliberately
//! omitted), and the frame encode (`socket_io::encode_application`).

use std::collections::VecDeque;

use axum::extract::ws::Message;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::InteractionSessionId;
use serde::{Deserialize, Serialize};
use server_core::session::ActionResult;
use server_core::FullSessionKey;

use super::StateUpdateSource;
use crate::socket_mailbox::Envelope;

const UPDATE_ENV_VAR: &str = "UPDATE_BANDWIDTH_BUDGET";
const UPDATE_COMMAND: &str = "UPDATE_BANDWIDTH_BUDGET=1 cargo test -p phase-server --bin phase-server bandwidth_budget -- --nocapture";
const BUDGET_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/src/game_executor/bandwidth_budget.json"
);
const BUDGET_JSON: &str = include_str!("bandwidth_budget.json");

const BUDGET_SEED: u64 = 42;
const SEATS: usize = 4;
/// The scripted game starts on turn 2 (`at_phase`) and ends when the turn
/// counter first reaches 6: one full round of four turns.
const END_TURN: u32 = 6;
const ITERATION_CAP: usize = 2000;

// Oracle texts, verbatim. Grizzly Bears is a vanilla 2/2 and has none.
const LLANOWAR_ELVES_ORACLE: &str = "{T}: Add {G}.";
const SERRA_ANGEL_ORACLE: &str =
    "Flying\nVigilance (Attacking doesn't cause this creature to tap.)";
const ELVISH_VISIONARY_ORACLE: &str = "When this creature enters, draw a card.";
const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

const LIBRARY_CARDS: [&str; 8] = [
    "Library Card 1",
    "Library Card 2",
    "Library Card 3",
    "Library Card 4",
    "Library Card 5",
    "Library Card 6",
    "Library Card 7",
    "Library Card 8",
];

const fn seat(index: usize) -> PlayerId {
    PlayerId(index as u8)
}

fn mana_cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

// ---------------------------------------------------------------------------
// Budget file
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Budget {
    schema: u32,
    scenario: String,
    update_procedure: String,
    tolerance_pct: Tolerances,
    seats: Vec<SeatBudget>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tolerances {
    json_bytes: f64,
    wire_bytes: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeatBudget {
    seat: usize,
    updates: u64,
    json_bytes: u64,
    wire_bytes: u64,
}

fn load_budget() -> Budget {
    serde_json::from_str(BUDGET_JSON).unwrap_or_else(|error| {
        panic!(
            "bandwidth_budget.json is malformed ({error}); fix it by hand: the update command below keeps schema, scenario and tolerances from this file, so it needs a parseable file:\n  {UPDATE_COMMAND}"
        )
    })
}

fn delta_pct(actual: u64, budget: u64) -> f64 {
    if budget == 0 {
        return if actual == 0 { 0.0 } else { f64::INFINITY };
    }
    (actual as f64 - budget as f64) / budget as f64 * 100.0
}

fn measured_table(measured: &[SeatBudget], budget: &Budget) -> String {
    let mut table = String::from(
        "seat | updates (actual/budget) | json_bytes (actual/budget, delta%) | wire_bytes (actual/budget, delta%)\n",
    );
    for actual in measured {
        let expected = budget.seats.iter().find(|s| s.seat == actual.seat);
        let (updates, json, wire) = match expected {
            Some(b) => (
                b.updates.to_string(),
                format!(
                    "{} (delta {:+.2}%)",
                    b.json_bytes,
                    delta_pct(actual.json_bytes, b.json_bytes)
                ),
                format!(
                    "{} (delta {:+.2}%)",
                    b.wire_bytes,
                    delta_pct(actual.wire_bytes, b.wire_bytes)
                ),
            ),
            None => ("-".into(), "-".into(), "-".into()),
        };
        table.push_str(&format!(
            "{:>4} | {}/{} | {}/{} | {}/{}\n",
            actual.seat, actual.updates, updates, actual.json_bytes, json, actual.wire_bytes, wire,
        ));
    }
    table
}

fn rewrite_budget(measured: &[SeatBudget]) {
    let existing = load_budget();
    let rewritten = Budget {
        seats: measured.to_vec(),
        ..existing
    };
    let mut text = serde_json::to_string_pretty(&rewritten).expect("budget serializes");
    text.push('\n');
    std::fs::write(BUDGET_PATH, text).expect("write bandwidth_budget.json");
    eprintln!("budget rewritten at {BUDGET_PATH}; re-run without {UPDATE_ENV_VAR}");
}

fn assert_within_budget(measured: &[SeatBudget], budget: &Budget) {
    let mut failures = Vec::new();
    if budget.schema != 1 {
        failures.push(format!("unsupported budget schema {}", budget.schema));
    }
    if budget.seats.len() != measured.len() {
        failures.push(format!(
            "budget has {} seats, scenario has {}",
            budget.seats.len(),
            measured.len()
        ));
    }
    for actual in measured {
        let Some(expected) = budget.seats.iter().find(|s| s.seat == actual.seat) else {
            failures.push(format!("seat {} has no budget entry", actual.seat));
            continue;
        };
        if actual.updates != expected.updates {
            failures.push(format!(
                "seat {}: updates {} != budget {}",
                actual.seat, actual.updates, expected.updates
            ));
        }
        let json_delta = delta_pct(actual.json_bytes, expected.json_bytes);
        if json_delta.abs() > budget.tolerance_pct.json_bytes {
            failures.push(format!(
                "seat {}: json_bytes {} vs budget {} ({:+.2}%, tolerance +/-{}%)",
                actual.seat,
                actual.json_bytes,
                expected.json_bytes,
                json_delta,
                budget.tolerance_pct.json_bytes
            ));
        }
        let wire_delta = delta_pct(actual.wire_bytes, expected.wire_bytes);
        if wire_delta.abs() > budget.tolerance_pct.wire_bytes {
            failures.push(format!(
                "seat {}: wire_bytes {} vs budget {} ({:+.2}%, tolerance +/-{}%)",
                actual.seat,
                actual.wire_bytes,
                expected.wire_bytes,
                wire_delta,
                budget.tolerance_pct.wire_bytes
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "StateUpdate bandwidth is outside the checked-in budget (scenario {}):\n{}\n{}\n\
         If the size change is intended and explained, re-baseline and say why in the commit:\n  {UPDATE_COMMAND}",
        budget.scenario,
        failures.join("\n"),
        measured_table(measured, budget),
    );
}

// ---------------------------------------------------------------------------
// Scripted game
// ---------------------------------------------------------------------------

/// A scripted main-phase intent, consumed when its seat holds priority in its
/// own precombat main phase with an empty stack.
#[derive(Debug, Clone, Copy)]
enum Intent {
    PlayLand {
        object_id: ObjectId,
        card_id: CardId,
    },
    CastSpell {
        object_id: ObjectId,
        card_id: CardId,
    },
}

struct ScriptedGame {
    runner: GameRunner,
    /// One pre-existing attacker per seat; attacks the next seat each turn.
    attackers: [ObjectId; SEATS],
    scripts: [VecDeque<Intent>; SEATS],
    /// Targets chosen in order for the target prompts the script produces.
    targets: VecDeque<TargetRef>,
    hand_lands: [ObjectId; SEATS],
    /// Hand spell each seat casts (Bolt, Elvish Visionary, Grizzly Bears, Llanowar Elves).
    hand_spells: [ObjectId; SEATS],
    /// The Llanowar Elves that seat 0's Lightning Bolt kills.
    bolt_victim: ObjectId,
}

fn build_game() -> ScriptedGame {
    let mut scenario = GameScenario::new_n_player(SEATS as u8, BUDGET_SEED);
    scenario.at_phase(Phase::PreCombatMain);

    let bears_cost = mana_cost(1, &[ManaCostShard::Green]);
    let elves_cost = mana_cost(0, &[ManaCostShard::Green]);
    let angel_cost = mana_cost(3, &[ManaCostShard::White, ManaCostShard::White]);
    let visionary_cost = mana_cost(1, &[ManaCostShard::Green]);
    let bolt_cost = mana_cost(0, &[ManaCostShard::Red]);

    // Battlefield lands (the hand land has no mana ability, so every spell is
    // paid from these).
    for _ in 0..3 {
        scenario.add_basic_land(seat(0), ManaColor::Red);
    }
    scenario.add_basic_land(seat(1), ManaColor::Green);
    scenario.add_basic_land(seat(1), ManaColor::Green);
    scenario.add_basic_land(seat(1), ManaColor::White);
    for index in [2, 3] {
        for _ in 0..3 {
            scenario.add_basic_land(seat(index), ManaColor::Green);
        }
    }

    // Pre-existing attackers.
    let attack0 = scenario
        .add_creature(seat(0), "Grizzly Bears", 2, 2)
        .with_mana_cost(bears_cost.clone())
        .id();
    let attack1 = scenario
        .add_creature(seat(1), "Serra Angel", 4, 4)
        .from_oracle_text_with_keywords(&["Flying", "Vigilance"], SERRA_ANGEL_ORACLE)
        .with_mana_cost(angel_cost)
        .id();
    let attack2 = scenario
        .add_creature(seat(2), "Grizzly Bears", 2, 2)
        .with_mana_cost(bears_cost.clone())
        .id();
    let attack3 = scenario
        .add_creature(seat(3), "Grizzly Bears", 2, 2)
        .with_mana_cost(bears_cost.clone())
        .id();
    // The mana creature on seat 1 that seat 0's Lightning Bolt kills.
    let bolt_victim = scenario
        .add_creature_from_oracle(seat(1), "Llanowar Elves", 1, 1, LLANOWAR_ELVES_ORACLE)
        .with_mana_cost(elves_cost.clone())
        .id();

    // Hand: one land and one spell per seat.
    let hand_lands = [
        scenario.add_land_to_hand(seat(0), "Mountain").id(),
        scenario.add_land_to_hand(seat(1), "Forest").id(),
        scenario.add_land_to_hand(seat(2), "Forest").id(),
        scenario.add_land_to_hand(seat(3), "Forest").id(),
    ];
    let hand_spells = [
        scenario
            .add_spell_to_hand_from_oracle(seat(0), "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
            .with_mana_cost(bolt_cost)
            .id(),
        scenario
            .add_creature_to_hand_from_oracle(
                seat(1),
                "Elvish Visionary",
                1,
                1,
                ELVISH_VISIONARY_ORACLE,
            )
            .with_mana_cost(visionary_cost)
            .id(),
        scenario
            .add_creature_to_hand(seat(2), "Grizzly Bears", 2, 2)
            .with_mana_cost(bears_cost)
            .id(),
        scenario
            .add_creature_to_hand_from_oracle(
                seat(3),
                "Llanowar Elves",
                1,
                1,
                LLANOWAR_ELVES_ORACLE,
            )
            .with_mana_cost(elves_cost)
            .id(),
    ];

    for index in 0..SEATS {
        scenario.with_library_top(seat(index), &LIBRARY_CARDS);
    }

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.log_player_names = (0..SEATS).map(|index| format!("Seat {index}")).collect();
        // Without a bound authority `derive_viewer_interaction` short-circuits
        // to `AuthorityUnbound` and the frames are unrealistically small
        // (server_core::session::bind_interaction_session does this for real games).
        engine::game::interaction::bind_interaction_authority(
            state,
            InteractionSessionId("BUDGET".into()),
        )
        .expect("bind interaction authority");
    }

    let state = runner.state();
    let scripts = std::array::from_fn(|index| {
        let land = Intent::PlayLand {
            object_id: hand_lands[index],
            card_id: state.objects[&hand_lands[index]].card_id,
        };
        let spell = Intent::CastSpell {
            object_id: hand_spells[index],
            card_id: state.objects[&hand_spells[index]].card_id,
        };
        VecDeque::from([land, spell])
    });

    ScriptedGame {
        runner,
        attackers: [attack0, attack1, attack2, attack3],
        scripts,
        targets: VecDeque::from([TargetRef::Object(bolt_victim)]),
        hand_lands,
        hand_spells,
        bolt_victim,
    }
}

impl ScriptedGame {
    /// Choose the next production submission from the current prompt. Any
    /// prompt the script does not cover fails loudly rather than defaulting.
    fn next_action(&mut self) -> GameAction {
        let state = self.runner.state();
        match &state.waiting_for {
            WaitingFor::Priority { player } => {
                let seat_index = usize::from(player.0);
                let own_main = state.active_player == *player
                    && state.phase == Phase::PreCombatMain
                    && state.stack.is_empty();
                let intent = if own_main {
                    self.scripts[seat_index].pop_front()
                } else {
                    None
                };
                match intent {
                    Some(Intent::PlayLand { object_id, card_id }) => {
                        GameAction::PlayLand { object_id, card_id }
                    }
                    Some(Intent::CastSpell { object_id, card_id }) => GameAction::CastSpell {
                        object_id,
                        card_id,
                        targets: vec![],
                        payment_mode: CastPaymentMode::Auto,
                    },
                    None => GameAction::PassPriority,
                }
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                let target = self.targets.pop_front().unwrap_or_else(|| {
                    panic!(
                        "unscripted target prompt WaitingFor::{} on turn {}",
                        self.runner.waiting_for_kind(),
                        self.runner.state().turn_number
                    )
                });
                GameAction::ChooseTarget {
                    target: Some(target),
                }
            }
            WaitingFor::DeclareAttackers { player, .. } => {
                let seat_index = usize::from(player.0);
                GameAction::DeclareAttackers {
                    attacks: vec![(
                        self.attackers[seat_index],
                        AttackTarget::Player(seat((seat_index + 1) % SEATS)),
                    )],
                    bands: vec![],
                }
            }
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            WaitingFor::ResolveAllConsent { .. }
            | WaitingFor::ResolveAllReady { .. }
            | WaitingFor::MeldPairChoice { .. }
            | WaitingFor::MeldAttackTargetChoice { .. }
            | WaitingFor::EntryAttackTargetChoice { .. }
            | WaitingFor::MulliganDecision { .. }
            | WaitingFor::OpeningHandBottomCards { .. }
            | WaitingFor::ManaPayment { .. }
            | WaitingFor::ManaSourceSelection { .. }
            | WaitingFor::AssistChoosePlayer { .. }
            | WaitingFor::AssistPayment { .. }
            | WaitingFor::ChooseXValue { .. }
            | WaitingFor::UntapChoice { .. }
            | WaitingFor::ChooseUntapSubset { .. }
            | WaitingFor::ExertChoice { .. }
            | WaitingFor::EnlistChoice { .. }
            | WaitingFor::GameOver { .. }
            | WaitingFor::ReplacementChoice { .. }
            | WaitingFor::EntryControllerChoice { .. }
            | WaitingFor::OrderTriggers { .. }
            | WaitingFor::CopyTargetChoice { .. }
            | WaitingFor::ExploreChoice { .. }
            | WaitingFor::ReturnAsAuraTarget { .. }
            | WaitingFor::ChooseTokenHost { .. }
            | WaitingFor::EquipTarget { .. }
            | WaitingFor::CrewVehicle { .. }
            | WaitingFor::StationTarget { .. }
            | WaitingFor::SaddleMount { .. }
            | WaitingFor::ScryChoice { .. }
            | WaitingFor::RippleRevealChoice { .. }
            | WaitingFor::RippleBottomOrder { .. }
            | WaitingFor::RevealUntilBottomOrder { .. }
            | WaitingFor::ArrangePlanarDeckTopChoice { .. }
            | WaitingFor::RedistributeLifeTotals { .. }
            | WaitingFor::CoinFlipKeepChoice { .. }
            | WaitingFor::DieKeepChoice { .. }
            | WaitingFor::DigChoice { .. }
            | WaitingFor::DigRestSplitChoice { .. }
            | WaitingFor::SurveilChoice { .. }
            | WaitingFor::RevealChoice { .. }
            | WaitingFor::SearchChoice { .. }
            | WaitingFor::SearchPartitionChoice { .. }
            | WaitingFor::OutsideGameChoice { .. }
            | WaitingFor::ChooseFromZoneChoice { .. }
            | WaitingFor::BeholdChoice { .. }
            | WaitingFor::EmpowerJaceChoice { .. }
            | WaitingFor::SpellCopyOrderChoice { .. }
            | WaitingFor::ChooseOneOfBranch { .. }
            | WaitingFor::ConniveDiscard { .. }
            | WaitingFor::DiscardChoice { .. }
            | WaitingFor::EffectZoneChoice { .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { .. }
            | WaitingFor::LearnChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::BetweenGamesSideboard { .. }
            | WaitingFor::BetweenGamesChoosePlayDraw { .. }
            | WaitingFor::NamedChoice { .. }
            | WaitingFor::OpponentGuess { .. }
            | WaitingFor::SpellbookDraft { .. }
            | WaitingFor::DamageSourceChoice { .. }
            | WaitingFor::ModeChoice { .. }
            | WaitingFor::DiscardToHandSize { .. }
            | WaitingFor::OptionalCostChoice { .. }
            | WaitingFor::ChooseGiftRecipient { .. }
            | WaitingFor::SpliceOffer { .. }
            | WaitingFor::DefilerPayment { .. }
            | WaitingFor::OrderCostReductions { .. }
            | WaitingFor::CastOffer { .. }
            | WaitingFor::ModalFaceChoice { .. }
            | WaitingFor::AlternativeCastChoice { .. }
            | WaitingFor::MutateMergeChoice { .. }
            | WaitingFor::CipherEncodeChoice { .. }
            | WaitingFor::CastingVariantChoice { .. }
            | WaitingFor::ChoosePermanentTypeSlot { .. }
            | WaitingFor::MultiTargetSelection { .. }
            | WaitingFor::AbilityModeChoice { .. }
            | WaitingFor::OptionalEffectChoice { .. }
            | WaitingFor::ResolutionOptionalPaymentChoice { .. }
            | WaitingFor::PairChoice { .. }
            | WaitingFor::TributeChoice { .. }
            | WaitingFor::MiracleReveal { .. }
            | WaitingFor::OpponentMayChoice { .. }
            | WaitingFor::LoopShortcut { .. }
            | WaitingFor::RespondToShortcut { .. }
            | WaitingFor::PrecastCopyShortcutOffer { .. }
            | WaitingFor::RespondToPrecastCopyShortcut { .. }
            | WaitingFor::UnlessPayment { .. }
            | WaitingFor::UnlessPaymentChooseCost { .. }
            | WaitingFor::WardDiscardChoice { .. }
            | WaitingFor::WardSacrificeChoice { .. }
            | WaitingFor::UnlessBounceChoice { .. }
            | WaitingFor::ChooseRingBearer { .. }
            | WaitingFor::ChooseRoomDoor { .. }
            | WaitingFor::ChooseDungeon { .. }
            | WaitingFor::ChooseDungeonRoom { .. }
            | WaitingFor::SpecializeColor { .. }
            | WaitingFor::PayCost { .. }
            | WaitingFor::ActivationCostOneOfChoice { .. }
            | WaitingFor::CostTypeChoice { .. }
            | WaitingFor::BlightChoice { .. }
            | WaitingFor::PayManaAbilityMana { .. }
            | WaitingFor::ManaAbilityManaPayment { .. }
            | WaitingFor::ChooseManaColor { .. }
            | WaitingFor::CollectEvidenceChoice { .. }
            | WaitingFor::HarmonizeTapChoice { .. }
            | WaitingFor::RevealUntilKeptChoice { .. }
            | WaitingFor::RepeatDecision { .. }
            | WaitingFor::TopOrBottomChoice { .. }
            | WaitingFor::PopulateChoice { .. }
            | WaitingFor::ClashChooseOpponent { .. }
            | WaitingFor::ChooseFromZoneOpponentChooser { .. }
            | WaitingFor::ChooseAnnouncingOpponent { .. }
            | WaitingFor::ClashCardPlacement { .. }
            | WaitingFor::PhysicalContest { .. }
            | WaitingFor::VoteChoice { .. }
            | WaitingFor::SeparatePilesChooseOpponent { .. }
            | WaitingFor::SeparatePilesPartition { .. }
            | WaitingFor::SeparatePilesChoice { .. }
            | WaitingFor::CompanionReveal { .. }
            | WaitingFor::ChooseLegend { .. }
            | WaitingFor::CommanderZoneChoice { .. }
            | WaitingFor::BattleProtectorChoice { .. }
            | WaitingFor::ProliferateChoice { .. }
            | WaitingFor::TimeTravelChoice { .. }
            | WaitingFor::ChooseObjectsSelection { .. }
            | WaitingFor::CategoryChoice { .. }
            | WaitingFor::EachPlayerCopyChosenSelection { .. }
            | WaitingFor::KeepWithinTotalPowerChoice { .. }
            | WaitingFor::KeepExactPermanentsChoice { .. }
            | WaitingFor::CopyRetarget { .. }
            | WaitingFor::AssignCombatDamage { .. }
            | WaitingFor::AssignBlockerDamage { .. }
            | WaitingFor::DistributeAmong { .. }
            | WaitingFor::MoveCountersDistribution { .. }
            | WaitingFor::RemoveCountersChoice { .. }
            | WaitingFor::PayAmountChoice { .. }
            | WaitingFor::RetargetChoice { .. }
            | WaitingFor::CombatTaxPayment { .. }
            | WaitingFor::PhyrexianPayment { .. } => panic!(
                "unscripted prompt WaitingFor::{} on turn {}; extend the script deliberately",
                self.runner.waiting_for_kind(),
                state.turn_number
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// Mirrors the tail of `GameSession::handle_action_with_card_db_outcome`
/// (server-core `session.rs`): the raw post-action state, the engine's events and
/// log entries, and the legal-action bundle recomputed from that state.
fn server_action_result(
    state: &engine::types::game_state::GameState,
    result: engine::types::game_state::ActionResult,
) -> ActionResult {
    let (legal_actions, spell_costs, by_object) = engine::ai_support::legal_actions_full(state);
    let auto_pass = engine::ai_support::auto_pass_recommended(state, &legal_actions);
    (
        state.clone(),
        result.events,
        legal_actions,
        result.log_entries,
        auto_pass,
        spell_costs,
        by_object,
    )
}

/// Mirrors the per-seat loop of `StateUpdatePublisher::publish` and the frame
/// encode of `socket_io::encode_application`: build the seat's `StateUpdate`,
/// wrap it in the writer's `Envelope`, serialize, then gzip-envelope it.
fn measure_publication(result: &ActionResult, revision: u64, totals: &mut [SeatBudget]) {
    let source = StateUpdateSource::from_result(result, revision);
    source
        .guard()
        .expect("the snapshot guard admits the scripted board");
    let full_key = FullSessionKey {
        game_code: "BUDGET".into(),
        generation: 1,
    };
    for (index, total) in totals.iter_mut().enumerate() {
        let message = crate::build_state_update_message(
            &source,
            &full_key,
            seat(index),
            Vec::new(),
            &source.state.eliminated_players,
        );
        let json = serde_json::to_string(&Envelope {
            message,
            command_id: None,
            quota_refusal: None,
        })
        .expect("StateUpdate serializes");
        let json_len = json.len() as u64;
        let wire_len = match crate::wire::encode_json_message_sync(json, true)
            .expect("wire envelope encodes")
        {
            Message::Binary(bytes) => bytes.len() as u64,
            other => panic!("expected a binary wire envelope, got {other:?}"),
        };
        total.updates += 1;
        total.json_bytes += json_len;
        total.wire_bytes += wire_len;
    }
}

fn play_round() -> Vec<SeatBudget> {
    let mut game = build_game();
    let mut totals: Vec<SeatBudget> = (0..SEATS)
        .map(|index| SeatBudget {
            seat: index,
            updates: 0,
            json_bytes: 0,
            wire_bytes: 0,
        })
        .collect();

    let mut revision = 0u64;
    let mut reached_end = false;
    for _ in 0..ITERATION_CAP {
        let action = game.next_action();
        let applied = game
            .runner
            .act(action.clone())
            .unwrap_or_else(|error| panic!("scripted action {action:?} rejected: {error:?}"));
        let result = server_action_result(game.runner.state(), applied);
        revision += 1;
        measure_publication(&result, revision, &mut totals);
        if game.runner.state().turn_number >= END_TURN {
            reached_end = true;
            break;
        }
    }
    assert!(
        reached_end,
        "scripted game did not reach turn {END_TURN} within {ITERATION_CAP} submissions (stuck on WaitingFor::{})",
        game.runner.waiting_for_kind()
    );

    // Guards against a vacuous pass: the script must have run to completion.
    let state = game.runner.state();
    assert_eq!(
        state.turn_number, END_TURN,
        "the round ends on turn {END_TURN}"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the round ends at a priority window, got WaitingFor::{}",
        game.runner.waiting_for_kind()
    );
    assert!(state.stack.is_empty(), "the stack is empty at the end");
    assert!(
        !matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        "nobody lost the game"
    );
    assert!(
        game.scripts.iter().all(VecDeque::is_empty) && game.targets.is_empty(),
        "every scripted intent was consumed"
    );
    for (index, land) in game.hand_lands.iter().enumerate() {
        assert_eq!(
            state.objects[land].zone,
            Zone::Battlefield,
            "seat {index} played its hand land"
        );
    }
    for (index, spell) in game.hand_spells.iter().enumerate() {
        let expected = if index == 0 {
            Zone::Graveyard
        } else {
            Zone::Battlefield
        };
        assert_eq!(
            state.objects[spell].zone, expected,
            "seat {index}'s hand spell resolved"
        );
    }
    assert_eq!(
        state.objects[&game.bolt_victim].zone,
        Zone::Graveyard,
        "Lightning Bolt killed the Llanowar Elves"
    );
    assert!(
        state.players[1].graveyard.contains(&game.bolt_victim),
        "the Bolted creature is in its owner's graveyard"
    );
    assert!(
        totals
            .iter()
            .all(|t| t.updates > 0 && t.updates == totals[0].updates),
        "every seat receives the same positive number of updates: {totals:?}"
    );
    totals
}

#[test]
fn state_update_bandwidth_stays_within_budget() {
    let measured = play_round();
    let budget = load_budget();
    eprintln!(
        "measured StateUpdate bandwidth (scenario {}):\n{}",
        budget.scenario,
        measured_table(&measured, &budget)
    );
    if std::env::var_os(UPDATE_ENV_VAR).is_some() {
        rewrite_budget(&measured);
        return;
    }
    assert_within_budget(&measured, &budget);
}
