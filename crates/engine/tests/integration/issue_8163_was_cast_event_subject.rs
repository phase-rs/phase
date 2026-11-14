//! Issue #8163: a `WasCast` intervening-if reads the TRIGGER EVENT's subject —
//! live while it is still the entrant, its own `ZoneChangeRecord` last known
//! information once it is not (CR 608.2h / CR 113.7a) — and reads the trigger
//! SOURCE only when the event names no subject at all.
//!
//! Before the fix, `TriggerCondition::WasCast` fell through an unguarded `||` to
//! the trigger source whenever the event's subject failed the check. A searched-up
//! Aura has `cast_from_zone == None` (it was PUT, not cast — CR 601.2a), so
//! Light-Paws read its OWN `Some(Hand)` stamp and re-triggered on every Aura it
//! fetched, chain-searching the library dry.
//!
//! REVERT DISCRIMINATORS (three, in different directions):
//!  * `light_paws_cast_aura_fetches_exactly_one_aura` — restore the unguarded
//!    `||` and BOTH staged library Auras reach the battlefield.
//!  * `preston_creates_token_for_reanimated_creature` — the same revert fails in
//!    the OPPOSITE direction (no token at all).
//!  * `feasting_troll_king_etb_still_makes_food_after_removal_in_response` —
//!    guards the OTHER cliff: gating the source fallback on
//!    `event_object_id.is_none()` (the withdrawn round-1 design) makes this
//!    produce ZERO Food tokens, silently breaking all 61 self-referential
//!    `WasCast` cards at the CR 603.4 resolution re-check.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, Effect, TargetFilter, TriggerCondition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// ---------------------------------------------------------------------------
// Verbatim Oracle text (data/card-data.json, confirmed in this worktree)
// ---------------------------------------------------------------------------

const LIGHT_PAWS: &str = "Whenever an Aura you control enters, if you cast it, you may search your library for an Aura card with mana value less than or equal to that Aura and with a different name than each Aura you control, put that card onto the battlefield attached to Light-Paws, then shuffle.";

const FEASTING_TROLL_KING: &str = "Vigilance, trample\nWhen this creature enters, if you cast it from your hand, create three Food tokens. (They're artifacts with \"{2}, {T}, Sacrifice this token: You gain 3 life.\")\nSacrifice three Foods: Return this card from your graveyard to the battlefield. Activate only during your turn.";

const PRESTON: &str = "Whenever another nontoken creature you control enters, if it wasn't cast, create a token that's a copy of that creature, except it's a 0/1 white Illusion.\n{1}{W}, Sacrifice five Illusions: Exile target nonland permanent.";

const WILD_PAIR: &str = "Whenever a creature enters, if you cast it from your hand, you may search your library for a creature card with the same total power and toughness, put it onto the battlefield, then shuffle.";

const SEVINNES_RECLAMATION: &str = "Return target permanent card with mana value 3 or less from your graveyard to the battlefield. If this spell was cast from a graveyard, you may copy this spell and may choose a new target for the copy.\nFlashback {4}{W} (You may cast this card from your graveyard for its flashback cost. Then exile it.)";

const REANIMATE: &str = "Put target creature card from a graveyard onto the battlefield under your control. You lose life equal to that card's mana value.";

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// CR 303.4f: an Aura entering the battlefield other than by resolving as an
/// Aura spell has its host chosen as it enters.
fn answer_aura_hosts(runner: &mut GameRunner) {
    while let WaitingFor::ReturnAsAuraTarget { legal_targets, .. } =
        runner.state().waiting_for.clone()
    {
        let host = legal_targets
            .first()
            .expect("CR 303.4f offers a host")
            .clone();
        runner
            .act(GameAction::ChooseTarget { target: Some(host) })
            .expect("the chooser answers the CR 303.4f host choice");
    }
}

/// CR 117.3b + CR 608.1: pass priority until the committed SPELL has left the
/// stack — and stop there, BEFORE the ETB trigger it produced resolves.
///
/// `GameRunner::resolve_top` cannot express this window: it breaks only when
/// `stack.len() < initial_stack_len`, and the ETB trigger pushed during the
/// same resolving apply restores the length, so that loop resolves the
/// trigger too. The predicate here is the stack's SHAPE.
fn pass_until_spell_resolves(runner: &mut GameRunner) {
    for _ in 0..8 {
        let spell_on_stack = runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::Spell { .. }));
        if !spell_on_stack {
            return;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "CR 117.1: expected a priority window while the spell is on the stack, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority with a spell on the stack is legal");
    }
    panic!("the committed spell never left the stack within 8 priority passes");
}

