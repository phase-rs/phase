//! **Gates a card prints that the engine does not actually gate**, and the
//! runtime proof that fixing them changes what the game allows. Every fix here
//! is in the PARSER — no `layers.rs` change.
//!
//! The file started at issue #8183 (a gate condition the parser could not type
//! lands as `StaticCondition::Unrecognized`, which the layer system evaluates
//! as ALWAYS TRUE — fail-open) and now covers four layers of the same genus:
//!
//!   * sections 1–3 — #8183's own three: a gate that failed to TYPE;
//!   * sections 4–5 — the defending-player gates. Ayesha's gate (section 5)
//!     and Graxiplon's (section 1) are parser gaps like #8183's;
//!     **Dandân's (section 4) is different in kind** — its gate types
//!     correctly and the ANCHOR cannot bind;
//!   * section 6 — the group-share counting gap: the gate types, the anchor
//!     binds, and the gate is STILL inert because of what the filter carries.
//!
//! CR 604.1: a static ability is "simply true" — its gate decides whether it
//! currently applies, so a gate the parser drops or mis-polarizes silently
//! turns the restriction on (or off) for every board state.
//!
//!  1. **Graxiplon** — "This creature can't be blocked unless defending player
//!     controls three or more creatures that share a creature type."
//!     The `unless` NEGATION was dropped at the `can't be blocked` fallback
//!     branch, leaving a bare `Unrecognized` that evaluates TRUE, so
//!     `CantBeBlocked` applied unconditionally: Graxiplon was permanently
//!     unblockable. #8183 restored the NEGATION but not its operand: the gate
//!     became `Not(Unrecognized)`, and since `Unrecognized` evaluates TRUE the
//!     `Not` evaluates FALSE, so the restriction applied on NO board. That was
//!     an honest improvement over permanent unblockability, but it is still not
//!     the printed card. The `defending player controls ` leaf of
//!     `parse_control_scope_prefix` types the operand, so the gate finally
//!     answers the board: with the printed gate UNMET the `Not` is TRUE, the
//!     restriction APPLIES, and the block is ILLEGAL (CR 509.1b — the
//!     defending player checks each creature for blocking restrictions, and an
//!     evasion ability creates one).
//!
//!  2. **Training Drone** — "This creature can't attack or block unless it's
//!     equipped." The anaphoric "it" in the gate was never resolved to the
//!     source, so the gate stayed `Not(Unrecognized)` = FALSE and the
//!     restriction NEVER applied: an unequipped Training Drone could attack.
//!     With the fix the gate is `Not(SourceIsEquipped)` (CR 301.5a — the
//!     creature an Equipment is attached to is the "equipped creature"), so an
//!     unequipped Drone cannot be declared as an attacker (CR 508.1c) and an
//!     equipped one can.
//!
//!  3. **Ancestral Katana** — `Equipped creature gets +2/+2 and has "This
//!     creature has first strike as long as it's attacking."` The predicate was
//!     split on the ` as long as ` INSIDE the quoted granted ability, so the
//!     granted first strike was destroyed and its gate was mis-attached to the
//!     +2/+2. With the fix the quoted ability survives as a granted static whose
//!     own gate is evaluated against the EQUIPPED CREATURE (CR 611.3a — a
//!     continuous effect from a static ability applies at any given moment to
//!     whatever its text indicates; CR 508.1k — an attacking creature).
//!
//!  4. **Dandân — the defending-player anchor that cannot bind.**
//!     "Can't attack unless defending player controls an Island" is the same
//!     printed grammar as section 5's on the OTHER side of combat, and it is a
//!     RUNTIME ANCHOR gap rather than a parser gap. Its gate types correctly,
//!     but attack legality is checked before the candidate is recorded as an
//!     attacker, so the anchor resolves to nothing and the gate reads UNMET on
//!     every board. That test pins the gap deliberately; read its doc comment
//!     before changing it.
//!
//!  5. **Ayesha Tanaka, Armorer — the defending-player count that never typed.**
//!     "Can't be blocked as long as defending player controls three or more
//!     artifacts" — and, on the same combinator, **Graxiplon**'s "…unless
//!     defending player controls three or more creatures that share a creature
//!     type" from section 1 — both fell to `Unrecognized` because the
//!     control-count combinator had only `you control` and `your opponents
//!     control` leaves. With the `defending player controls ` leaf the count is
//!     scoped through `ControllerRef::DefendingPlayer` and the anchor binds: for
//!     a `CantBeBlocked` static the source is already in `combat.attackers` when
//!     CR 509.1b's blocking-restriction check runs (CR 508.5 / CR 508.5a
//!     determine which seat).
//!
//!  6. **The group-share counting gap.**
//!     **Littjara Kinseekers** ("if you control three or more creatures that
//!     share a creature type", CR 603.4) and **Synchronized Eviction** ("costs
//!     {2} less to cast if you control at least two creatures that share a
//!     creature type") both parsed, both reported as supported, and both were
//!     WRONG: `parse_type_phrase` consumes the relative clause into a
//!     `FilterProp::SharesQuality { reference: None }`, which is a
//!     group-SELECTION marker that `filter::evaluate_shares_quality` answers
//!     TRUE for every object. Inside a `QuantityRef::ObjectCount` that silently
//!     drops the constraint, so the gate fired on ANY three (respectively two)
//!     creatures. The marker is now lifted onto the counting authority,
//!     `QuantityRef::ObjectCountBySharedQuality { aggregate: Max }`.
//!
//!     These are the first CAST-SIDE rows in this file, and the inert marker
//!     reaches the runtime through two different consumers: a trigger's
//!     intervening-if (Littjara, driven through the cast pipeline per the
//!     `/card-test` recipe) and a cost-modifying static (Synchronized Eviction,
//!     via `casting::self_spell_cost_condition_matches` →
//!     `layers::evaluate_condition`, asserted through `can_cast_object_now` at a
//!     tuned pool after `hollow_one_cost_reduction.rs`).
//!
//! Every card here is built from VERBATIM Oracle text through
//! `GameScenario`/`GameRunner`, so the whole production route runs: Oracle text
//! → static parser → `StaticDefinition.condition` → `evaluate_condition*` →
//! combat legality / cost modification / layer evaluation. No test in this file
//! asserts an AST shape as its primary claim; the AST assertions present are
//! explicitly labelled reach-guards.
//!
//! # FIXTURE CONTRACT
//!
//! Three separate rounds of this work produced three instances of ONE failure
//! class: an assertion that could not fail for the reason it claimed, because
//! the fixture never reached the state under test. Per-row repairs did not
//! hold, because each covered only the instance it found. The properties are
//! therefore stated once, here, and constructed by helpers rather than by each
//! row.
//!
//! **R1 — every combat fixture leaves at least one legal block available.**
//! The engine's auto-pass loop AUTO-SUBMITS the declare-blockers step when
//! `valid_blocker_ids` is empty, so a fixture where the restriction under test
//! correctly applies and nothing else is blockable never enters the step at
//! all. Adding defenders does not help — the step is skipped on "no legal block
//! anywhere", not on "no blockers". Constructed by declaring TWO attackers in
//! every combat row: the card under test and an unrestricted ally. Checked by
//! `declare_attackers_and_reach_blockers`, which asserts both `valid_blocker_ids`
//! and `valid_block_targets` are non-empty on arrival; each row then also
//! asserts the defender's blocker lists the ALLY among its legal targets, which
//! is what makes its `Err` on the card under test specific rather than global.
//!
//! **R2 — `state.all_creature_types` matches the board.**
//! `filter::shared_quality_values` filters an object's subtypes against that
//! registry (CR 205.3m) and `GameScenario` leaves it EMPTY, so every bucket is
//! empty, `AggregateFunction::Max` answers 0, and every gate-MET fixture
//! silently reads UNMET. `seed_creature_types_from_board` derives it from the
//! battlefield.
//!
//! **That seeding is structural ONLY for rows that enter combat**, because
//! `declare_attackers_and_reach_blockers` is what calls the seeder. A row that
//! does NOT enter combat is obliged to call `seed_creature_types_from_board`
//! itself — as the Littjara and Synchronized Eviction rows do. There is no
//! compiler or helper protection against omitting it: a non-combat row with an
//! unseeded registry gets `Max = 0`, every gate reads UNMET, and any "no
//! counter" / "not castable" assertion passes for the wrong reason, which is
//! exactly the vacuous negative R4 exists to forbid.
//!
//! Board-derived seeding is a SUBSET of CR 205.3m's full list. That is faithful
//! only while nothing reads the registry as a whole, which `Keyword::Changeling`
//! does. A changeling row is admissible under exactly one of two discharges and
//! must state which: (1) seed the full CR 205.3m list, or (2) prove the row's
//! assertion is invariant under registry extension. The Littjara pair takes (2).
//!
//! **R3 — nothing between declare-attackers and declare-blockers needs player
//! input.** The pass loop stops on any interactive `WaitingFor`. Ayesha's own
//! attack trigger lowers to `Effect::Dig`, which with a non-empty library yields
//! a `DigChoice` and strands the loop. It is a no-op here only because
//! `GameScenario` builds every library EMPTY unless a fixture explicitly adds
//! cards. A row that ever needs library cards must resolve the prompt rather
//! than pass through it. The helper's arrival assertion names this property so
//! such a failure reads as "R3 violated" rather than as an opaque panic.
//!
//! **R4 — negative assertions are paired, and the pair differs in exactly one
//! variable.** Every row whose primary claim is an ABSENCE — a block that must
//! be `Err`, a counter that must not be placed, a cast that must not be legal —
//! has a PRESENCE sibling on an otherwise identical fixture. For the group-share
//! pairs the creature COUNT is held equal across the pair, so the pre-fix inert
//! count answers the same in both and the pair isolates the lift rather than the
//! board size; the Synchronized Eviction pair additionally holds the mana pool
//! byte-identical, since there the pool is the instrument.

