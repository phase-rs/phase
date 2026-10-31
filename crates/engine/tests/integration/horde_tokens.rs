//! Horde Magic PR4 — the token-in-library primitive.
//!
//! Tokens can live in the Horde's library and ENTER THE BATTLEFIELD when the
//! Horde reveals them (a token is never cast — CR 111). Two pieces are exercised:
//!
//!   1. `GameObject.is_horde_deck_token` EXEMPTS a library-resident token from the
//!      CR 704.5d token cease-to-exist state-based action, so it can sit in the
//!      library without being swept.
//!   2. On the Horde's precombat main, revealing a library token creates a FRESH
//!      battlefield token under the Horde's control (CR 111.1 + CR 111.2) and
//!      removes the library placeholder so no duplicate remains.
//!
//! Seat 0 = Horde, seat 1 = survivor. Every negative assertion is paired with a
//! positive reach-guard proving the input reached the seam under test.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::GameRunner;
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{CopyTargetPurpose, ReplacementDefinition, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::{ChallengeDeck, FormatConfig, WaveTermination};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const HORDE: PlayerId = PlayerId(0);

/// A Horde game (seat 0 = Horde) with a `FixedCount(wave)` wave, parked at the
/// Horde's Upkeep with priority on a mid-game turn (turn 4 avoids any
/// first-turn draw-skip interaction). Mirrors `horde_spine_runtime`'s fixture.
fn horde_game(wave: u32) -> GameState {
    let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
    ruleset.wave = WaveTermination::FixedCount(wave);
    let mut state = GameState::new(FormatConfig::horde(ruleset), 2, 42);
    state.turn_number = 4;
    state.active_player = HORDE;
    state.phase = Phase::Upkeep;
    state.priority_player = HORDE;
    state.waiting_for = WaitingFor::Priority { player: HORDE };
    state
}

/// Place a creature token on top of `owner`'s library. `flagged` sets the new
/// `is_horde_deck_token` cease-to-exist exemption.
fn add_library_token(
    state: &mut GameState,
    owner: PlayerId,
    name: &str,
    p: i32,
    t: i32,
    flagged: bool,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.is_token = true;
    obj.is_horde_deck_token = flagged;
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(p);
    obj.toughness = Some(t);
    obj.base_power = Some(p);
    obj.base_toughness = Some(t);
    id
}

/// Place a vanilla castable (nontoken) creature on top of `owner`'s library.
fn add_library_creature(state: &mut GameState, owner: PlayerId, name: &str, mv: u32) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), Zone::Library);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::Cost {
        shards: vec![],
        generic: mv,
    };
    obj.power = Some(mv as i32);
    obj.toughness = Some(mv as i32);
    obj.base_power = Some(mv as i32);
    obj.base_toughness = Some(mv as i32);
    id
}

fn horde_battlefield_tokens(state: &GameState) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == HORDE && o.is_token)
        })
        .collect()
}

