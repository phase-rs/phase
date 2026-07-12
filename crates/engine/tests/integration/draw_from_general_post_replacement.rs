//! Pre-rewrite pins for the general-continuation -> `Effect::Draw` edge (Plan 03).
//!
//! Plan 03 replaces the ownerless `GameState::post_replacement_continuation`
//! field with an owned `PostReplacementDrainStack`, and reroutes true draws
//! through a three-stage CR 121.2 state machine. The hard part of that rewrite
//! is the edge where a **non-draw** replacement's continuation itself performs a
//! true draw: the draw has to become a *child* of the general drain, run to
//! completion (including its own replacements and any Miracle pause), and only
//! then wake the general parent — instead of starting an independent draw root.
//!
//! These tests exist so that edge cannot change silently. They encode what the
//! engine does TODAY, on real cards, through the real parser and the real
//! pipeline. They are not a specification of the new design; they are the
//! before-picture the rewrite must reproduce.
//!
//! The draw arrives through
//! `engine_replacement::apply_pending_post_replacement_effect`, whose two arms
//! (`Template` / `Resolved`) are the seam Plan 03 rewrites. Neither arm filters on
//! effect kind, which is exactly why `Effect::Draw` can arrive here at all.
//!
//!  1. **Swans of Bryn Argoll** — `Template`. A static prevention rider
//!     (CR 615.5): damage to it is prevented and the rider draws that many cards
//!     for the source's controller. Count rides `EventContextAmount`.
//!
//!  2. **Nefarious Lich** — `Template`. An ordinary substitution (CR 614.6): the
//!     life gain never happens and an equal-sized draw happens instead.
//!
//!  3. **New Way Forward** — the card that *should* be the `Resolved` witness,
//!     pinned at the point where it fails to be one. See its test.
//!
//! # Which arm a card takes — and why `Resolved` has no draw witness at all
//!
//! `apply_single_replacement` builds `Resolved` **only** from a shield's
//! `runtime_execute`, and otherwise falls back to `Template` from the `execute`
//! AST. A static permanent ability parsed from Oracle text carries only an
//! `execute` — never a `runtime_execute`. So every parsed static shield takes
//! `Template`; only a resolving *spell* that installs a shield with a rider
//! (`effects/prevent_damage.rs`) can take `Resolved`.
//!
//! The Plan 03 census named Swans as the `Resolved` witness, reasoning that a
//! combat prevention rider must reach
//! `combat_damage.rs::fire_combat_prevention_riders`. It does not: that path is
//! gated on `runtime_execute.is_some()`, which Swans never has. Instrumenting both
//! arms confirms it — Swans and Nefarious Lich print `Template`, and `Resolved`
//! never fires.
//!
//! New Way Forward is the only card in the pool whose prevention rider draws, and
//! its rider is never installed (it parses as `SequentialSibling` rather than
//! `ContinuationStep`, which is what `prevent_damage.rs` requires). So the
//! `Resolved` -> `Effect::Draw` edge is **type-reachable but not
//! production-reachable**: no card exercises it today. Test 3 pins that, and goes
//! red the moment it stops being true.
//!
//! CR 121.2 + CR 614.6 + CR 615.1 + CR 615.5 + CR 119.3.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Swans of Bryn Argoll's Oracle text, verified against the card pool
/// (`data/card-data.json`) rather than recalled. It parses to a `DamageDone`
/// replacement with `shield_kind: Prevention { amount: All }` whose execute is
/// `Draw { count: Ref(EventContextAmount), target: PostReplacementSourceController }`.
const SWANS_TEXT: &str = "Flying\n\
     If a source would deal damage to this creature, prevent that damage. \
     The source's controller draws cards equal to the damage prevented this way.";

fn library_len(state: &engine::types::game_state::GameState, player: PlayerId) -> usize {
    state.players[player.0 as usize].library.len()
}

/// Advance to the declare-blockers prompt, passing any priority window on the
/// way (CR 508.2 opens one after attackers are declared).
///
/// Panics rather than returning if the prompt never arrives. Silently falling
/// through would let the attacker go unblocked, deal no damage to Swans, fire no
/// prevention — and the draw assertions would then be measuring nothing.
fn advance_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority before blockers must succeed");
            }
            ref other => panic!("expected DeclareBlockers or Priority, got {other:?}"),
        }
    }
    panic!("never reached the DeclareBlockers prompt");
}