use engine::game::casting::can_cast_object_now;
use engine::game::combat::AttackTarget;
use engine::game::game_object::AttachTarget;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{CastOutcome, GameRunner, GameScenario, P0, P1};
use engine::types::ability::{StaticCondition, TargetFilter, TypeFilter};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

/// Verbatim Graxiplon Oracle text (MTGJSON AtomicCards).
const GRAXIPLON: &str = "This creature can't be blocked unless defending player controls three or more creatures that share a creature type.";

/// Verbatim Ayesha Tanaka, Armorer Oracle text (MTGJSON AtomicCards).
/// The evasion line uses the PRINTED CARD NAME, not "This creature" — `~`
/// normalization is what resolves it, so no fixture may paraphrase it.
const AYESHA: &str = "Whenever Ayesha Tanaka attacks, look at the top four cards of your library. You may put any number of artifact cards with mana value less than or equal to Ayesha Tanaka's power from among them onto the battlefield tapped. Put the rest on the bottom of your library in a random order.\nAyesha Tanaka can't be blocked as long as defending player controls three or more artifacts.";

/// Verbatim Littjara Kinseekers Oracle text (MTGJSON AtomicCards).
/// Line 1 is a KEYWORD line with reminder text and MUST be supplied through
/// `from_oracle_text_with_keywords(&["Changeling"], ..)`.
const LITTJARA_KINSEEKERS: &str = "Changeling (This card is every creature type.)\nWhen this creature enters, if you control three or more creatures that share a creature type, put a +1/+1 counter on this creature, then scry 1.";

/// Verbatim Synchronized Eviction Oracle text (MTGJSON AtomicCards).
const SYNCHRONIZED_EVICTION: &str = "This spell costs {2} less to cast if you control at least two creatures that share a creature type.\nPut target nonland permanent into its owner's library second from the top.";

/// Verbatim Training Drone Oracle text (MTGJSON AtomicCards).
const TRAINING_DRONE: &str = "This creature can't attack or block unless it's equipped.";

/// Ancestral Katana Oracle text — the Alchemy rebalanced printing,
/// which MTGJSON keys as `A-Ancestral Katana` and whose printed card name is
/// "Ancestral Katana". Lines 1-2 are verbatim; the trailing `Equip {2}` drops
/// MTGJSON's reminder text, which no assertion here reads.
/// The PAPER Ancestral Katana reads
/// "Equipped creature gets +2/+1." and carries no quoted granted ability at
/// all, so a fixture built from the paper text could not exercise this defect.
const ANCESTRAL_KATANA: &str = "Whenever a Samurai or Warrior you control attacks alone, you may pay {1}. When you do, attach Ancestral Katana to it.\nEquipped creature gets +2/+2 and has \"This creature has first strike as long as it's attacking.\"\nEquip {2}";

/// Verbatim Dandân Oracle text (MTGJSON AtomicCards).
/// The SECOND line is load-bearing for the fixture, not decoration: without an
/// Island under Dandân's OWN controller the creature is sacrificed and the
/// attack-legality assertions below become vacuous.
const DANDAN: &str = "This creature can't attack unless defending player controls an Island.\nWhen you control no Islands, sacrifice this creature.";

/// Wire `equipment` onto `host` the way the equip action does (CR 301.5).
/// Mirrors the local `attach` helper in `mjolnir_hammer_double_damage.rs`.
fn attach(runner: &mut GameRunner, equipment: ObjectId, host: ObjectId) {
    let state = runner.state_mut();
    state.objects.get_mut(&equipment).unwrap().attached_to = Some(AttachTarget::Object(host));
    state
        .objects
        .get_mut(&host)
        .unwrap()
        .attachments
        .push(equipment);
}

/// True iff `id` has `keyword` after a fresh layer evaluation (CR 613).
/// Same idiom as `knighthood_first_strike_grant.rs`.
fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

/// From `Phase::PreCombatMain`, pass to the declare-attackers step and assert we
/// actually arrived — so a harness change surfaces as a clear failure rather
/// than silently making every legality assertion below vacuous.
fn advance_to_declare_attackers(runner: &mut GameRunner) {
    // CR 508.1: use the scenario harness's purpose-built combat advance rather
    // than a single priority round-trip. A lone `pass_both_players()` lands on
    // the next `Priority` in the same phase, never on the declare-attackers
    // turn-based action.
    runner.advance_to_combat();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "fixture must reach the declare-attackers step; got {:?}",
        runner.state().waiting_for
    );
}

/// FIXTURE PROPERTY R2 (see the module doc's FIXTURE CONTRACT).
///
/// CR 205.3m: `SharedQuality::CreatureType` filters an object's subtypes
/// against `state.all_creature_types` (`game::filter::shared_quality_values`),
/// and `GameScenario` leaves that registry EMPTY — the same trap
/// `borg_queen_assimilate.rs` records for a different consumer. With it empty
/// every bucket is empty, `AggregateFunction::Max` answers 0, and every
/// gate-MET fixture in this file silently reads UNMET.
///
/// Seeded FROM THE BOARD rather than from a hand-written list, so a fixture
/// cannot add typed creatures and forget to declare them. Board-derived seeding
/// is a SUBSET of CR 205.3m's full creature-type list; that is faithful only
/// while nothing reads the registry as a whole, which `Keyword::Changeling`
/// does — `shared_quality_values` early-returns the WHOLE registry for a
/// changeling.
///
/// One caller here does use a changeling: the Littjara Kinseekers pair. It is
/// admissible because its assertion is INVARIANT under registry extension, not
/// because the registry happens to be complete. Extending the registry beyond
/// the board's own subtypes can only create buckets whose sole member is the
/// changeling itself, so every added bucket has size 1 and cannot raise
/// `AggregateFunction::Max`. Any future changeling caller must either repeat
/// that argument in its own doc or seed the full CR 205.3m list instead.
fn seed_creature_types_from_board(runner: &mut GameRunner) {
    let mut creature_types: Vec<String> = runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == Zone::Battlefield && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .flat_map(|obj| obj.card_types.subtypes.iter().cloned())
        .collect();
    // `objects` iterates in unspecified order; sort + dedup so the seeded
    // registry is deterministic across runs.
    creature_types.sort();
    creature_types.dedup();
    runner.state_mut().all_creature_types = creature_types;
}

