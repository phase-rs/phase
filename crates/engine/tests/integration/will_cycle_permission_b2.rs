//! Phase B2 — the Will-cycle PERMISSION half.
//!
//! B1 (`will_cycle_duration_seam_b1`) bound the stated window on the graveyard
//! permission and stamped the replacement half's `expiry`. It deliberately made
//! ZERO cards supported. This phase makes the permission itself parse and function,
//! so **Yawgmoth's Will, Gaea's Will and Magus of the Will all work**.
//!
//! # The two mechanisms, and why neither guard was weakened
//!
//! `"you may play lands and cast spells from your graveyard"` is ONE permission
//! naming two actions (CR 305.1: playing a land is a special action; CR 601.2a:
//! casting is not). `"cast "` is a bare-`and` clause starter, so the sequence
//! splitter cut the sentence in half and orphaned `"you may play lands"` — a
//! fragment the CR 305.2a guard in `try_parse_cast_effect` then correctly refused.
//!
//! The fix SUPPRESSES that split (beside the splitter's existing suppressions for
//! from-among compounds, CR 603.7a temporal prefixes, and CR 608.2c targeted
//! continuations) rather than weakening either mechanism: the `"cast "` verb-list
//! entry stays, and the CR 305.2a guard still refuses a genuinely bare
//! `"play lands"` — pinned by `b2_bare_play_lands_fragment_is_still_refused`.
//!
//! # Why a transient continuous effect, and not a per-card stamp
//!
//! CR 611.2c: a continuous effect that modifies no characteristic and changes no
//! controller "modifies the rules of the game, so it can affect objects that
//! weren't affected when that continuous effect began." A cast permission is
//! exactly that, so the affected set must stay OPEN — a card milled or discarded
//! later this turn is covered. Stamping the permission onto the graveyard's
//! contents at resolution would freeze that set and silently miss every later
//! arrival, so the mode rides a duration-bound `TransientContinuousEffect` whose
//! filter `casting::graveyard_permission_sources` re-evaluates per query.
//!
//! # Stack size
//!
//! `parse_oracle_text` overflows the default 8 MB test stack and prints a
//! convincing PARTIAL negative on the way down — a blown stack looks like a
//! plausible parse, not like a crash. Every body that calls it runs on 256 MB.

use engine::game::casting::graveyard_lands_playable_by_permission;
use engine::game::layers::prune_end_of_turn_effects;
use engine::game::zones::create_object;
use engine::parser::oracle::ParsedAbilities;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    CardPlayMode, ContinuousModification, Duration, Effect, StaticDefinition, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::statics::{CastFrequency, StaticMode};
use engine::types::zones::Zone;

const YAWGMOTHS_WILL: &str = "Until end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const GAEAS_WILL: &str = "Suspend 4—{G}\nUntil end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const MAGUS_OF_THE_WILL: &str = "{2}{B}, {T}, Exile this creature: Until end of turn, you may play lands and cast spells from your graveyard. If a card would be put into your graveyard from anywhere this turn, exile that card instead.";

fn on_big_stack<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .expect("spawn 256MB parser thread")
        .join()
        .expect("parser thread must not panic")
}

fn parse_with_types(
    text: &'static str,
    name: &'static str,
    types: &'static [&'static str],
) -> ParsedAbilities {
    on_big_stack(move || {
        let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
        parse_oracle_text(text, name, &[], &types, &[])
    })
}

/// The `GraveyardCastPermission` mode carried by a parsed `GenericEffect`, plus the
/// window bound to it. `None` if the parse produced no such effect.
fn permission_and_duration(parsed: &ParsedAbilities) -> Option<(StaticMode, Option<Duration>)> {
    parsed.abilities.iter().find_map(|a| match &*a.effect {
        Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } => static_abilities
            .iter()
            .flat_map(|s| s.modifications.iter())
            .find_map(|m| match m {
                ContinuousModification::AddStaticMode { mode }
                    if matches!(mode, StaticMode::GraveyardCastPermission { .. }) =>
                {
                    Some((mode.clone(), duration.clone()))
                }
                _ => None,
            }),
        _ => None,
    })
}

// ── DISCRIMINATING ────────────────────────────────────────────────────────────
// Each of these FAILS before the change and PASSES after.