/// CR 615.1 + CR 615.5 + CR 121.2: Swans of Bryn Argoll prevents damage dealt to
/// it, and the prevention's *additional effect* (CR 615.5 — "which may refer to
/// the amount of damage that was prevented") draws that many cards for **the
/// source's controller**.
///
/// The rider is stashed by `replacement.rs::apply_single_replacement` as a
/// `PostReplacementContinuation::Template` (see the "Coverage boundary" section
/// at the top of this file — a static parsed ability has no `runtime_execute`, so
/// it does NOT take the batched `fire_combat_prevention_riders` path) and drained
/// by `apply_pending_post_replacement_effect`, which runs `Effect::Draw` as a
/// general (non-draw-owned) post-replacement continuation.
///
/// # Self-scope (issue #5652, FIXED).
///
/// Swans' Oracle text scopes its shield to damage dealt **to this creature**.
/// The parser now sets `valid_card: SelfRef` for self-scoped damage shields,
/// which `replacement.rs` applies against `ProposedEvent::Damage`'s recipient
/// (`affected_object_id`), so the shield fires only for damage dealt TO Swans —
/// not for the damage Swans *deals*. The blocker takes Swans' full 4 and P0
/// draws nothing (see the two assertions at the end of this test).
///
/// What the test pins that is genuinely correct, and that the rewrite must keep:
///   * **who draws** — P1, the *source's* controller, for the damage dealt to
///     Swans. A `Controller`-projection instead of
///     `PostReplacementSourceController` would give those cards to P0.
///   * **how many** — exactly the damage prevented, carried as
///     `EventContextAmount` off `state.last_effect_count`. Lose that stamping and
///     the counts collapse to 0 or 1.
///   * **that a prevention rider reaches `Effect::Draw` at all** — the G→D edge.
#[test]
fn swans_prevented_damage_draws_that_many_for_the_sources_controller() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Both libraries need enough cards that the draw is real and cannot be
    // confused with a draw-from-empty loss (CR 704.5b).
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Card A", "Card B", "Card C", "Card D", "Card E"]);
    }

    // Swans of Bryn Argoll is a 4/3 (verified against the pool).
    let swans = scenario
        .add_creature_from_oracle(P0, "Swans of Bryn Argoll", 4, 3, SWANS_TEXT)
        .id();

    // P1's blocker is a 3/5 with reach: Swans has flying, so only a flying or
    // reach creature may block it (CR 509.1b). It deals 3 damage to Swans (all
    // prevented) and survives Swans' 4, so no death/SBA noise enters the
    // assertions.
    let blocker = scenario
        .add_creature_from_oracle(P1, "Canopy Sentinel", 3, 5, "Reach")
        .id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(swans, AttackTarget::Player(P1))])
        .expect("P0 attacks with Swans");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, swans)])
        .expect("P1 blocks Swans with the 3/5");

    let p1_library_before = library_len(runner.state(), P1);

    // `combat_damage()` baselines hands and life at the call, so the deltas
    // below are exactly the combat-damage step's effect.
    let outcome = runner.combat_damage();

    // ── CORRECT: the blocker's 3 damage to Swans is prevented, and the SOURCE's
    //    controller (P1) draws that many. This is the G→D edge itself.
    assert_eq!(
        outcome.hand_drawn(P1),
        3,
        "P1 controls the damage source (the 3/5 blocker), so CR 615.5's additional \
         effect draws 3 cards — the damage prevented — for P1. got {}",
        outcome.hand_drawn(P1)
    );

    // ── How many: exactly the prevented amount (EventContextAmount = 3). ──
    assert_eq!(
        library_len(runner.state(), P1),
        p1_library_before - 3,
        "P1's library must lose exactly 3 cards — the draw count is the prevented \
         damage (CR 615.5), carried as EventContextAmount"
    );

    // ── The damage to Swans really was prevented (CR 615.1). ──
    assert_eq!(
        runner.state().objects[&swans].damage_marked,
        0,
        "Swans must have 0 marked damage: its shield prevents damage dealt to it"
    );
    assert_eq!(
        outcome.zone_of(swans),
        Zone::Battlefield,
        "Swans (4/3) survives — the 3 damage was prevented, not merely sub-lethal"
    );

    // ── #5652 FIXED: the shield is now scoped to damage dealt TO Swans
    //    (`valid_card: SelfRef`), so it no longer intercepts the 4 damage Swans
    //    DEALS to the blocker. The blocker takes the full 4, and no draw fires
    //    for Swans' own damage — P0 draws nothing. ─────────────────────────────
    assert_eq!(
        outcome.hand_drawn(P0),
        0,
        "P0 must draw 0: Swans' self-scoped shield only prevents damage dealt TO \
         Swans, so no prevention rider fires for the damage Swans deals (#5652)"
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        4,
        "the blocker takes Swans' full 4 combat damage — the self-scoped shield \
         no longer prevents damage Swans deals (#5652)"
    );
}

