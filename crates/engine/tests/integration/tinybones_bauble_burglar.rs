//! Tinybones, Bauble Burglar — the counter-linked static exile-play permission.
//!
//! Verbatim Oracle (Scryfall, Foundations): "Whenever an opponent discards a
//! card, exile it from their graveyard with a stash counter on it. / During
//! your turn, you may play cards you don't own with stash counters on them
//! from exile, and mana of any type can be spent to cast those spells. /
//! {3}{B}, {T}: Each opponent discards a card. Activate only as a sorcery."
//!
//! The static lowers to `StaticMode::ExileCastPermission` over the
//! counter-keyed, source-agnostic `ExileCardPool::CounterLinked` pool with the
//! any-TYPE spend concession. Rows: V-trigger-e2e (discard → stash → cast
//! through the production pipeline), V-cast-spell (off-color + {C} payment,
//! paired refusals, elected-authority hostile), V-land (main-phase-only play),
//! V-no-flash (normal timing enforced), V-ruling1 (source-agnostic pool across
//! two Tinybones, opponent's copy grants nothing, Evelyn cross-talk,
//! controller-change follows the current controller).
//!
//! Staging note: rows other than V-trigger-e2e stage the stash counter
//! directly (`stage_stash`) instead of re-driving the discard trigger. The
//! trigger → exile-with-counter production writer is proven by V-trigger-e2e;
//! every staged row tests a downstream gate (pool, ownership, timing, grantee,
//! payment) that reads only the counter's presence.

use std::sync::Arc;

use engine::ai_support::legal_actions;
use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::{add_to_zone, create_object, remove_from_zone};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    CardPlayMode, CastingPermission, Duration, Effect, PlayFromExileProvenance,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastingVariant, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::{EtbTapState, Zone};

/// Verbatim Oracle text (see module docs).
const TINYBONES: &str = "Whenever an opponent discards a card, exile it from their graveyard with a stash counter on it.\nDuring your turn, you may play cards you don't own with stash counters on them from exile, and mana of any type can be spent to cast those spells.\n{3}{B}, {T}: Each opponent discards a card. Activate only as a sorcery.";

/// Evelyn, the Covetous — verbatim Oracle (sibling counter permission; the
/// collection marker must not cross-talk with the stash pool).
const EVELYN: &str = "Flash\nWhenever Evelyn or another Vampire you control enters, exile the top card of each player's library with a collection counter on it.\nOnce each turn, you may play a card from exile with a collection counter on it if it was exiled by an ability you controlled, and you may spend mana as though it were mana of any color to cast it.";

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn stash_counter() -> CounterType {
    CounterType::Generic("stash".to_string())
}

/// Stage `id` as stashed: the counter record the Tinybones trigger's exile
/// resolver writes (CR 122.1 + CR 406.2). See the module docs for why staging
/// is sound outside V-trigger-e2e.
fn stage_stash(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .counters
        .insert(stash_counter(), 1);
}

fn stage_counter(runner: &mut GameRunner, id: ObjectId, name: &str) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .counters
        .insert(CounterType::Generic(name.to_string()), 1);
}

/// Fund `player`'s pool directly (mana sourcing is covered elsewhere); each
/// cast below still travels the full announce/pay pipeline.
fn add_mana(runner: &mut GameRunner, player: PlayerId, color: ManaType, count: usize) {
    for _ in 0..count {
        let unit = ManaUnit::new(color, ObjectId(0), false, vec![]);
        runner.state_mut().players[player.0 as usize]
            .mana_pool
            .add(unit);
    }
}

fn pool_is_empty(runner: &GameRunner, player: PlayerId) -> bool {
    runner.state().players[player.0 as usize]
        .mana_pool
        .is_empty()
}

fn set_mana_cost(runner: &mut GameRunner, id: ObjectId, shards: Vec<ManaCostShard>, generic: u32) {
    runner.state_mut().objects.get_mut(&id).unwrap().mana_cost = ManaCost::Cost { shards, generic };
}

fn can_play_land(runner: &GameRunner, id: ObjectId) -> bool {
    legal_actions(runner.state())
        .iter()
        .any(|action| matches!(action, GameAction::PlayLand { object_id, .. } if *object_id == id))
}

/// Index of Tinybones's "{3}{B}, {T}: Each opponent discards a card"
/// activation on `source`.
fn tinybones_discard_index(runner: &GameRunner, source: ObjectId) -> usize {
    runner.state().objects[&source]
        .abilities
        .iter()
        .position(|ability| matches!(ability.effect.as_ref(), Effect::Discard { .. }))
        .expect("Tinybones must expose its discard activation")
}

fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Pass priority until the stack is empty (bounded). No-op when the driver
/// already resolved everything.
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to drain the stack");
    }
}

fn assert_priority_empty_stack(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "the stack must be empty at the completion boundary"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected a Priority window at the completion boundary, got {:?}",
        runner.state().waiting_for
    );
}

/// Install a second Tinybones mid-game carrying only the parsed static (its
/// trigger/activation are unneeded for the pool-membership claim). Mirrors
/// `evelyn_regression.rs`'s mid-game static install.
fn install_tinybones_static(state: &mut GameState, card_id: u64) -> ObjectId {
    let parsed = parse_oracle_text(
        TINYBONES,
        "Tinybones, Bauble Burglar",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(
        !parsed.statics.is_empty(),
        "Tinybones must parse a static to install"
    );
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Tinybones, Bauble Burglar".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    for static_def in parsed.statics {
        obj.static_definitions.push(static_def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(static_def);
    }
    id
}

/// V-trigger-e2e: the primary production flow — activate Tinybones → the lone
/// opponent discards (auto-resolves with a one-card hand) → the trigger exiles
/// it from their graveyard with a stash counter → P0 casts it from exile at
/// its on-color cost. This is the production writer every staged row relies on.
#[test]
fn tinybones_discard_trigger_stashes_then_casts_from_exile() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P/T is not under test.
    let tinybones = scenario
        .add_creature_from_oracle(P0, "Tinybones, Bauble Burglar", 2, 2, TINYBONES)
        .id();
    // P1's only card: a {1}{B} instant, so the discard auto-resolves.
    let bolt = scenario.add_spell_to_hand(P1, "Borrowed Bolt", true).id();
    let mut runner = scenario.build();
    set_mana_cost(&mut runner, bolt, vec![ManaCostShard::Black], 1);

    // Exact {3}{B} for the activation, staged apart from the cast's mana so no
    // allocation order between the two payments can matter.
    add_mana(&mut runner, P0, ManaType::Colorless, 3);
    add_mana(&mut runner, P0, ManaType::Black, 1);

    let index = tinybones_discard_index(&runner, tinybones);
    runner.activate(tinybones, index).resolve();
    drain_stack(&mut runner);
    assert!(
        pool_is_empty(&runner, P0),
        "the activation must have spent its exact funding"
    );

    // Reach-guard: the counter object is observed in exile pre-cast.
    assert_eq!(zone(&runner, bolt), Zone::Exile);
    assert_eq!(
        runner.state().objects[&bolt].counters.get(&stash_counter()),
        Some(&1),
        "the trigger must exile the discard with a stash counter"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&bolt),
        "the stashed opponent card must be offered on your turn"
    );

    // Exact {1}{B} for the on-color cast.
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    add_mana(&mut runner, P0, ManaType::Black, 1);
    runner.cast(bolt).resolve();

    assert_eq!(
        zone(&runner, bolt),
        Zone::Graveyard,
        "the spell was cast from exile and resolved"
    );
    assert!(
        pool_is_empty(&runner, P0),
        "the cast must have spent its exact funding"
    );
    assert_priority_empty_stack(&runner);
}