/// FIXTURE PROPERTIES R1 + R3 (see the module doc's FIXTURE CONTRACT).
///
/// Declare `attackers`, seed the creature-type registry from the board, then
/// pass priority until the declare-blockers step and PROVE we arrived with a
/// block actually available.
///
/// CR 508.2: the active player gets priority after attackers are declared.
/// CR 117.3d: a player who takes no action passes and the next player receives
/// priority. CR 117.4: when all players pass in succession the step ends —
/// which is what reaches CR 509.1's declare-blockers step. A single
/// `pass_both_players()` is not enough: it passes only two seats, so a
/// three-player game needs more, and an attack trigger that goes on the stack
/// adds another round.
///
/// The registry is seeded after the declaration and before the pass loop
/// because the gates under test are read at CR 509.1b's blocking-restriction
/// check, which happens at the far end of that loop.
///
/// The arrival assertion names which property failed, because the three failure
/// modes are not distinguishable from the panic alone:
///   * still `Priority` — the pass budget is too small;
///   * any other `WaitingFor` — R3 violated: an ability of a fixture card needs
///     player input (Ayesha's attack trigger is an `Effect::Dig`; it is a no-op
///     only because `GameScenario` builds empty libraries);
///   * `DeclareBlockers` with an empty `valid_blocker_ids` cannot be observed,
///     because the engine's auto-pass loop auto-submits that case — which is
///     R1's mechanism, and why every row here declares a second unrestricted
///     attacker.
fn declare_attackers_and_reach_blockers(
    runner: &mut GameRunner,
    attackers: &[(ObjectId, AttackTarget)],
) {
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "fixture must be at the declare-attackers step before declaring; got {}",
        runner.state().waiting_for.variant_name()
    );
    runner
        .declare_attackers(attackers)
        .expect("every attacker in this fixture must be a legal attacker");
    seed_creature_types_from_board(runner);

    // CR 117.4: bounded so a stuck transition fails loudly instead of spinning.
    // Two seats per iteration; a three-player game and a triggered ability on
    // the stack each cost extra rounds.
    for _ in 0..8 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner.pass_both_players();
    }

    let WaitingFor::DeclareBlockers {
        valid_blocker_ids,
        valid_block_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "fixture must reach the declare-blockers step (CR 509.1); got {}. \
             Still `Priority` means the pass budget is too short; any other \
             prompt means FIXTURE PROPERTY R3 is violated — some ability of a \
             fixture card needs player input",
            runner.state().waiting_for.variant_name()
        );
    };
    assert!(
        !valid_blocker_ids.is_empty(),
        "FIXTURE PROPERTY R1: the declare-blockers step must be reached with at \
         least one legal block available, or the engine auto-submits empty \
         blockers and the row's block assertion is never evaluated"
    );
    assert!(
        !valid_block_targets.is_empty(),
        "FIXTURE PROPERTY R1: at least one blocker must have a legal attacker \
         to block; got an empty valid_block_targets map"
    );
}

/// The defending player's per-blocker legal-attacker list at the
/// declare-blockers step (`valid_block_targets` maps blocker → the attackers it
/// may legally block, CR 509.1a).
fn legal_attackers_for(runner: &GameRunner, blocker: ObjectId) -> Vec<ObjectId> {
    let WaitingFor::DeclareBlockers {
        valid_block_targets,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "must be at the declare-blockers step to read valid_block_targets; got {}",
            runner.state().waiting_for.variant_name()
        );
    };
    valid_block_targets
        .get(&blocker)
        .cloned()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 1. Graxiplon — the dropped `unless` negation
// ---------------------------------------------------------------------------

/// REACH-GUARD for the Graxiplon runtime test below.
///
/// `graxiplon_cannot_be_blocked_when_gate_unmet` asserts a block is ILLEGAL.
/// That assertion is satisfied for the wrong reason if Graxiplon parsed a
/// `CantBeBlocked` static carrying NO gate, which would refuse every block
/// unconditionally and reproduce the pre-#8183 bug while reading green. This
/// pins that the static exists AND carries a condition, so the illegal block
/// below is evidence about the GATE and not about an unconditional
/// restriction.
#[test]
fn graxiplon_parses_a_condition_gated_cant_be_blocked_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let graxiplon = scenario
        .add_creature_from_oracle(P0, "Graxiplon", 4, 4, GRAXIPLON)
        .id();
    let runner = scenario.build();

    let statics = &runner.state().objects[&graxiplon].static_definitions;
    let gated: Vec<_> = statics
        .iter_unchecked()
        .filter(|d| matches!(d.mode, StaticMode::CantBeBlocked))
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "Graxiplon must parse to exactly one CantBeBlocked static: {statics:#?}"
    );
    assert!(
        gated[0].condition.is_some(),
        "the CantBeBlocked static must carry the `unless` gate as a condition, \
         not be unconditional: {statics:#?}"
    );
}

/// CR 509.1b + CR 604.1: with the `unless` gate UNMET (the defending player
/// controls a single creature, not three sharing a creature type), the
/// restriction the printed card imposes DOES apply and the block is ILLEGAL.
///
/// **Polarity note — this assertion was flipped by this change.** Graxiplon
/// reads "can't be blocked **unless** …", so the gate is `Not(X)`: with `X`
/// false the restriction applies. The previous version of this test asserted
/// the block was LEGAL and documented that as correct; it was not. It encoded
/// #8183's honestly-reported intermediate state, where the gate was
/// `Not(Unrecognized)` — `Unrecognized` evaluates TRUE, the `Not` makes it
/// FALSE, and the restriction never applied. #8183 was a real improvement over
/// an unconditionally-unblockable Graxiplon, but "less wrong" is not the card.
///
/// Discriminating: revert the `defending player controls ` leaf of
/// `parse_control_scope_prefix` and the gate falls back to
/// `Not(Unrecognized)` = FALSE, the restriction stops applying, and the block
/// becomes legal — the `is_err()` assertion is what flips. Reverting the Branch
/// A lift alone does NOT move this row: on a one-Bear board the inert
/// `ObjectCount` answers 1, `1 >= 3` is false, `Not(false)` is true, and the
/// block is still refused. The lift's runtime discriminators are the two
/// non-sharing boards below.
#[test]
fn graxiplon_cannot_be_blocked_when_gate_unmet() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let graxiplon = scenario
        .add_creature_from_oracle(P0, "Graxiplon", 4, 4, GRAXIPLON)
        .id();
    // FIXTURE PROPERTY R1: a second, unrestricted attacker is REQUIRED. With
    // Graxiplon alone and its restriction correctly applying there is no legal
    // block anywhere, the engine auto-submits empty blockers, and the arrival
    // guard fires before this row's assertion is ever evaluated.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // ONE creature for the defender: the printed gate ("three or more creatures
    // that share a creature type") is unmet by construction.
    let blocker = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    declare_attackers_and_reach_blockers(
        &mut runner,
        &[
            (graxiplon, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ],
    );

    // IN-FIXTURE POSITIVE: the defender's Bear can still block the ally, so the
    // Err below is specific to Graxiplon rather than a global blocking failure.
    let legal = legal_attackers_for(&runner, blocker);
    assert!(
        legal.contains(&ally),
        "the defender's blocker must still be able to block the unrestricted \
         ally; got {legal:?}"
    );
    assert!(
        !legal.contains(&graxiplon),
        "with the `unless` gate unmet the evasion restriction applies, so \
         Graxiplon must not appear among the blocker's legal targets; got {legal:?}"
    );
    assert!(
        runner.declare_blockers(&[(blocker, graxiplon)]).is_err(),
        "CR 509.1b: `can't be blocked UNLESS defending player controls three or \
         more creatures that share a creature type` — the defender controls one \
         creature, so the gate is unmet, the restriction APPLIES, and the block \
         is illegal"
    );
}