/// Run the Horde's precombat-main wave to completion (bounded).
fn run_wave(runner: &mut GameRunner) {
    runner.advance_to_phase(Phase::PreCombatMain);
    for _ in 0..40 {
        if runner.state().horde_wave_remaining == 0 && runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// A token flagged `is_horde_deck_token` in the Horde's library SURVIVES a
/// state-based-action pass; an otherwise-identical UNFLAGGED off-battlefield
/// token IS swept.
///
/// Revert guard: dropping the `!obj.is_horde_deck_token` clause in
/// `check_token_cease_to_exist` sweeps the flagged token too — the survival
/// assertion flips. Positive reach-guard: the unflagged token being swept proves
/// the CR 704.5d SBA actually ran (so the survival is the exemption's doing, not
/// an inert SBA).
#[test]
fn flagged_library_token_survives_sba_unflagged_is_swept() {
    let mut state = horde_game(0);
    let flagged = add_library_token(&mut state, HORDE, "Cyberman", 2, 2, true);
    let unflagged = add_library_token(&mut state, HORDE, "Dalek", 3, 3, false);

    let mut events: Vec<GameEvent> = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(
        state.objects.contains_key(&flagged),
        "an is_horde_deck_token token must survive the CR 704.5d cease-to-exist SBA"
    );
    assert!(
        state.players[HORDE.0 as usize].library.contains(&flagged),
        "the flagged token must remain in the Horde's library"
    );
    // Positive reach-guard: the SBA is reachable and sweeps unflagged tokens.
    assert!(
        !state.objects.contains_key(&unflagged),
        "an UNFLAGGED off-battlefield token MUST be swept — proves the exemption \
         is scoped to the flag and the SBA is actually running"
    );
}

/// On the Horde's precombat main, revealing a library token puts a token onto the
/// BATTLEFIELD under the Horde's control and REMOVES the library placeholder (no
/// duplicate; library count -1).
///
/// Revert guard: restoring the old graveyard stub (no CreateToken) leaves the
/// battlefield token count at 0. Dropping the library-object removal leaves the
/// placeholder object present and the library count unchanged.
#[test]
fn precombat_main_reveal_puts_library_token_onto_battlefield() {
    let mut state = horde_game(1);
    let placeholder = add_library_token(&mut state, HORDE, "Cyberman", 2, 2, true);
    let library_before = state.players[HORDE.0 as usize].library.len();

    let mut runner = GameRunner::from_state(state);
    run_wave(&mut runner);

    let bf_tokens = horde_battlefield_tokens(runner.state());
    assert_eq!(
        bf_tokens.len(),
        1,
        "revealing a library token must put exactly one token onto the battlefield under the Horde"
    );
    let bf = runner.state().objects.get(&bf_tokens[0]).unwrap();
    assert_eq!(
        (bf.power, bf.toughness),
        (Some(2), Some(2)),
        "the battlefield token must carry the revealed body's P/T"
    );
    assert!(
        !runner.state().objects.contains_key(&placeholder),
        "the library placeholder object must be removed — no duplicate may remain"
    );
    assert_eq!(
        runner.state().players[HORDE.0 as usize].library.len(),
        library_before - 1,
        "the Horde's library count must decrement by exactly 1"
    );
}

/// The revealed battlefield token is a NORMAL token: it does NOT carry
/// `is_horde_deck_token`, so it ceases to exist normally (CR 704.5d) if it later
/// leaves the battlefield.
///
/// Revert guard: if the reveal path erroneously stamped `is_horde_deck_token` on the
/// fresh token, the flag assertion flips AND the token would survive leaving the
/// battlefield (the final cease-to-exist assertion flips too).
#[test]
fn revealed_battlefield_token_is_normal_and_ceases_when_it_leaves() {
    let mut state = horde_game(1);
    add_library_token(&mut state, HORDE, "Cyberman", 2, 2, true);

    let mut runner = GameRunner::from_state(state);
    run_wave(&mut runner);

    let bf_tokens = horde_battlefield_tokens(runner.state());
    assert_eq!(bf_tokens.len(), 1, "one battlefield token expected");
    let bf = bf_tokens[0];
    assert!(
        !runner.state().objects.get(&bf).unwrap().is_horde_deck_token,
        "the fresh battlefield token must NOT carry is_horde_deck_token"
    );

    // Sanity: it is a normal token now — move it off the battlefield and the
    // CR 704.5d SBA sweeps it (the exemption no longer applies).
    let mut events: Vec<GameEvent> = Vec::new();
    move_to_zone(runner.state_mut(), bf, Zone::Graveyard, &mut events);
    check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        !runner.state().objects.contains_key(&bf),
        "a normal token must cease to exist once it leaves the battlefield"
    );
}

/// Hostile mixed wave: a `FixedCount(2)` wave over [token, nontoken] yields one
/// battlefield token PLUS one resolved nontoken permanent, both under the Horde.
/// Exercises the synchronous-token → continue-to-nontoken loop in
/// `maybe_reveal_next` (the token cannot pause the wave via the stack).
///
/// Revert guard: reverting the token branch to the graveyard stub drops the
/// token permanent (token count 0). Reverting the loop (returning `None` after a
/// synchronous token) strands the nontoken, which never resolves.
#[test]
fn mixed_wave_yields_one_token_and_one_nontoken_under_horde() {
    let mut state = horde_game(2);
    // `create_object` pushes to the back of the library and the wave reveals the
    // front first, so the token (created first) is revealed before the nontoken.
    add_library_token(&mut state, HORDE, "Dalek", 3, 3, true);
    add_library_creature(&mut state, HORDE, "Cyber Controller", 2);

    let mut runner = GameRunner::from_state(state);
    run_wave(&mut runner);

    let horde_creatures: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|o| {
                o.controller == HORDE && o.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .collect();

    let tokens = horde_creatures
        .iter()
        .filter(|id| runner.state().objects.get(id).unwrap().is_token)
        .count();
    let nontokens = horde_creatures
        .iter()
        .filter(|id| !runner.state().objects.get(id).unwrap().is_token)
        .count();

    assert_eq!(
        tokens, 1,
        "the token half of the wave must yield exactly one battlefield token"
    );
    assert_eq!(
        nontokens, 1,
        "the nontoken half of the wave must resolve exactly one permanent (loop reached it)"
    );
}

/// Verbatim Esix, Fractal Bloom Oracle text (copied from
/// `esix_fractal_bloom.rs`, whose helpers are private by design): "The first
/// time you would create one or more tokens during each of your turns, you may
/// instead choose a creature other than Esix and create that many tokens that
/// are copies of that creature."
fn esix_replacement() -> ReplacementDefinition {
    use engine::types::replacements::ReplacementEvent;
    let oracle = "Flying\n\
        The first time you would create one or more tokens during each of your turns, you may \
        instead choose a creature other than Esix and create that many tokens that are copies \
        of that creature.";
    parse_oracle_text(
        oracle,
        "Esix, Fractal Bloom",
        &["Flying".to_string()],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Fractal".to_string()],
    )
    .replacements
    .into_iter()
    .find(|r| r.event == ReplacementEvent::CreateToken)
    .expect("Esix must parse to a CreateToken replacement")
}

/// Install Esix on the battlefield under `controller` with its parsed
/// replacement live. Mirrors `esix_fractal_bloom.rs::install_esix`.
fn install_esix(state: &mut GameState, controller: PlayerId) -> ObjectId {
    use std::sync::Arc;
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        "Esix, Fractal Bloom".to_string(),
        Zone::Battlefield,
    );
    let reps = vec![esix_replacement()];
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.card_types.supertypes = vec![Supertype::Legendary];
    obj.card_types.subtypes = vec!["Fractal".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(4);
    obj.toughness = Some(4);
    obj.base_power = Some(4);
    obj.base_toughness = Some(4);
    obj.replacement_definitions = reps.clone().into();
    obj.base_replacement_definitions = Arc::new(reps);
    id
}

/// A Horde creature that can serve as Esix's chosen copy source, with a
/// copiable subtype on both live and base types (copies read the base).
fn add_copy_source(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Creature];
    obj.card_types.subtypes = vec!["Ox".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(5);
    obj.toughness = Some(4);
    obj.base_power = Some(5);
    obj.base_toughness = Some(4);
    id
}

/// Drive priority until the wave budget is spent, the stack is empty, AND any
/// parked reveal drained (bounded). The pending clause is load-bearing: a
/// mid-wave replacement choice consumes its budget BEFORE parking, so a
/// budget/stack-only check exits before the re-entry seam resumes the park.
/// Used AFTER answering a mid-wave prompt.
fn drive_wave_to_completion(runner: &mut GameRunner) {
    for _ in 0..60 {
        if runner.state().horde_wave_remaining == 0
            && runner.state().stack.is_empty()
            && runner.state().pending_horde_reveal.is_none()
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
}

/// DECLINE: a token-creation replacement the Horde controls (Esix) prompts on
/// a revealed token, and declining falls through to the original token.
/// Full public-action drive: wave → ReplacementChoice → decline → completion.
///
/// Revert guard: the pre-fix code swallowed `NeedsChoice` like prevention —
/// no prompt raises, the wave consumes the card with no token, and both the
/// prompt assertion and the battlefield-token assertion flip.
#[test]
fn revealed_token_replacement_decline_creates_original_token() {
    let mut state = horde_game(1);
    let placeholder = add_library_token(&mut state, HORDE, "Cyberman", 2, 2, true);
    install_esix(&mut state, HORDE);
    add_copy_source(&mut state, HORDE, "Decoy Ox");

    let mut runner = GameRunner::from_state(state);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Esix's optional substitution must prompt on the revealed token, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline Esix's substitution");
    drive_wave_to_completion(&mut runner);

    assert_eq!(
        runner.state().horde_wave_remaining,
        0,
        "the wave must complete after the declined choice"
    );
    let bf_tokens = horde_battlefield_tokens(runner.state());
    assert_eq!(
        bf_tokens.len(),
        1,
        "declining must create the original revealed token, got {} battlefield tokens",
        bf_tokens.len()
    );
    let bf = runner.state().objects.get(&bf_tokens[0]).unwrap();
    assert_eq!(
        (bf.name.as_str(), bf.power, bf.toughness),
        ("Cyberman", Some(2), Some(2)),
        "the battlefield token must be the revealed body, not a copy"
    );
    assert!(
        !runner.state().objects.contains_key(&placeholder),
        "the library placeholder must be removed after the resumed reveal"
    );
    assert!(
        runner.state().pending_horde_reveal.is_none(),
        "the parked reveal must drain on resume"
    );
}

/// ACCEPT: accepting Esix's substitution on a revealed token creates a copy
/// of the CHOSEN creature instead of the revealed body, and the wave still
/// completes (placeholder removed, budget spent).
///
/// Revert guard: same as the decline row — pre-fix, no prompt raises and the
/// card is consumed with no token and no choice.
#[test]
fn revealed_token_replacement_accept_creates_chosen_copy() {
    let mut state = horde_game(1);
    let placeholder = add_library_token(&mut state, HORDE, "Cyberman", 2, 2, true);
    install_esix(&mut state, HORDE);
    let decoy = add_copy_source(&mut state, HORDE, "Decoy Ox");

    let mut runner = GameRunner::from_state(state);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Esix's optional substitution must prompt on the revealed token, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Esix's substitution");

    // Prompt 2 — the copy-source choice.
    match &runner.state().waiting_for {
        WaitingFor::CopyTargetChoice {
            valid_targets,
            purpose,
            ..
        } => {
            assert_eq!(
                *purpose,
                CopyTargetPurpose::CopyTokenSource,
                "the chosen-source arm must raise CopyTokenSource"
            );
            assert!(
                valid_targets.contains(&decoy),
                "the Decoy Ox must be a legal copy source, pool was {valid_targets:?}"
            );
        }
        other => panic!("expected a CopyTargetChoice prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(decoy)),
        })
        .expect("choose Decoy Ox as the copy source");
    drive_wave_to_completion(&mut runner);

    assert_eq!(
        runner.state().horde_wave_remaining,
        0,
        "the wave must complete after the accepted substitution"
    );
    let bf_tokens = horde_battlefield_tokens(runner.state());
    assert_eq!(
        bf_tokens.len(),
        1,
        "accepting must create exactly one substitute token, got {}",
        bf_tokens.len()
    );
    let bf = runner.state().objects.get(&bf_tokens[0]).unwrap();
    assert_eq!(
        bf.name.as_str(),
        "Decoy Ox",
        "the substitute must copy the CHOSEN creature, not the revealed body"
    );
    assert_eq!(
        (bf.power, bf.toughness),
        (Some(5), Some(4)),
        "the copy must carry the chosen creature's P/T, not the 2/2 Cyberman spec"
    );
    assert!(
        !runner.state().objects.contains_key(&placeholder),
        "the library placeholder must be removed after the substituted reveal"
    );
    assert!(
        runner.state().pending_horde_reveal.is_none(),
        "the parked reveal must drain on resume"
    );
}

/// Place an Orc Army creature token on top of `owner`'s library. Subtypes
/// `["Orc", "Army"]` are the Amass dispatch key: the reveal amasses the
/// Horde's single Army (CR 701.47a) instead of entering a fresh 0/0.
fn add_library_army_token(state: &mut GameState, owner: PlayerId) -> ObjectId {
    let id = add_library_token(state, owner, "Orc Army", 0, 0, true);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.subtypes = vec!["Orc".to_string(), "Army".to_string()];
    obj.base_card_types = obj.card_types.clone();
    id
}

/// ARMY DECLINE: Esix prompts on the Army an Amass reveal creates, and
/// declining grows the Horde's 1/1 Army — then the wave continues to the
/// card behind the Army. Full public-action drive: wave → ReplacementChoice
/// → decline → completion.
///
/// Revert guard: the pre-fix code ignored the Amass resolver's pause and
/// deleted the placeholder mid-continuation — no prompt raises, the
/// prompt assertion flips, and the Army never gets its counter.
#[test]
fn revealed_army_replacement_decline_amasses_then_continues_wave() {
    let mut state = horde_game(2);
    // Front of the library first: the Army reveals, then the Cyberman.
    let placeholder = add_library_army_token(&mut state, HORDE);
    add_library_token(&mut state, HORDE, "Cyberman", 2, 2, true);
    install_esix(&mut state, HORDE);
    add_copy_source(&mut state, HORDE, "Decoy Ox");

    let mut runner = GameRunner::from_state(state);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Esix's optional substitution must prompt on the amassed Army, got {:?}",
        runner.state().waiting_for
    );
    // The wave must NOT reveal past the open prompt: the Army's budget is
    // spent but the card behind it is untouched and the placeholder survives
    // until the continuation settles.
    assert_eq!(
        runner.state().horde_wave_remaining,
        1,
        "only the Army's budget may be spent while its prompt is open"
    );
    assert!(
        runner.state().objects.contains_key(&placeholder),
        "the Army placeholder must survive until the continuation settles"
    );
    assert!(
        !runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .any(|obj| obj.controller == HORDE && obj.name == "Cyberman"),
        "the card behind the Army must not reveal before its prompt is answered"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline Esix's substitution");
    drive_wave_to_completion(&mut runner);

    assert_eq!(
        runner.state().horde_wave_remaining,
        0,
        "the wave must complete after the declined choice"
    );
    assert!(
        !runner.state().objects.contains_key(&placeholder),
        "the Army placeholder must be removed after the resumed reveal"
    );
    assert!(
        runner.state().pending_horde_reveal.is_none(),
        "the parked reveal must drain on resume"
    );
    let armies: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| {
            obj.controller == HORDE && obj.card_types.subtypes.iter().any(|s| s == "Army")
        })
        .collect();
    assert_eq!(
        armies.len(),
        1,
        "declining must grow exactly one Horde Army, got {}",
        armies.len()
    );
    assert_eq!(
        armies[0].counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "the amassed Army must carry the +1/+1 counter"
    );
    assert_eq!(
        (armies[0].power, armies[0].toughness),
        (Some(1), Some(1)),
        "the amassed Army must be 1/1"
    );
    // The wave continued past the Army to the card behind it.
    assert!(
        runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .any(|obj| obj.controller == HORDE && obj.name == "Cyberman"),
        "the card behind the Army must still reveal after the resume"
    );
}

/// ARMY ACCEPT: accepting Esix's substitution on the amassed Army creates a
/// copy of the CHOSEN creature instead — and since the copy is no Army,
/// there is nothing to put counters on — while the wave still completes
/// (placeholder removed, budget spent, card behind revealed).
///
/// Revert guard: same as the decline row — pre-fix, no prompt raises and
/// the assertions flip.
#[test]
fn revealed_army_replacement_accept_substitutes_then_continues_wave() {
    let mut state = horde_game(2);
    let placeholder = add_library_army_token(&mut state, HORDE);
    add_library_token(&mut state, HORDE, "Cyberman", 2, 2, true);
    install_esix(&mut state, HORDE);
    let decoy = add_copy_source(&mut state, HORDE, "Decoy Ox");

    let mut runner = GameRunner::from_state(state);
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Esix's optional substitution must prompt on the amassed Army, got {:?}",
        runner.state().waiting_for
    );
    // The wave must NOT reveal past the open prompt: the Army's budget is
    // spent but the card behind it is untouched and the placeholder survives
    // until the continuation settles.
    assert_eq!(
        runner.state().horde_wave_remaining,
        1,
        "only the Army's budget may be spent while its prompt is open"
    );
    assert!(
        runner.state().objects.contains_key(&placeholder),
        "the Army placeholder must survive until the continuation settles"
    );
    assert!(
        !runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .any(|obj| obj.controller == HORDE && obj.name == "Cyberman"),
        "the card behind the Army must not reveal before its prompt is answered"
    );
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Esix's substitution");

    // Prompt 2 — the copy-source choice.
    match &runner.state().waiting_for {
        WaitingFor::CopyTargetChoice {
            valid_targets,
            purpose,
            ..
        } => {
            assert_eq!(
                *purpose,
                CopyTargetPurpose::CopyTokenSource,
                "the chosen-source arm must raise CopyTokenSource"
            );
            assert!(
                valid_targets.contains(&decoy),
                "the Decoy Ox must be a legal copy source, pool was {valid_targets:?}"
            );
        }
        other => panic!("expected a CopyTargetChoice prompt, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(decoy)),
        })
        .expect("choose Decoy Ox as the copy source");
    drive_wave_to_completion(&mut runner);

    assert_eq!(
        runner.state().horde_wave_remaining,
        0,
        "the wave must complete after the accepted substitution"
    );
    assert!(
        !runner.state().objects.contains_key(&placeholder),
        "the Army placeholder must be removed after the substituted reveal"
    );
    assert!(
        runner.state().pending_horde_reveal.is_none(),
        "the parked reveal must drain on resume"
    );
    // The substitute is a Decoy Ox copy — no Army, so the Amass continuation
    // finds nothing to put counters on.
    let armies = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            runner.state().objects.get(id).is_some_and(|o| {
                o.controller == HORDE && o.card_types.subtypes.iter().any(|s| s == "Army")
            })
        })
        .count();
    assert_eq!(
        armies, 0,
        "the accepted substitute is no Army, so no Army must exist"
    );
    assert!(
        runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .any(|obj| obj.controller == HORDE
                && obj.is_token
                && obj.name == "Decoy Ox"
                && obj.power == Some(5)),
        "the substitute must be a 5-power Decoy Ox copy token"
    );
    assert!(
        runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .any(|obj| obj.controller == HORDE && obj.name == "Cyberman"),
        "the card behind the Army must still reveal after the resume"
    );
}