/// V-cast-spell (payment): a {1}{B} spell is payable from a green-only pool
/// (off-color via the concession), a {C} spell is payable from green mana
/// (any-TYPE, CR 106.1b + CR 118.14 — colored mana pays colorless), and the
/// same {1}{B} shape casts on-color. Each leg drains its exact funding, so a
/// free-cast bug cannot hide behind a success.
#[test]
fn tinybones_any_type_concession_pays_off_color_and_colorless_costs() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Tinybones, Bauble Burglar", 2, 2, TINYBONES);
    let off_color = scenario.add_spell_to_exile(P1, "Off-Color Bolt", true).id();
    let colorless = scenario.add_spell_to_exile(P1, "Colorless Bolt", true).id();
    let on_color = scenario.add_spell_to_exile(P1, "On-Color Bolt", true).id();
    let mut runner = scenario.build();
    set_mana_cost(&mut runner, off_color, vec![ManaCostShard::Black], 1);
    set_mana_cost(&mut runner, colorless, vec![ManaCostShard::Colorless], 0);
    set_mana_cost(&mut runner, on_color, vec![ManaCostShard::Black], 1);
    stage_stash(&mut runner, off_color);
    stage_stash(&mut runner, colorless);
    stage_stash(&mut runner, on_color);

    for spell in [off_color, colorless, on_color] {
        assert!(
            spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
            "every stashed opponent spell must be offered"
        );
    }

    // Off-color: {1}{B} from exactly 2 green — the black pip can only be paid
    // through the any-type concession.
    add_mana(&mut runner, P0, ManaType::Green, 2);
    runner.cast(off_color).resolve();
    assert_eq!(zone(&runner, off_color), Zone::Graveyard);
    assert!(pool_is_empty(&runner, P0));

    // Any-TYPE probe: {C} from 1 green. Under an any-COLOR concession this
    // payment would fail (CR 106.1b: colorless is a type, not a color).
    add_mana(&mut runner, P0, ManaType::Green, 1);
    runner.cast(colorless).resolve();
    assert_eq!(zone(&runner, colorless), Zone::Graveyard);
    assert!(pool_is_empty(&runner, P0));

    // On-color reach-guard: the same {1}{B} shape casts for its exact cost.
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    add_mana(&mut runner, P0, ManaType::Black, 1);
    runner.cast(on_color).resolve();
    assert_eq!(zone(&runner, on_color), Zone::Graveyard);
    assert!(pool_is_empty(&runner, P0));

    assert_priority_empty_stack(&runner);
}

/// V-cast-spell (refusals): off-turn, unstashed, and own-card casts are each
/// refused while the paired positive in the same test casts. The off-turn leg
/// isolates the YourTurnOnly gate (instant timing holds on the opponent's turn
/// with priority); the other legs isolate pool membership and ownership.
#[test]
fn tinybones_refuses_off_turn_unstashed_and_own_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Tinybones, Bauble Burglar", 2, 2, TINYBONES);
    scenario.with_library_top(P1, &["Filler A", "Filler B"]);
    let positive = scenario.add_spell_to_exile(P1, "Stashed Bolt", true).id();
    let unstashed = scenario.add_spell_to_exile(P1, "Unstashed Bolt", true).id();
    let own = scenario
        .add_spell_to_exile(P0, "Own Stashed Bolt", true)
        .id();
    let off_turn = scenario.add_spell_to_exile(P1, "Off-Turn Bolt", true).id();
    let mut runner = scenario.build();
    stage_stash(&mut runner, positive);
    stage_stash(&mut runner, own);
    stage_stash(&mut runner, off_turn);

    // Discovery trio on your turn: only the stashed don't-own card is offered.
    let offered = spell_objects_available_to_cast(runner.state(), P0);
    assert!(offered.contains(&positive));
    assert!(
        !offered.contains(&unstashed),
        "a card without a stash counter must not be offered"
    );
    assert!(
        !offered.contains(&own),
        "a stashed card you own must not be offered"
    );

    // Production-attempt refusals: the reducer rejects each cast and the card
    // never leaves exile.
    for refused in [unstashed, own] {
        assert!(
            runner.cast(refused).try_resolve().is_err(),
            "the unpermitted cast must be refused"
        );
        assert_eq!(zone(&runner, refused), Zone::Exile);
    }

    // Paired positive in the same test.
    runner.cast(positive).resolve();
    assert_eq!(zone(&runner, positive), Zone::Graveyard);

    // Off-turn leg: P1's turn, P0 holding priority. Instant timing holds, so
    // only the YourTurnOnly gate refuses. Production priority passing to P1's
    // turn (mirrors `burden_of_proof.rs`): Tinybones can attack, so the
    // DeclareAttackers window opens and must be passed explicitly — a bare
    // `advance_to_end_step` stalls there and the turn never passes.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("declaring no attackers is legal");
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P1, "reach-guard: P1's turn");
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "reach-guard: P1's main phase"
    );
    grant_priority(&mut runner, P0);
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&off_turn),
        "the stash permission must not offer off-turn"
    );
    assert!(
        runner.cast(off_turn).try_resolve().is_err(),
        "the off-turn cast must be refused"
    );
    assert_eq!(zone(&runner, off_turn), Zone::Exile);
    assert!(runner.state().stack.is_empty());
}