/// GATE MET — three Goblins and a Zombie: the largest same-creature-type group
/// is 3, so `AggregateFunction::Max` answers 3, the `unless` gate is satisfied
/// and the evasion restriction does NOT apply.
///
/// Aggregate discrimination: this board is deliberately NOT three bare Goblins.
/// On a bare three-Goblin board `Max`, `Sum` and the pre-fix inert count all
/// answer 3, so it would discriminate nothing. Adding a fourth creature of a
/// different type keeps `Max` at 3 while ruling out `Min` (which would answer 1
/// and refuse the block).
///
/// Revert-discrimination: NEITHER unit alone turns this row red — it is the
/// presence sibling for the three `Err` rows around it and the canary for
/// FIXTURE PROPERTY R2 (an unseeded creature-type registry makes `Max` answer 0
/// and this row fails for a reason unrelated to the code under test).
#[test]
fn graxiplon_can_be_blocked_when_three_share_a_creature_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let graxiplon = scenario
        .add_creature_from_oracle(P0, "Graxiplon", 4, 4, GRAXIPLON)
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let blocker = scenario
        .add_creature(P1, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    scenario
        .add_creature(P1, "Goblin Raider", 2, 2)
        .with_subtypes(vec!["Goblin"]);
    scenario
        .add_creature(P1, "Goblin Brigand", 2, 2)
        .with_subtypes(vec!["Goblin"]);
    scenario
        .add_creature(P1, "Zombie Brute", 3, 3)
        .with_subtypes(vec!["Zombie"]);

    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    declare_attackers_and_reach_blockers(
        &mut runner,
        &[
            (graxiplon, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ],
    );

    let legal = legal_attackers_for(&runner, blocker);
    assert!(
        legal.contains(&graxiplon),
        "with three Goblins on the defender's board the gate is MET (Max = 3), \
         so Graxiplon must be blockable; got {legal:?}"
    );
    assert!(
        runner.declare_blockers(&[(blocker, graxiplon)]).is_ok(),
        "CR 509.1b: the `unless` gate is satisfied by three creatures sharing \
         the Goblin type, so the evasion restriction does not apply"
    );
}

/// GATE UNMET, and the row that discriminates the LIFT from the shipped defect:
/// three creatures, all of DIFFERENT creature types. The largest same-type
/// group is 1, so `Max` answers 1 and the gate is unmet.
///
/// **The inert pre-fix count answers 3 on this board and would allow the
/// block.** That is the whole point: the creature COUNT is held at the
/// threshold while the shared-type arithmetic is not, so only a parse that
/// actually enforces the shared-quality constraint refuses here.
///
/// Revert-discrimination: this row goes RED on a revert of EITHER unit. Revert
/// the `defending player controls ` leaf and the gate is `Not(Unrecognized)` =
/// FALSE, so the restriction never applies and the block is legal. Revert the
/// Branch A lift and the inert `ObjectCount` answers 3, `3 >= 3` is true,
/// `Not(true)` is false, and the block is legal again.
#[test]
fn graxiplon_cannot_be_blocked_when_no_three_share_a_creature_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let graxiplon = scenario
        .add_creature_from_oracle(P0, "Graxiplon", 4, 4, GRAXIPLON)
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let blocker = scenario
        .add_creature(P1, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    scenario
        .add_creature(P1, "Zombie Brute", 3, 3)
        .with_subtypes(vec!["Zombie"]);
    scenario
        .add_creature(P1, "Elvish Scout", 1, 1)
        .with_subtypes(vec!["Elf"]);

    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    declare_attackers_and_reach_blockers(
        &mut runner,
        &[
            (graxiplon, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ],
    );

    let legal = legal_attackers_for(&runner, blocker);
    assert!(
        legal.contains(&ally),
        "in-fixture positive: the blocker must still be able to block the ally; \
         got {legal:?}"
    );
    assert!(
        runner.declare_blockers(&[(blocker, graxiplon)]).is_err(),
        "three creatures of three DIFFERENT types share no creature type \
         (Max = 1 < 3), so the `unless` gate is unmet and the evasion \
         restriction applies. A count that ignored the shared-type constraint \
         would answer 3 and wrongly allow this block"
    );
}

/// GATE UNMET with the largest shared group at TWO: two Goblins and two
/// Zombies. `Max` answers 2 (< 3) so the gate is unmet.
///
/// This row rules out `AggregateFunction::Sum`, which the sibling above cannot:
/// `Sum` over the two buckets answers 4 and would allow the block, as would the
/// pre-fix inert count (4 creatures).
///
/// Revert-discrimination: RED on a revert of either unit, by the same
/// arithmetic as its sibling.
#[test]
fn graxiplon_cannot_be_blocked_when_the_largest_shared_group_is_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let graxiplon = scenario
        .add_creature_from_oracle(P0, "Graxiplon", 4, 4, GRAXIPLON)
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let blocker = scenario
        .add_creature(P1, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin"])
        .id();
    scenario
        .add_creature(P1, "Goblin Raider", 2, 2)
        .with_subtypes(vec!["Goblin"]);
    scenario
        .add_creature(P1, "Zombie Brute", 3, 3)
        .with_subtypes(vec!["Zombie"]);
    scenario
        .add_creature(P1, "Zombie Warrior", 2, 2)
        .with_subtypes(vec!["Zombie"]);

    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    declare_attackers_and_reach_blockers(
        &mut runner,
        &[
            (graxiplon, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ],
    );

    let legal = legal_attackers_for(&runner, blocker);
    assert!(
        legal.contains(&ally),
        "in-fixture positive: the blocker must still be able to block the ally; \
         got {legal:?}"
    );
    assert!(
        runner.declare_blockers(&[(blocker, graxiplon)]).is_err(),
        "the largest same-type group is 2 (< 3), so the gate is unmet. \
         `AggregateFunction::Sum` would answer 4 and wrongly allow this block, \
         as would a count that ignored the shared-type constraint"
    );
}

/// CR 508.5a (multi-authority hostile fixture): in a multiplayer game
/// "defending player" names ONE specific defending player — the one the
/// attacking creature is attacking — not any defender and not a merged
/// population.
///
/// Graxiplon and its ally attack P1, who controls a single Runeclaw Bear. Seat
/// 2 controls three Goblins, a board that WOULD satisfy the gate. The gate must
/// read P1's board and refuse the block.
///
/// Goes RED if the anchor resolves to a coarse "any defender" fallback, to a
/// merged count across seats, or to the wrong seat.
///
/// Revert-discrimination: the `defending player controls ` leaf only — on P1's
/// one-Bear board the lift's arithmetic is identical either way (see the
/// gate-unmet row above). Its job is the ANCHOR claim (which seat's board is
/// read), not the lift.
#[test]
fn graxiplon_reads_only_the_seat_it_is_attacking() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let graxiplon = scenario
        .add_creature_from_oracle(P0, "Graxiplon", 4, 4, GRAXIPLON)
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    // The seat that is NOT being attacked holds a gate-satisfying board.
    let bystander = PlayerId(2);
    scenario
        .add_creature(bystander, "Goblin Piker", 2, 1)
        .with_subtypes(vec!["Goblin"]);
    scenario
        .add_creature(bystander, "Goblin Raider", 2, 2)
        .with_subtypes(vec!["Goblin"]);
    scenario
        .add_creature(bystander, "Goblin Brigand", 2, 2)
        .with_subtypes(vec!["Goblin"]);

    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    declare_attackers_and_reach_blockers(
        &mut runner,
        &[
            (graxiplon, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ],
    );

    let legal = legal_attackers_for(&runner, blocker);
    assert_eq!(
        legal,
        vec![ally],
        "CR 508.5a: the gate must be evaluated against P1 — the seat Graxiplon \
         is attacking — whose single creature leaves it unmet. Seat 2's three \
         Goblins must not satisfy it"
    );
    assert!(
        runner.declare_blockers(&[(blocker, graxiplon)]).is_err(),
        "CR 508.5a: a third seat's gate-satisfying board must not make \
         Graxiplon blockable by the seat it is actually attacking"
    );
}

// ---------------------------------------------------------------------------
// 2. Training Drone — the unresolved source anaphor
// ---------------------------------------------------------------------------

/// CR 508.1c + CR 301.5a: an UNEQUIPPED Training Drone cannot be declared as an
/// attacker.
///
/// Discriminating: revert U1 (make the helper's SelfRef arm unreachable) and the
/// gate stays `Not(Unrecognized)` = FALSE, the restriction never applies, and
/// `declare_attackers` returns Ok. The `is_err()` assertion is what flips.
///
/// Paired positive reach-guard: `training_drone_can_attack_while_equipped`
/// below. Without it this negative could be satisfied by summoning sickness or
/// a tapped state rather than by the restriction under test.
#[test]
fn training_drone_cannot_attack_while_unequipped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drone = scenario
        .add_creature_from_oracle(P0, "Training Drone", 1, 1, TRAINING_DRONE)
        .id();
    // CR 508.1: a second, unrestricted attacker is REQUIRED, not decorative.
    // With the Drone as the only creature its restriction leaves zero legal
    // attackers, the engine never surfaces the declare-attackers turn-based
    // action, and the harness advances turns until a player decks — the test
    // then fails in the reach-guard on `GameOver` rather than exercising its
    // claim. The bear guarantees the step is reached so the assertion below
    // isolates the DRONE's legality specifically.
    let _bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    assert!(
        runner
            .declare_attackers(&[(drone, AttackTarget::Player(P1))])
            .is_err(),
        "an unequipped Training Drone must not be a legal attacker \
         (CR 508.1c; the `unless it's equipped` gate is unmet)"
    );
}

/// Paired positive for the negative above: attach an Equipment and the SAME
/// declaration becomes legal. This is what proves the negative is caused by the
/// gate and not by an unrelated attack-legality failure.
#[test]
fn training_drone_can_attack_while_equipped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drone = scenario
        .add_creature_from_oracle(P0, "Training Drone", 1, 1, TRAINING_DRONE)
        .id();
    // CR 301.5 + CR 704.5p: a bare attached noncreature permanent that is
    // neither Aura, Equipment, nor Fortification is unattached by SBAs, so the
    // Equipment subtype is load-bearing for the fixture.
    let equipment = scenario
        .add_creature(P0, "Bone Saw", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, equipment, drone);

    advance_to_declare_attackers(&mut runner);
    assert!(
        runner
            .declare_attackers(&[(drone, AttackTarget::Player(P1))])
            .is_ok(),
        "an EQUIPPED Training Drone must be a legal attacker — the \
         `unless it's equipped` gate is met, so the restriction does not apply"
    );
}

// ---------------------------------------------------------------------------
// 3. Ancestral Katana — the quoted granted ability and its own gate
// ---------------------------------------------------------------------------

/// CR 611.3a + CR 508.1k: the ability written in quotation marks is a separate
/// static granted to the equipped creature, and ITS gate ("as long as it's
/// attacking") is evaluated against the EQUIPPED CREATURE, not against the
/// Equipment and not against the +2/+2 grant.
///
/// Discriminating: revert any one of U3's three ` as long as ` quote guards and
/// the predicate is split inside the quotation marks — the granted first strike
/// is destroyed entirely and its gate is mis-attached to the +2/+2 — so the
/// attacking case has no first strike to find. The `has_kw(... FirstStrike)`
/// assertion under `state.combat` is what flips.
///
/// The not-attacking case is the paired reach-guard: a grant that applied
/// unconditionally (or never applied at all) would make one of the two halves
/// vacuous, and the pair rules both out.
#[test]
fn ancestral_katana_granted_first_strike_binds_to_equipped_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Deliberately NOT a Samurai or Warrior: the Equipment's own attack trigger
    // ("Whenever a Samurai or Warrior you control attacks alone") must not fire
    // and add an unrelated prompt to this fixture.
    let bearer = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    let katana = scenario
        .add_creature(P0, "Ancestral Katana", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(ANCESTRAL_KATANA)
        .id();

    let mut runner = scenario.build();
    attach(&mut runner, katana, bearer);

    // Not attacking: the granted static's own gate is FALSE, so no first strike.
    assert!(
        !has_kw(&mut runner, bearer, &Keyword::FirstStrike),
        "the granted first strike is gated on `as long as it's attacking`; a \
         non-attacking equipped creature must NOT have it"
    );
    // The +2/+2 half of the same line must apply unconditionally — the gate
    // belongs to the QUOTED ability, not to the P/T grant. Without this the
    // not-attacking assertion above is also satisfied by a line that parsed to
    // nothing at all.
    assert_eq!(
        runner.state().objects[&bearer].power,
        Some(4),
        "the +2/+2 grant is ungated and must apply to the equipped creature \
         whether or not it is attacking"
    );

    // Attacking: CR 508.1k — the equipped creature is now an attacking
    // creature, so the granted static's gate is TRUE. Drive this through the
    // engine's own declaration rather than installing a hand-built CombatState:
    // a hand-built one makes the assertion depend on this test's idea of how
    // attackers are recorded, so it could stay green while real combat stopped
    // granting first strike. The bearer is an unrestricted Runeclaw Bear, so it
    // is itself the legal attacker that makes the step reachable, and the
    // Katana's own trigger needs a Samurai or Warrior and so cannot fire here.
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(bearer, AttackTarget::Player(P1))])
        .expect("the equipped bearer must be a legal attacker");
    assert!(
        has_kw(&mut runner, bearer, &Keyword::FirstStrike),
        "an ATTACKING equipped creature must have the granted first strike \
         (CR 611.3a: the granted static's `it` names the equipped creature)"
    );
}