/// The headline: all three cards, three different arrival shapes (bare sorcery,
/// sorcery behind a Suspend line, creature's activated ability), reach the same
/// verdict — proving the outcome keys on the BODY, not on `AbilityKind`, cost
/// presence, or line index.
#[test]
fn b2_all_three_will_cards_produce_a_bound_graveyard_permission() {
    for (text, name, types) in [
        (YAWGMOTHS_WILL, "Yawgmoth's Will", &["Sorcery"][..]),
        (GAEAS_WILL, "Gaea's Will", &["Sorcery"][..]),
        (MAGUS_OF_THE_WILL, "Magus of the Will", &["Creature"][..]),
    ] {
        let parsed = parse_with_types(text, name, types);

        assert!(
            !parsed
                .abilities
                .iter()
                .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
            "{name}: must not report an Unimplemented effect"
        );

        let (mode, duration) = permission_and_duration(&parsed)
            .unwrap_or_else(|| panic!("{name}: expected a GraveyardCastPermission grant"));

        let StaticMode::GraveyardCastPermission {
            frequency,
            play_mode,
            ..
        } = &mode
        else {
            panic!("{name}: wrong mode {mode:?}");
        };
        // CR 305.1: `Play` covers the land half; the cast half rides the same grant.
        assert_eq!(*play_mode, CardPlayMode::Play, "{name}");
        assert_eq!(*frequency, CastFrequency::Unlimited, "{name}");

        // CR 611.2a: the window must be EXPLICIT. `None` is silently rescued by the
        // resolver's `unwrap_or(UntilEndOfTurn)` fallback, so asserting only "it
        // resolves" would pass over a DROPPED duration. An unstated duration lasts
        // until end of GAME, which would make this permission permanent.
        assert_eq!(
            duration,
            Some(Duration::UntilEndOfTurn),
            "{name}: window must be explicitly bound, not left to the resolver fallback"
        );

        // The emblem route was measured wrong and abandoned; pin that it stays gone.
        assert!(
            !parsed
                .abilities
                .iter()
                .any(|a| matches!(&*a.effect, Effect::CreateEmblem { .. })),
            "{name}: must not manufacture an emblem"
        );
    }
}

/// RUNTIME, both directions. The parser producing the right shape proves nothing
/// about whether the permission FUNCTIONS: the single most likely defect here is a
/// silent no-op, where the reader consults the O(1) `static_mode_presence` index
/// (battlefield statics only) and never sees a transient grant — every parser test
/// would still pass.
///
/// Three assertions, because a happy-path probe is not a result:
///   * NEGATIVE CONTROL — nothing playable before the permission exists, so a
///     later positive cannot be a pre-existing condition.
///   * GRANTS — the land in the graveyard becomes playable.
///   * EXPIRES — CR 514.2. A permission surviving cleanup would be PERMANENT,
///     which is strictly worse than not parsing at all.
#[test]
fn b2_permission_grants_at_runtime_and_expires_at_cleanup() {
    let mut state = GameState::new_two_player(42);
    let p0 = PlayerId(0);

    let land = create_object(
        &mut state,
        CardId(0),
        p0,
        "Forest".to_string(),
        Zone::Graveyard,
    );
    if let Some(obj) = state.objects.get_mut(&land) {
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        };
    }

    assert!(
        graveyard_lands_playable_by_permission(&state, p0).is_empty(),
        "negative control: no permission exists yet"
    );

    let source = create_object(
        &mut state,
        CardId(1),
        p0,
        "Yawgmoth's Will".to_string(),
        Zone::Graveyard,
    );
    install_will_permission(&mut state, p0, source);

    assert!(
        !graveyard_lands_playable_by_permission(&state, p0).is_empty(),
        "SILENT NO-OP: the permission is installed but the reader does not see it"
    );

    // CR 514.2: all "until end of turn" effects end at cleanup.
    prune_end_of_turn_effects(&mut state);
    assert!(
        graveyard_lands_playable_by_permission(&state, p0).is_empty(),
        "CR 514.2 + CR 611.2a: the permission survived cleanup — it is PERMANENT"
    );
}

/// CR 611.2c: the affected set must stay OPEN. A card that reaches the graveyard
/// AFTER the permission is installed must be covered — that is the whole point of
/// Yawgmoth's Will, which is normally cast late with an empty-ish graveyard and
/// then fed by whatever it mills or discards.
///
/// This is the sharpest discriminator in the file: it is the assertion that a
/// resolution-time snapshot design would FAIL while passing every other test here.
#[test]
fn b2_permission_covers_cards_that_arrive_after_it_resolves() {
    let mut state = GameState::new_two_player(42);
    let p0 = PlayerId(0);

    let source = create_object(
        &mut state,
        CardId(1),
        p0,
        "Yawgmoth's Will".to_string(),
        Zone::Graveyard,
    );
    install_will_permission(&mut state, p0, source);

    assert!(
        graveyard_lands_playable_by_permission(&state, p0).is_empty(),
        "control: the graveyard holds no land yet"
    );

    // The land arrives AFTER the permission was installed.
    let land = create_object(
        &mut state,
        CardId(0),
        p0,
        "Forest".to_string(),
        Zone::Graveyard,
    );
    if let Some(obj) = state.objects.get_mut(&land) {
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        };
    }

    assert!(
        !graveyard_lands_playable_by_permission(&state, p0).is_empty(),
        "CR 611.2c: a card arriving after the effect began must still be covered"
    );
}