/// CR 614.6 + CR 119.3 + CR 121.2: Nefarious Lich replaces life gain with an
/// equal-sized draw. "If an event is replaced, it never happens" — so the life
/// total is NOT adjusted (CR 119.3 never runs), and the controller draws that
/// many cards instead.
///
/// This is the `PostReplacementContinuation::Template` witness: the substitution
/// is stashed by `replacement.rs::apply_single_replacement` and drained by the
/// same `apply_pending_post_replacement_effect` seam, again running `Effect::Draw`
/// as a general post-replacement continuation.
///
/// The life-unchanged assertion is the discriminating one: a substitution that
/// *added* a draw rather than *replacing* the gain would leave life at +5 and
/// still draw 5.
#[test]
fn nefarious_lich_replaces_life_gain_with_an_equal_draw() {
    let Some(db) = load_db() else {
        return;
    };
    nefarious_lich_case(db);
}

fn nefarious_lich_case(db: &CardDatabase) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Real card, real type line (an enchantment) — `add_real_card` panics if the
    // card is missing from the fixture DB, so a dropped fixture row fails loudly
    // rather than silently skipping this pin.
    scenario.add_real_card(P0, "Nefarious Lich", Zone::Battlefield, db);

    // The draw must have somewhere to come from; 8 cards comfortably covers the 5.
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8"]);
    }

    // A plain "You gain 5 life." sorcery, parsed from Oracle text through the
    // real parser — this drives a genuine GainLife event (unlike DebugAction::
    // SetLife, which writes the life total directly and would never be seen by
    // the replacement).
    let blessing = scenario
        .add_spell_to_hand_from_oracle(P0, "Sanguine Blessing", false, "You gain 5 life.")
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();

    let life_before = runner.life(P0);
    let library_before = library_len(runner.state(), P0);

    let outcome = runner.cast(blessing).resolve();

    // ── The life gain never happened (CR 614.6 -> CR 119.3 never runs). ──
    assert_eq!(
        outcome.life_delta(P0),
        0,
        "Nefarious Lich REPLACES the life gain (CR 614.6: a replaced event never \
         happens), so P0's life total must be unchanged. life_before={life_before}, \
         after={}",
        runner.life(P0)
    );

    // ── An equal-sized draw happened instead. ──
    assert_eq!(
        outcome.hand_drawn(P0),
        5,
        "the replacement's execute draws that many cards — 5, the life that would \
         have been gained (EventContextAmount)"
    );
    assert_eq!(
        library_len(runner.state(), P0),
        library_before - 5,
        "P0's library must lose exactly the 5 cards drawn"
    );
}