// ---------------------------------------------------------------------------
// 4. Dandân — the defending-player anchor that cannot bind (V0 pre-flight)
// ---------------------------------------------------------------------------

/// REACH-GUARD for `dandan_attack_legality_ignores_the_defending_players_board`.
///
/// That test asserts Dandân is an ILLEGAL attacker in BOTH arms. That is
/// satisfied for the wrong reason if Dandân's gate mis-parsed into something
/// that can never be satisfied. Destructuring the exact two-node tree
/// `Not(DefendingPlayerControls{Island})` pins that the gate typed correctly
/// and — because the whole tree is named here — that no
/// `StaticCondition::Unrecognized` hides anywhere inside it.
///
/// CR 508.1c: "can't attack unless <condition>" is a restriction, so the
/// printed `unless` is the negation the `Not` carries.
#[test]
fn dandan_parses_a_condition_gated_cant_attack_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dandan = scenario
        .add_creature_from_oracle(P0, "Dandan", 4, 1, DANDAN)
        .id();
    let runner = scenario.build();

    let statics = &runner.state().objects[&dandan].static_definitions;
    let gated: Vec<_> = statics
        .iter_unchecked()
        .filter(|d| matches!(d.mode, StaticMode::CantAttack))
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "Dandân must parse to exactly one CantAttack static: {statics:#?}"
    );
    let Some(StaticCondition::Not { condition }) = &gated[0].condition else {
        panic!(
            "the CantAttack static must carry the printed `unless` gate as \
             Not(..), not be unconditional or Unrecognized: {statics:#?}"
        );
    };
    let StaticCondition::DefendingPlayerControls { filter } = condition.as_ref() else {
        panic!(
            "the negated gate must be a typed DefendingPlayerControls, not an \
             Unrecognized leaf: {condition:#?}"
        );
    };
    let TargetFilter::Typed(tf) = filter else {
        panic!("the gate's filter must be Typed: {filter:#?}");
    };
    assert!(
        tf.type_filters
            .contains(&TypeFilter::Subtype("Island".to_string())),
        "the gate must name the Island subtype the printed card asks about, \
         got {:?}",
        tf.type_filters
    );
}