/// CR 109.5: "you" is the player who cast the spell / activated the ability. An
/// opponent's graveyard must be unaffected — the permission is latched to the
/// installing player, not broadcast.
#[test]
fn b2_permission_does_not_leak_to_opponents() {
    let mut state = GameState::new_two_player(42);
    let (p0, p1) = (PlayerId(0), PlayerId(1));

    let land = create_object(
        &mut state,
        CardId(0),
        p1,
        "Forest".to_string(),
        Zone::Graveyard,
    );
    if let Some(obj) = state.objects.get_mut(&land) {
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        };
    }

    let source = create_object(
        &mut state,
        CardId(1),
        p0,
        "Yawgmoth's Will".to_string(),
        Zone::Graveyard,
    );
    install_will_permission(&mut state, p0, source);

    assert!(
        graveyard_lands_playable_by_permission(&state, p1).is_empty(),
        "CR 109.5: the opponent must not gain the permission"
    );
}

// ── REGRESSION GUARDS ─────────────────────────────────────────────────────────
// These pass BEFORE and AFTER. They pin what must not move.

/// The CR 305.2a guard is CORRECT and was not weakened: a genuinely bare
/// `"play lands"` fragment — one with no zone clause — must still be refused. The
/// fix suppresses the SPLIT that manufactured such fragments; it does not teach the
/// parser to accept them.
#[test]
fn b2_bare_play_lands_fragment_is_still_refused() {
    let parsed = parse_with_types("You may play lands.", "Bare", &["Sorcery"][..]);
    assert!(
        parsed
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "a bare 'play lands' with no zone clause must still be refused"
    );
}

/// HOSTILE FIXTURES. All four carry the identical `"play lands and cast spells"`
/// conjunction and parse CLEAN today via the STATIC path. They differ from the Will
/// cycle only by lacking a leading duration head, so they must keep routing to
/// `statics` and must NOT be captured by the new effect-path arm.
///
/// Chosen for structural spread: a graveyard permission that is the closest possible
/// neighbour (Agenda — same zone, same conjunction, no head), a different zone
/// anchor (Citadel — top of library), a different frequency (Muldrotha — per
/// permanent type), and a Creature host (Magus of the Future — proving no
/// type-keyed behaviour).
#[test]
fn b2_headless_conjunction_cards_keep_the_static_path() {
    for (label, text, types) in [
        (
            "Yawgmoth's Agenda",
            "You may play lands and cast spells from your graveyard.",
            &["Enchantment"][..],
        ),
        (
            "Bolas's Citadel",
            "You may play lands and cast spells from the top of your library.",
            &["Artifact"][..],
        ),
        (
            "Muldrotha, the Gravetide",
            "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard.",
            &["Creature"][..],
        ),
        (
            "Magus of the Future",
            "You may play lands and cast spells from the top of your library.",
            &["Creature"][..],
        ),
    ] {
        let parsed = parse_with_types(text, label, types);
        assert!(
            !parsed.statics.is_empty(),
            "{label}: must keep producing a printed static"
        );
        assert!(
            permission_and_duration(&parsed).is_none(),
            "{label}: must NOT be captured by the duration-headed effect arm"
        );
        assert!(
            !parsed
                .abilities
                .iter()
                .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
            "{label}: must not regress into an Unimplemented effect"
        );
    }
}

/// Install the exact transient effect the parser arm + `register_transient_effect`
/// produce at resolution, so the runtime tests exercise the real shape rather than
/// a hand-rolled approximation.
fn install_will_permission(state: &mut GameState, controller: PlayerId, source_id: ObjectId) {
    let mode = StaticMode::GraveyardCastPermission {
        frequency: CastFrequency::Unlimited,
        play_mode: CardPlayMode::Play,
        graveyard_destination_replacement: None,
        extra_cost: None,
        enters_with_counter: None,
    };
    let affected = TargetFilter::Typed(TypedFilter::new(TypeFilter::Land));
    let def = StaticDefinition::continuous()
        .affected(affected.clone())
        .modifications(vec![ContinuousModification::AddStaticMode { mode }]);
    state.add_transient_continuous_effect(
        source_id,
        controller,
        Duration::UntilEndOfTurn,
        affected,
        def.modifications.clone(),
        None,
    );
}