/// New Way Forward — the only card in the pool that could reach the `Resolved`
/// arm with a Draw. It does not, today. This test pins that.
///
/// Oracle (pool-verified): "The next time a source of your choice would deal
/// damage to you this turn, prevent that damage. When damage is prevented this
/// way, New Way Forward deals that much damage to that source's controller and
/// you draw that many cards."
///
/// # What runs, and what silently does not
///
/// The prevention half works: the chosen source's combat damage is prevented
/// (CR 615.1). The CR 615.5 rider half — deal that much back, draw that many —
/// never fires.
///
/// `effects/prevent_damage.rs` installs a shield's rider as its `runtime_execute`
/// **only** when the sub-ability is linked `SubAbilityLink::ContinuationStep`.
/// New Way Forward's rider chain (`DealDamage` -> `Draw`) parses as
/// `SequentialSibling` — an independent instruction — so it is never installed.
/// `combat_damage.rs::fire_combat_prevention_riders` then finds the shield in the
/// prevention tally (3 damage prevented, definition found) but bails on
/// `repl_def.runtime_execute.clone()` returning `None`. Verified by instrumenting
/// that loop: it prints `has_runtime=false`.
///
/// The consequence for Plan 03 is the point of this test. `Resolved` is built
/// only from a `runtime_execute`; parsed static shields never carry one (they
/// take `Template` — see the file header); and New Way Forward is the ONLY pool
/// card whose prevention rider draws. So **the `Resolved` -> `Effect::Draw` edge
/// has no working production witness at all**. It is type-reachable, not
/// production-reachable. Anyone rewriting `apply_pending_post_replacement_effect`
/// should know that its `Resolved` arm is, for draws, currently dead in practice.
///
/// The BUG-PIN assertions below encode today's (wrong) zeroes (issue #5658).
/// Fixing the
/// `sub_link` classification SHOULD turn them red — at which point this test
/// becomes the real `Resolved`->`Draw` pin, and the two marked values become 3.
#[test]
fn new_way_forward_prevents_damage_but_its_draw_rider_never_fires() {
    let Some(db) = load_db() else {
        return;
    };

    // Control first: the SAME combat with no shield. It proves the attack really
    // connects for 3, so the "P1 took no damage" assertion below is measuring a
    // prevention rather than an attack that silently never happened.
    let control = new_way_forward_combat(db, Shield::NotCast);
    assert_eq!(
        control.p1_life_delta, -3,
        "control: with no shield, the 3/3 attacker's combat damage must reach P1. \
         If this is 0 the scenario is broken and the shielded case proves nothing."
    );

    let shielded = new_way_forward_combat(db, Shield::Cast);

    // ── The prevention half works (CR 615.1). Non-vacuous: the control above
    //    shows this same attack otherwise deals 3.
    assert_eq!(
        shielded.p1_life_delta, 0,
        "New Way Forward prevents the chosen source's damage: P1 takes none"
    );

    // ── BUG-PIN — issue #5658: the CR 615.5 rider never fires. ──
    // CORRECT: p0_life_delta == -3 (New Way Forward deals the prevented amount
    // back to the source's controller) and p1_library_delta == -3 (P1 draws that
    // many). Both are 0 because the rider parsed as `SequentialSibling` and was
    // never installed as the shield's `runtime_execute`.
    assert_eq!(
        shielded.p0_life_delta, 0,
        "BUG-PIN (#5658): the rider does not deal the prevented damage back to \
         the source's controller. Correct value is -3. Fixing #5658 flips this."
    );
    assert_eq!(
        shielded.p1_library_delta, 0,
        "BUG-PIN (#5658): the rider does not draw. Correct value is -3 — and this \
         is the `Resolved`->`Effect::Draw` edge, which therefore has NO working \
         production witness today. Fixing #5658 makes this test the real \
         Resolved->Draw pin."
    );
}

/// Whether the New Way Forward scenario casts the shield spell.
enum Shield {
    Cast,
    NotCast,
}

struct CombatDeltas {
    p0_life_delta: i32,
    p1_life_delta: i32,
    p1_library_delta: i64,
}