/// **This test pins a KNOWN ENGINE GAP, not correct behaviour.**
///
/// Dandân prints "This creature can't attack unless defending player controls
/// an Island." The gate types correctly (see the reach-guard above), but the
/// `DefendingPlayerControls` anchor cannot bind during the attack-legality
/// check: `combat::creature_cant_attack_gated` evaluates the static's condition
/// through `functioning_abilities::active_static_definitions` →
/// `layers::evaluate_condition` with no combat context, and both
/// `validate_attack_declaration` and `get_valid_attacker_ids` run BEFORE the
/// candidate is recorded in `state.combat.attackers`. The anchor resolves to
/// `None`, which the filter door renders as "no matching permanent", so the
/// `unless` gate reads UNMET on every board and the restriction always applies.
///
/// CR 506.2 + CR 508.1c: this is an ENGINE gap, not a rules gap. In this
/// two-player fixture the nonactive player is the defending player for the
/// whole combat phase (CR 506.2), so the defender is already determined when
/// CR 508.1c's restriction check runs. **CR 508.1b does not run here** — its
/// antecedent (the defending player controls planeswalkers, is the protector of
/// a battle, or the game allows attacking multiple players) is false in this
/// fixture — which is why CR 506.2 is the cite and 508.1b is not.
///
/// The 2×2, and what each outcome would mean:
///
/// | (island arm, no-island arm) | meaning |
/// |---|---|
/// | **(Err, Err)** — what this test asserts | the anchor never binds; the gap is real |
/// | (Ok, Err) | the anchor DOES bind — the gap is closed |
/// | (Ok, Ok) | the restriction never applies at all — a different defect |
/// | (Err, Ok) | inverted — escalate |
///
/// **If this test ever fails because the Island arm became `Ok`, do not relax
/// it — the engine gap was fixed.** Flip the Island arm to `is_ok()` and re-open
/// the `CantAttack` cards that are scoped out on this mechanism (Chained
/// Throatseeker, Crown-Hunter Hireling, Goblin Goon, Mogg Toady, Monstrous
/// Hound, Vantress Gargoyle, and the other 32 statics gated on
/// `DefendingPlayerControls`).
///
/// Paired positive control, in BOTH arms: an unrestricted Grizzly Bears is
/// listed in `valid_attacker_ids` and is a legal attacker. That excludes
/// summoning sickness, an illegal attack target, a Dandân sacrificed by its own
/// second line, and a harness artifact as causes of the `Err`.
#[test]
fn dandan_attack_legality_ignores_the_defending_players_board() {
    for defender_island in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let dandan = scenario
            .add_creature_from_oracle(P0, "Dandan", 4, 1, DANDAN)
            .id();
        let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        // Dandân's OWN controller must hold an Island in both arms, or its
        // second line ("When you control no Islands, sacrifice this creature.")
        // removes the creature under test and every assertion below is vacuous.
        scenario.add_basic_land(P0, ManaColor::Blue);
        let defender_land = defender_island.then(|| scenario.add_basic_land(P1, ManaColor::Blue));

        let mut runner = scenario.build();

        // BOARD REACH-GUARD: without this an `(Err, Err)` is also compatible
        // with "the fixture never gave the defender an Island", which would
        // make the insensitivity claim vacuous rather than true.
        if let Some(land) = defender_land {
            let obj = &runner.state().objects[&land];
            assert!(
                obj.card_types.core_types.contains(&CoreType::Land)
                    && obj.card_types.subtypes.iter().any(|s| s == "Island"),
                "the Island arm must actually give P1 a Land with the Island \
                 subtype; got {:?} / {:?}",
                obj.card_types.core_types,
                obj.card_types.subtypes
            );
        }

        advance_to_declare_attackers(&mut runner);
        let WaitingFor::DeclareAttackers {
            valid_attacker_ids, ..
        } = &runner.state().waiting_for
        else {
            panic!(
                "advance_to_declare_attackers must leave the declare-attackers \
                 prompt in place; got {:?}",
                runner.state().waiting_for.variant_name()
            );
        };
        assert!(
            valid_attacker_ids.contains(&bear),
            "positive control: the unrestricted bear must be a valid attacker \
             (defender_island = {defender_island}); got {valid_attacker_ids:?}"
        );

        assert!(
            runner
                .declare_attackers(&[(dandan, AttackTarget::Player(P1))])
                .is_err(),
            "ENGINE GAP (CR 506.2 + CR 508.1c): Dandân's attack legality is \
             insensitive to the defending player's board — it is refused even \
             with defender_island = {defender_island}. If this arm is now Ok, \
             the anchor binds and this test's premise is obsolete; read the \
             doc comment before changing it"
        );
        assert!(
            runner
                .declare_attackers(&[(bear, AttackTarget::Player(P1))])
                .is_ok(),
            "positive control: the unrestricted bear must be a legal attacker \
             (defender_island = {defender_island})"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Ayesha Tanaka, Armorer — the defending-player count that never typed
// ---------------------------------------------------------------------------

/// REACH-GUARD for the two Ayesha runtime rows below (labelled as such per this
/// file's convention).
///
/// Both rows assert a blocking outcome. Either is satisfied for the wrong
/// reason if Ayesha parsed no `CantBeBlocked` static, or parsed one whose gate
/// is still an always-true `Unrecognized`. The pattern names the VARIANT, not
/// the whole tree (`QuantityComparison { .. }`), which is sufficient here: it
/// pins that the gate exists and is typed, and a `QuantityComparison` carries
/// only `QuantityExpr` / `Comparator` fields, `QuantityExpr` is closed over
/// arithmetic on `QuantityRef` / `i32`, and no `QuantityRef` variant embeds a
/// `StaticCondition` — so no `Unrecognized` can hide inside it. Contrast
/// Dandân's guard above, which destructures every node and needs no such
/// type-level argument.
///
/// Revert-discrimination: the `defending player controls ` leaf only. The
/// Branch A lift never touches Ayesha, whose gate carries no shared-quality
/// clause.
#[test]
fn ayesha_parses_a_condition_gated_cant_be_blocked_static() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ayesha = scenario
        .add_creature_from_oracle(P0, "Ayesha Tanaka, Armorer", 2, 2, AYESHA)
        .id();
    let runner = scenario.build();

    let statics = &runner.state().objects[&ayesha].static_definitions;
    let gated: Vec<_> = statics
        .iter_unchecked()
        .filter(|d| matches!(d.mode, StaticMode::CantBeBlocked))
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "Ayesha must parse to exactly one CantBeBlocked static: {statics:#?}"
    );
    let Some(StaticCondition::QuantityComparison { .. }) = &gated[0].condition else {
        panic!(
            "the CantBeBlocked static's `as long as defending player controls \
             three or more artifacts` gate must be a typed quantity comparison, \
             not absent and not an Unrecognized leaf: {statics:#?}"
        );
    };
}