/// Stage a Lara-Croft-class object-attached grant ("you may play that card
/// this turn", no mana concession) on an exiled card: the competing authority
/// for the election hostile below.
fn stage_lara_class_grant(runner: &mut GameRunner, id: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&id)
        .unwrap()
        .casting_permissions
        .push(CastingPermission::PlayFromExile {
            provenance: PlayFromExileProvenance::Impulse,
            duration: Duration::UntilEndOfTurn,
            granted_to: P0,
            mode: CardPlayMode::Play,
            frequency: CastFrequency::Unlimited,
            source_id: None,
            invalidation: None,
            exiled_by_ability_controller: None,
            mana_spend_permission: None,
            card_filter: None,
            single_use_group: None,
            single_use: false,
            cast_cost_modifier: None,
            alt_ability_cost: None,
            without_paying_mana_cost: false,
            land_enter_tapped: EtbTapState::Unspecified,
        });
}

/// V-cast-spell (elected-authority hostile, CR 118.14): a stashed card covered
/// by BOTH a concession-free object grant (Lara class) and Tinybones's static
/// commits through the static path — the stack entry carries
/// `CastingVariant::ExilePermission { tinybones, Unlimited }`, so payment reads
/// the elected static's concession, not a rescan. No `CastingVariantChoice` is
/// forced: object grants are not cast candidates, so the engine never prompts
/// between the two authorities and the election is observed on the committed
/// spell instead. Funding is exact on-color {1}{B} (pool drained exactly):
/// off-color funding cannot drive this overlap because the variant-blind
/// pre-announcement feasibility check prices the static option under the
/// grant's concession-free terms and drops it before election — the sibling
/// payment test proves the concession's off-color breadth through the
/// sole-authority route. The no-Tinybones runner below proves the authorities
/// differ (the object path alone cannot pay off-color).
#[test]
fn tinybones_elected_static_authority_pays_under_its_concession() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tinybones = scenario
        .add_creature_from_oracle(P0, "Tinybones, Bauble Burglar", 2, 2, TINYBONES)
        .id();
    let contested = scenario.add_spell_to_exile(P1, "Contested Bolt", true).id();
    let mut runner = scenario.build();
    set_mana_cost(&mut runner, contested, vec![ManaCostShard::Black], 1);
    stage_stash(&mut runner, contested);
    stage_lara_class_grant(&mut runner, contested);

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&contested),
        "reach-guard: the contested card is authorized"
    );
    // Exact on-color {1}{B}: payable under either authority's terms, so the
    // static option survives feasibility and the natural single-candidate
    // pre-election commits it.
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    add_mana(&mut runner, P0, ManaType::Black, 1);
    let commit = runner.cast(contested).commit();
    assert!(
        matches!(
            commit
                .state()
                .stack
                .iter()
                .find(|entry| entry.id == contested)
                .map(|entry| &entry.kind),
            Some(StackEntryKind::Spell {
                casting_variant: CastingVariant::ExilePermission { source, frequency },
                ..
            }) if *source == tinybones && *frequency == CastFrequency::Unlimited
        ),
        "the overlapping cast must commit through the static ExilePermission path"
    );
    commit.resolve();
    assert_eq!(
        zone(&runner, contested),
        Zone::Graveyard,
        "the ExilePermission election path must pay under its concession"
    );
    assert!(pool_is_empty(&runner, P0));
    assert_priority_empty_stack(&runner);

    // Paired runner without Tinybones: the object grant authorizes the card
    // (reach-guard) but cannot pay {1}{B} from green (no concession there).
    let mut solo = GameScenario::new();
    solo.at_phase(Phase::PreCombatMain);
    let solo_spell = solo.add_spell_to_exile(P1, "Solo Bolt", true).id();
    let mut solo_runner = solo.build();
    set_mana_cost(&mut solo_runner, solo_spell, vec![ManaCostShard::Black], 1);
    stage_stash(&mut solo_runner, solo_spell);
    stage_lara_class_grant(&mut solo_runner, solo_spell);
    assert!(
        spell_objects_available_to_cast(solo_runner.state(), P0).contains(&solo_spell),
        "reach-guard: the object grant alone authorizes the card"
    );
    add_mana(&mut solo_runner, P0, ManaType::Green, 2);
    assert!(
        solo_runner.cast(solo_spell).try_resolve().is_err(),
        "without the static's concession the off-color payment must fail"
    );
    assert_eq!(zone(&solo_runner, solo_spell), Zone::Exile);
}