/// P0 (the active player) attacks P1 with a 3/3. With `Shield::Cast`, P1 first
/// casts New Way Forward and names that attacker as the damage source.
///
/// The roles are this way round because New Way Forward protects *its
/// controller*: the shield has to sit with the player the damage is aimed at, and
/// only the non-active player can be that. Do NOT instead rewrite
/// `active_player` after `build()` to make P1 attack — that desynchronises
/// combat, the attack never happens, and every "took no damage" assertion then
/// passes while measuring nothing.
fn new_way_forward_combat(db: &CardDatabase, shield: Shield) -> CombatDeltas {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8"]);
    }

    let attacker = scenario.add_creature(P0, "Charging Badger", 3, 3).id();

    // New Way Forward is {2}{U}{R}{W}: 2 generic + one each of U/R/W.
    let nwf = scenario.add_real_card(P1, "New Way Forward", Zone::Hand, db);
    scenario.with_mana_pool(
        P1,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    if let Shield::Cast = shield {
        // Hand P1 the priority window to cast the instant on P0's turn, and
        // nothing else. `waiting_for` is the engine's authority: set only
        // `priority_player` and the cast is rejected with `NotYourPriority`.
        runner.state_mut().priority_player = P1;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

        let cast = runner.cast(nwf).resolve();
        assert!(
            matches!(
                cast.final_waiting_for(),
                WaitingFor::DamageSourceChoice { .. }
            ),
            "New Way Forward must prompt for the damage source (CR 609.7a), got {:?}",
            cast.final_waiting_for()
        );
        runner
            .act(GameAction::ChooseDamageSource { source: attacker })
            .expect("choose P0's attacker as the damage source");
    }

    let p0_life_before = runner.life(P0);
    let p1_life_before = runner.life(P1);
    let p1_library_before = library_len(runner.state(), P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("P0 attacks P1");
    runner.combat_damage();
    runner.advance_until_stack_empty();

    CombatDeltas {
        p0_life_delta: runner.life(P0) - p0_life_before,
        p1_life_delta: runner.life(P1) - p1_life_before,
        p1_library_delta: library_len(runner.state(), P1) as i64 - p1_library_before as i64,
    }
}

/// END-TO-END regression witness for the `install(KeepResident)` guard.
///
/// This is the full path the unit tests in `drain_stack_reentrancy_tests` model at
/// the API level, driven through real cards and the real pipeline:
///
/// ```text
/// combat damage -> Swans prevention -> apply_pending_post_replacement_effect
///   -> begin_dispatch            (Swans' drain is now resident + DISPATCHING)
///   -> Swans' continuation DRAWS for the damage source's controller (P1)
///     -> draw_through_replacement -> replace_event
///       -> Jace, Wielder of Mysteries matches (P1's library is empty)
///         -> apply_single_replacement stashes its MANDATORY post-effect (WinTheGame)
///           -> install(KeepResident)   <-- the seam under test
/// ```
///
/// The stash arrives while Swans' drain is resident but **already dispatching**.
/// Guarding the install on `!drains.is_empty()` drops it, `draw_through_replacement`
/// (which gates its drain on `has_ready()`) then never runs it, and **P1 never wins**.
/// The predecessor slot installed it, because the continuation had been moved out of
/// the slot before dispatching and the slot read empty.
///
/// Jace's replacement is `mode: Mandatory` with `execute: WinTheGame` — verified from
/// the card data, not recalled. It is exactly the "nested mandatory post-effect" class
/// the fix exists for.
///
/// CR 616.1g + CR 615.5 + CR 104.2a.
#[test]
fn nested_mandatory_post_effect_runs_when_a_dispatching_continuation_draws() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P0 draws normally; only P1's library is empty, so only P1's draw is replaced.
    scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D", "Card E"]);

    let swans = scenario
        .add_creature_from_oracle(P0, "Swans of Bryn Argoll", 4, 3, SWANS_TEXT)
        .id();

    // P1 blocks with a reach creature (Swans has flying). Its damage to Swans is
    // prevented, and Swans' CR 615.5 rider draws that many for the SOURCE's
    // controller — P1.
    let blocker = scenario
        .add_creature_from_oracle(P1, "Canopy Sentinel", 3, 5, "Reach")
        .id();

    // P1's library is EMPTY and P1 controls Jace, so P1's very first drawn card is
    // replaced by "you win the game instead".
    scenario.add_real_card(P1, "Jace, Wielder of Mysteries", Zone::Battlefield, db);

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(swans, AttackTarget::Player(P1))])
        .expect("P0 attacks with Swans");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, swans)])
        .expect("P1 blocks Swans with the 3/5 reach creature");

    assert!(
        runner.state().players[1].library.is_empty(),
        "precondition: P1's library must be empty, or Jace's replacement never matches \
         and this test proves nothing"
    );

    runner.combat_damage();
    runner.advance_until_stack_empty();

    // The nested mandatory post-effect ran: P1 won.
    //
    // Before the `has_ready()` fix this assertion failed — the stash arrived while
    // Swans' drain was Dispatching, `install(KeepResident)` dropped it on
    // `!drains.is_empty()`, and WinTheGame never executed.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1)),
                ..
            }
        ),
        "P1 must win: Swans' prevention rider draws for P1, P1's library is empty, and \
         Jace, Wielder of Mysteries replaces that draw with a MANDATORY WinTheGame. \
         Dropping the re-entrant stash strands that post-effect and P1 never wins — \
         exactly the regression the has_ready() guard fixes. got {:?}",
        runner.state().waiting_for
    );
}