/// CR 509.1b + CR 508.5: gate MET — the defending player controls three
/// artifacts, so Ayesha's evasion restriction APPLIES and she cannot be
/// blocked.
///
/// Ayesha's polarity is `as long as` (positive), the mirror of Graxiplon's
/// `unless`.
///
/// Revert-discrimination: NEITHER unit alone turns this row red. Reverting the
/// `defending player controls ` leaf leaves the gate as an always-true
/// `Unrecognized`, so the restriction still applies and the block is still
/// refused. This row is the presence sibling of the two-artifact row below,
/// which is the actual discriminator.
///
/// FIXTURE PROPERTY R3: Ayesha's own attack trigger lowers to `Effect::Dig`,
/// which would open a `DigChoice` prompt and strand the pass loop. It is a
/// no-op here only because `GameScenario` builds every library EMPTY. A future
/// edit that gives P0 library cards must resolve that prompt explicitly.
#[test]
fn ayesha_cannot_be_blocked_when_defender_controls_three_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ayesha = scenario
        .add_creature_from_oracle(P0, "Ayesha Tanaka, Armorer", 2, 2, AYESHA)
        .id();
    // FIXTURE PROPERTY R1.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    for name in ["Bone Saw", "Chrome Mox", "Star Compass"] {
        scenario.add_artifact_from_oracle(P1, name, "");
    }

    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    declare_attackers_and_reach_blockers(
        &mut runner,
        &[
            (ayesha, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ],
    );

    let legal = legal_attackers_for(&runner, blocker);
    assert!(
        legal.contains(&ally),
        "in-fixture positive: the blocker must still be able to block the \
         unrestricted ally; got {legal:?}"
    );
    assert!(
        !legal.contains(&ayesha),
        "with three artifacts on the defender's board the gate is MET, so \
         Ayesha must be absent from the blocker's legal targets; got {legal:?}"
    );
    assert!(
        runner.declare_blockers(&[(blocker, ayesha)]).is_err(),
        "CR 509.1b: `can't be blocked as long as defending player controls \
         three or more artifacts` — the defender controls three, so the \
         restriction applies and the block is illegal"
    );
}

/// CR 509.1b + CR 508.5: gate UNMET — the defending player controls only TWO
/// artifacts, one short of the printed threshold, so the restriction does NOT
/// apply and the block is legal.
///
/// **This is the discriminating runtime row for the `defending player
/// controls ` leaf.** Revert it and the gate falls back to an always-true
/// `Unrecognized`: the restriction applies on every board, Ayesha becomes
/// permanently unblockable, and the `is_ok()` assertion flips. The Branch A
/// lift does not move this row — Ayesha's gate has no shared-quality clause.
///
/// Its sibling above holds every board fact equal except the artifact count, so
/// the pair isolates the threshold rather than the fixture.
#[test]
fn ayesha_can_be_blocked_when_defender_controls_two_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ayesha = scenario
        .add_creature_from_oracle(P0, "Ayesha Tanaka, Armorer", 2, 2, AYESHA)
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    for name in ["Bone Saw", "Chrome Mox"] {
        scenario.add_artifact_from_oracle(P1, name, "");
    }

    let mut runner = scenario.build();

    advance_to_declare_attackers(&mut runner);
    declare_attackers_and_reach_blockers(
        &mut runner,
        &[
            (ayesha, AttackTarget::Player(P1)),
            (ally, AttackTarget::Player(P1)),
        ],
    );

    let legal = legal_attackers_for(&runner, blocker);
    assert!(
        legal.contains(&ayesha),
        "with only two artifacts the gate is UNMET, so Ayesha must appear among \
         the blocker's legal targets; got {legal:?}"
    );
    assert!(
        runner.declare_blockers(&[(blocker, ayesha)]).is_ok(),
        "CR 509.1b: two artifacts do not satisfy `three or more`, so the \
         evasion restriction does not apply and the block is legal"
    );
}

// ---------------------------------------------------------------------------
// 6. The group-share counting gap — a gate that TYPED but is still inert
// ---------------------------------------------------------------------------

/// CR 603.4 + CR 205.3m: Littjara Kinseekers' intervening-if is UNMET on a
/// board whose largest same-creature-type group is two, so the ETB ability
/// never goes on the stack and no +1/+1 counter is placed.
///
/// **This is the discriminating runtime row for the `you`-scope Branch A
/// repair.** Both this row and its sibling below hold exactly THREE creatures
/// once Littjara resolves, so the pre-fix inert `ObjectCount` answers 3 in
/// both — on `main` both rows get the counter. Only the lifted
/// `ObjectCountBySharedQuality { aggregate: Max }` tells them apart. Revert the
/// Branch A lift and this row goes RED while its sibling stays green, which is
/// the signature of a lift that did not land. Reverting the `defending player
/// controls ` leaf moves neither row: Littjara's gate is `you`-scoped.
///
/// The arithmetic: Littjara is a changeling, so it belongs to EVERY creature
/// type. With one Goblin and one Zombie beside it the buckets are
/// `{Littjara, Goblin}` = 2 and `{Littjara, Zombie}` = 2, so `Max` = 2 < 3.
///
/// FIXTURE PROPERTY R2, changeling discharge (2): the registry is seeded from
/// the board while Littjara is still in hand, so it holds `{Goblin, Zombie}`.
/// The assertion is INVARIANT under any superset of that: extending the
/// registry can only add buckets whose sole member is the changeling itself, so
/// every added bucket has size 1 and cannot raise `Max` above 2.
///
/// This row does not enter combat, so `declare_attackers_and_reach_blockers`
/// never runs and R2's seeding is this row's own obligation — see the call
/// below.
///
/// Reach guards for the absence claim: `assert_zone` proves the cast actually
/// resolved onto the battlefield, and `final_waiting_for` proves the pipeline
/// halted on a clean priority window rather than on an unanswered prompt. Its
/// presence sibling proves the trigger path fires at all.
#[test]
fn littjara_gets_no_counter_when_only_two_share_a_creature_type() {
    let (outcome, littjara) = cast_littjara_into_board(&["Goblin", "Zombie"]);
    outcome.assert_zone(&[littjara], Zone::Battlefield);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the cast pipeline must halt on a clean priority window, not on an \
         unanswered prompt; got {}",
        outcome.final_waiting_for().variant_name()
    );
    outcome.assert_counters(littjara, CounterType::Plus1Plus1, 0);
}

/// PRESENCE SIBLING and reach guard for the row above (FIXTURE PROPERTY R4).
///
/// Same construction, same creature COUNT, one variable changed: both of
/// Littjara's neighbours are Goblins, so the bucket `{Littjara, Goblin, Goblin}`
/// has three members, `Max` = 3, CR 603.4's intervening-if is satisfied and the
/// +1/+1 counter is placed.
///
/// Revert-discrimination: NEITHER unit. On `main` the inert count also answers
/// 3 here, so this row is green before and after. Its job is to prove the ETB
/// trigger path is live, so its sibling's absence claim cannot be satisfied by
/// a trigger that never fires.
///
/// R2 discharge (2) again: `Max` = 3 under any superset of `{Goblin}`.
#[test]
fn littjara_gets_a_counter_when_three_share_a_creature_type() {
    let (outcome, littjara) = cast_littjara_into_board(&["Goblin", "Goblin"]);
    outcome.assert_zone(&[littjara], Zone::Battlefield);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the cast pipeline must halt on a clean priority window, not on an \
         unanswered prompt; got {}",
        outcome.final_waiting_for().variant_name()
    );
    outcome.assert_counters(littjara, CounterType::Plus1Plus1, 1);
}