/// Food tokens on the battlefield. CR 111.1: a token is an object, so it
/// appears in `state.objects` like any permanent.
fn food_tokens(state: &GameState) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(_, o)| {
            o.zone == Zone::Battlefield
                && o.is_token
                && o.card_types.subtypes.iter().any(|s| s == "Food")
        })
        .map(|(id, _)| *id)
        .collect()
}

/// 0/1 white Illusion tokens on the battlefield (Preston's copy tokens).
fn illusion_tokens(state: &GameState) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(_, o)| {
            o.zone == Zone::Battlefield
                && o.is_token
                && o.card_types.subtypes.iter().any(|s| s == "Illusion")
        })
        .map(|(id, _)| *id)
        .collect()
}

fn white_mana(count: usize, id_base: u64) -> Vec<ManaUnit> {
    (0..count)
        .map(|i| ManaUnit::new(ManaType::White, ObjectId(id_base + i as u64), false, vec![]))
        .collect()
}

fn green_mana(count: usize, id_base: u64) -> Vec<ManaUnit> {
    (0..count)
        .map(|i| ManaUnit::new(ManaType::Green, ObjectId(id_base + i as u64), false, vec![]))
        .collect()
}

fn black_mana(count: usize, id_base: u64) -> Vec<ManaUnit> {
    (0..count)
        .map(|i| ManaUnit::new(ManaType::Black, ObjectId(id_base + i as u64), false, vec![]))
        .collect()
}

/// True if any effect in `ability`'s chain (`sub_ability`/`else_ability`) is
/// `Effect::Unimplemented` — a structural guard against a vacuous parse
/// satisfying the runtime rows below (R1/R5) for the wrong reason.
fn ability_chain_has_unimplemented(ability: &AbilityDefinition) -> bool {
    matches!(ability.effect.as_ref(), Effect::Unimplemented { .. })
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_chain_has_unimplemented)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_chain_has_unimplemented)
}

// ---------------------------------------------------------------------------
// R1/R2/R3/R4 — Light-Paws, Emperor's Voice
// ---------------------------------------------------------------------------

struct LightPawsBoard {
    runner: GameRunner,
    light_paws: ObjectId,
    aura_lib_one: ObjectId,
    aura_lib_two: ObjectId,
    /// A fourth, distinctly-named MV-2 Aura pre-staged (and pre-funded) in
    /// hand, uncast. R1 ignores it entirely (an uncast card in hand affects
    /// nothing); R2 casts it afterward to prove the fix does not over-suppress.
    aura_hand_two: ObjectId,
}