/// V-land (CR 305.1): a stashed opponent land plays in the main phase with an
/// empty stack, and the same land is refused while the stack is non-empty —
/// the permission adds zone authority only; normal land timing still applies.
#[test]
fn tinybones_plays_stashed_opponent_land_only_at_proper_timing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Tinybones, Bauble Burglar", 2, 2, TINYBONES);
    let land = scenario.add_land_to_exile(P1, "Stashed Grove").id();
    let filler = scenario.add_spell_to_hand(P0, "Filler", false).id();
    let mut runner = scenario.build();
    stage_stash(&mut runner, land);

    // Reach-guard: the permission offers the land with an empty stack.
    assert!(
        can_play_land(&runner, land),
        "the stashed opponent land must be offered in the main phase"
    );

    // Non-empty stack: refused by normal land timing (CR 305.1).
    runner.cast(filler).commit();
    assert!(
        !can_play_land(&runner, land),
        "a land play with a non-empty stack must not be offered"
    );
    let card_id = runner.state().objects[&land].card_id;
    assert!(
        runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .is_err(),
        "the mistimed land play must be refused"
    );
    assert_eq!(zone(&runner, land), Zone::Exile);

    // Empty stack again: the same land plays through the land-play pipeline.
    runner.pass_both_players();
    assert!(runner.state().stack.is_empty());
    assert!(can_play_land(&runner, land));
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .expect("playing the stashed land must succeed");
    assert_eq!(zone(&runner, land), Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&land].controller,
        P0,
        "CR 305.1: the player who plays a land controls it"
    );
    assert_priority_empty_stack(&runner);
}

/// V-no-flash: stashed spells keep their normal timing — a sorcery is refused
/// at instant timing (non-empty stack, own main phase, permission live) while
/// a stashed instant casts in the same window, and the same sorcery casts once
/// the stack is empty.
#[test]
fn tinybones_grants_no_flash_timing_to_stashed_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Tinybones, Bauble Burglar", 2, 2, TINYBONES);
    let sorcery = scenario
        .add_spell_to_exile(P1, "Stashed Sorcery", false)
        .id();
    let instant = scenario
        .add_spell_to_exile(P1, "Stashed Instant", true)
        .id();
    let filler = scenario.add_spell_to_hand(P0, "Filler", false).id();
    let mut runner = scenario.build();
    stage_stash(&mut runner, sorcery);
    stage_stash(&mut runner, instant);

    // Non-empty stack on your turn: the permission is live but sorcery timing
    // fails. Discovery still authorizes (reach-guard: only timing refuses).
    runner.cast(filler).commit();
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&sorcery),
        "reach-guard: the permission authorizes the sorcery"
    );
    assert!(
        runner.cast(sorcery).try_resolve().is_err(),
        "a sorcery-speed stashed spell must be refused at instant timing"
    );
    assert_eq!(zone(&runner, sorcery), Zone::Exile);

    // Instant sibling casts in the same window.
    runner.cast(instant).resolve();
    assert_eq!(zone(&runner, instant), Zone::Graveyard);

    // Empty stack: the same sorcery casts at sorcery timing. The instant's
    // `.resolve()` already drained the filler too (`drive_resolution` breaks at
    // the first empty-stack Priority window), so no passes are needed — passing
    // twice with an empty stack would leave the main phase.
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "reach-guard: still in the main phase for sorcery timing"
    );
    runner.cast(sorcery).resolve();
    assert_eq!(zone(&runner, sorcery), Zone::Graveyard);
    assert_priority_empty_stack(&runner);
}