/// Shared construction for the Littjara pair: two vanilla creatures carrying
/// `subtypes` on P0's battlefield, Littjara cast from P0's hand, and the
/// creature-type registry seeded from the board BEFORE the cast (the registry
/// is read when the intervening-if is evaluated).
///
/// The neighbours are vanilla bodies with no Oracle text, so nothing else in
/// the fixture can trigger. `then scry 1` needs no handling: the cast driver
/// auto-answers a scry prompt, and the library is empty anyway.
fn cast_littjara_into_board(subtypes: &[&str; 2]) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for (index, subtype) in subtypes.iter().enumerate() {
        scenario
            .add_creature(P0, &format!("Vanilla {index}"), 2, 2)
            .with_subtypes(vec![subtype]);
    }
    // FOOT-GUN: line 1 is a KEYWORD line with reminder text. It must be
    // supplied through `from_oracle_text_with_keywords(&["Changeling"], ..)` or
    // it lowers to `Effect::Unimplemented` and the changeling expansion this
    // fixture's arithmetic depends on never happens.
    let littjara = scenario
        .add_creature_to_hand(P0, "Littjara Kinseekers", 2, 4)
        .from_oracle_text_with_keywords(&["Changeling"], LITTJARA_KINSEEKERS)
        .id();

    let mut runner = scenario.build();
    // FIXTURE PROPERTY R2 — this row never reaches
    // `declare_attackers_and_reach_blockers`, so the seeding is its own
    // obligation. It must run BEFORE the cast: the registry is read when the
    // intervening-if is evaluated. Littjara is still in hand here, so the
    // registry is exactly the board's own subtypes.
    seed_creature_types_from_board(&mut runner);

    (runner.cast(littjara).resolve(), littjara)
}

/// CR 601.2f + CR 604.1: Synchronized Eviction's cost-reduction gate is UNMET
/// on a board whose largest same-creature-type group is one, so no reduction is
/// due, the full printed {4}{U} must be paid, and a pool holding exactly the
/// REDUCED cost cannot pay it.
///
/// **This is the discriminating runtime row for the `you`-scope Branch A repair
/// on the cost-modifying consumer**, the second of the two runtime routes the
/// inert marker reaches (the other is Littjara's trigger intervening-if). The
/// path is `casting::self_spell_cost_modifier_applies_before_targets` →
/// `self_spell_cost_condition_matches` → `layers::evaluate_condition`.
///
/// Both this row and its sibling hold exactly TWO creatures and a
/// byte-identical pool, so the pre-fix inert `ObjectCount` answers 2 in both and
/// on `main` both rows are castable. Revert the Branch A lift and this row goes
/// RED while its sibling stays green. Reverting the `defending player
/// controls ` leaf moves neither: Synchronized Eviction is `you`-scoped.
///
/// Pattern precedent: `hollow_one_cost_reduction.rs`, which asserts a cost
/// delta through `can_cast_object_now` at a tuned pool because `CastOutcome`
/// exposes no mana-paid accessor.
#[test]
fn synchronized_eviction_is_not_reduced_when_creatures_share_no_type() {
    let (runner, eviction) = build_synchronized_eviction_board(&["Goblin", "Zombie"]);
    assert!(
        !can_cast_object_now(runner.state(), P0, eviction),
        "one Goblin and one Zombie share no creature type (Max = 1 < 2), so the \
         {{2}} reduction is NOT due, the full printed {{4}}{{U}} is owed, and a \
         three-mana pool cannot pay it. A count that ignored the shared-type \
         constraint would answer 2, grant the reduction, and wrongly make this \
         cast legal"
    );
}

/// PRESENCE SIBLING and reach guard for the row above (FIXTURE PROPERTY R4).
///
/// Same construction, same creature COUNT, byte-identical pool, one variable
/// changed: both creatures are Goblins, so `Max` = 2, the {2} reduction applies
/// and {2}{U} is due — exactly the pool held.
///
/// This is the primary guard against a vacuous negative: it proves the entire
/// path is live (spell in hand, printed cost set, pool funded, priority
/// available, a legal target present), so its sibling's `false` can only be the
/// missing reduction. Target availability is symmetric by construction —
/// Synchronized Eviction targets a nonland permanent and both boards hold
/// exactly two creatures — so it cannot be the variable.
///
/// Revert-discrimination: NEITHER unit. On `main` the inert count also answers
/// 2 here, so this row is green before and after.
#[test]
fn synchronized_eviction_is_reduced_when_two_creatures_share_a_type() {
    let (runner, eviction) = build_synchronized_eviction_board(&["Goblin", "Goblin"]);
    assert!(
        can_cast_object_now(runner.state(), P0, eviction),
        "two Goblins share the Goblin creature type (Max = 2), so the {{2}} \
         reduction applies, {{2}}{{U}} is due, and the pool holds exactly that"
    );
}

/// Shared construction for the Synchronized Eviction pair.
///
/// **No lands and no other mana source on either board.** `can_cast_object_now`
/// asks whether the cost is feasibly PAYABLE, not whether the floating pool
/// alone covers it: it reaches
/// `casting::can_feasibly_pay_mana_cost_without_x_with_probe`, whose first act
/// is the auto-tap probe `casting::can_pay_cost_after_auto_tap_with_probe`, and
/// only the residual is charged against the pool. So the floating pool must be
/// the only mana available — the same reason `hollow_one_cost_reduction.rs`
/// puts no lands on its board.
///
/// Breaking it FAILS LOUDLY, which is why it is documented rather than
/// asserted: `synchronized_eviction_is_not_reduced_when_creatures_share_no_type`
/// asserts `!can_cast_object_now`, so any added mana source turns THAT row RED.
/// "Both rows go green proving nothing" is not a reachable state. Only P0's
/// side is load-bearing either way.
///
/// The phase is `PreCombatMain` to match that precedent; Synchronized Eviction
/// is an INSTANT, so the phase is not load-bearing for legality.
///
/// FIXTURE PROPERTY R2: seeded from the board before the verdict, because
/// `filter::shared_quality_values` reads the registry when
/// `layers::evaluate_condition` evaluates the gate. No changeling is on either
/// board, so R2's changeling side condition does not fire and board-derived
/// seeding is exact: extending the registry beyond `{Goblin, Zombie}` /
/// `{Goblin}` adds only buckets no object on the board belongs to, so `Max` is
/// unchanged under any superset.
fn build_synchronized_eviction_board(subtypes: &[&str; 2]) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for (index, subtype) in subtypes.iter().enumerate() {
        scenario
            .add_creature(P0, &format!("Vanilla {index}"), 2, 2)
            .with_subtypes(vec![subtype]);
    }
    // Both printed lines: the second is what makes it a targeted spell, and
    // dropping it would change what the cast pipeline sees.
    let eviction = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Synchronized Eviction",
            /* is_instant */ true,
            SYNCHRONIZED_EVICTION,
        )
        // The PRINTED cost, not a convenience cost: the reduction is what is
        // under test, so the pre-reduction figure has to be real.
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 4,
        })
        .id();
    // Exactly the REDUCED cost {2}{U}, and byte-identical between the two rows.
    // The pool is the instrument; if the two rows' pools differed the pair
    // would prove nothing.
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, eviction, false, Vec::new()),
            ManaUnit::new(ManaType::Colorless, eviction, false, Vec::new()),
            ManaUnit::new(ManaType::Colorless, eviction, false, Vec::new()),
        ],
    );

    let mut runner = scenario.build();
    seed_creature_types_from_board(&mut runner);
    (runner, eviction)
}