/// Casts Light-Paws for real (`cast_from_zone = Some(Hand)`), then casts one
/// MV-2 Aura for real (also from hand) targeting Light-Paws, with two more
/// distinctly-named MV-2 Auras staged in the library — all three MV-2 and
/// distinctly named so that, UNDER THE BUG, the second fetch is legal on both
/// filter axes (`Cmc LE ObjectManaValue{CostPaidObject}` and
/// `DifferentNameFrom`). Resolves through Light-Paws' own optional search.
fn build_and_resolve_light_paws_board() -> LightPawsBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let light_paws = scenario
        .add_creature_to_hand_from_oracle(P0, "Light-Paws, Emperor's Voice", 2, 2, LIGHT_PAWS)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let cast_aura = scenario
        .add_spell_to_hand(P0, "Cast Aura One", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let aura_lib_one = scenario
        .add_spell_to_library_top(P0, "Library Aura One", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let aura_lib_two = scenario
        .add_spell_to_library_top(P0, "Library Aura Two", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let aura_hand_two = scenario
        .add_spell_to_hand(P0, "Cast Aura Three", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    // Enough mana for Light-Paws ({1}{W}), Cast Aura One ({1}{W}), AND the
    // pre-staged fourth Aura ({1}{W}) that only R2 spends.
    scenario.with_mana_pool(P0, white_mana(6, 9_000));

    let mut runner = scenario.build();

    runner.cast(light_paws).resolve();
    runner
        .cast(cast_aura)
        .target_object(light_paws)
        .accept_optional()
        .search_first_legal()
        .resolve();

    LightPawsBoard {
        runner,
        light_paws,
        aura_lib_one,
        aura_lib_two,
        aura_hand_two,
    }
}

/// R1 — PRIMARY: exactly one cast Aura => exactly one fetch. Revert (restore
/// the unguarded `||`) => BOTH staged library Auras reach the battlefield.
#[test]
fn light_paws_cast_aura_fetches_exactly_one_aura() {
    let board = build_and_resolve_light_paws_board();
    let one_on_bf = board.runner.state().objects[&board.aura_lib_one].zone == Zone::Battlefield;
    let two_on_bf = board.runner.state().objects[&board.aura_lib_two].zone == Zone::Battlefield;
    assert_eq!(
        one_on_bf as u8 + two_on_bf as u8,
        1,
        "exactly one library Aura must be fetched, got aura_lib_one on Battlefield={one_on_bf} \
         aura_lib_two on Battlefield={two_on_bf}"
    );
    let fetched = if one_on_bf {
        board.aura_lib_one
    } else {
        board.aura_lib_two
    };
    // R4 — attach guard: the fetch completed, not a no-op masquerading as "one".
    assert_eq!(
        board.runner.state().objects[&fetched].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(
            board.light_paws
        )),
        "CR 701.3: the fetched Aura must attach to Light-Paws"
    );
}

/// R2 — reach-guard for R1: the fix must not over-suppress. A FOURTH,
/// distinctly-named, genuinely cast MV-2 Aura must still fetch the Aura R1
/// left stranded in the library.
#[test]
fn light_paws_second_cast_aura_fetches_the_remaining_aura() {
    let mut board = build_and_resolve_light_paws_board();
    let stranded = if board.runner.state().objects[&board.aura_lib_one].zone == Zone::Library {
        board.aura_lib_one
    } else {
        board.aura_lib_two
    };
    assert_eq!(
        board.runner.state().objects[&stranded].zone,
        Zone::Library,
        "reach-guard: R1's board leaves exactly one library Aura stranded"
    );

    board
        .runner
        .cast(board.aura_hand_two)
        .target_object(board.light_paws)
        .accept_optional()
        .search_first_legal()
        .resolve();

    assert_eq!(
        board.runner.state().objects[&stranded].zone,
        Zone::Battlefield,
        "a genuinely cast Aura must still fetch the previously-stranded library Aura"
    );
}

/// R3 — structural guard against a vacuous parse.
#[test]
fn light_paws_parses_with_no_unimplemented() {
    let parsed = parse_oracle_text(
        LIGHT_PAWS,
        "Light-Paws, Emperor's Voice",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .first()
        .expect("Light-Paws must parse an ETB trigger");
    assert_eq!(
        trigger.condition,
        Some(TriggerCondition::WasCast {
            zone: None,
            controller: None,
            owner: None,
        }),
        "Light-Paws' intervening-if must be the zoneless WasCast"
    );
    assert!(
        trigger.optional,
        "the search must be parsed as optional (\"you may\")"
    );
    let execute = trigger
        .execute
        .as_deref()
        .expect("Light-Paws' trigger must carry an execute ability");
    assert!(
        !ability_chain_has_unimplemented(execute),
        "Light-Paws must parse with zero Effect::Unimplemented in its trigger chain"
    );
}

/// R5 — put-not-cast: an Aura returned from the graveyard (not cast) must not
/// trigger Light-Paws' search, even though `.accept_optional().search_first_legal()`
/// is armed and WOULD fetch if the trigger wrongly fired.
#[test]
fn light_paws_put_aura_does_not_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let light_paws = scenario
        .add_creature_to_hand_from_oracle(P0, "Light-Paws, Emperor's Voice", 2, 2, LIGHT_PAWS)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let graveyard_aura = scenario
        .add_spell_to_graveyard(P0, "Graveyard Aura", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let library_aura = scenario
        .add_spell_to_library_top(P0, "Library Aura", false)
        .as_enchantment()
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature(),
        )))
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let sevinnes = scenario
        .add_spell_to_hand_from_oracle(P0, "Sevinne's Reclamation", false, SEVINNES_RECLAMATION)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::White],
        })
        .id();

    scenario.with_mana_pool(P0, white_mana(5, 9_200));

    let mut runner = scenario.build();

    runner.cast(light_paws).resolve();
    runner
        .cast(sevinnes)
        .target_object(graveyard_aura)
        .accept_optional()
        .search_first_legal()
        .resolve();
    answer_aura_hosts(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&graveyard_aura].zone,
        Zone::Battlefield,
        "reach-guard: Sevinne's Reclamation returned the graveyard Aura"
    );
    assert_eq!(
        runner.state().objects[&library_aura].zone,
        Zone::Library,
        "CR 601.2a: a put (not cast) Aura must not trigger Light-Paws' search"
    );
}