/// V-ruling1 (source-agnostic pool, CR 406.2): a card stashed by Tinybones-A's
/// trigger stays playable while only Tinybones-B is live (A removed) — pool
/// membership reads the counter, never an exile-link record. The mid-test
/// no-source leg proves liveness is still required.
#[test]
fn tinybones_pool_is_source_agnostic_across_two_tinybones() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tinybones_a = scenario
        .add_creature_from_oracle(P0, "Tinybones, Bauble Burglar", 2, 2, TINYBONES)
        .id();
    let spell = scenario.add_spell_to_hand(P1, "Shared Bolt", true).id();
    let mut runner = scenario.build();
    set_mana_cost(&mut runner, spell, vec![ManaCostShard::Black], 1);

    // Exact {3}{B} for A's activation, staged apart from the cast's mana.
    add_mana(&mut runner, P0, ManaType::Colorless, 3);
    add_mana(&mut runner, P0, ManaType::Black, 1);

    let index = tinybones_discard_index(&runner, tinybones_a);
    runner.activate(tinybones_a, index).resolve();
    drain_stack(&mut runner);
    assert!(
        pool_is_empty(&runner, P0),
        "the activation must have spent its exact funding"
    );

    // Reach-guards: A stashed it (counter present) and offers it while live.
    assert_eq!(zone(&runner, spell), Zone::Exile);
    assert_eq!(
        runner.state().objects[&spell]
            .counters
            .get(&stash_counter()),
        Some(&1)
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "reach-guard: A offers the card it stashed"
    );

    // Remove A: with no live source the card is unoffered (liveness required).
    remove_from_zone(runner.state_mut(), tinybones_a, Zone::Battlefield, P0);
    add_to_zone(runner.state_mut(), tinybones_a, Zone::Graveyard, P0);
    runner
        .state_mut()
        .objects
        .get_mut(&tinybones_a)
        .unwrap()
        .zone = Zone::Graveyard;
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "with no live source nothing may be offered"
    );

    // Only B is live: the A-stashed card is offered and casts at its cost.
    install_tinybones_static(runner.state_mut(), 1000);
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "B must offer the card A stashed"
    );
    // Exact {1}{B} for the paid cast through B.
    add_mana(&mut runner, P0, ManaType::Colorless, 1);
    add_mana(&mut runner, P0, ManaType::Black, 1);
    runner.cast(spell).resolve();
    assert_eq!(zone(&runner, spell), Zone::Graveyard);
    assert!(pool_is_empty(&runner, P0));
    assert_priority_empty_stack(&runner);
}

/// V-ruling1 (grantee gate + controller-change hostile, CR 109.5): an
/// opponent's Tinybones grants you nothing — turn, ownership, and pool all
/// pass, only the SourceController grantee refuses — and flipping the
/// controller alone flips the verdict, proving the permission follows the
/// current controller (`exile_permission_sources` gate).
#[test]
fn tinybones_grant_follows_the_current_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tinybones = scenario
        .add_creature_from_oracle(P1, "Tinybones, Bauble Burglar", 2, 2, TINYBONES)
        .id();
    let spell = scenario.add_spell_to_exile(P1, "Foreign Bolt", true).id();
    let mut runner = scenario.build();
    stage_stash(&mut runner, spell);

    // Reach-guard: the static object is present and parsed; only the grantee
    // relation can refuse below (your turn, don't-own, stashed).
    assert!(
        runner.state().objects[&tinybones]
            .static_definitions
            .iter_all()
            .any(|def| matches!(def.mode, StaticMode::ExileCastPermission { .. })),
        "reach-guard: the opponent's Tinybones carries the permission"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "an opponent's Tinybones must grant you nothing"
    );
    assert!(
        runner.cast(spell).try_resolve().is_err(),
        "the grantee-gated cast must be refused"
    );
    assert_eq!(zone(&runner, spell), Zone::Exile);

    // The ONLY change: Tinybones's controller. The refusal flips to success.
    runner
        .state_mut()
        .objects
        .get_mut(&tinybones)
        .unwrap()
        .controller = P0;
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&spell),
        "gaining control must open the permission"
    );
    runner.cast(spell).resolve();
    assert_eq!(zone(&runner, spell), Zone::Graveyard);
    assert_priority_empty_stack(&runner);
}

/// V-ruling1 (no cross-talk, CR 122.1): with Evelyn live alongside Tinybones,
/// a collection-countered card is unoffered while a stash-countered card casts
/// — the `OfType(stash)` matcher never admits another counter kind.
#[test]
fn tinybones_ignores_collection_counters_alongside_evelyn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Tinybones, Bauble Burglar", 2, 2, TINYBONES);
    scenario.add_creature_from_oracle(P0, "Evelyn, the Covetous", 2, 5, EVELYN);
    let stashed = scenario.add_spell_to_exile(P1, "Stashed Bolt", true).id();
    let collected = scenario.add_spell_to_exile(P1, "Collected Bolt", true).id();
    let mut runner = scenario.build();
    stage_stash(&mut runner, stashed);
    stage_counter(&mut runner, collected, "collection");

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&stashed),
        "the stashed card must be offered"
    );
    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&collected),
        "a collection-countered card must not enter the stash pool"
    );

    runner.cast(stashed).resolve();
    assert_eq!(zone(&runner, stashed), Zone::Graveyard);
    assert_eq!(
        zone(&runner, collected),
        Zone::Exile,
        "the collection-countered card stays exiled"
    );
    assert_priority_empty_stack(&runner);
}
