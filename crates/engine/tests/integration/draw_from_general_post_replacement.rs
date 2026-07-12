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
//! These two tests exist so that edge cannot change silently. They encode what
//! the engine does TODAY, on real cards, through the real parser and the real
//! pipeline. They are not a specification of the new design; they are the
//! before-picture the rewrite must reproduce.
//!
//! Both witnesses reach `Effect::Draw` through
//! `engine_replacement::apply_pending_post_replacement_effect`, whose two arms
//! (`Resolved` / `Template`) are the seam Plan 03 rewrites. Neither arm filters on
//! effect kind, which is exactly why `Effect::Draw` can arrive here at all.
//!
//!  1. **Swans of Bryn Argoll** — a *prevention rider* (CR 615.5): the damage is
//!     prevented and the rider draws that many cards for the source's controller.
//!     The count rides `EventContextAmount`.
//!
//!  2. **Nefarious Lich** — an ordinary *substitution* (CR 614.6): the life gain
//!     never happens and an equal-sized draw happens instead.
//!
//! # Coverage boundary: both of these take the `Template` arm
//!
//! Verified by instrumenting both arms and running these tests: each prints
//! `Template`, and `Resolved` never fires. A static permanent ability parsed from
//! Oracle text carries only an `execute` AST — never a `runtime_execute` — and
//! `replacement.rs::apply_single_replacement` builds `Resolved` *only* from
//! `runtime_execute` (the `batched_combat_all_shield` branch requires it). So
//! Swans, despite being a combat prevention rider, does **not** reach
//! `combat_damage.rs::fire_combat_prevention_riders`.
//!
//! The `Resolved` arm is built by resolving *spells* that install a shield with a
//! runtime rider (`effects/prevent_damage.rs`). The only card in the pool whose
//! prevention rider then draws is **New Way Forward** ("When damage is prevented
//! this way, ... you draw that many cards"). It is the genuine `Resolved`→`Draw`
//! witness and is **not** pinned here — a gap the rewrite should close.
//!
//! CR 121.2 + CR 614.6 + CR 615.1 + CR 615.5 + CR 119.3.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
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
                    .act(engine::types::actions::GameAction::PassPriority)
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
/// # This test pins a KNOWN BUG. Read before "fixing" it.
///
/// Swans' Oracle text scopes its shield to damage dealt **to this creature**.
/// `DamageTargetFilter` has no variant that can express that (`CreatureOnly`,
/// `Player`, `PlayerOrPermanentsControlledBy` — none mean "the host object"), so
/// the parser leaves `damage_target_filter: None`, and `replacement.rs`'s
/// `if let Some(ref tf) = repl_def.damage_target_filter` then applies **no target
/// restriction at all**. The shield therefore also fires on the damage Swans
/// *deals*: the blocker takes 0 damage and P0 draws 4.
///
/// Correct behaviour is `hand_drawn(P0) == 0` and `blocker.damage_marked == 4`.
/// The assertions below deliberately encode the CURRENT (wrong) values, because
/// Plan 03's job is to rewrite the draw-delivery seam **without changing
/// observable behaviour** — a pin that encoded the fix would fail for the entire
/// rewrite and tell us nothing. When the scoping bug is fixed, this test SHOULD
/// go red; flip the two marked assertions then, and delete this section.
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

    // ── BUG-PIN (see the "KNOWN BUG" section on this test) ────────────────────
    // Swans' shield has `damage_target_filter: None`, so it is not scoped to
    // damage dealt TO Swans. It also intercepts the 4 damage Swans DEALS to the
    // blocker: the blocker takes none, and the rider draws for that damage's
    // source's controller — P0.
    //
    // CORRECT: hand_drawn(P0) == 0 and blocker damage_marked == 4.
    // These two assertions encode today's wrong values on purpose, so the Plan 03
    // rewrite cannot change them silently. Fixing the scoping SHOULD break them.
    assert_eq!(
        outcome.hand_drawn(P0),
        4,
        "BUG-PIN: P0 draws 4 because Swans' unscoped shield also prevents the \
         damage Swans deals. Correct value is 0 — Swans is not the source's \
         controller for any damage dealt TO it."
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        0,
        "BUG-PIN: the blocker takes 0 of Swans' 4 damage because the unscoped \
         shield prevented it. Correct value is 4."
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