// ---------------------------------------------------------------------------
// R6 — Wild Pair (three-axis: zone + caster + owner)
// ---------------------------------------------------------------------------

/// R6 — three-axis: Wild Pair cast from hand, so its own `Some(Hand)/P0/P0`
/// exactly satisfies `{Hand, You, You}`. Two matching library creatures (same
/// total P/T as each other AND as the genuinely cast creature) let a wrongly
/// re-fetched first library creature's own entry recurse onto the second
/// under the bug (Light-Paws-shaped recursion, driven by Wild Pair's own cast
/// stamp).
#[test]
fn wild_pair_cast_creature_fetches_exactly_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wild_pair = scenario
        .add_spell_to_hand(P0, "Wild Pair", false)
        .as_enchantment()
        .from_oracle_text(WILD_PAIR)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
        })
        .id();

    let library_one = scenario
        .add_creature_to_hand(P0, "Library Beast One", 2, 2)
        .id();
    let library_two = scenario
        .add_creature_to_hand(P0, "Library Beast Two", 2, 2)
        .id();
    let cast_creature = scenario
        .add_creature_to_hand(P0, "Cast Beast", 2, 2)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    scenario.with_mana_pool(P0, green_mana(6, 9_300));

    let mut runner = scenario.build();
    move_to_zone(
        runner.state_mut(),
        library_one,
        Zone::Library,
        &mut Vec::new(),
    );
    move_to_zone(
        runner.state_mut(),
        library_two,
        Zone::Library,
        &mut Vec::new(),
    );

    runner.cast(wild_pair).resolve();
    runner
        .cast(cast_creature)
        .accept_optional()
        .search_first_legal()
        .resolve();

    let fetched_count = [library_one, library_two]
        .into_iter()
        .filter(|&id| runner.state().objects[&id].zone == Zone::Battlefield)
        .count();
    assert_eq!(
        fetched_count, 1,
        "exactly one matching library creature must be fetched; revert => both"
    );
}

// ---------------------------------------------------------------------------
// R7/R8 — Preston, the Vanisher (opposite polarity)
// ---------------------------------------------------------------------------

/// R7 — opposite polarity, primary discriminator: reanimating (putting, not
/// casting) a creature must satisfy `Not(WasCast)` and create Preston's
/// Illusion copy token. Revert (restore the unguarded `||`) fails in the
/// OPPOSITE direction from Light-Paws: no token at all.
#[test]
fn preston_creates_token_for_reanimated_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let preston = scenario
        .add_creature_to_hand_from_oracle(P0, "Preston, the Vanisher", 2, 5, PRESTON)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let bait = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .id();

    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();

    let mut pool = white_mana(4, 9_400);
    pool.extend(black_mana(1, 9_450));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();

    runner.cast(preston).resolve();
    runner.cast(reanimate).target_object(bait).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bait].zone,
        Zone::Battlefield,
        "reach-guard: Reanimate put the bait creature onto the battlefield"
    );
    assert_eq!(
        illusion_tokens(runner.state()).len(),
        1,
        "issue #8163, opposite polarity: Not(WasCast) must fire for a put (not cast) creature"
    );
}

/// R8 — reach-guard for R7: a genuinely cast creature must NOT satisfy
/// `Not(WasCast)`, confirming R7's token is a real decision, not an
/// unconditional ETB copy.
#[test]
fn preston_makes_no_token_for_cast_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let preston = scenario
        .add_creature_to_hand_from_oracle(P0, "Preston, the Vanisher", 2, 5, PRESTON)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::White],
        })
        .id();

    let creature = scenario
        .add_creature_to_hand(P0, "Cast Creature", 2, 2)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    scenario.with_mana_pool(P0, white_mana(4, 9_500));

    let mut runner = scenario.build();

    runner.cast(preston).resolve();
    runner.cast(creature).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Battlefield,
        "reach-guard: the creature was cast and resolved"
    );
    assert!(
        illusion_tokens(runner.state()).is_empty(),
        "paired reach-guard: a genuinely cast creature must not create an Illusion token"
    );
}

// ---------------------------------------------------------------------------
// R9/R10 — Feasting Troll King (LKI regression, Blocker-1 first-class
// deliverable)
// ---------------------------------------------------------------------------

/// R9 — LKI REGRESSION: Feasting Troll King is `SelfRef` with
/// `WasCast{Hand, You, You}`. Cast it, let it resolve and enter, then remove
/// it (through the REAL exit seam) WHILE its own ETB trigger is still on the
/// stack, then let the trigger resolve. CR 608.2h + CR 113.7a: the event
/// record's own last known information must still answer `{Hand, You, You}`,
/// even though the live subject's cast stamps were cleared on exit.
///
/// Round-1's withdrawn design (`event_object_id.is_none() &&`) produces ZERO
/// Food tokens here — this is the regression guard for all 61 self-referential
/// `WasCast` cards at the CR 603.4 resolution re-check.
#[test]
fn feasting_troll_king_etb_still_makes_food_after_removal_in_response() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Staging: real printed stats (7/6) and real cost ({2}{G}{G}{G}{G}).
    let troll = scenario
        .add_creature_to_hand_from_oracle(P0, "Feasting Troll King", 7, 6, FEASTING_TROLL_KING)
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Green; 4],
        })
        .id();
    scenario.with_mana_pool(P0, green_mana(6, 9_600));

    let mut runner = scenario.build();

    // (1) CR 601.2i: commit the spell to the stack WITHOUT draining it. The
    //     `CastCommit` borrow of `runner` ends at the semicolon.
    runner.cast(troll).commit();

    // (2) Pass priority until the SPELL leaves the stack — and no further.
    pass_until_spell_resolves(&mut runner);

    // (3) THE WINDOW ASSERTION. Exactly the ETB trigger is on the stack, the
    //     permanent has entered, and nothing has resolved past it.
    {
        let state = runner.state();
        assert_eq!(
            state.stack.len(),
            1,
            "CR 603.3: the ETB trigger was put on the stack the next time a player would \
             receive priority, got {:?}",
            state.stack
        );
        let entry = state.stack.last().expect("one stack entry");
        assert!(
            matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }),
            "the sole stack entry must be the ETB trigger, got {:?}",
            entry.kind
        );
        assert_eq!(
            entry.source_id, troll,
            "the trigger's source is Feasting Troll King"
        );
        assert_eq!(
            state.objects[&troll].zone,
            Zone::Battlefield,
            "CR 608.3: the permanent spell resolved and entered before its trigger resolves"
        );
        assert!(
            food_tokens(state).is_empty(),
            "no Food may exist before the trigger resolves"
        );
    }

    // (4) Removal in response, through the REAL exit seam (pre-move snapshot
    //     zones.rs:1248 -> apply_zone_exit_cleanup :1298 -> reset_for_battlefield_exit,
    //     which clears cast_from_zone / cast_controller on the LIVE object per CR 400.7).
    move_to_zone(runner.state_mut(), troll, Zone::Graveyard, &mut Vec::new());
    assert_eq!(
        runner.state().objects[&troll].cast_from_zone,
        None,
        "CR 400.7: the live object's cast stamp is cleared on battlefield exit — the record \
         LKI is the only remaining authority"
    );

    // (5) Resolve the trigger. CR 603.4 re-checks the intervening-if here.
    runner.advance_until_stack_empty();

    // (6) CR 608.2h + CR 113.7a: the record LKI answers {Hand, You, You}.
    assert_eq!(
        food_tokens(runner.state()).len(),
        3,
        "three Food tokens (CR 113.7a)"
    );
}

/// R10 — reach-guard for R9: a REANIMATED (put, not cast) Feasting Troll King
/// must create ZERO Food tokens, confirming R9's Foods are a real `WasCast`
/// decision, not an unconditional ETB. Reanimate drives the real put through
/// the real pipeline, and the explicit drain guarantees the trigger had its
/// chance to resolve.
#[test]
fn feasting_troll_king_reanimated_makes_no_food() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let troll = scenario
        .add_creature_to_graveyard(P0, "Feasting Troll King", 7, 6)
        .from_oracle_text(FEASTING_TROLL_KING)
        .id();

    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", false, REANIMATE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::Black],
        })
        .id();

    scenario.with_mana_pool(P0, black_mana(1, 9_700));

    let mut runner = scenario.build();

    runner.cast(reanimate).target_object(troll).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&troll].zone,
        Zone::Battlefield,
        "reach-guard: the Troll was actually reanimated"
    );
    assert!(
        food_tokens(runner.state()).is_empty(),
        "put-into-play (not cast) must not create Food tokens"
    );
}
