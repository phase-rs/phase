//! Regression coverage for self-library "look ... cast from among them" chains.
//!
//! These tests exercise production Oracle parsing and the resolution-time cast
//! path. They distinguish that one-shot private-library flow from the durable
//! exile permission used by ordinary impulse-draw chains.

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, CastFromZoneDriver, CastPermissionConstraint, ChoiceType, Comparator,
    ControllerRef, Duration, Effect, FilterProp, ObjectScope, QuantityExpr, QuantityRef,
    ResolutionCastWindow, ResolvedAbility, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastOfferKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KIORA: &str = "Vigilance, ward {3}\nWhenever you cast a Kraken, Leviathan, Octopus, or Serpent spell from your hand, look at the top X cards of your library, where X is that spell's mana value. You may cast a spell with mana value less than X from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const AETHERWORKS_MARVEL: &str = "Whenever a permanent you control is put into a graveyard, you get {E} (an energy counter).\n{T}, Pay six {E}: Look at the top six cards of your library. You may cast a spell from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const COSMIC_CUBE: &str = "Ward {2}\nWhenever you attack, look at the top six cards of your library. You may cast a spell from among them with mana value less than or equal to the greatest power among attacking creatures you control without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const BOBBLEHEAD: &str = "{T}: Add one mana of any color.\n{3}, {T}: Look at the top X cards of your library, where X is the number of Bobbleheads you control. You may cast a spell with mana value 3 or less from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.\n{3}, {T}: Create a colorless snow artifact token named Icy Manalith with \"{T}: Add one mana of any color.\"";
const SVELLA: &str = "{6}{R}{G}, {T}: Look at the top four cards of your library. You may cast a spell from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const VELOMACHUS: &str = "Flying, vigilance, haste\nWhenever Velomachus Lorehold attacks, look at the top seven cards of your library. You may cast an instant or sorcery spell with mana value less than or equal to Velomachus Lorehold's power from among them without paying its mana cost. Put the rest on the bottom of your library in a random order.";
const APEX: &str = "Exile the top seven cards of your library. Until end of turn, you may cast spells from among them.\nIf this spell was cast from your hand, add ten mana of any one color.";
const TALENT: &str = "Target opponent reveals the top seven cards of their library. You may cast an instant or sorcery spell from among them without paying its mana cost. Then that player puts the rest into their graveyard.\nSpell mastery — If there are two or more instant and/or sorcery cards in your graveyard, you may cast up to two instant and/or sorcery spells from among the revealed cards instead of one.";
const JACE: &str = "Flying\nWhen this creature enters, target opponent mills five cards. You may cast an instant or sorcery spell from among them without paying its mana cost.";
const SILENT_BLADE: &str = "Ninjutsu {4}{U}{B} ({4}{U}{B}, Return an unblocked attacker you control to hand: Put this card onto the battlefield from your hand tapped and attacking.)\nWhenever this creature deals combat damage to a player, look at that player's hand. You may cast a spell from among those cards without paying its mana cost.";
const MINDCLAW_SHAMAN: &str = "When this creature enters, target opponent reveals their hand. You may cast an instant or sorcery spell from among those cards without paying its mana cost.";
const MINDLEECH_MASS: &str = "Trample\nWhenever this creature deals combat damage to a player, you may look at that player's hand. If you do, you may cast a spell from among those cards without paying its mana cost.";
const EPIC_EXPERIMENT: &str = "Exile the top X cards of your library. You may cast instant and sorcery spells with mana value X or less from among them without paying their mana costs. Then put all cards exiled this way that weren't cast into your graveyard.";
const COLLECTED_CONJURING: &str = "Exile the top six cards of your library. You may cast up to two sorcery spells with mana value 3 or less from among them without paying their mana costs. Put the exiled cards not cast this way on the bottom of your library in a random order.";
const HAZORET: &str = "Shuffle your library, then exile the top four cards. You may cast any number of spells with mana value 5 or less from among them without paying their mana costs. Lands you control don't untap during your next untap step.";
const PRIMEVAL_SPAWN: &str = "If this creature would enter and it wasn't cast or no mana was spent to cast it, exile it instead.\nVigilance, trample, lifelink\nWhen this creature leaves the battlefield, exile the top ten cards of your library. You may cast any number of spells with total mana value 10 or less from among them without paying their mana costs.";
const CAPSTONE: &str = "Exile cards from the top of your library until you exile cards with total mana value 4 or greater. You may cast any number of spells from among them without paying their mana costs.\nParadigm (Then exile this spell. After you first resolve a spell with this name, you may cast a copy of it from exile without paying its mana cost at the beginning of each of your first main phases.)";
const VILLAINOUS_WEALTH: &str = "Target opponent exiles the top X cards of their library. You may cast any number of spells with mana value X or less from among them without paying their mana costs.";
const FOUNDING: &str = "Read ahead (Choose a chapter and start with that many lore counters. Add one after your draw step. Skipped chapters don't trigger. Sacrifice after III.)\nI — You may cast an instant or sorcery spell with mana value 1 or 2 from your hand without paying its mana cost.\nII — Target player mills four cards.\nIII — Exile target instant or sorcery card from your graveyard. Copy it. You may cast the copy.";

const MEETING_OF_THE_FIVE: &str = "Exile the top ten cards of your library. You may cast spells with exactly three colors from among them this turn. Add {W}{W}{U}{U}{B}{B}{R}{R}{G}{G}. Spend this mana only to cast spells with exactly three colors.";

/// Verbatim Scryfall Oracle text. LEADING duration + a free cast — the
/// non-vacuous leading-position control for `a_stated_duration_keeps_the_grant_a_lingering_permission`.
/// Ruling: "Any exiled cards you don't cast remain in exile."
const NARSET_ENLIGHTENED_MASTER: &str = "First strike, hexproof\nWhenever Narset attacks, exile the top four cards of your library. Until end of turn, you may cast noncreature spells from among those cards without paying their mana costs.";
/// Verbatim Scryfall Oracle text. A LEADING duration that scopes a COORDINATED
/// pair ("you may play lands AND cast spells …"), so the duration is stripped off
/// the sentence head and the cast half becomes a sibling clause that never sees
/// it. Rulings: "You must follow the normal timing permissions and restrictions
/// for cards you cast this way." + "Any of the cards you don't play will remain
/// in exile." — the lingering signature, not the resolution-scoped one.
const MAGUS_OF_THE_MIND: &str = "{U}, {T}, Sacrifice this creature: Shuffle your library, then exile the top X cards, where X is one plus the number of spells cast this turn. Until end of turn, you may play lands and cast spells from among cards exiled this way without paying their mana costs.";
/// Verbatim Scryfall Oracle text. The same coordinated "play lands and cast
/// spells from among cards exiled this way" grammar with NO duration, whose own
/// ruling is the resolution-scoped one: "You must play the cards as you resolve
/// the last ability. You can't wait and play them later."
const GIX_YAWGMOTH_PRAETOR: &str = "Whenever a creature deals combat damage to one of your opponents, its controller may pay 1 life. If they do, they draw a card.\n{4}{B}{B}{B}, Discard X cards: Exile the top X cards of target opponent's library. You may play lands and cast spells from among cards exiled this way without paying their mana costs.";
// Verbatim Scryfall Oracle text. The COORDINATED leading-duration collateral
// class: "Until end of turn, you may play lands **and** cast spells from …".
// Not "from among them" batch grants — they never reach
// `from_among_batch_cast_driver` — but they share the sentence-grouping pass
// `compute_sentence_leading_duration`, and before it the cast conjunct was
// lowered with no duration at all.
const YAWGMOTHS_WILL: &str = "Until end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const GAEAS_WILL: &str = "Suspend 4—{G}\nUntil end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";
const MAGUS_OF_THE_WILL: &str = "{2}{B}, {T}, Exile this creature: Until end of turn, you may play lands and cast spells from your graveyard. If a card would be put into your graveyard from anywhere this turn, exile that card instead.";
/// A THREE-way coordination ("look at the top card …, and you may play lands and
/// cast spells from the top of your library") over a library pool.
const THE_BELLIGERENT: &str = "Whenever The Belligerent attacks, create a Treasure token. Until end of turn, you may look at the top card of your library any time, and you may play lands and cast spells from the top of your library.\nCrew 3";
/// The duration sits after the trigger condition rather than at the very head of
/// the line, and the grant is a PAID one (the trailing sentence replaces the
/// mana cost with a life payment) — so this row also proves the sentence binding
/// is not accidentally scoped to free casts.
const GWENOM_REMORSELESS: &str = "Deathtouch, lifelink\nWhenever Gwenom attacks, until end of turn, you may look at the top card of your library any time and you may play cards from the top of your library. If you cast a spell this way, pay life equal to its mana value rather than pay its mana cost.";
/// A duration whose printed scope is a CONJUNCTION — "Until end of turn, for as
/// long as that card remains on top of your library". Only the first half is
/// modelled; see the note on the test row.
const TEMPORAL_APERTURE: &str = "{5}, {T}: Shuffle your library, then reveal the top card. Until end of turn, for as long as that card remains on top of your library, play with the top card of your library revealed and you may play that card without paying its mana cost. (If it has X in its mana cost, X is 0.)";

/// Verbatim Scryfall Oracle text. The class member whose "may" is a
/// SUBJECT-PHRASE modal ("then THEY may cast a spell from among those cards"),
/// not the leading "you may …" every other member prints. That phrasing reaches
/// `AbilityDefinition::optional` through a second, independent seam in
/// `oracle_effect/assembly.rs` (`clause_ir.parsed.optional`), so it is the
/// discriminating case for
/// `resolution_window_grants_are_never_double_wrapped_in_an_optional_choice`.
const ITAZURA: &str = "At the beginning of your upkeep, exile the top three cards of your library. Each opponent secretly chooses a number 0 or greater. Then those numbers are revealed. Choose an opponent with the highest number. Itazura deals that much damage to them, then they may cast a spell from among those cards without paying its mana cost. You put a card from among them that wasn't cast this way into your hand.";

fn parse(oracle: &str, name: &str, types: &[&str]) -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        oracle,
        name,
        &[],
        &types.iter().map(|ty| ty.to_string()).collect::<Vec<_>>(),
        &[],
    )
}

/// The `AbilityDefinition` that *carries* the `CastFromZone`, not just its
/// effect — the optionality flag under test in
/// `resolution_window_grants_are_never_double_wrapped_in_an_optional_choice`
/// lives on the definition, one level above the effect.
fn cast_from_zone_def_in(definition: &AbilityDefinition) -> Option<&AbilityDefinition> {
    if matches!(definition.effect.as_ref(), Effect::CastFromZone { .. }) {
        return Some(definition);
    }
    definition
        .sub_ability
        .as_deref()
        .and_then(cast_from_zone_def_in)
}

fn parsed_cast_from_zone_def(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> &AbilityDefinition {
    parsed
        .abilities
        .iter()
        .find_map(cast_from_zone_def_in)
        .or_else(|| {
            parsed
                .triggers
                .iter()
                .filter_map(|trigger| trigger.execute.as_deref())
                .find_map(cast_from_zone_def_in)
        })
        .expect("exact Oracle text must parse a real CastFromZone effect")
}

fn parsed_cast_from_zone(parsed: &engine::parser::oracle::ParsedAbilities) -> &Effect {
    parsed_cast_from_zone_def(parsed).effect.as_ref()
}

fn has_self_library_peek(definition: &AbilityDefinition) -> bool {
    matches!(
        definition.effect.as_ref(),
        Effect::Dig {
            player: TargetFilter::Controller,
            destination: None,
            keep_count: Some(0),
            reveal: false,
            source,
            ..
        } if source.is_library()
    ) || definition
        .sub_ability
        .as_deref()
        .is_some_and(has_self_library_peek)
}

#[test]
fn self_library_peek_casts_route_during_resolution() {
    for (name, oracle, types) in [
        ("Kiora, Sovereign of the Deep", KIORA, &["Creature"][..]),
        ("Aetherworks Marvel", AETHERWORKS_MARVEL, &["Artifact"][..]),
        ("Construct a Cosmic Cube", COSMIC_CUBE, &["Artifact"][..]),
        ("Perception Bobblehead", BOBBLEHEAD, &["Artifact"][..]),
        ("Svella, Ice Shaper", SVELLA, &["Creature"][..]),
        ("Velomachus Lorehold", VELOMACHUS, &["Creature"][..]),
    ] {
        let parsed = parse(oracle, name, types);
        assert!(
            parsed.abilities.iter().any(has_self_library_peek)
                || parsed
                    .triggers
                    .iter()
                    .filter_map(|trigger| trigger.execute.as_deref())
                    .any(has_self_library_peek),
            "{name} must first parse its self-library Dig producer"
        );
        assert!(
            matches!(
                parsed_cast_from_zone(&parsed),
                Effect::CastFromZone {
                    driver: CastFromZoneDriver::DuringResolution,
                    ..
                }
            ),
            "{name} must use the one-shot DuringResolution driver"
        );
    }
}

#[test]
fn self_library_peek_constraints_are_retained() {
    let kiora = parse(KIORA, "Kiora, Sovereign of the Deep", &["Creature"]);
    assert!(matches!(
        parsed_cast_from_zone(&kiora),
        Effect::CastFromZone {
            constraint: Some(CastPermissionConstraint::ManaValue {
                comparator: Comparator::LT,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::EventSource,
                    },
                },
            }),
            ..
        }
    ));

    let bobblehead = parse(BOBBLEHEAD, "Perception Bobblehead", &["Artifact"]);
    assert!(matches!(
        parsed_cast_from_zone(&bobblehead),
        Effect::CastFromZone {
            constraint: Some(CastPermissionConstraint::ManaValue {
                comparator: Comparator::LE,
                value: QuantityExpr::Fixed { value: 3 },
            }),
            ..
        }
    ));

    for (name, oracle, types, expected_constraint) in [
        (
            "Aetherworks Marvel",
            AETHERWORKS_MARVEL,
            &["Artifact"][..],
            false,
        ),
        ("Svella, Ice Shaper", SVELLA, &["Creature"][..], false),
        (
            "Construct a Cosmic Cube",
            COSMIC_CUBE,
            &["Artifact"][..],
            true,
        ),
        ("Velomachus Lorehold", VELOMACHUS, &["Creature"][..], true),
    ] {
        let parsed = parse(oracle, name, types);
        let Effect::CastFromZone { constraint, .. } = parsed_cast_from_zone(&parsed) else {
            unreachable!("helper returns CastFromZone")
        };
        assert_eq!(
            constraint.is_some(),
            expected_constraint,
            "{name} constraint shape"
        );
    }
}

/// CR 611.2a: a stated duration keeps the grant a lingering permission at EVERY
/// position the printed grammar puts it in.
///
/// Three genuinely distinct seams, one row each:
/// * MID-clause (Ral, Leyline Prodigy) — "… from among them THIS TURN without
///   paying their mana costs" sits between the anaphor and the free-cast rider,
///   so neither the leading-duration pass (`with_clause_duration`) nor the
///   trailing-duration fixup ever sees it; only the in-clause scan can catch it.
/// * LEADING (Narset, Enlightened Master) — "Until end of turn, you may cast
///   noncreature spells from among those cards without paying their mana costs"
///   is stripped off the head and reconciled by `with_clause_duration`.
/// * LEADING across a COORDINATED pair (Magus of the Mind) — "Until end of turn,
///   you may play lands AND cast spells from among cards exiled this way without
///   paying their mana costs". The stripped duration lands on the FIRST
///   coordinated clause ("play lands"); the cast half is a sibling clause whose
///   own fragment contains no duration token at all. This row is the reason
///   `from_among_batch_cast_driver` takes a sentence-level `scope`.
///
/// The `without paying` reach guard is load-bearing, not decoration. This test
/// previously used Apex of Power and Meeting of the Five as its leading /
/// trailing controls; NEITHER card says "without paying their mana costs", so
/// `from_among_batch_cast_driver` rejected both at its `without_paying` gate
/// before any duration logic ran. Both rows passed without ever exercising the
/// code they were supposed to pin. (Their lingering classification is still
/// covered — Apex by `duration_and_hand_anaphors_stay_lingering_permissions`,
/// Meeting by `untyped_from_among_them_cast_stays_a_bare_exile_anaphor`.)
#[test]
fn a_stated_duration_keeps_the_grant_a_lingering_permission() {
    for (name, oracle, types) in [
        (
            "Ral, Leyline Prodigy",
            RAL_LEYLINE_PRODIGY,
            &["Planeswalker"][..],
        ),
        (
            "Narset, Enlightened Master",
            NARSET_ENLIGHTENED_MASTER,
            &["Creature"][..],
        ),
        ("Magus of the Mind", MAGUS_OF_THE_MIND, &["Creature"][..]),
    ] {
        assert!(
            oracle
                .to_lowercase()
                .contains("without paying their mana costs"),
            "reach guard: {name} must be a FREE cast, otherwise \
             `from_among_batch_cast_driver` short-circuits at its `without_paying` \
             gate and this row proves nothing about the duration seam"
        );
        let parsed = parse(oracle, name, types);
        let Effect::CastFromZone { driver, .. } = parsed_cast_from_zone(&parsed) else {
            unreachable!("helper returns CastFromZone")
        };
        assert_eq!(
            driver.window_bounds(),
            None,
            "{name} states a duration, so it must NOT become a resolution-scoped window"
        );
        assert_eq!(*driver, CastFromZoneDriver::LingeringPermission, "{name}");
    }
}

/// KNOWN RESIDUAL, recorded as an executable note rather than left implicit.
///
/// The Ral row above pins that a MID-clause duration keeps the grant a lingering
/// permission — `clause_states_a_duration` sees "… from among them THIS TURN …"
/// and declines the window. But nothing then STAMPS that duration onto the
/// lowered effect, so Ral's permission is currently lingering AND indefinite
/// (`duration: None`) when CR 611.2a says it should expire at end of turn.
///
/// This predates the resolution-window change (the old lowering produced the
/// same indefinite grant); the new detector is merely what makes the gap
/// visible. It is NOT fixed here on purpose: `clause_states_a_duration`'s
/// word-boundary scan is documented as safe under false positives *precisely
/// because* its only consumer is a boolean that falls back to the pre-existing
/// path. Feeding its result into `duration` would make a false positive
/// actively wrong — it would time-bound a permission the card prints as
/// indefinite — so promoting the detector needs its own corpus evidence, not a
/// closing-pass one-liner.
///
/// Fail-on-fix: when the stamping lands, this assertion flips and the row moves
/// to `coordinated_leading_durations_bind_to_the_cast_half` below.
#[test]
fn ral_leyline_prodigy_mid_clause_duration_is_detected_but_not_yet_stamped() {
    let parsed = parse(
        RAL_LEYLINE_PRODIGY,
        "Ral, Leyline Prodigy",
        &["Planeswalker"],
    );
    let Effect::CastFromZone { duration, .. } = parsed_cast_from_zone(&parsed) else {
        unreachable!("helper returns CastFromZone")
    };
    assert_eq!(
        *duration, None,
        "known residual: Ral's mid-clause \"this turn\" is detected by \
         `clause_states_a_duration` (which is why the grant stays lingering) but \
         is never stamped onto the effect. If this now reads \
         `Some(UntilEndOfTurn)`, the residual is fixed — move the row."
    );
}

/// CR 611.2a: a duration printed at the HEAD of a sentence scopes the WHOLE
/// coordinated predicate, not just the conjunct it sits next to.
///
/// `split_clause_sequence` cuts "Until end of turn, you may play lands **and**
/// cast spells from your graveyard" into two chunks and only the first still
/// carries the stripped prefix, so the cast half was previously lowered from a
/// fragment containing no duration token at any position — an INDEFINITE
/// graveyard-cast permission on some of the most powerful cards in the game.
/// `compute_sentence_leading_duration` re-binds the head duration across the
/// sentence, on the same sentence grouping CR 107.3i's `where X is` binding
/// already uses.
///
/// These six are the collateral beneficiaries of that fix — they are NOT
/// "from among them" batch grants and never reach `from_among_batch_cast_driver`
/// at all, so nothing else in this file covers them. They are pinned here
/// because the sentence-grouping pass is shared: a regression in it would
/// silently restore an unbounded Yawgmoth's Will.
#[test]
fn coordinated_leading_durations_bind_to_the_cast_half() {
    for (name, oracle, types) in [
        // "Until end of turn, you may play lands and cast spells from your
        // graveyard." — the canonical coordinated pair.
        ("Yawgmoth's Will", YAWGMOTHS_WILL, &["Sorcery"][..]),
        ("Gaea's Will", GAEAS_WILL, &["Sorcery"][..]),
        ("Magus of the Will", MAGUS_OF_THE_WILL, &["Creature"][..]),
        // The same shape with a three-way coordination and a top-of-library
        // pool instead of a graveyard.
        ("The Belligerent", THE_BELLIGERENT, &["Artifact"][..]),
        ("Gwenom, Remorseless", GWENOM_REMORSELESS, &["Creature"][..]),
        // A duration whose PRINTED scope is a conjunction ("Until end of turn,
        // for as long as that card remains on top of your library"); only the
        // first half is modelled. `UntilEndOfTurn` is a correct upper bound —
        // the permission cannot outlive the turn — but it is a PARTIAL model:
        // shuffling the library mid-turn should end the permission early and
        // currently does not. Asserted at the bound the engine does model.
        ("Temporal Aperture", TEMPORAL_APERTURE, &["Artifact"][..]),
    ] {
        let parsed = parse(oracle, name, types);
        let Effect::CastFromZone { duration, .. } = parsed_cast_from_zone(&parsed) else {
            unreachable!("helper returns CastFromZone")
        };
        assert_eq!(
            *duration,
            Some(Duration::UntilEndOfTurn),
            "{name}: the sentence-leading \"Until end of turn\" must reach the \
             cast conjunct, not just the conjunct it sits next to — an unbound \
             graveyard/library cast permission is the failure mode"
        );
    }
}

/// The paired positive for the Magus of the Mind row above: Gix, Yawgmoth
/// Praetor prints the IDENTICAL coordinated grammar ("you may play lands and
/// cast spells from among cards exiled this way without paying their mana
/// costs") with NO duration, and its own ruling is the resolution-scoped one —
/// "You must play the cards as you resolve the last ability. You can't wait and
/// play them later."
///
/// Without this row, "make Magus lingering" could be satisfied by a fix that
/// simply excluded the whole coordinated `play lands and cast spells` shape,
/// silently regressing Gix back to the indefinite permission this change exists
/// to remove.
#[test]
fn an_undated_coordinated_play_and_cast_grant_stays_a_resolution_window() {
    let parsed = parse(
        GIX_YAWGMOTH_PRAETOR,
        "Gix, Yawgmoth Praetor",
        &["Creature", "Legendary"],
    );
    let Effect::CastFromZone { driver, .. } = parsed_cast_from_zone(&parsed) else {
        unreachable!("helper returns CastFromZone")
    };
    assert!(
        driver.window_bounds().is_some(),
        "Gix states no duration, so its cast half must stay resolution-scoped, got {driver:?}"
    );
}

/// CR 611.2a + CR 118.9: the anaphors that legitimately stay lingering
/// permissions.
///
/// Apex of Power states a DURATION ("Until end of turn, you may cast spells from
/// among them"), and its own ruling confirms the permission outlives the
/// resolution: "Apex of Power doesn't change WHEN you can cast the exiled cards
/// … if you exile a sorcery card, you can cast it only during your main phase
/// when the stack is empty." Silent-Blade Oni binds a hand pool rather than an
/// exile batch and is routed by `open_private_zone_cast_selection`, whose
/// resolution-time hand pick already casts during resolution.
#[test]
fn duration_and_hand_anaphors_stay_lingering_permissions() {
    for (name, oracle, types) in [
        ("Apex of Power", APEX, &["Sorcery"][..]),
        ("Silent-Blade Oni", SILENT_BLADE, &["Creature"][..]),
    ] {
        let parsed = parse(oracle, name, types);
        assert!(
            matches!(
                parsed_cast_from_zone(&parsed),
                Effect::CastFromZone {
                    driver: CastFromZoneDriver::LingeringPermission,
                    ..
                }
            ),
            "{name} must keep the lingering permission mechanism"
        );
    }
}

/// CR 608.2g: the singular non-exile batch anaphors ("target opponent mills five
/// cards / reveals the top seven cards of their library. You may cast an instant
/// or sorcery spell from among them without paying its mana cost") are
/// resolution-scoped one-shot windows bounded at ONE cast, not lingering
/// permissions.
///
/// Jace's Mindseeker ruling: "If you cast an instant or sorcery card this way,
/// you do so while the ability of Jace's Mindseeker is resolving. If you choose
/// not to (or can't), you won't get a chance to cast one later."
/// Talent of the Telepath ruling: "You cast the instant and/or sorcery card(s)
/// from your opponent's library as Talent of the Telepath is resolving."
#[test]
fn singular_batch_anaphors_are_one_shot_resolution_windows() {
    for (name, oracle, types) in [
        ("Talent of the Telepath", TALENT, &["Sorcery"][..]),
        ("Jace's Mindseeker", JACE, &["Creature"][..]),
    ] {
        let parsed = parse(oracle, name, types);
        assert!(
            matches!(
                parsed_cast_from_zone(&parsed),
                Effect::CastFromZone {
                    driver: CastFromZoneDriver::ResolutionWindow {
                        bounds: ResolutionCastWindow {
                            max_casts: Some(1),
                            max_total_mv: None,
                        },
                    },
                    ..
                }
            ),
            "{name} must be a one-shot resolution-scoped window"
        );
    }
}

/// CR 608.2g + CR 608.2c + CR 202.3: the exile-batch class and the two bound axes
/// its Oracle grammar states independently.
///
/// Every card here has a published WotC ruling saying the casts happen during
/// the source's resolution and cannot be deferred (Hazoret's Undying Fury: "you
/// do so as part of the resolution … You can't wait to cast them later in the
/// turn"; Collected Conjuring: "You must cast any of the exiled cards you wish
/// to cast while Collected Conjuring is resolving"; Improvisation Capstone: "You
/// cast the spells while Improvisation Capstone is resolving and still on the
/// stack"; Villainous Wealth: "You can't wait to cast them later in the turn";
/// Primeval Spawn: "The spells are cast one after the other during the
/// resolution of Primeval Spawn's last ability").
#[test]
fn exile_batch_anaphors_are_resolution_windows_with_their_stated_bounds() {
    for (name, oracle, types, expected) in [
        (
            "Hazoret's Undying Fury",
            HAZORET,
            &["Sorcery"][..],
            ResolutionCastWindow::default(),
        ),
        (
            "Improvisation Capstone",
            CAPSTONE,
            &["Sorcery"][..],
            ResolutionCastWindow::default(),
        ),
        (
            "Epic Experiment",
            EPIC_EXPERIMENT,
            &["Sorcery"][..],
            ResolutionCastWindow::default(),
        ),
        (
            "Villainous Wealth",
            VILLAINOUS_WEALTH,
            &["Sorcery"][..],
            ResolutionCastWindow::default(),
        ),
        // CR 608.2c: an explicit printed cast cap.
        (
            "Collected Conjuring",
            COLLECTED_CONJURING,
            &["Sorcery"][..],
            ResolutionCastWindow {
                max_casts: Some(2),
                max_total_mv: None,
            },
        ),
        // CR 202.3: a running TOTAL budget, a different axis from the per-spell
        // ceiling the other rows carry as a `CastPermissionConstraint`.
        (
            "Primeval Spawn",
            PRIMEVAL_SPAWN,
            &["Creature"][..],
            ResolutionCastWindow {
                max_casts: None,
                max_total_mv: Some(10),
            },
        ),
        // CR 608.2c: "a spell" is an implicit cap of one, reached through the
        // subject-phrase "may" seam rather than the leading "you may".
        (
            "Itazura, Lingering Wick",
            ITAZURA,
            &["Creature"][..],
            ResolutionCastWindow {
                max_casts: Some(1),
                max_total_mv: None,
            },
        ),
    ] {
        let parsed = parse(oracle, name, types);
        let Effect::CastFromZone { driver, .. } = parsed_cast_from_zone(&parsed) else {
            unreachable!("helper returns CastFromZone")
        };
        assert_eq!(
            driver.window_bounds(),
            Some(expected),
            "{name} must lower to a resolution-scoped window with its stated bounds"
        );
    }
}

/// CR 608.2d: the printed "may" on a resolution-scoped batch grant IS the
/// window's own accept/decline, so the grant must never ALSO be lowered as an
/// optional `AbilityDefinition`.
///
/// `cast_from_zone::resolve` turns a `ResolutionWindow` driver into a
/// `CastOfferKind::FreeCastWindow`, whose decline option ("cast nothing") is
/// exactly the choice the printed "may" describes. A surviving
/// `def.optional = true` wraps that offer in a generic `OptionalEffectChoice`,
/// so the controller answers the same question twice — and, worse, an
/// automated/declining actor answers the OUTER prompt and never sees the
/// window at all.
///
/// The class reaches `def.optional` through **two independent seams**, and both
/// are pinned here because a carve-out added to one silently leaves the other
/// broken (this is exactly how Itazura survived the first pass):
///
/// 1. The chunk-level flag (`oracle_effect/mod.rs`) — the leading "**You may**
///    cast …" every other member prints.
/// 2. The subject-phrase modal (`oracle_effect/assembly.rs`,
///    `clause_ir.parsed.optional`) — Itazura's "…, then **they may** cast a
///    spell from among those cards".
///
/// Fail-on-revert: drop either carve-out and the corresponding rows flip to
/// `optional == true`.
#[test]
fn resolution_window_grants_are_never_double_wrapped_in_an_optional_choice() {
    for (name, oracle, types) in [
        // Seam 1: leading "you may".
        ("Epic Experiment", EPIC_EXPERIMENT, &["Sorcery"][..]),
        ("Hazoret's Undying Fury", HAZORET, &["Sorcery"][..]),
        ("Collected Conjuring", COLLECTED_CONJURING, &["Sorcery"][..]),
        ("Villainous Wealth", VILLAINOUS_WEALTH, &["Sorcery"][..]),
        ("Improvisation Capstone", CAPSTONE, &["Sorcery"][..]),
        ("Primeval Spawn", PRIMEVAL_SPAWN, &["Creature"][..]),
        ("Talent of the Telepath", TALENT, &["Sorcery"][..]),
        ("Jace's Mindseeker", JACE, &["Creature"][..]),
        (
            "Gix, Yawgmoth Praetor",
            GIX_YAWGMOTH_PRAETOR,
            &["Creature"][..],
        ),
        // Seam 2: subject-phrase "they may".
        ("Itazura, Lingering Wick", ITAZURA, &["Creature"][..]),
    ] {
        let parsed = parse(oracle, name, types);
        let def = parsed_cast_from_zone_def(&parsed);
        // Reach guard: only assert optionality for grants that actually became
        // resolution-scoped windows, so a driver regression can't make this test
        // pass vacuously.
        assert!(
            matches!(
                def.effect.as_ref(),
                Effect::CastFromZone {
                    driver: CastFromZoneDriver::ResolutionWindow { .. },
                    ..
                }
            ),
            "reach guard: {name} must lower to a resolution-scoped window"
        );
        assert!(
            !def.optional,
            "{name}: the printed \"may\" belongs to the free-cast window's own \
             accept/decline (CR 608.2d); an OptionalEffectChoice wrapper would \
             prompt twice for one choice and let a decline swallow the window"
        );
        assert_eq!(
            def.optional_for, None,
            "{name}: an unwrapped grant carries no optional actor scope"
        );
    }
}

#[test]
fn dig_peek_suffix_constraints_and_negative_siblings() {
    for (name, oracle, expected) in [
        ("Collected Conjuring", COLLECTED_CONJURING, 3),
        ("Hazoret's Undying Fury", HAZORET, 5),
    ] {
        let parsed = parse(oracle, name, &["Sorcery"]);
        assert!(matches!(
            parsed_cast_from_zone(&parsed),
            Effect::CastFromZone {
                constraint: Some(CastPermissionConstraint::ManaValue {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value },
                }),
                ..
            } if *value == expected
        ));
    }

    // CR 202.3 + CR 608.2g: the PER-SPELL ceiling rides on the grant's
    // constraint ("with mana value X or less"), while the resolution-scoped
    // mechanism rides on the driver. Both axes are asserted together so neither
    // can regress silently.
    let epic = parse(EPIC_EXPERIMENT, "Epic Experiment", &["Sorcery"]);
    assert!(matches!(
        parsed_cast_from_zone(&epic),
        Effect::CastFromZone {
            driver: CastFromZoneDriver::ResolutionWindow { .. },
            constraint: Some(CastPermissionConstraint::ManaValue {
                comparator: Comparator::LE,
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name },
                },
            }),
            ..
        } if name == "X"
    ));

    for (name, oracle) in [
        ("Primeval Spawn", PRIMEVAL_SPAWN),
        ("Improvisation Capstone", CAPSTONE),
        ("Founding the Third Path", FOUNDING),
    ] {
        let parsed = parse(oracle, name, &["Sorcery"]);
        let Effect::CastFromZone { constraint, .. } = parsed_cast_from_zone(&parsed) else {
            unreachable!("helper returns CastFromZone")
        };
        assert!(
            constraint.is_none(),
            "{name} must not gain this suffix constraint"
        );
    }
}

fn reach_kiora_library_choice() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
        .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA);
    let legal = scenario
        .add_spell_to_library_top(P0, "Kiora Legal Spell", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let rest = scenario
        .add_spell_to_library_top(P0, "Kiora Illegal Spell", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 2 life.")
        .id();
    let kraken = scenario
        .add_creature_to_hand(P0, "Triggering Kraken", 2, 2)
        .with_subtypes(vec!["Kraken"])
        .with_mana_cost(ManaCost::generic(2))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, kraken, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner.cast(kraken).commit();
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Kiora's optional cast must succeed");
    (runner, legal, rest)
}

#[test]
fn kiora_accept_casts_during_resolution_and_bottoms_the_rest() {
    let (mut runner, legal, rest) = reach_kiora_library_choice();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("Kiora must park the private library choice")
    };
    assert_eq!(zone, Zone::Library);
    assert_eq!(cards, vec![legal], "MV equal to X is not legal for Kiora");
    runner
        .act(GameAction::SelectCards { cards: vec![legal] })
        .expect("choosing Kiora's legal spell must succeed");
    assert_eq!(runner.state().objects[&legal].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
    assert!(
        runner.state().objects[&rest].casting_permissions.is_empty(),
        "the unchosen library card must not receive a cast permission"
    );
}

#[test]
fn kiora_decline_bottoms_every_looked_at_card_without_a_permission() {
    let (mut runner, legal, rest) = reach_kiora_library_choice();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("Kiora decline must reach the private library choice")
    };
    assert_eq!(zone, Zone::Library);
    assert_eq!(cards, vec![legal]);

    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining Kiora's cast must succeed");

    assert_eq!(runner.state().objects[&legal].zone, Zone::Library);
    assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
    assert!(
        runner.state().objects[&legal]
            .casting_permissions
            .is_empty()
            && runner.state().objects[&rest].casting_permissions.is_empty(),
        "declining the one-shot cast must leave no standing permission"
    );
}

#[test]
fn kiora_zero_eligible_cards_bottom_without_parking_a_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
        .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA);
    let equal_to_x = scenario
        .add_spell_to_library_top(P0, "Kiora Equal Spell", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let kraken = scenario
        .add_creature_to_hand(P0, "One-Mana Triggering Kraken", 1, 1)
        .with_subtypes(vec!["Kraken"])
        .with_mana_cost(ManaCost::generic(1))
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Colorless, kraken, false, vec![])],
    );

    let mut runner = scenario.build();
    runner.cast(kraken).commit();
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Kiora's optional cast must succeed");

    assert_eq!(
        runner.state().last_revealed_ids,
        vec![equal_to_x],
        "Kiora's look must run before the empty eligible pool auto-bottoms"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "no legal MV < X spell must not open an empty choice"
    );
    assert_eq!(runner.state().objects[&equal_to_x].zone, Zone::Library);
    assert!(
        runner.state().objects[&equal_to_x]
            .casting_permissions
            .is_empty(),
        "an ineligible looked-at card must not receive a permission"
    );
}

fn reach_kiora_multi_candidate_choice() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
        .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA);
    let legal_one = scenario
        .add_spell_to_library_top(P0, "Kiora Legal One", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let legal_two = scenario
        .add_spell_to_library_top(P0, "Kiora Legal Two", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let equal_to_x = scenario
        .add_spell_to_library_top(P0, "Kiora Equal Spell", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("You gain 1 life.")
        .id();
    let kraken = scenario
        .add_creature_to_hand(P0, "Triggering Kraken", 3, 3)
        .with_subtypes(vec!["Kraken"])
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, kraken, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner.cast(kraken).commit();
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Kiora's optional cast must succeed");
    (runner, legal_one, legal_two, equal_to_x)
}

#[test]
fn kiora_multi_candidate_choice_casts_exactly_one_and_bottoms_the_rest() {
    let (mut runner, legal_one, legal_two, equal_to_x) = reach_kiora_multi_candidate_choice();
    let library_before = runner.state().players[P0.0 as usize].library.len();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("multiple eligible Kiora cards must reach the private choice")
    };
    assert_eq!(zone, Zone::Library);
    assert_eq!(cards.len(), 2);
    assert!(cards.contains(&legal_one) && cards.contains(&legal_two));
    assert!(!cards.contains(&equal_to_x));

    runner
        .act(GameAction::SelectCards {
            cards: vec![legal_one],
        })
        .expect("choosing one of Kiora's eligible spells must succeed");

    assert_eq!(runner.state().objects[&legal_one].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&legal_two].zone, Zone::Library);
    assert_eq!(runner.state().objects[&equal_to_x].zone, Zone::Library);
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        library_before - 1,
        "exactly the selected spell leaves the looked-at library set"
    );
}

#[test]
fn kiora_bottom_order_is_deterministic_under_a_fixed_seed() {
    let run_once = || {
        let mut scenario = GameScenario::new_with_format(FormatConfig::standard(), 2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        scenario
            .add_creature(P0, "Kiora, Sovereign of the Deep", 4, 5)
            .from_oracle_text_with_keywords(&["vigilance", "ward {3}"], KIORA);
        for name in ["Kiora First", "Kiora Second", "Kiora Third"] {
            scenario
                .add_spell_to_library_top(P0, name, false)
                .with_mana_cost(ManaCost::generic(1))
                .from_oracle_text("You gain 1 life.");
        }
        let kraken = scenario
            .add_creature_to_hand(P0, "Three-Mana Triggering Kraken", 3, 3)
            .with_subtypes(vec!["Kraken"])
            .with_mana_cost(ManaCost::generic(3))
            .id();
        scenario.with_mana_pool(
            P0,
            (0..3)
                .map(|_| ManaUnit::new(ManaType::Colorless, kraken, false, vec![]))
                .collect(),
        );

        let mut runner = scenario.build();
        runner.cast(kraken).commit();
        runner.resolve_top();
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("accepting Kiora's optional cast must succeed");
        let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
        else {
            panic!("three looked-at Kiora cards must reach the private choice")
        };
        assert_eq!(zone, Zone::Library);
        assert_eq!(cards.len(), 3);
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .expect("declining Kiora's cast must bottom the looked-at cards");
        runner.state().players[P0.0 as usize].library.clone()
    };

    assert_eq!(
        run_once(),
        run_once(),
        "the same seeded Kiora setup must randomize its bottom order deterministically"
    );
}

#[test]
fn svella_activated_peek_casts_one_spell_and_bottoms_the_rest() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let svella = scenario
        .add_creature(P0, "Svella, Ice Shaper", 2, 4)
        .from_oracle_text(SVELLA)
        .id();
    let chosen = scenario
        .add_spell_to_library_top(P0, "Svella Chosen Spell", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    let rest = scenario
        .add_spell_to_library_top(P0, "Svella Rest Spell", false)
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text("You gain 1 life.")
        .id();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Colorless, svella, false, vec![]))
            .chain([
                ManaUnit::new(ManaType::Red, svella, false, vec![]),
                ManaUnit::new(ManaType::Green, svella, false, vec![]),
            ])
            .collect(),
    );

    let mut runner = scenario.build();
    let outcome = runner.activate(svella, 0).accept_optional().resolve();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = outcome.final_waiting_for() else {
        panic!("Svella's activated ability must reach the library cast choice")
    };
    assert_eq!(*zone, Zone::Library);
    assert!(cards.contains(&chosen) && cards.contains(&rest));

    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("choosing Svella's free spell must succeed");
    assert_eq!(runner.state().objects[&chosen].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&rest].zone, Zone::Library);
}

#[test]
fn perception_bobblehead_excludes_mana_value_four() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bobblehead = scenario
        .add_creature(P0, "Perception Bobblehead", 1, 1)
        .as_artifact()
        .with_subtypes(vec!["Bobblehead"])
        .from_oracle_text(BOBBLEHEAD)
        .id();
    scenario
        .add_creature(P0, "Perception Bobblehead", 1, 1)
        .as_artifact()
        .with_subtypes(vec!["Bobblehead"]);
    let mana_value_three = scenario
        .add_spell_to_library_top(P0, "Bobblehead MV3", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mana_value_four = scenario
        .add_spell_to_library_top(P0, "Bobblehead MV4", false)
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text("You gain 1 life.")
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, bobblehead, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    let look_ability_index = runner.state().objects[&bobblehead]
        .abilities
        .iter()
        .position(|definition| matches!(definition.effect.as_ref(), Effect::Dig { .. }))
        .expect("the verbatim Bobblehead Oracle text must produce its look ability");
    let outcome = runner
        .activate(bobblehead, look_ability_index)
        .accept_optional()
        .resolve();
    let WaitingFor::EffectZoneChoice { cards, zone, .. } = outcome.final_waiting_for() else {
        panic!("Bobblehead's look must reach a library cast choice")
    };
    assert_eq!(*zone, Zone::Library);
    assert!(cards.contains(&mana_value_three));
    assert!(
        !cards.contains(&mana_value_four),
        "Bobblehead's fixed mana-value cap must exclude MV 4"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![mana_value_three],
        })
        .expect("choosing the MV 3 Bobblehead spell must succeed");
    assert_eq!(runner.state().objects[&mana_value_three].zone, Zone::Stack);
    assert_eq!(runner.state().objects[&mana_value_four].zone, Zone::Library);
}

#[test]
fn velomachus_power_constraint_is_frozen_before_the_library_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let velomachus = scenario
        .add_creature(P0, "Velomachus Lorehold", 5, 5)
        .from_oracle_text_with_keywords(&["flying", "vigilance", "haste"], VELOMACHUS)
        .id();
    let mana_value_five = scenario
        .add_spell_to_library_top(P0, "Velomachus MV5", false)
        .with_mana_cost(ManaCost::generic(5))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mana_value_six = scenario
        .add_spell_to_library_top(P0, "Velomachus MV6", false)
        .with_mana_cost(ManaCost::generic(6))
        .from_oracle_text("You gain 1 life.")
        .id();
    for index in 0..5 {
        scenario
            .add_spell_to_library_top(P0, &format!("Velomachus Filler {index}"), false)
            .with_mana_cost(ManaCost::generic(6))
            .from_oracle_text("You gain 1 life.");
    }

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: vec![velomachus],
        valid_attack_targets: vec![AttackTarget::Player(P1)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    runner
        .declare_attackers(&[(velomachus, AttackTarget::Player(P1))])
        .expect("Velomachus must be able to attack");
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Velomachus's optional cast must succeed");

    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("Velomachus's attack trigger must reach the library cast choice")
    };
    assert_eq!(zone, Zone::Library);
    assert!(cards.contains(&mana_value_five));
    assert!(
        !cards.contains(&mana_value_six),
        "Velomachus at power 5 must exclude a mana-value 6 spell"
    );
}

/// CR 608.2h + CR 608.2g: Epic Experiment freezes X once as it resolves, offers
/// the within-ceiling exiled card through its resolution-scoped window, and then
/// runs its own trailing "Then put all cards exiled this way that weren't cast
/// into your graveyard" instruction in the SAME resolution.
///
/// The continuation half is the load-bearing part: converting the grant into a
/// free-cast window must not swallow the chain's remaining sub-ability, or every
/// uncast card would be stranded in exile forever.
#[test]
fn epic_experiment_window_casts_within_x_then_runs_its_uncast_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let epic = scenario
        .add_spell_to_hand_from_oracle(P0, "Epic Experiment", false, EPIC_EXPERIMENT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    let mana_value_two = scenario
        .add_spell_to_library_top(P0, "Epic MV2", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mana_value_three = scenario
        .add_spell_to_library_top(P0, "Epic MV3", false)
        .with_mana_cost(ManaCost::generic(3))
        .from_oracle_text("You gain 1 life.")
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, epic, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(epic).x(2).accept_optional().resolve();

    // CR 608.2h: X was determined once, as Epic Experiment resolved, and bounds
    // the offer. Both exiled cards are instants/sorceries, so only the frozen
    // ceiling can separate them.
    let WaitingFor::CastOffer {
        kind: CastOfferKind::FreeCastWindow { candidates, .. },
        ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!(
            "Epic Experiment must park its resolution-scoped window, got {:?}",
            outcome.final_waiting_for()
        )
    };
    assert!(
        candidates.contains(&mana_value_two),
        "mana value 2 <= X = 2 must be offered; offered = {candidates:?}"
    );
    assert!(
        !candidates.contains(&mana_value_three),
        "mana value 3 > X = 2 must not be offered; offered = {candidates:?}"
    );

    // CR 608.2g: the chosen card is cast as Epic Experiment resolves.
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(mana_value_two),
        })
        .expect("free-casting the within-ceiling card must succeed");
    assert_eq!(
        runner.state().objects[&mana_value_two].zone,
        Zone::Stack,
        "the chosen card goes onto the stack during Epic Experiment's resolution"
    );

    // Declining the rest closes the window; Epic Experiment then finishes
    // resolving and runs its own trailing instruction.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { .. },
            ..
        }
    ) {
        runner
            .act(GameAction::FreeCastWindowChoice { selection: None })
            .expect("declining the remaining casts must succeed");
    }

    // CR 608.2c: "Then put all cards exiled this way that weren't cast into your
    // graveyard" — the continuation must survive the window conversion.
    assert_eq!(
        runner.state().objects[&mana_value_three].zone,
        Zone::Graveyard,
        "the uncast exiled card must be swept to the graveyard by the trailing \
         instruction, not stranded in exile"
    );

    for _ in 0..24 {
        if runner.state().stack.is_empty() {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    assert_eq!(
        runner.state().objects[&mana_value_two].zone,
        Zone::Graveyard,
        "the free-cast spell resolves and goes to its owner's graveyard"
    );
}

/// CR 608.2d + CR 608.2c: DECLINING the whole window must still run the
/// mandatory trailing cleanup.
///
/// Two things are pinned here, both on the production cast path:
///
/// 1. **One prompt, not two.** This test deliberately does NOT call
///    `accept_optional()`, so the driver's default `OptionalPolicy::Decline`
///    answers any `OptionalEffectChoice` it meets. If the resolution-scoped
///    driver were still wrapped in a generic "you may cast…?" prompt (the
///    optionality-reconciliation entry removed), that prompt would be declined
///    and NO window would ever open — the `WaitingFor::CastOffer` assertion below
///    fails. The printed "may" belongs to the window's own accept/decline.
/// 2. **Cleanup on total decline.** "Then put all cards exiled this way that
///    weren't cast into your graveyard" is a separate, MANDATORY instruction
///    (CR 608.2c in-order instructions); it is not conditioned on any card having
///    been cast. Declining every cast must still sweep the whole batch, not
///    strand it in exile.
///
/// The sibling test above covers the accept-then-decline path; this one covers
/// decline-everything, which was previously unexercised.
#[test]
fn epic_experiment_decline_still_runs_its_uncast_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let epic = scenario
        .add_spell_to_hand_from_oracle(P0, "Epic Experiment", false, EPIC_EXPERIMENT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    let mana_value_two = scenario
        .add_spell_to_library_top(P0, "Epic Decline MV2", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mana_value_one = scenario
        .add_spell_to_library_top(P0, "Epic Decline MV1", false)
        .with_mana_cost(ManaCost::generic(1))
        .from_oracle_text("You gain 1 life.")
        .id();
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, epic, false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    let outcome = runner.cast(epic).x(2).resolve();

    let WaitingFor::CastOffer {
        kind: CastOfferKind::FreeCastWindow { candidates, .. },
        ..
    } = outcome.final_waiting_for().clone()
    else {
        panic!(
            "Epic Experiment must park its resolution-scoped window as the SOLE \
             \"you may\" prompt, got {:?}",
            outcome.final_waiting_for()
        )
    };
    // Reach guard: both exiled cards are within X = 2 and are legal offers, so the
    // decline below is a real decision rather than an empty prompt.
    assert!(
        candidates.contains(&mana_value_one) && candidates.contains(&mana_value_two),
        "reach guard: both exiled cards must be offered; offered = {candidates:?}"
    );

    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the whole window must succeed");

    for card in [mana_value_one, mana_value_two] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Graveyard,
            "CR 608.2c: the mandatory trailing cleanup must sweep every uncast \
             exiled card even when the window is declined outright, not strand \
             it in exile"
        );
        assert!(
            runner.state().objects[&card].casting_permissions.is_empty(),
            "declining the resolution-scoped window must leave no standing \
             casting permission behind"
        );
    }
}

/// CR 608.2d, the SUBJECT-PHRASE "may" seam at runtime.
///
/// Itazura, Lingering Wick's upkeep trigger reaches its cast grant through the
/// second optionality seam (`clause_ir.parsed.optional` in
/// `oracle_effect/assembly.rs`) because its "may" sits on a subject phrase —
/// "…, then **they** may cast a spell from among those cards without paying its
/// mana cost" — rather than leading the clause. The chunk-level carve-out that
/// covers every other member of the class does not reach it.
///
/// What this pins, on verbatim Oracle text driven end to end through the real
/// upkeep trigger: **no `OptionalEffectChoice` is ever raised.** The printed
/// "may" belongs to the free-cast window's own accept/decline, so a surviving
/// `def.optional = true` shows up here as a generic "do you want to do this?"
/// prompt in front of the grant — both a second prompt for one choice and, for
/// any declining actor, a prompt that swallows the window outright. The loop
/// below treats that prompt as a failure.
///
/// It also pins that the trigger runs to completion: the damage lands
/// (CR 120.3a — proof the chain reached the cast clause) and the MANDATORY
/// trailing instruction, "You put a card from among them that wasn't cast this
/// way into your hand", still moves the batch out of exile (CR 608.2c).
///
/// KNOWN RESIDUALS, deliberately asserted *around* rather than locked in:
///
/// * Itazura's window does not currently open at all, for a reason that
///   predates this change and is independent of the driver: the cast clause's
///   anaphor lowers to `TargetFilter::ExiledBySource`, which resolves from the
///   trigger's CR 608.2h LKI `linked_exile_snapshot` — captured when the trigger
///   was put on the stack, i.e. BEFORE this same trigger's own `ExileTop`
///   instruction ran — so the batch reads as empty and `cast_from_zone::resolve`
///   takes its no-targets exit. The trailing cleanup gets the batch right
///   because it binds to the resolution's tracked set (`TrackedSetFiltered`)
///   instead. Under the previous lowering the clause was equally dead (an empty
///   target list reached `grant_lingering_permissions` and stamped nothing) — it
///   merely also cost the controller a meaningless prompt, which is what this
///   change removes. Binding the cast anaphor to the tracked set is a separate
///   fix; the assertions below say the batch leaves exile without saying WHICH
///   mechanism offered it, so they will not need rewriting when that lands.
/// * `free_cast_from_zones::resolve` would offer the window to the ability's
///   controller, not to the damaged opponent the card names ("**they** may
///   cast"). Also pre-existing, also untouched here.
#[test]
fn itazura_raises_no_redundant_optional_prompt_and_still_runs_its_hand_instruction() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);
    scenario.add_creature_from_oracle(P0, "Itazura, Lingering Wick", 3, 3, ITAZURA);

    // Keep P0's library non-empty through the draw step (CR 104.3c). Seeded
    // FIRST so the three cards below sit above it and are the ones exiled.
    scenario.with_library_top(P0, &["Itazura Filler"]);
    // The three cards the trigger exiles. Real castable spells, so a surviving
    // permission would be observable rather than vacuous. Instants, because the
    // trigger resolves in the upkeep step and the offer would still enforce the
    // card's own timing (CR 307.1).
    let exiled: Vec<ObjectId> = ["Itazura Top 1", "Itazura Top 2", "Itazura Top 3"]
        .into_iter()
        .map(|name| {
            scenario
                .add_spell_to_library_top(P0, name, true)
                .with_mana_cost(ManaCost::generic(1))
                .from_oracle_text("You gain 1 life.")
                .id()
        })
        .collect();

    let mut runner = scenario.build();
    // A pre-existing permanent whose upkeep trigger fires on the coming turn.
    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.auto_advance_to_main_phase();

    let mut chose_a_number = false;
    for _ in 0..256 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice {
                player,
                options,
                choice_type,
                ..
            } => {
                let choice = match choice_type {
                    // "Each opponent secretly chooses a number 0 or greater."
                    ChoiceType::NumberRange { .. } => {
                        assert_eq!(player, P1, "only opponents choose a number");
                        chose_a_number = true;
                        "3".to_string()
                    }
                    // "Choose an opponent with the highest number." — P1 is the
                    // only opponent, so trivially the highest.
                    ChoiceType::Opponent { .. } => options
                        .first()
                        .cloned()
                        .expect("an opponent must be offered"),
                    other => panic!("unexpected choice {other:?}"),
                };
                runner
                    .act(GameAction::ChooseOption { choice })
                    .unwrap_or_else(|e| panic!("answering {player:?} must succeed: {e:?}"));
            }
            // Should the anaphor residual documented above be fixed, the window
            // opens here. Casting nothing keeps this test about the prompt shape
            // while still exercising the trailing instruction.
            WaitingFor::CastOffer {
                kind: CastOfferKind::FreeCastWindow { .. },
                ..
            } => {
                runner
                    .act(GameAction::FreeCastWindowChoice { selection: None })
                    .expect("declining the window must succeed");
            }
            WaitingFor::OptionalEffectChoice { .. } => panic!(
                "CR 608.2d: the printed \"then they may cast\" is the free-cast \
                 window's own accept/decline, not a separate OptionalEffectChoice. \
                 This prompt is the double prompt the ResolutionWindow carve-out in \
                 `assembly.rs` removes — and a declining actor answering it would \
                 never reach the grant at all."
            ),
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().phase != Phase::Upkeep {
                    break;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }

    // Reach guard: the trigger really did resolve, rather than never firing and
    // making the "no OptionalEffectChoice" assertion vacuous.
    assert!(
        chose_a_number,
        "the trigger must reach its secret-number choice"
    );
    // CR 120.3a: "Itazura deals that much damage to them" — the instruction
    // immediately before the cast clause, so the chain reached that clause.
    assert_eq!(
        runner.life(P1),
        17,
        "P1 chose 3 and takes exactly that much"
    );

    // CR 608.2c: the mandatory trailing instruction runs whether or not anything
    // was cast.
    for card in &exiled {
        let object = &runner.state().objects[card];
        assert_ne!(
            object.zone,
            Zone::Exile,
            "the mandatory \"put a card from among them … into your hand\" \
             instruction must move the batch out of exile"
        );
        assert!(
            object.casting_permissions.is_empty(),
            "a resolution-scoped grant must leave no standing casting permission \
             behind once the trigger has finished resolving"
        );
    }
}

/// Issue #6880: the "from among them" cast anaphor must carry the clause's
/// card-type restriction, not just its mana-value constraint.
///
/// CR 601.3: "A player can begin to cast a spell only if a rule or effect
/// allows that player to cast it." Velomachus Lorehold allows casting only *an
/// instant or sorcery spell*, so the type gate is part of the cast-legality
/// predicate exactly as much as the mana-value bound is. The parser bound the
/// permission to a bare `TargetFilter::ExiledBySource`, dropping the type leg
/// entirely — the mana-value ceiling survived as a `CastPermissionConstraint`
/// while any card type at or below that ceiling became castable.
///
/// The composed shape mirrors the already-correct
/// "from among cards exiled with [self]" sibling branch: the typed leg AND the
/// exile-set anaphor.
fn cast_target_of(oracle: &str, name: &str, types: &[&str]) -> TargetFilter {
    let parsed = parse(oracle, name, types);
    let Effect::CastFromZone { target, .. } = parsed_cast_from_zone(&parsed) else {
        unreachable!("helper returns CastFromZone")
    };
    target.clone()
}

#[test]
fn from_among_them_cast_retains_the_instant_or_sorcery_gate() {
    let instant_or_sorcery = TypeFilter::AnyOf(vec![TypeFilter::Instant, TypeFilter::Sorcery]);

    for (name, oracle, types) in [
        ("Velomachus Lorehold", VELOMACHUS, &["Creature"][..]),
        ("Jace's Mindseeker", JACE, &["Creature"][..]),
        ("Talent of the Telepath", TALENT, &["Sorcery"][..]),
    ] {
        let target = cast_target_of(oracle, name, types);
        let TargetFilter::And { filters } = &target else {
            panic!(
                "{name}: the cast permission must AND the card-type gate with the \
                 exile-set anaphor, got {target:?}"
            );
        };
        assert!(
            filters.contains(&TargetFilter::ExiledBySource),
            "{name}: the exile-set anaphor leg must survive the composition, got {filters:?}"
        );
        let typed = filters
            .iter()
            .find_map(|f| match f {
                TargetFilter::Typed(tf) => Some(tf),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{name}: expected a typed leg, got {filters:?}"));
        assert_eq!(
            typed.type_filters,
            vec![instant_or_sorcery.clone()],
            "{name}: the clause restricts the cast to instant or sorcery spells"
        );
    }
}

/// Sibling guard: an *untyped* "cast a spell from among them" clause (Svella,
/// Aetherworks Marvel, Apex of Power, ... — the untyped majority of this
/// anaphor family) must keep its bare `ExiledBySource` binding. A type gate
/// synthesized where the Oracle text names no type would silently narrow every
/// one of those cards.
///
/// The last three rows are the hostile cases, and they are the point of this
/// test: Oracle text that carries a restrictive qualifier immediately next to
/// "spell"/"spells" which is NOT a card type. CR 601.3 does restrict those
/// casts, but along the color and mana-value axes — which this filter does not
/// model, and which `parse_cast_type_gate` must therefore never mistake for a
/// card type. Meeting of the Five ("spells with exactly three colors") probes
/// the color axis; Perception Bobblehead and Kiora ("a spell with mana value
/// N or less") probe the property axis that rides on
/// `CastPermissionConstraint` instead.
#[test]
fn untyped_from_among_them_cast_stays_a_bare_exile_anaphor() {
    for (name, oracle, types) in [
        ("Svella, Ice Shaper", SVELLA, &["Creature"][..]),
        ("Aetherworks Marvel", AETHERWORKS_MARVEL, &["Artifact"][..]),
        ("Apex of Power", APEX, &["Sorcery"][..]),
        ("Meeting of the Five", MEETING_OF_THE_FIVE, &["Sorcery"][..]),
        ("Perception Bobblehead", BOBBLEHEAD, &["Artifact"][..]),
        ("Kiora, Sovereign of the Deep", KIORA, &["Creature"][..]),
        // Issue #6960 rows: the grammar now consumes a leading quantifier, so
        // these clauses reach the leg list with `"spells"` in the leg position.
        // The head-noun guard yields zero legs there, which is what keeps them
        // bare. Without these rows the quantifier axis could swallow the whole
        // untyped majority of this family.
        ("Hazoret's Undying Fury", HAZORET, &["Sorcery"][..]),
        ("Primeval Spawn", PRIMEVAL_SPAWN, &["Creature"][..]),
        ("Improvisation Capstone", CAPSTONE, &["Sorcery"][..]),
    ] {
        assert_eq!(
            cast_target_of(oracle, name, types),
            TargetFilter::ExiledBySource,
            "{name} names no card type, so its cast permission must stay unrestricted"
        );
    }
}

/// Extracts the three legs of a hand-bound cast permission.
///
/// The hand-bound branch composes its filter from two independent sources: the
/// prior `Effect::RevealHand` clause supplies the zone and the revealed player,
/// and the cast clause supplies the card type. A test that read only one leg
/// would pass while the branch silently dropped another, so every assertion
/// below reads all three.
fn hand_bound_cast_filter(oracle: &str, name: &str, types: &[&str]) -> TypedFilter {
    match cast_target_of(oracle, name, types) {
        TargetFilter::Typed(tf) => tf,
        other => panic!(
            "{name}: a hand-reveal chain must bind the cast to the revealed hand \
             as a single typed filter, got {other:?}"
        ),
    }
}

/// The hand-bound half of issue #6880, which the exile-bound tests above do not
/// reach: `chain_prior_hand_reveal_target` is set (no exile producer ever ran),
/// so the anaphor resolves against the revealed player's hand rather than
/// `ExiledBySource`, and the type gate has to be grafted onto that filter
/// instead of AND-ed with an exile anaphor.
///
/// Mindclaw Shaman is the only type-gated card in that family. Pre-fix the
/// branch emitted a bare `TypeFilter::Card`, so "an instant or sorcery spell"
/// reached every card in the revealed hand — a creature or land was castable
/// for free, contrary to CR 601.3.
///
/// The branch OVERWRITES `type_filters` rather than appending, so the bare
/// `Card` head noun must be gone, not merely accompanied. Asserting equality on
/// the whole vector (not `contains`) is what pins that.
#[test]
fn hand_bound_cast_retains_the_instant_or_sorcery_gate() {
    let typed = hand_bound_cast_filter(MINDCLAW_SHAMAN, "Mindclaw Shaman", &["Creature"]);

    assert_eq!(
        typed.type_filters,
        vec![TypeFilter::AnyOf(vec![
            TypeFilter::Instant,
            TypeFilter::Sorcery
        ])],
        "the clause restricts the cast to instant or sorcery spells, and replaces \
         the bare `Card` head noun rather than joining it"
    );
    assert_eq!(
        typed.controller,
        Some(ControllerRef::Opponent),
        "the candidate cards belong to the opponent who revealed, not the caster"
    );
    assert!(
        typed
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }),
        "the cards never left the revealed hand, so the zone leg must survive the \
         type graft, got {:?}",
        typed.properties
    );
}

/// Sibling guard for the untyped majority of the hand-bound family.
///
/// Silent-Blade Oni and Mindleech Mass say "cast a spell from among those
/// cards" — no card type is named, so the permission is unrestricted and the
/// filter must keep its bare `Card` head noun. Synthesizing a type gate here
/// would silently narrow both cards.
///
/// Their zone and controller legs are asserted for the same reason as above:
/// this test also has to fail if the type graft is generalized in a way that
/// clobbers the hand binding.
#[test]
fn untyped_hand_bound_cast_keeps_a_bare_card_filter() {
    for (name, oracle) in [
        ("Silent-Blade Oni", SILENT_BLADE),
        ("Mindleech Mass", MINDLEECH_MASS),
    ] {
        let typed = hand_bound_cast_filter(oracle, name, &["Creature"]);

        assert_eq!(
            typed.type_filters,
            vec![TypeFilter::Card],
            "{name} names no card type, so its cast permission must stay unrestricted"
        );
        assert_eq!(
            typed.controller,
            Some(ControllerRef::TriggeringPlayer),
            "{name} looks at the hand of the player it damaged"
        );
        assert!(
            typed
                .properties
                .contains(&FilterProp::InZone { zone: Zone::Hand }),
            "{name}: the cards stay in the looked-at hand, got {:?}",
            typed.properties
        );
    }
}

/// The user-visible half of issue #6880, driven through the real attack-trigger
/// resolution pipeline rather than asserted on the AST.
///
/// Velomachus has power 5. The looked-at cards include a mana-value 3 *creature*
/// — comfortably inside the mana-value ceiling but outside the "instant or
/// sorcery" permission. Pre-fix the engine offered it; CR 601.3 says it was
/// never a legal choice.
///
/// Reach guard against a vacuous negative: the same assertion block requires the
/// legal mana-value 4 sorcery to BE offered, proving the choice was actually
/// opened and populated rather than short-circuited to an empty prompt.
#[test]
fn velomachus_does_not_offer_a_creature_inside_its_mana_value_ceiling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let velomachus = scenario
        .add_creature(P0, "Velomachus Lorehold", 5, 5)
        .from_oracle_text_with_keywords(&["flying", "vigilance", "haste"], VELOMACHUS)
        .id();
    let legal_sorcery = scenario
        .add_spell_to_library_top(P0, "Velomachus Legal Sorcery", false)
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text("You gain 1 life.")
        .id();
    // The trap: mana value 3 <= Velomachus's power 5, so only the card-type
    // gate can exclude it.
    let creature_inside_ceiling = scenario
        .add_spell_to_library_top(P0, "Velomachus Trap Creature", false)
        .with_mana_cost(ManaCost::generic(3))
        .as_creature()
        .id();
    for index in 0..5 {
        scenario
            .add_spell_to_library_top(P0, &format!("Velomachus Filler {index}"), false)
            .with_mana_cost(ManaCost::generic(6))
            .from_oracle_text("You gain 1 life.");
    }

    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P0,
        valid_attacker_ids: vec![velomachus],
        valid_attack_targets: vec![AttackTarget::Player(P1)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    runner
        .declare_attackers(&[(velomachus, AttackTarget::Player(P1))])
        .expect("Velomachus must be able to attack");
    runner.resolve_top();
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting Velomachus's optional cast must succeed");

    let WaitingFor::EffectZoneChoice { cards, zone, .. } = runner.state().waiting_for.clone()
    else {
        panic!("Velomachus's attack trigger must reach the library cast choice")
    };
    assert_eq!(zone, Zone::Library);
    assert!(
        cards.contains(&legal_sorcery),
        "reach guard: the legal instant-or-sorcery card must be offered, \
         otherwise the negative assertion below is vacuous; offered = {cards:?}"
    );
    assert!(
        !cards.contains(&creature_inside_ceiling),
        "CR 601.3: Velomachus permits casting only an instant or sorcery spell — \
         a creature within the mana-value ceiling must NEVER be offered \
         (issue #6880); offered = {cards:?}"
    );

    // Observable outcome: taking the only legal card puts it on the stack and
    // leaves the illegal creature in the library.
    runner
        .act(GameAction::SelectCards {
            cards: vec![legal_sorcery],
        })
        .expect("choosing Velomachus's legal sorcery must succeed");
    assert_eq!(runner.state().objects[&legal_sorcery].zone, Zone::Stack);
    assert_eq!(
        runner.state().objects[&creature_inside_ceiling].zone,
        Zone::Library,
        "the ineligible creature must stay in the library"
    );
    assert!(
        runner.state().objects[&creature_inside_ceiling]
            .casting_permissions
            .is_empty(),
        "the ineligible creature must not receive a casting permission"
    );
}

/// Runtime sibling guard for the untyped majority: Svella says "cast a spell",
/// so a creature in its looked-at set must STILL be offered. This is the
/// runtime counterpart of `untyped_from_among_them_cast_stays_a_bare_exile_anaphor`
/// and fails if the type gate is applied where no type was named.
#[test]
fn svella_untyped_peek_still_offers_a_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let svella = scenario
        .add_creature(P0, "Svella, Ice Shaper", 2, 4)
        .from_oracle_text(SVELLA)
        .id();
    let creature = scenario
        .add_spell_to_library_top(P0, "Svella Creature", false)
        .with_mana_cost(ManaCost::generic(2))
        .as_creature()
        .id();
    scenario.with_mana_pool(
        P0,
        (0..6)
            .map(|_| ManaUnit::new(ManaType::Colorless, svella, false, vec![]))
            .chain([
                ManaUnit::new(ManaType::Red, svella, false, vec![]),
                ManaUnit::new(ManaType::Green, svella, false, vec![]),
            ])
            .collect(),
    );

    let mut runner = scenario.build();
    let outcome = runner.activate(svella, 0).accept_optional().resolve();
    let WaitingFor::EffectZoneChoice { cards, .. } = outcome.final_waiting_for() else {
        panic!("Svella's activated ability must reach the library cast choice")
    };
    assert!(
        cards.contains(&creature),
        "Svella's untyped \"cast a spell\" permission must still reach a creature; \
         offered = {cards:?}"
    );
}

#[test]
fn kiora_library_choice_is_private_across_serde_round_trip() {
    let (runner, legal, rest) = reach_kiora_library_choice();
    let controller = filter_state_for_viewer(runner.state(), P0);
    let opponent = filter_state_for_viewer(runner.state(), P1);
    let WaitingFor::EffectZoneChoice { cards, .. } = &controller.waiting_for else {
        panic!("controller must retain Kiora's library choice")
    };
    assert_eq!(cards, &vec![legal]);
    assert_eq!(controller.objects[&legal].name, "Kiora Legal Spell");
    let WaitingFor::EffectZoneChoice { cards, .. } = &opponent.waiting_for else {
        panic!("opponent still sees a redacted choice envelope")
    };
    assert!(cards.iter().all(|id| *id == ObjectId(0)));
    assert_eq!(opponent.objects[&legal].name, "Hidden Card");
    assert_eq!(opponent.objects[&rest].name, "Hidden Card");

    let restored: engine::types::game_state::GameState = serde_json::from_str(
        &serde_json::to_string(runner.state()).expect("parked state serializes"),
    )
    .expect("parked state deserializes");
    let restored_opponent = filter_state_for_viewer(&restored, P1);
    let WaitingFor::EffectZoneChoice { cards, .. } = &restored_opponent.waiting_for else {
        panic!("restored opponent view must retain redaction")
    };
    assert!(cards.iter().all(|id| *id == ObjectId(0)));
    assert_eq!(restored_opponent.objects[&legal].name, "Hidden Card");
}

// ---------------------------------------------------------------------------
// Issue #6960 — `parse_cast_type_disjunction` missed conjunctive, counted, and
// subtype forms, so seven cards kept a bare (or `Any`) cast target and ANY card
// type could be cast from the exiled set.
//
// The helper is now a per-axis composed grammar
// (`opt(quantifier) opt(article) leg (sep leg)* head_noun`). These rows pin the
// three axes it unfroze, the anti-swallow acceptance boundary that keeps the
// untyped majority bare, and the runtime consequence.
// ---------------------------------------------------------------------------

const RAL_LEYLINE_PRODIGY: &str = "Ral enters with an additional loyalty counter on him for each instant and sorcery spell you've cast this turn.\n[+1]: Until your next turn, instant and sorcery spells you cast cost {1} less to cast.\n[\u{2212}2]: Ral deals 2 damage divided as you choose among one or two targets. Draw a card if you control a blue permanent other than Ral.\n[\u{2212}8]: Exile the top eight cards of your library. You may cast instant and sorcery spells from among them this turn without paying their mana costs.";
const KYLOX: &str = "Menace, ward {2}, haste\nWhenever Kylox attacks, sacrifice any number of other creatures, then exile the top X cards of your library, where X is their total power. You may cast any number of instant and/or sorcery spells from among the exiled cards without paying their mana costs.";
const SANWELL: &str = "As long as an artifact creature you control is attacking, prevent all damage that would be dealt to Sanwell.\nWhenever Sanwell becomes tapped, exile the top six cards of your library. You may cast a Vehicle or artifact creature spell from among them. Then put the rest on the bottom of your library in a random order.";
/// Sanwell's becomes-tapped trigger body with its head noun PLURALIZED, and nothing else
/// changed. Synthetic; called out as synthetic in the PR body.
///
/// Sanwell's own printed clause is REFUSED now — see
/// `real_cards_whose_printed_cap_no_mechanism_can_carry_are_refused`. It is a
/// PAID batch cast printing a cap of ONE over a batch of SIX, and no mechanism
/// this engine has can enforce that bound: the free-cast window models no mana
/// payment, and `LingeringPermission`'s resolver writes an INDEPENDENT
/// `CastingPermission` per object with no grant-scoped ledger, so the old
/// lowering let the controller cast every matching one of the six. CR 608.2c
/// makes the printed "a … spell" part of the instruction, so the honest outcome
/// is a refusal.
///
/// The CR 205.3g + CR 205.2b type-gate GRAMMAR that clause exercises — a subtype
/// leg (`Vehicle`) standing beside a multi-word core-type leg (`artifact
/// creature`) — is orthogonal to the cap, which is read from the head noun's
/// grammatical number alone. Pluralizing that ONE token is therefore the minimal
/// edit that keeps the gate grammar and the paid-offer runtime under test on a
/// clause that still lowers, without inventing a different gate.
const SANWELL_PLURAL_GATE_TRIGGER_BODY: &str = "exile the top six cards of your library. You may cast Vehicle or artifact creature spells from among them. Then put the rest on the bottom of your library in a random order.";
/// The whole-card form of `SANWELL_PLURAL_GATE_TRIGGER_BODY`, for the rows that
/// parse a full card rather than a trigger body.
const SANWELL_PLURAL_GATE: &str = "As long as an artifact creature you control is attacking, prevent all damage that would be dealt to Sanwell.\nWhenever Sanwell becomes tapped, exile the top six cards of your library. You may cast Vehicle or artifact creature spells from among them. Then put the rest on the bottom of your library in a random order.";
/// Synthetic Oracle text: Wand of Wonder is the ONLY printed card that puts a
/// type list before the `"from among "` anchor in the counted `exiled this way`
/// form, and its printed cap is a non-literal `X` fixed by a d20 roll. CR
/// 608.2c: that bound is part of the instruction and no `CastFromZoneDriver`
/// can carry it, so the clause is now refused outright rather than downgraded to
/// an uncapped lingering permission — which means Wand of Wonder no longer
/// produces a `CastFromZone` to read the pre-anchor probe off. This fixture is
/// Wand of Wonder's clause verbatim except for the printed cap ("up to two"),
/// so the pre-anchor type-list probe stays covered on a representable bound.
/// Called out as synthetic in the PR body.
const SYNTHETIC_TYPED_PRE_ANCHOR_EXILED_THIS_WAY: &str = "{4}, {T}: Roll a d20. Each opponent exiles cards from the top of their library until they exile an instant or sorcery card, then shuffles the rest into their library. You may cast up to two instant and/or sorcery spells from among cards exiled this way without paying their mana costs.";
const SCHOLAR_OF_THE_LOST_TROVE: &str = "Flying\nWhen this creature enters, you may cast target instant, sorcery, or artifact card from your graveyard without paying its mana cost. If an instant or sorcery spell cast this way would be put into your graveyard, exile it instead.";
const ETALI_PRIMAL_CONQUEROR: &str = "Trample\nWhen Etali enters, each player exiles cards from the top of their library until they exile a nonland card. You may cast any number of spells from among the nonland cards exiled this way without paying their mana costs.\n{9}{G/P}: Transform Etali. Activate only as a sorcery.";
const HELLCARVER_DEMON: &str = "Flying\nWhenever this creature deals combat damage to a player, sacrifice all other permanents you control and discard your hand. Exile the top six cards of your library. You may cast any number of spells from among cards exiled this way without paying their mana costs.";
/// Synthetic Oracle text: no printed card puts an `Or`-shaped (multi-word-leg)
/// type gate on the hand-bound branch, so the `And` arm of that branch's match
/// has no production card. This fixture drives the real `parse_oracle_text`
/// path to reach it. Called out as synthetic in the PR body.
const SYNTHETIC_HAND_BOUND_VEHICLE: &str = "When this creature enters, target opponent reveals their hand. You may cast a Vehicle or artifact creature spell from among those cards without paying its mana cost.";
const SYNTHETIC_HAND_BOUND_CMC: &str = "When this creature enters, target opponent reveals their hand. You may cast a creature spell with mana value 2 or less from among those cards without paying its mana cost.";

fn instant_or_sorcery() -> TypeFilter {
    TypeFilter::AnyOf(vec![TypeFilter::Instant, TypeFilter::Sorcery])
}

/// Reads the exile-set-anaphor composition: `And { [gate, ExiledBySource] }`.
/// Asserts BOTH legs, so a gate that replaced the anaphor rather than AND-ing
/// with it fails just as loudly as a dropped gate.
fn exile_gated_cast_legs(oracle: &str, name: &str, types: &[&str]) -> Vec<TargetFilter> {
    let target = cast_target_of(oracle, name, types);
    let TargetFilter::And { filters } = &target else {
        panic!("{name}: expected And {{ gate, ExiledBySource }}, got {target:?}");
    };
    assert!(
        filters.contains(&TargetFilter::ExiledBySource),
        "{name}: the exile-set anaphor leg must survive the composition, got {filters:?}"
    );
    filters.clone()
}

fn typed_leg_of(filters: &[TargetFilter], name: &str) -> TypedFilter {
    filters
        .iter()
        .find_map(|f| match f {
            TargetFilter::Typed(tf) => Some(tf.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{name}: expected a typed gate leg, got {filters:?}"))
}

fn hand_bound_typed_leg_of(filters: &[TargetFilter], name: &str) -> TypedFilter {
    filters
        .iter()
        .find_map(|filter| match filter {
            TargetFilter::Typed(typed)
                if typed
                    .properties
                    .contains(&FilterProp::InZone { zone: Zone::Hand }) =>
            {
                Some(typed.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("{name}: expected a hand-bound typed leg, got {filters:?}"))
}

/// R1 — CR 601.3 + CR 205.2b: the connector spelling `" and "` enumerates
/// ALTERNATIVE members of the permission's candidate set, so it lowers to
/// `TypeFilter::AnyOf`, exactly like `" or "`.
///
/// The `assert_eq!` on the whole `type_filters` vector is the load-bearing
/// assertion: `game/filter.rs` evaluates that vector with `.all()`, so the
/// tempting literal reading — `vec![Instant, Sorcery]` — is a per-object
/// conjunction that matches NOTHING (see `no_card_is_both_instant_and_sorcery`),
/// i.e. strictly worse than the bare filter this replaces. Equality, not
/// `contains`, is what fails on that refactor.
#[test]
fn and_joined_cast_type_gate_is_a_disjunction() {
    for (name, oracle, types) in [
        ("Epic Experiment", EPIC_EXPERIMENT, &["Sorcery"][..]),
        (
            "Ral, Leyline Prodigy",
            RAL_LEYLINE_PRODIGY,
            &["Planeswalker"][..],
        ),
    ] {
        let filters = exile_gated_cast_legs(oracle, name, types);
        assert_eq!(
            typed_leg_of(&filters, name).type_filters,
            vec![instant_or_sorcery()],
            "{name}: \"instant and sorcery spells\" is a plural over the permitted \
             SET (CR 601.3), not a conjunction over one object"
        );
    }
}

/// R3 — the `" and/or "` spelling is the same axis as `" and "` / `" or "`, and
/// a leading `"any number of "` quantifier is consumed without becoming a type.
#[test]
fn and_or_joined_cast_type_gate_is_a_disjunction() {
    let filters = exile_gated_cast_legs(KYLOX, "Kylox, Visionary Inventor", &["Creature"]);
    assert_eq!(
        typed_leg_of(&filters, "Kylox, Visionary Inventor").type_filters,
        vec![instant_or_sorcery()]
    );
}

/// R4 — CR 601.2: a leading count is a count of CAST EVENTS, not an object
/// quality, so it is consumed and discarded rather than folded into the filter.
///
/// Two authorities in one test: the type gate (`[Sorcery]`, single leg accepted
/// only because the quantifier was consumed) and the mana-value
/// `CastPermissionConstraint`. A fix that ate the constraint while consuming the
/// count fails the second assertion.
#[test]
fn counted_cast_type_gate_keeps_the_type_leg() {
    let parsed = parse(COLLECTED_CONJURING, "Collected Conjuring", &["Sorcery"]);
    let Effect::CastFromZone {
        target, constraint, ..
    } = parsed_cast_from_zone(&parsed)
    else {
        unreachable!("helper returns CastFromZone")
    };
    let TargetFilter::And { filters } = target else {
        panic!("Collected Conjuring: expected And {{ gate, ExiledBySource }}, got {target:?}");
    };
    assert!(filters.contains(&TargetFilter::ExiledBySource));
    assert_eq!(
        typed_leg_of(filters, "Collected Conjuring").type_filters,
        vec![TypeFilter::Sorcery],
        "\"up to two sorcery spells\" names exactly one card type — no AnyOf wrapper"
    );
    assert_eq!(
        constraint,
        &Some(CastPermissionConstraint::ManaValue {
            comparator: Comparator::LE,
            value: QuantityExpr::Fixed { value: 3 },
        }),
        "consuming the leading count must not eat the mana-value bound"
    );
}

/// R5 — serial-comma lists yield every leg, in source order. Pins the `many0`
/// arity against a regression that hard-codes two legs.
///
/// Scholar of the Lost Trove is a real printed card with the serial-comma
/// surface (`"target instant, sorcery, or artifact card"`), so this row is not
/// synthetic. Its non-type legs (`you control`, `InZone { Graveyard }`) are
/// asserted too: the composed grammar returns `controller: None, properties:
/// []` and relies on `apply_cast_target_suffixes` to re-add them, so dropping
/// that re-add would silently widen the permission to every graveyard.
#[test]
fn serial_comma_cast_type_gate_yields_all_three_legs() {
    let target = cast_target_of(
        SCHOLAR_OF_THE_LOST_TROVE,
        "Scholar of the Lost Trove",
        &["Creature"],
    );
    let TargetFilter::Typed(typed) = &target else {
        panic!("Scholar of the Lost Trove: expected a single typed filter, got {target:?}");
    };
    assert_eq!(
        typed.type_filters,
        vec![TypeFilter::AnyOf(vec![
            TypeFilter::Instant,
            TypeFilter::Sorcery,
            TypeFilter::Artifact,
        ])],
        "three legs, order-preserving"
    );
    assert_eq!(typed.controller, Some(ControllerRef::You));
    assert!(typed.properties.contains(&FilterProp::InZone {
        zone: Zone::Graveyard
    }));
}

/// R10 — CR 205.3g + CR 205.2b: a subtype leg (`Vehicle`, an artifact subtype)
/// stands beside a multi-word core-type leg (`artifact creature`).
///
/// The multi-word leg must be ONE `Typed` carrying TWO atoms, not two legs:
/// CR 205.2b says adjacent type words with no connector describe one object
/// bearing both types. A grammar that split them would permit any artifact.
#[test]
fn subtype_and_multiword_cast_type_gate() {
    let filters = exile_gated_cast_legs(SANWELL_PLURAL_GATE, "Sanwell, Avenger Ace", &["Creature"]);
    let gate = filters
        .iter()
        .find(|f| matches!(f, TargetFilter::Or { .. }))
        .unwrap_or_else(|| panic!("Sanwell: expected an Or-shaped gate leg, got {filters:?}"));
    let TargetFilter::Or { filters: legs } = gate else {
        unreachable!("matched Or above")
    };
    assert_eq!(
        legs,
        &vec![
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Subtype("Vehicle".to_string())],
                controller: None,
                properties: Vec::new(),
            }),
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact, TypeFilter::Creature],
                controller: None,
                properties: Vec::new(),
            }),
        ],
        "CR 205.2b: \"artifact creature\" is one leg with two atoms; the Vehicle \
         subtype is canonicalized, not lowercased"
    );
}

/// R6 — the trap row. `TargetFilter::references_exiled_by_source` uses `.any()`
/// for `And` but **`.all()` for `Or`**. The composed shape is always
/// `And { [gate, ExiledBySource] }`, so the `And` arm answers and the exile
/// binding survives an `Or`-shaped gate. A future refactor that hoisted the gate
/// to top level (`Or { [legA, legB] }`) would silently return `false` here and
/// the runtime would stop remapping the library-peek set.
#[test]
fn or_shaped_cast_gate_still_references_the_exile_set() {
    let target = cast_target_of(SANWELL_PLURAL_GATE, "Sanwell, Avenger Ace", &["Creature"]);
    assert!(
        matches!(&target, TargetFilter::And { filters }
            if filters.iter().any(|f| matches!(f, TargetFilter::Or { .. }))),
        "reach guard: this row is only meaningful on an Or-shaped gate, got {target:?}"
    );
    assert!(
        target.references_exiled_by_source(),
        "the Or-shaped gate must not break the exile-set binding (Or evaluates \
         `references_exiled_by_source` with .all(), And with .any())"
    );
}

/// R8 — anti-swallow on the NEW pre-anchor probe.
///
/// `parse_from_among_exiled_this_way` now probes the text BEFORE the
/// `"from among "` anchor, because WotC puts the type list there in the counted
/// form. The untyped members of that family carry `"any number of spells "` /
/// `"up to two spells "` in exactly that position, and must not gain a gate.
///
/// `SYNTHETIC_TYPED_PRE_ANCHOR_EXILED_THIS_WAY` in the same test is the
/// mandatory paired positive: it proves the prefix probe actually ran, so the
/// negatives below are not vacuous. It carries Wand of Wonder's clause with a
/// representable printed cap, because Wand's own `"up to X"` is now a strict
/// refusal (see the constant's doc comment).
#[test]
fn untyped_pre_anchor_prefix_adds_no_type_gate() {
    // Positive reach guard: the pre-anchor type list IS consumed.
    let filters = exile_gated_cast_legs(
        SYNTHETIC_TYPED_PRE_ANCHOR_EXILED_THIS_WAY,
        "Typed Pre-Anchor Fixture",
        &["Artifact"],
    );
    let typed = typed_leg_of(&filters, "Typed Pre-Anchor Fixture");
    assert_eq!(typed.type_filters, vec![instant_or_sorcery()]);
    assert!(
        typed
            .properties
            .contains(&FilterProp::InZone { zone: Zone::Exile }),
        "the exiled-this-way arm pins the candidate cards to exile, got {:?}",
        typed.properties
    );

    // Negatives: same branch, same prefix position, no card type named.
    assert_eq!(
        cast_target_of(HELLCARVER_DEMON, "Hellcarver Demon", &["Creature"]),
        TargetFilter::ExiledBySource,
        "\"any number of spells from among cards exiled this way\" names no type"
    );
    assert_eq!(
        cast_target_of(CAPSTONE, "Improvisation Capstone", &["Sorcery"]),
        TargetFilter::ExiledBySource
    );
    // Etali has a real POST-anchor typed leg ("the nonland cards exiled this
    // way"); the prefix probe must not shadow or duplicate it.
    let etali = cast_target_of(
        ETALI_PRIMAL_CONQUEROR,
        "Etali, Primal Conqueror",
        &["Creature"],
    );
    let TargetFilter::And { filters } = &etali else {
        panic!("Etali: expected And {{ typed, ExiledBySource }}, got {etali:?}");
    };
    assert!(filters.contains(&TargetFilter::ExiledBySource));
    assert_eq!(
        typed_leg_of(filters, "Etali, Primal Conqueror").type_filters,
        vec![
            TypeFilter::Card,
            TypeFilter::Non(Box::new(TypeFilter::Land))
        ],
        "Etali's post-anchor nonland leg must be unchanged"
    );
}

/// R9 — the hand-bound branch's `And` arm. No printed card reaches it, so the
/// fixture Oracle text is synthetic; it still runs through production
/// `parse_oracle_text`.
///
/// Paired positive: `hand_bound_cast_retains_the_instant_or_sorcery_gate`
/// (Mindclaw Shaman) must stay on the `Typed` graft arm — that test failing
/// would mean the `Typed` arm regressed into the `And` arm.
#[test]
fn hand_bound_or_shaped_gate_ands_rather_than_grafts() {
    let target = cast_target_of(
        SYNTHETIC_HAND_BOUND_VEHICLE,
        "Synthetic Hand Reveal Pilot",
        &["Creature"],
    );
    let TargetFilter::And { filters } = &target else {
        panic!("expected And {{ Or-gate, hand binding }}, got {target:?}");
    };
    assert!(
        filters.iter().any(|f| matches!(f, TargetFilter::Or { .. })),
        "the Or-shaped gate must be AND-ed beside the hand binding, got {filters:?}"
    );
    let hand = hand_bound_typed_leg_of(filters, "Synthetic Hand Reveal Pilot");
    assert_eq!(
        hand.type_filters,
        vec![TypeFilter::Card],
        "the hand binding keeps its bare Card head noun; the type gate rides beside it"
    );
    assert_eq!(hand.controller, Some(ControllerRef::Opponent));
    assert!(hand
        .properties
        .contains(&FilterProp::InZone { zone: Zone::Hand }));
}

/// A typed cast gate can carry property predicates as well as type atoms. Those
/// predicates cannot be grafted into the hand binding's type vector; the whole
/// gate must remain an `And` leg beside the revealed-hand binding.
#[test]
fn hand_bound_cast_keeps_rich_typed_gate_as_a_complete_predicate() {
    let target = cast_target_of(
        SYNTHETIC_HAND_BOUND_CMC,
        "Synthetic Hand Reveal CMC",
        &["Creature"],
    );
    let TargetFilter::And { filters } = &target else {
        panic!("expected And {{ typed gate, hand binding }}, got {target:?}");
    };
    assert!(
        filters.iter().any(|filter| {
            matches!(filter, TargetFilter::Typed(typed)
            if typed.type_filters == vec![TypeFilter::Creature]
                && typed.properties.contains(&FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 2 },
                }))
        }),
        "the complete creature + mana-value gate must survive, got {filters:?}"
    );
    let hand = hand_bound_typed_leg_of(filters, "Synthetic Hand Reveal CMC");
    assert_eq!(hand.type_filters, vec![TypeFilter::Card]);
    assert_eq!(hand.controller, Some(ControllerRef::Opponent));
    assert!(
        hand.properties
            .contains(&FilterProp::InZone { zone: Zone::Hand }),
        "the hand binding must remain alongside the rich gate, got {:?}",
        hand.properties
    );
}

/// R2 — the semantic trap, documented executably rather than in a comment.
///
/// `game/filter.rs` evaluates `TypedFilter::type_filters` with `.all()`, so a
/// literal `vec![Instant, Sorcery]` demands one object be BOTH. No such object
/// exists, which is why every connector spelling must lower to `AnyOf`.
///
/// Loaded through `support::shared_card_export_json()` (the sanctioned loader —
/// `scripts/check-test-card-data-load.sh` fails any test that opens
/// `client/public/card-data.json` directly). That loader returns `None` when the
/// gitignored export is absent, so this row SELF-SKIPS in CI and is local
/// documentation only; the real pin is `and_joined_cast_type_gate_is_a_disjunction`'s
/// `assert_eq!`.
#[test]
fn no_card_is_both_instant_and_sorcery() {
    let Some(export) = crate::support::shared_card_export_json() else {
        return;
    };
    assert!(
        export.len() >= 30_000,
        "reach guard: a truncated export would satisfy the count below vacuously, \
         got {} entries",
        export.len()
    );
    let both: Vec<&String> = export
        .iter()
        .filter(|(_, value)| {
            let types = value
                .get("card_type")
                .and_then(|ct| ct.get("core_types"))
                .and_then(|t| t.as_array());
            types.is_some_and(|t| {
                t.iter().any(|v| v.as_str() == Some("Instant"))
                    && t.iter().any(|v| v.as_str() == Some("Sorcery"))
            })
        })
        .map(|(key, _)| key)
        .collect();
    assert!(
        both.is_empty(),
        "no card carries both Instant and Sorcery, so a literal per-object `And` \
         of the two legs would match nothing; found {both:?}"
    );
}

// ---------------------------------------------------------------------------
// R11-R13 — RUNTIME coverage for the exile-set ("from among them") site.
//
// The runtime shape of this site is NOT the private-library `EffectZoneChoice`
// used by Kiora/Velomachus/Svella. Those cards LOOK at library cards and pick
// one during resolution (CR 608.2g). The cards below EXILE first, and their
// "you may cast ..." instruction then offers that batch.
//
// CR 608.2g: for the FREE form (Epic Experiment, Collected Conjuring) the offer
// is a resolution-scoped `CastOfferKind::FreeCastWindow` opened while the source
// is still resolving — no casting permission is granted and no priority window
// is handed back, because "no other spells can normally be cast … during
// resolution" and the WotC rulings say the controller "can't wait to cast them
// later in the turn". For the PAID form (Sanwell, Avenger Ace) the controller
// must still be able to pay, so it stays a lingering
// `CastingPermission::ExileWithAltCost` (CR 118.9) observable on the
// legal-action surface at the next priority window. `free_cast_offers` reads
// whichever surface the card's own mechanism produced.
// ---------------------------------------------------------------------------

/// CR 608.2c: "Then put all cards exiled this way that weren't cast into your
/// graveyard" (Epic Experiment), "Put the exiled cards not cast this way on the
/// bottom of your library" (Collected Conjuring) and "Then put the rest on the
/// bottom of your library" (Sanwell) are SEPARATE instructions that follow the
/// cast permission. The engine grants the permission and then resolves that
/// cleanup inside the same resolution; its zone change runs
/// `zones::apply_zone_exit_cleanup`, which strips the grant before the
/// controller ever reaches a priority window. Detach exactly that trailing
/// instruction so the permission set the cast instruction produced is
/// observable.
///
/// Everything else — including the parse itself — is the card's real, unmodified
/// Oracle text. The detached node's identity is asserted, so no other
/// instruction can be silently dropped, and the cleanup's own behaviour is
/// covered by `issue_3267_sanwell_rest_on_bottom.rs`.
fn exile_then_cast_chain_without_uncast_cleanup(oracle: &str) -> AbilityDefinition {
    let mut execute = engine::parser::oracle_effect::parse_effect_chain(
        oracle,
        engine::types::ability::AbilityKind::Spell,
    );
    let cast = execute
        .sub_ability
        .as_mut()
        .expect("the exile step must chain into the \"you may cast\" instruction");
    assert!(
        matches!(cast.effect.as_ref(), Effect::CastFromZone { .. }),
        "expected the chained cast instruction, got {:?}",
        cast.effect
    );
    let detached: Vec<_> = cast
        .sub_ability
        .take()
        .into_iter()
        .chain(cast.else_ability.take())
        .collect();
    assert!(
        !detached.is_empty(),
        "reach guard: this card's trailing uncast-cleanup instruction must exist, \
         otherwise this helper is silently doing nothing"
    );
    for cleanup in &detached {
        assert!(
            is_uncast_cleanup(cleanup),
            "only the uncast-cleanup instruction may be detached, got {:?}",
            cleanup.effect
        );
    }
    execute
}

/// True for a chain made only of "put the uncast cards somewhere" instructions
/// (`PutAtLibraryPosition`, or a mass move to the graveyard).
fn is_uncast_cleanup(def: &AbilityDefinition) -> bool {
    matches!(
        def.effect.as_ref(),
        Effect::PutAtLibraryPosition { .. }
            | Effect::ChangeZoneAll {
                destination: Zone::Graveyard,
                ..
            }
    ) && def.sub_ability.as_deref().is_none_or(is_uncast_cleanup)
        && def.else_ability.as_deref().is_none_or(is_uncast_cleanup)
}

/// Resolve an exile-then-cast chain and accept its "you may cast" offer, leaving
/// the runner at the priority window where the granted permissions are live.
fn accept_exile_set_cast(
    runner: &mut GameRunner,
    source: ObjectId,
    execute: &AbilityDefinition,
    chosen_x: Option<u32>,
) {
    let resolved = exile_set_cast_ability(execute, source, chosen_x);
    resolve_and_accept_exile_set_cast(runner, &resolved);
}

/// The resolved form of an exile-then-cast chain, with X stamped across it.
fn exile_set_cast_ability(
    execute: &AbilityDefinition,
    source: ObjectId,
    chosen_x: Option<u32>,
) -> ResolvedAbility {
    let mut resolved = engine::game::ability_utils::build_resolved_from_def(execute, source, P0);
    // CR 107.3i: every instance of X in a single announcement shares one value,
    // so it is stamped on the whole chain — on Epic Experiment X sizes both the
    // exile step and the cast permission's mana-value ceiling.
    fn stamp_x(ability: &mut ResolvedAbility, chosen_x: Option<u32>) {
        ability.chosen_x = chosen_x;
        if let Some(sub) = ability.sub_ability.as_mut() {
            stamp_x(sub, chosen_x);
        }
        if let Some(alt) = ability.else_ability.as_mut() {
            stamp_x(alt, chosen_x);
        }
    }
    stamp_x(&mut resolved, chosen_x);
    resolved
}

/// Resolve the chain and leave the runner at the ONE prompt the cast instruction
/// presents.
///
/// CR 608.2d: the printed "you may" is a single choice, so it must produce a
/// single prompt. Which prompt depends on the mechanism the card's own grammar
/// selected, and the two are mutually exclusive:
/// * FREE batch (`CastFromZoneDriver::ResolutionWindow`) — the interactive
///   `CastOfferKind::FreeCastWindow` IS the "may": accepting is selecting a card,
///   declining is `FreeCastWindowChoice { selection: None }`. A generic
///   `OptionalEffectChoice` wrapper in front of it would ask the same question
///   twice, which is why `parse_effect_chain_ir` lowers this driver as mandatory
///   (mirroring `Effect::FreeCastFromZones`).
/// * PAID batch (Sanwell, Avenger Ace) — no window exists; the "may" is the
///   generic `OptionalEffectChoice`, and accepting it installs a lingering
///   permission (CR 118.9) observable at the following priority window.
///
/// The `assert!` on the free branch is the anti-double-prompt regression pin: it
/// fails if the `ResolutionWindow` arm is dropped from the optionality
/// reconciliation and the redundant wrapper comes back.
fn resolve_and_accept_exile_set_cast(runner: &mut GameRunner, resolved: &ResolvedAbility) {
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), resolved, &mut events, 0)
        .expect("the exile-then-cast chain must resolve");
    if matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { .. },
            ..
        }
    ) {
        // CR 608.2g: the free batch opened its resolution-scoped window directly.
        // No `OptionalEffectChoice` was ever parked in front of it.
        return;
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "CR 608.2d: the \"you may cast\" offer must be presented, parked at {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional cast must succeed");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "CR 118.9: the PAID batch grants a lingering permission and hands back \
         priority; a free-cast window here would mean the resolution-scoped \
         driver was double-prompted behind an `OptionalEffectChoice`, parked at {:?}",
        runner.state().waiting_for
    );
}

/// CR 601.3: the cards the cast instruction actually authorizes.
///
/// Two surfaces, one per mechanism, because the two mechanisms are genuinely
/// different: a FREE batch anaphor ("… from among them without paying their mana
/// costs") is a resolution-scoped window (CR 608.2g) whose offer IS its
/// candidate list, while a PAID batch anaphor (Sanwell, Avenger Ace) still
/// grants a lingering permission (CR 118.9) that shows up on the legal-action
/// surface at the following priority window. Reading the window's candidates
/// rather than `legal_actions` for the former is required, not a convenience:
/// during a resolution-scoped window no player has priority, so no
/// `CastSpell` action exists to inspect.
fn free_cast_offers(runner: &GameRunner) -> Vec<ObjectId> {
    if let WaitingFor::CastOffer {
        kind: CastOfferKind::FreeCastWindow { candidates, .. },
        ..
    } = &runner.state().waiting_for
    {
        return candidates.clone();
    }
    engine::ai_support::legal_actions(runner.state())
        .iter()
        .filter_map(|action| match action {
            GameAction::CastSpell { object_id, .. }
            | GameAction::CastSpellForFree { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect()
}

/// Positive reach guard: take the offer and prove the card reaches the stack.
fn take_offer_onto_the_stack(runner: &mut GameRunner, card: ObjectId) {
    if matches!(
        runner.state().waiting_for,
        WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow { .. },
            ..
        }
    ) {
        runner
            .act(GameAction::FreeCastWindowChoice {
                selection: Some(card),
            })
            .unwrap_or_else(|e| panic!("{card:?} must be free-castable from the window: {e:?}"));
    } else {
        let action = engine::ai_support::legal_actions(runner.state())
            .into_iter()
            .find(|action| {
                matches!(
                    action,
                    GameAction::CastSpell { object_id, .. }
                    | GameAction::CastSpellForFree { object_id, .. } if *object_id == card
                )
            })
            .unwrap_or_else(|| panic!("{card:?} must be castable from the granted permission"));
        runner.act(action).expect("casting the offered card");
        if matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }) {
            runner
                .act(GameAction::PassPriority)
                .expect("finalizing the cast's mana payment");
        }
    }
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Stack,
        "the offered card must land on the stack"
    );
}

/// R11 — RUNTIME. Epic Experiment with X = 2 exiles two mana-value-2 cards: a
/// sorcery and a creature. Both are inside the `ManaValue LE X` ceiling, so ONLY
/// the card-type gate (`AnyOf([Instant, Sorcery])`, the `" and "` connector this
/// change learned to read) can exclude the creature.
///
/// Reach guard: the sorcery must BE offered and must land on the stack, so the
/// negative cannot pass by an empty or short-circuited permission set.
#[test]
fn epic_experiment_does_not_offer_a_creature_inside_its_mana_value_ceiling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let epic = scenario
        .add_spell_to_hand_from_oracle(P0, "Epic Experiment", false, EPIC_EXPERIMENT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    // The trap: mana value 2 <= X = 2, so only the type gate excludes it.
    let creature_inside_ceiling = scenario
        .add_spell_to_library_top(P0, "Epic Trap Creature", false)
        .with_mana_cost(ManaCost::generic(2))
        .as_creature()
        .id();
    let legal_sorcery = scenario
        .add_spell_to_library_top(P0, "Epic Legal Sorcery", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&creature_inside_ceiling]
            .card_types
            .core_types,
        vec![CoreType::Creature],
        "anti-vacuity: the trap must be a creature and NOTHING else — a fixture \
         that is still also a Sorcery would satisfy the gate legitimately"
    );

    let execute = exile_then_cast_chain_without_uncast_cleanup(EPIC_EXPERIMENT);
    accept_exile_set_cast(&mut runner, epic, &execute, Some(2));

    let offers = free_cast_offers(&runner);
    assert!(
        offers.contains(&legal_sorcery),
        "reach guard: the legal sorcery must be offered, otherwise the negative \
         below is vacuous; offered = {offers:?}"
    );
    assert!(
        !offers.contains(&creature_inside_ceiling),
        "CR 601.3: \"cast instant and sorcery spells\" permits only instants and \
         sorceries — a creature inside the mana-value ceiling must never be \
         offered (issue #6960); offered = {offers:?}"
    );
    assert!(
        runner.state().objects[&creature_inside_ceiling]
            .casting_permissions
            .is_empty(),
        "the ineligible creature must not receive a casting permission"
    );
    // CR 608.2g: the free form grants NO lingering permission at all — the offer
    // exists only for as long as Epic Experiment is resolving. Reverting the
    // resolution-scoped lowering flips this: the offered sorcery would carry a
    // standing `ExileWithAltCost` grant instead.
    assert!(
        runner.state().objects[&legal_sorcery]
            .casting_permissions
            .is_empty(),
        "even the OFFERED card must carry no standing permission — the window is \
         resolution-scoped, not a lingering grant"
    );

    take_offer_onto_the_stack(&mut runner, legal_sorcery);
}

/// R12 — RUNTIME. Collected Conjuring names ONE type behind a leading count
/// ("up to two sorcery spells"), the form whose quantifier prefix had to be
/// consumed before the type phrase. The mana-value-3 instant is inside the
/// `ManaValue LE 3` ceiling, so only the type gate excludes it; the
/// mana-value-3 sorcery is the paired positive.
#[test]
fn collected_conjuring_does_not_offer_an_instant() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let conjuring = scenario
        .add_spell_to_hand_from_oracle(P0, "Collected Conjuring", false, COLLECTED_CONJURING)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    // Seeded as an instant outright: `add_spell_to_library_top(.., false)`
    // seeds Sorcery, and `CardBuilder::as_instant` only strips Creature — the
    // resulting Sorcery-AND-Instant card would satisfy a Sorcery gate honestly
    // and make the negative below vacuous.
    let instant_inside_ceiling = scenario
        .add_spell_to_library_top(P0, "Conjuring Trap Instant", true)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let legal_sorcery = scenario
        .add_spell_to_library_top(P0, "Conjuring Legal Sorcery", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    for index in 0..4 {
        scenario
            .add_spell_to_library_top(P0, &format!("Conjuring Filler {index}"), false)
            .with_mana_cost(ManaCost::generic(6));
    }

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&instant_inside_ceiling]
            .card_types
            .core_types,
        vec![CoreType::Instant],
        "anti-vacuity: the trap must be an instant and NOTHING else"
    );

    let execute = exile_then_cast_chain_without_uncast_cleanup(COLLECTED_CONJURING);
    accept_exile_set_cast(&mut runner, conjuring, &execute, None);

    let offers = free_cast_offers(&runner);
    assert!(
        offers.contains(&legal_sorcery),
        "reach guard: the legal sorcery must be offered; offered = {offers:?}"
    );
    assert!(
        !offers.contains(&instant_inside_ceiling),
        "CR 601.3: \"up to two sorcery spells\" permits sorceries only — an \
         instant inside the mana-value ceiling must never be offered; \
         offered = {offers:?}"
    );
    assert!(
        runner.state().objects[&instant_inside_ceiling]
            .casting_permissions
            .is_empty(),
        "the ineligible instant must not receive a casting permission"
    );
    // CR 608.2c: "up to two" is a printed hard cap the window's stop-early loop owns.
    let WaitingFor::CastOffer {
        kind: CastOfferKind::FreeCastWindow {
            remaining_casts, ..
        },
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Collected Conjuring must open its resolution-scoped window, parked at {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(
        remaining_casts,
        Some(2),
        "\"up to two sorcery spells\" must bound the window at two casts"
    );

    take_offer_onto_the_stack(&mut runner, legal_sorcery);
}

/// R12b — RUNTIME. CR 608.2c: an UNBOUNDED window ("you may cast any number of
/// spells … from among them") has no printed cast cap, so a batch larger than
/// 255 must stay fully castable.
///
/// Epic Experiment is the type specimen for a batch whose size is chosen at
/// announcement (`Exile the top X cards`), which is exactly how this bound is
/// crossed in play: X is only limited by available mana, and the same window
/// shape carries Villainous Wealth ("the top X cards of their library") and
/// Hazoret's Undying Fury.
///
/// REVERT GUARD — this test crosses the cap through the production pipeline
/// rather than unit-testing the conversion. `open_resolution_cast_window`
/// previously lowered `ResolutionCastWindow { max_casts: None }` to
/// `Effect::FreeCastFromZones { count: pool.len().try_into().unwrap_or(u8::MAX) }`,
/// which for a 258-card pool produced a HARD 255-cast window. Under that code
/// both halves of this test fail: `remaining_casts` reads `Some(255)` instead of
/// `None`, and the 256th `FreeCastWindowChoice` has no window left to answer,
/// because the offer loop closed after the 255th cast decremented the count to
/// zero. CR 608.2g authorizes these casts during the granting resolution; it
/// supplies no 255-spell limit.
#[test]
fn unbounded_resolution_window_casts_past_the_former_255_cap() {
    // 258 > `u8::MAX`, so the old `try_into().unwrap_or(u8::MAX)` truncated.
    const POOL: usize = 258;
    // The first cast the old cap could not have serviced.
    const CASTS_CROSSING_THE_CAP: usize = 256;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let epic = scenario
        .add_spell_to_hand_from_oracle(P0, "Epic Experiment", false, EPIC_EXPERIMENT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    for i in 0..POOL {
        scenario.add_spell_to_library_top(P0, &format!("Cantrip {i}"), true);
    }
    let mut runner = scenario.build();

    let execute = exile_then_cast_chain_without_uncast_cleanup(EPIC_EXPERIMENT);
    accept_exile_set_cast(&mut runner, epic, &execute, Some(POOL as u32));

    let WaitingFor::CastOffer {
        kind:
            CastOfferKind::FreeCastWindow {
                candidates,
                remaining_casts,
                ..
            },
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Epic Experiment must open its resolution-scoped window, parked at {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(
        candidates.len(),
        POOL,
        "reach guard: every exiled card is inside the mana-value ceiling and must be offered"
    );
    // CR 608.2c: "any number of spells" — the window carries NO cast bound. This
    // is the typed encoding of unbounded, not a 255 sentinel.
    assert_eq!(
        remaining_casts, None,
        "an unbounded window must carry no cast cap; a `Some(_)` here is the \
         truncation this test guards against"
    );

    // Drive the real accept loop past the old cap. Each iteration goes through
    // `GameAction::FreeCastWindowChoice` → `initiate_cast_during_resolution` →
    // `FreeCastOfferRemaining`, i.e. the production re-offer path.
    let mut cast_ids = Vec::with_capacity(CASTS_CROSSING_THE_CAP);
    for nth in 1..=CASTS_CROSSING_THE_CAP {
        let WaitingFor::CastOffer {
            kind:
                CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts,
                    ..
                },
            ..
        } = runner.state().waiting_for.clone()
        else {
            panic!(
                "the unbounded window must still be open for cast #{nth}, parked at {:?}",
                runner.state().waiting_for
            )
        };
        assert_eq!(
            remaining_casts, None,
            "the unbounded bound must survive every re-offer (cast #{nth})"
        );
        let chosen = candidates[0];
        runner
            .act(GameAction::FreeCastWindowChoice {
                selection: Some(chosen),
            })
            .unwrap_or_else(|e| panic!("cast #{nth} must be accepted: {e:?}"));
        assert_eq!(
            runner.state().objects[&chosen].zone,
            Zone::Stack,
            "cast #{nth} must reach the stack"
        );
        cast_ids.push(chosen);
    }

    assert_eq!(
        cast_ids.len(),
        CASTS_CROSSING_THE_CAP,
        "every cast past the old cap must have gone through"
    );
    // The batch is not exhausted, so the window must still be offering the rest.
    let WaitingFor::CastOffer {
        kind:
            CastOfferKind::FreeCastWindow {
                candidates,
                remaining_casts,
                ..
            },
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the unbounded window must remain open while candidates remain, parked at {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(
        candidates.len(),
        POOL - CASTS_CROSSING_THE_CAP,
        "exactly the uncast remainder must still be offered"
    );
    assert_eq!(
        remaining_casts, None,
        "the window is still unbounded after crossing the former cap"
    );
}

/// R13 — RUNTIME. Sanwell's `Or`-shaped gate has TWO legs from two different CR
/// sections (CR 205.3g subtype, CR 205.2b core-type conjunction). Both positives
/// are asserted, so a gate that collapsed the `Or` to a single leg fails.
///
/// Sanwell's clause carries no "without paying its mana cost", so these are paid
/// casts — the mana pool covers every fixture equally and the only axis that can
/// separate them is the type gate.
#[test]
fn sanwell_offers_only_vehicles_and_artifact_creatures() {
    let mut fixture = sanwell_fixture();
    let execute = exile_then_cast_chain_without_uncast_cleanup(SANWELL_PLURAL_GATE_TRIGGER_BODY);
    accept_exile_set_cast(&mut fixture.runner, fixture.sanwell, &execute, None);
    fixture.assert_only_the_two_gate_legs_are_offered();
    take_offer_onto_the_stack(&mut fixture.runner, fixture.vehicle);
}

/// R13b — RUNTIME, under a REAL triggered-ability context. Sanwell's grant is
/// printed on a trigger ("Whenever Sanwell becomes tapped, …"), so in production
/// the resolving ability carries a `TriggerSourceContext`.
///
/// That context is captured when the trigger is put on the stack — BEFORE the
/// ability's own exile step runs — so its `linked_exile_snapshot` is empty.
/// `filter::ExiledBySource` prefers that snapshot over the live exile links
/// whenever `trigger_source.is_some()`, so a runtime gate that re-evaluated the
/// whole filter (anaphor leg included) against the chain-forwarded ids would
/// match NOTHING and grant NOTHING — turning the fix into a total no-op on
/// exactly the cards it targets. Discharging the anaphor
/// (`TargetFilter::without_exile_anaphor`) and testing only the clause's own
/// legs is what keeps this row green.
///
/// R13's sibling row above builds the same chain with no trigger context and so
/// cannot see this; that is why this variant exists.
#[test]
fn sanwell_type_gate_holds_under_a_real_trigger_context() {
    let mut fixture = sanwell_fixture();
    let execute = exile_then_cast_chain_without_uncast_cleanup(SANWELL_PLURAL_GATE_TRIGGER_BODY);
    let mut resolved = exile_set_cast_ability(&execute, fixture.sanwell, None);
    // CR 603.4: stamp the provenance a real "becomes tapped" trigger would carry.
    let (incarnation, card_id) = {
        let source = &fixture.runner.state().objects[&fixture.sanwell];
        (source.incarnation, source.card_id)
    };
    resolved.set_test_trigger_source_recursive(incarnation, card_id);
    assert!(
        resolved
            .sub_ability
            .as_ref()
            .is_some_and(|cast| cast.trigger_source.is_some()),
        "reach guard: the cast instruction itself must carry the trigger context, \
         otherwise this row degenerates into R13"
    );

    resolve_and_accept_exile_set_cast(&mut fixture.runner, &resolved);
    fixture.assert_only_the_two_gate_legs_are_offered();
    take_offer_onto_the_stack(&mut fixture.runner, fixture.artifact_creature);
}

/// Sanwell plus one card per gate outcome, with the seeded types pinned so the
/// negatives below cannot pass by accident.
struct SanwellFixture {
    runner: GameRunner,
    sanwell: ObjectId,
    vehicle: ObjectId,
    artifact_creature: ObjectId,
    plain_creature: ObjectId,
    instant: ObjectId,
}

impl SanwellFixture {
    /// Two positives and two negatives: a gate that collapsed the `Or` to a
    /// single leg fails one positive, and a gate that vanished fails a negative.
    fn assert_only_the_two_gate_legs_are_offered(&self) {
        let offers = free_cast_offers(&self.runner);
        assert!(
            offers.contains(&self.vehicle),
            "CR 205.3g: the Vehicle subtype leg must be offered; offered = {offers:?}"
        );
        assert!(
            offers.contains(&self.artifact_creature),
            "CR 205.2b: the artifact-creature leg must be offered; offered = {offers:?}"
        );
        assert!(
            !offers.contains(&self.plain_creature),
            "a nonartifact creature satisfies neither leg; offered = {offers:?}"
        );
        assert!(
            !offers.contains(&self.instant),
            "an instant satisfies neither leg; offered = {offers:?}"
        );
    }
}

fn sanwell_fixture() -> SanwellFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sanwell = scenario
        .add_creature(P0, "Sanwell, Avenger Ace", 3, 3)
        .from_oracle_text(SANWELL_PLURAL_GATE)
        .id();
    // CR 205.3g: a Vehicle is "Artifact — Vehicle"; `as_creature`
    // first strips the Sorcery seed, `as_artifact` then strips Creature.
    let vehicle = scenario
        .add_spell_to_library_top(P0, "Sanwell Vehicle", false)
        .with_mana_cost(ManaCost::generic(2))
        .as_creature()
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .id();
    let artifact_creature = scenario
        .add_spell_to_library_top(P0, "Sanwell Artifact Creature", false)
        .with_mana_cost(ManaCost::generic(2))
        .as_artifact()
        .as_creature()
        .id();
    let plain_creature = scenario
        .add_spell_to_library_top(P0, "Sanwell Plain Creature", false)
        .with_mana_cost(ManaCost::generic(2))
        .as_creature()
        .id();
    let instant = scenario
        .add_spell_to_library_top(P0, "Sanwell Instant", true)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    for index in 0..2 {
        scenario
            .add_spell_to_library_top(P0, &format!("Sanwell Filler {index}"), false)
            .with_mana_cost(ManaCost::generic(2));
    }
    scenario.with_mana_pool(
        P0,
        (0..2)
            .map(|_| ManaUnit::new(ManaType::Colorless, sanwell, false, vec![]))
            .collect(),
    );

    let runner = scenario.build();
    let types = |id: ObjectId| runner.state().objects[&id].card_types.core_types.clone();
    assert_eq!(types(vehicle), vec![CoreType::Artifact]);
    assert_eq!(
        runner.state().objects[&vehicle].card_types.subtypes,
        vec!["Vehicle".to_string()]
    );
    assert_eq!(
        types(artifact_creature),
        vec![CoreType::Artifact, CoreType::Creature]
    );
    assert_eq!(
        types(plain_creature),
        vec![CoreType::Creature],
        "anti-vacuity: the nonartifact creature must satisfy neither leg"
    );
    assert_eq!(types(instant), vec![CoreType::Instant]);

    SanwellFixture {
        runner,
        sanwell,
        vehicle,
        artifact_creature,
        plain_creature,
        instant,
    }
}

/// CR 607.2a + CR 608.2c: "them" is THIS resolution's batch, never the source's
/// lifetime exile pile.
///
/// Jace's Mindseeker's ruling states the scoping rule in as many words: "'From
/// among them' means the five cards put into the graveyard, not all cards in
/// that graveyard." The same holds for every exile-batch member of this class —
/// a card a PREVIOUS resolution of the same source left sitting in exile is not
/// part of "them".
///
/// The fixture resolves the SAME Epic Experiment chain twice from the same
/// source object with X = 1, using the uncast-cleanup-detached harness so the
/// first batch's card is still linked and still in exile when the second window
/// opens. Both cards are mana-value-1 sorceries, so the mana-value ceiling and
/// the type gate admit both — only the per-resolution batch binding can exclude
/// the stale one.
#[test]
fn a_second_resolution_offers_only_its_own_exile_batch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let epic = scenario
        .add_spell_to_hand_from_oracle(P0, "Epic Experiment", false, EPIC_EXPERIMENT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    // Seeded top-down: second batch's card on top, first batch's beneath it.
    let second_batch = scenario
        .add_spell_to_library_top(P0, "Epic Second Batch", false)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let first_batch = scenario
        .add_spell_to_library_top(P0, "Epic First Batch", false)
        .with_mana_cost(ManaCost::generic(1))
        .id();

    let mut runner = scenario.build();
    let execute = exile_then_cast_chain_without_uncast_cleanup(EPIC_EXPERIMENT);

    // First resolution: exiles `first_batch` and offers exactly it.
    accept_exile_set_cast(&mut runner, epic, &execute, Some(1));
    let first_offers = free_cast_offers(&runner);
    assert_eq!(
        first_offers,
        vec![first_batch],
        "reach guard: the first resolution must offer its own batch, otherwise \
         the negative below is vacuous"
    );
    // Decline, leaving the card linked and still in exile.
    runner
        .act(GameAction::FreeCastWindowChoice { selection: None })
        .expect("declining the first window must succeed");
    assert_eq!(
        runner.state().objects[&first_batch].zone,
        Zone::Exile,
        "the declined card stays in exile (its cleanup instruction is detached \
         by this harness), which is exactly the stale-pile hazard under test"
    );

    // Second resolution from the SAME source: exiles `second_batch`.
    accept_exile_set_cast(&mut runner, epic, &execute, Some(1));
    let second_offers = free_cast_offers(&runner);
    assert!(
        second_offers.contains(&second_batch),
        "reach guard: the second resolution must offer its own batch; \
         offered = {second_offers:?}"
    );
    assert!(
        !second_offers.contains(&first_batch),
        "CR 607.2a: a card left in exile by a PREVIOUS resolution of this source \
         is not part of \"them\" and must not be re-offered; offered = {second_offers:?}"
    );
}

const PRIMEVAL_SPAWN_TRIGGER_BODY: &str = "exile the top ten cards of your library. You may cast any number of spells with total mana value 10 or less from among them without paying their mana costs.";

/// CR 202.3 + CR 608.2g: Primeval Spawn's "TOTAL mana value 10 or less" is a
/// running cross-selection budget, a different axis from the per-spell ceiling
/// every other card in this class states.
///
/// Its ruling confirms both halves: "The spells are cast one after the other
/// during the resolution of Primeval Spawn's last ability", and "you may cast
/// the back face of a modal double-faced card or either face of a split card as
/// long as the spells you are casting together have a total mana value of 10 or
/// less."
///
/// Before this change the budget was parsed nowhere at all (the constraint
/// combinators anchor on "with mana value ", which "with total mana value" never
/// matches), so an unbounded lingering permission was granted instead — both the
/// MV 7 and the MV 4 spell would have been castable.
#[test]
fn primeval_spawn_enforces_its_running_total_mana_value_budget() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spawn = scenario
        .add_creature(P0, "Primeval Spawn", 7, 7)
        .from_oracle_text_with_keywords(&["vigilance", "trample", "lifelink"], PRIMEVAL_SPAWN)
        .id();
    // Seeded top-down. Total across all three is 13 > 10, so the budget must
    // start refusing selections partway through.
    let mv_two = scenario
        .add_spell_to_library_top(P0, "Spawn MV2", false)
        .with_mana_cost(ManaCost::generic(2))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mv_four = scenario
        .add_spell_to_library_top(P0, "Spawn MV4", false)
        .with_mana_cost(ManaCost::generic(4))
        .from_oracle_text("You gain 1 life.")
        .id();
    let mv_seven = scenario
        .add_spell_to_library_top(P0, "Spawn MV7", false)
        .with_mana_cost(ManaCost::generic(7))
        .from_oracle_text("You gain 1 life.")
        .id();

    let mut runner = scenario.build();
    let execute = engine::parser::oracle_effect::parse_effect_chain(
        PRIMEVAL_SPAWN_TRIGGER_BODY,
        engine::types::ability::AbilityKind::Spell,
    );
    accept_exile_set_cast(&mut runner, spawn, &execute, None);

    let WaitingFor::CastOffer {
        kind:
            CastOfferKind::FreeCastWindow {
                candidates,
                remaining_mv_budget,
                ..
            },
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "Primeval Spawn must open its resolution-scoped window, parked at {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(
        remaining_mv_budget,
        Some(10),
        "CR 202.3: the stated total-mana-value budget must reach the window"
    );
    for card in [mv_seven, mv_four, mv_two] {
        assert!(
            candidates.contains(&card),
            "every exiled spell fits the full 10 budget on the first offer; \
             offered = {candidates:?}"
        );
    }

    // Spend 7 of the 10.
    runner
        .act(GameAction::FreeCastWindowChoice {
            selection: Some(mv_seven),
        })
        .expect("free-casting the MV 7 spell must succeed");

    let WaitingFor::CastOffer {
        kind:
            CastOfferKind::FreeCastWindow {
                candidates,
                remaining_mv_budget,
                ..
            },
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the window must re-offer with a shrunken budget, parked at {:?}",
            runner.state().waiting_for
        )
    };
    assert_eq!(
        remaining_mv_budget,
        Some(3),
        "CR 202.3: the running total shrinks by the cast spell's mana value"
    );
    assert!(
        candidates.contains(&mv_two),
        "the MV 2 spell still fits the remaining 3; offered = {candidates:?}"
    );
    assert!(
        !candidates.contains(&mv_four),
        "CR 202.3: the MV 4 spell no longer fits the remaining 3 and must be \
         withdrawn from the offer; offered = {candidates:?}"
    );
}

/// The exact production outcome of a `from among` cast clause, so the
/// unrepresentable-cap rows can assert what DID happen rather than a list of
/// things that did not.
#[derive(Debug, PartialEq, Eq)]
enum FromAmongCastOutcome {
    /// A resolution-scoped free-cast window opened, carrying this bound.
    Window(Option<u8>),
    /// No window opened, but the exiled batch is still castable for free at a
    /// later priority. This is the `CastFromZoneDriver::LingeringPermission`
    /// downgrade: its resolver records per-object permissions with NO shared
    /// cast count, so a printed "up to N" becomes an uncapped, merely delayed,
    /// grant. It is the exact wrong answer this test forbids.
    UncappedLingeringPermission,
    /// No window, and no free-cast permission anywhere once the chain hands
    /// priority back: the clause was refused end to end.
    Refused,
}

/// CR 608.2c + CR 608.2g: the `from among` resolution-window route must REFUSE a
/// printed cast cap it cannot carry, not silently widen it.
///
/// Production-path companion to the parser regression
/// `from_among_cast_cap_the_window_cannot_represent_is_refused`. This is the
/// route-1 half of the maintainer's "both routes" ask; the route-2
/// (graveyard/hand) runtime half lives in `invoke_calamity_free_cast.rs`.
///
/// Two successive defects are pinned here, and only the second one is about the
/// runtime. Making `max_casts: None` mean "any number of spells" was correct for
/// the genuinely unbounded surface — but the cap reader produced that same
/// `None` from `u8::try_from(300).ok()`, so a card printing a FINITE cap of 300
/// became indistinguishable from one printing "any number of". The follow-up fix
/// separated those readings, then mapped the unrepresentable one to
/// `CastFromZoneDriver::LingeringPermission` — which grants a later-priority
/// permission with no shared cast count at all. That is still an uncapped grant.
/// CR 608.2c makes the printed "up to N" part of the instruction the controller
/// follows, so the only honest outcomes are to carry the bound or to refuse the
/// clause; `FromAmongCastOutcome` names all three possibilities so the
/// assertions below distinguish them instead of merely excluding one.
///
/// 256 (the exact boundary) and 300 (the wrap case) are the maintainer's
/// explicitly requested values, and `0` / `X` are the two other readings the
/// same authority refuses. None is a real card — the largest printed
/// "cast up to N" in the corpus is THREE — so all four are
/// representation-boundary hostile fixtures, not card surfaces.
#[test]
fn from_among_window_refuses_a_cap_it_cannot_carry_instead_of_granting_an_uncapped_permission() {
    /// The synthetic exile-then-cast surface under test, parameterized by its
    /// printed quantifier phrase ("up to two", "any number of", "up to 300") so
    /// the unbounded surface is exercised through the same fixture without
    /// fabricating "up to any number of".
    fn oracle_for(quantifier: &str) -> String {
        format!(
            "Exile the top four cards of your library. You may cast {quantifier} spells \
             from among them without paying their mana costs."
        )
    }

    /// Resolve the fixture and report what the production pipeline actually
    /// offered — a bounded/unbounded window, an uncapped lingering permission,
    /// or nothing at all.
    fn cast_outcome_for(quantifier: &str) -> FromAmongCastOutcome {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_spell_to_hand_from_oracle(P0, "Boundary Probe", false, &oracle_for(quantifier))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let mut probes = Vec::new();
        for i in 0..4 {
            probes.push(
                scenario
                    .add_spell_to_library_top(P0, &format!("Probe Spell {i}"), false)
                    .with_mana_cost(ManaCost::generic(1))
                    .from_oracle_text("You gain 1 life.")
                    .id(),
            );
        }
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Colorless, source, false, vec![])],
        );

        let mut runner = scenario.build();
        let outcome = runner.cast(source).accept_optional().resolve();
        if let WaitingFor::CastOffer {
            kind: CastOfferKind::FreeCastWindow {
                remaining_casts, ..
            },
            ..
        } = outcome.final_waiting_for()
        {
            return FromAmongCastOutcome::Window(*remaining_casts);
        }
        drop(outcome);

        // REACH GUARD: distinguishing "refused" from "uncapped lingering
        // permission" is only meaningful once the resolution chain has finished
        // and handed priority back. A run that parked on some other
        // `WaitingFor` would report an empty permission scan vacuously, so
        // require the empty-stack priority window before reading it.
        let mut reached_empty_stack_priority = false;
        for _ in 0..24 {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                && runner.state().stack.is_empty()
            {
                reached_empty_stack_priority = true;
                break;
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }
        assert!(
            reached_empty_stack_priority,
            "{quantifier:?}: the chain must finish and hand priority back with an empty \
             stack before the permission scan is meaningful; parked at {:?} with stack {}",
            runner.state().waiting_for,
            runner.state().stack.len(),
        );

        let state = runner.state();
        // REACH GUARD (producer): an empty permission scan is only evidence of a
        // REFUSAL if the cards the refused clause would have offered actually
        // exist in the zone it would have offered them from. Probes still
        // sitting in the LIBRARY are uncastable for a completely different
        // reason — an upstream parse or lowering loss that swallowed the exile
        // step along with the cast step — and would make every row below pass
        // without ever exercising the exile-then-refusal path. Require all four
        // in `Zone::Exile` first.
        for (index, probe) in probes.iter().enumerate() {
            let zone = state.objects.get(probe).map(|object| object.zone);
            assert_eq!(
                zone,
                Some(Zone::Exile),
                "{quantifier:?}: probe {index} must have been exiled by the clause \
                 preceding the refused cast — a probe left in {zone:?} is uncastable \
                 for the wrong reason and would make the refusal assertion vacuous"
            );
        }
        let available = spell_objects_available_to_cast(state, P0);
        if probes.iter().any(|probe| available.contains(probe)) {
            return FromAmongCastOutcome::UncappedLingeringPermission;
        }
        FromAmongCastOutcome::Refused
    }

    /// Every `Effect::Unimplemented` gap name the parser produced for `oracle`,
    /// walking the same ability / sub-ability spine the cast accessors walk.
    fn gap_names(oracle: &str) -> Vec<String> {
        fn walk(definition: &AbilityDefinition, out: &mut Vec<String>) {
            if let Effect::Unimplemented { name, .. } = definition.effect.as_ref() {
                out.push(name.clone());
            }
            if let Some(sub) = definition.sub_ability.as_deref() {
                walk(sub, out);
            }
        }
        let parsed = parse(oracle, "Boundary Probe", &["Sorcery"]);
        let mut names = Vec::new();
        for definition in &parsed.abilities {
            walk(definition, &mut names);
        }
        for execute in parsed.triggers.iter().filter_map(|t| t.execute.as_deref()) {
            walk(execute, &mut names);
        }
        names
    }

    // REACH GUARD (mandatory paired positive): the identical surface with an
    // in-range cap DOES open a resolution-scoped window carrying exactly that
    // printed bound, and raises no gap. Without this the refusals below could
    // pass simply because the fixture never reaches the window seam.
    assert_eq!(
        cast_outcome_for("up to two"),
        FromAmongCastOutcome::Window(Some(2)),
        "reach guard: an in-range printed cap must open a window bounded by it"
    );
    assert!(
        gap_names(&oracle_for("up to two")).is_empty(),
        "reach guard: the in-range surface must parse cleanly, with no gap node"
    );

    // The one surface that legitimately means unbounded still does.
    assert_eq!(
        cast_outcome_for("any number of"),
        FromAmongCastOutcome::Window(None),
        "\"any number of spells\" is the one surface that legitimately opens an \
         UNBOUNDED window, and must keep doing so"
    );

    for printed_cap in ["256", "300", "0", "X"] {
        let quantifier = format!("up to {printed_cap}");

        // The EXACT parser result: the shared strict-refusal gap node, and no
        // `CastFromZone` anywhere on the spine. Asserting the exact shape is
        // what keeps the runtime row below non-vacuous — an upstream parse loss
        // would also produce "no window", but it would not produce this name.
        assert_eq!(
            gap_names(&oracle_for(&quantifier)),
            vec!["unrepresentable_cast_cap".to_string()],
            "\"{quantifier}\" must lower to exactly the shared cast-cap refusal gap \
             (see UNREPRESENTABLE_CAST_CAP_GAP in parser::oracle_effect)"
        );

        // The EXACT production outcome: not a window, and — the part the
        // previous round got wrong — not a lingering permission either.
        assert_eq!(
            cast_outcome_for(&quantifier),
            FromAmongCastOutcome::Refused,
            "\"{quantifier}\" must be refused outright. A window would carry a bound the \
             engine invented; an uncapped lingering permission lets every exiled card be \
             cast for free at a later priority, which is strictly more permissive than \
             the printed instruction (CR 608.2c)."
        );
    }
}

/// Verbatim Scryfall Oracle text. Paid, capped at one, AND duration-bearing —
/// five cards exiled, one artifact spell castable "this turn".
const CHISS_GORIA_FORGE_TYRANT: &str = "Affinity for artifacts (This spell costs {1} less to cast for each artifact you control.)\nFlying, haste\nWhenever Chiss-Goria attacks, exile the top five cards of your library. You may cast an artifact spell from among them this turn. If you do, it has affinity for artifacts.";
/// Verbatim Scryfall Oracle text. The paid route with no duration at all, so it
/// isolates the `without_paying` axis from the duration axis.
const NATHAN_DRAKE: &str = "First strike\nYou may spend mana as though it were mana of any color to cast spells you don't own or to activate abilities of permanents you control but don't own.\nWhenever Nathan Drake attacks, exile the top card of each player's library. You may cast a spell from among those cards.";
/// Verbatim Scryfall Oracle text. Paid + a LEADING duration, so the printed cap
/// is present before the duration seam and the paid seam both run.
const LOCKE_TREASURE_HUNTER: &str = "Locke can't be blocked by creatures with greater power.\nMug — Whenever Locke attacks, each player mills a card. If a land card was milled this way, create a Treasure token. Until end of turn, you may cast a spell from among those cards.";
/// Verbatim Scryfall Oracle text. The CR 305.1 land-play sibling with a PLURAL
/// head noun ("play **lands** from among those cards"), i.e. no printed cap at
/// all. It is the control for the plural reader: the reader knew only "spells"
/// and "cards", so this card read as a printed cap of ONE and, once caps started
/// being honored, was falsely refused.
const THE_OMENKEEL: &str = "Whenever a Vehicle you control deals combat damage to a player, that player exiles that many cards from the top of their library. You may play lands from among those cards for as long as they remain exiled.\nCrew 1";

/// Every `Effect::Unimplemented` gap name on a parsed card's ability spine.
fn all_gap_names(oracle: &str, name: &str, types: &[&str]) -> Vec<String> {
    fn walk(definition: &AbilityDefinition, out: &mut Vec<String>) {
        if let Effect::Unimplemented { name, .. } = definition.effect.as_ref() {
            out.push(name.clone());
        }
        if let Some(sub) = definition.sub_ability.as_deref() {
            walk(sub, out);
        }
        if let Some(alt) = definition.else_ability.as_deref() {
            walk(alt, out);
        }
    }
    let parsed = parse(oracle, name, types);
    let mut names = Vec::new();
    for definition in &parsed.abilities {
        walk(definition, &mut names);
    }
    for execute in parsed.triggers.iter().filter_map(|t| t.execute.as_deref()) {
        walk(execute, &mut names);
    }
    names
}

/// CR 608.2c: every `from among` route whose selected mechanism cannot carry the
/// printed bound refuses the clause, on REAL cards.
///
/// This is the general form of the defect. The earlier rounds fixed the loudest
/// case — a bound the representation could not express at all (`up to 300`,
/// `up to X`) — but a bound of `1` or `2` is perfectly expressible and was still
/// dropped, silently, the moment the clause was paid, land-play, or
/// duration-bearing. `LingeringPermission`'s resolver
/// (`record_lingering_permissions`) writes an INDEPENDENT `CastingPermission`
/// per object with no grant-scoped ledger, so "cast **a** Vehicle or artifact
/// creature spell from among them" over a batch of six granted all six.
///
/// These four are the entire real-card fallout of the fix, taken from the
/// regenerated corpus diff: every one of them printed a cap the engine ignored.
/// Each row asserts the EXACT gap name, so an unrelated upstream parse loss
/// cannot satisfy it.
#[test]
fn real_cards_whose_printed_cap_no_mechanism_can_carry_are_refused() {
    for (name, oracle, types, axis) in [
        (
            "Sanwell, Avenger Ace",
            SANWELL,
            &["Creature", "Legendary"][..],
            "paid, cap of one over a batch of six",
        ),
        (
            "Chiss-Goria, Forge Tyrant",
            CHISS_GORIA_FORGE_TYRANT,
            &["Creature", "Legendary", "Artifact"][..],
            "paid + duration, cap of one over a batch of five",
        ),
        (
            "Nathan Drake, Treasure Hunter",
            NATHAN_DRAKE,
            &["Creature", "Legendary"][..],
            "paid, no duration",
        ),
        (
            "Locke, Treasure Hunter",
            LOCKE_TREASURE_HUNTER,
            &["Creature", "Legendary"][..],
            "paid + leading duration",
        ),
    ] {
        let gaps = all_gap_names(oracle, name, types);
        assert!(
            gaps.iter().any(|gap| gap == "unrepresentable_cast_cap"),
            "{name} ({axis}): the printed cap must refuse the clause outright — \
             granting an uncapped permission over the whole batch is strictly \
             more permissive than the printed instruction (CR 608.2c). gaps = {gaps:?}"
        );
    }
}

/// CR 305.1: the plural land-play sibling is NOT refused.
///
/// The paired negative for the row above, and the reason it is load-bearing: the
/// plural reader enumerated "spells" and "cards" only, so The Omenkeel's
/// "you may play **lands** from among those cards" read as a printed cap of ONE
/// and was falsely refused the moment caps started being honored. A fix that
/// refuses too much is not a fix — the bound has to be READ correctly before it
/// can be honored correctly.
#[test]
fn the_omenkeel_plural_land_play_is_not_refused() {
    let parsed = parse(
        THE_OMENKEEL,
        "The Omenkeel",
        &["Artifact", "Legendary", "Vehicle"],
    );
    let Effect::CastFromZone { driver, mode, .. } = parsed_cast_from_zone(&parsed) else {
        unreachable!("helper returns CastFromZone")
    };
    assert_eq!(
        *mode,
        engine::types::ability::CardPlayMode::Play,
        "The Omenkeel grants a CR 305.1 land play, not a cast"
    );
    assert_eq!(
        *driver,
        CastFromZoneDriver::LingeringPermission,
        "a plural head noun prints no cap, so the land-play permission stands"
    );
}

/// CR 608.2c: the PAID route, end to end through production — the batch is not
/// castable at all after the refusal.
///
/// Sanwell's printed instruction is one cast out of six exiled cards. The parser
/// refusal above is only half the claim; this row proves the runtime consequence,
/// which is what the defect actually was: before the fix, every matching exiled
/// card carried a standing casting permission the controller could exercise at
/// any later priority window.
///
/// The `Zone::Exile` reach guard is mandatory here for the same reason it is in
/// the unrepresentable-cap runtime row: probes left in the library are
/// uncastable for the wrong reason and would make the assertion vacuous.
#[test]
fn a_refused_paid_batch_cast_grants_no_casting_permission_in_production() {
    /// The synthetic exile-then-paid-cast surface — Sanwell's grammar with the
    /// type gate dropped so the probes are plain spells — parameterized by the
    /// printed quantifier so the unbounded control runs the identical fixture.
    fn oracle_for(quantifier: &str) -> String {
        format!(
            "Exile the top four cards of your library. You may cast {quantifier} from among them."
        )
    }

    fn probe_zones_and_castability(quantifier: &str) -> (Vec<Zone>, Vec<ObjectId>) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_spell_to_hand_from_oracle(P0, "Paid Batch Probe", false, &oracle_for(quantifier))
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let mut probes = Vec::new();
        for i in 0..4 {
            probes.push(
                scenario
                    .add_spell_to_library_top(P0, &format!("Paid Probe {i}"), false)
                    .with_mana_cost(ManaCost::generic(1))
                    .from_oracle_text("You gain 1 life.")
                    .id(),
            );
        }
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Colorless, source, false, vec![])],
        );

        let mut runner = scenario.build();
        let outcome = runner.cast(source).accept_optional().resolve();
        drop(outcome);

        // REACH GUARD: the permission scan is only meaningful once the chain has
        // finished and handed priority back with an empty stack.
        let mut reached_empty_stack_priority = false;
        for _ in 0..24 {
            if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
                && runner.state().stack.is_empty()
            {
                reached_empty_stack_priority = true;
                break;
            }
            if runner.act(GameAction::PassPriority).is_err() {
                break;
            }
        }
        assert!(
            reached_empty_stack_priority,
            "{quantifier:?}: the chain must hand priority back with an empty stack \
             before the permission scan is meaningful; parked at {:?}",
            runner.state().waiting_for,
        );

        let state = runner.state();
        let zones = probes
            .iter()
            .map(|probe| {
                state
                    .objects
                    .get(probe)
                    .map(|object| object.zone)
                    .expect("probe object must still exist")
            })
            .collect::<Vec<_>>();
        let available = spell_objects_available_to_cast(state, P0);
        let castable = probes
            .iter()
            .copied()
            .filter(|probe| available.contains(probe))
            .collect::<Vec<_>>();
        (zones, castable)
    }

    // REACH GUARD (mandatory paired positive): the identical surface with an
    // UNBOUNDED plural head prints no cap, keeps the paid lingering permission,
    // and therefore DOES leave the exiled batch castable. Without this row the
    // refusal below could pass because the fixture never produced an exile
    // batch, or because paid batch permissions stopped working entirely.
    let (control_zones, control_castable) = probe_zones_and_castability("spells");
    assert!(
        control_zones.iter().all(|zone| *zone == Zone::Exile),
        "reach guard: the unbounded control must exile all four probes, got {control_zones:?}"
    );
    // MORE than one, deliberately: this is the exact shape of the defect. A
    // per-object `CastingPermission` is written for EVERY member of the batch, so
    // the surface that a "cast a spell" clause used to reach offers the whole
    // batch at once. That is what makes the cap undroppable rather than merely
    // untidy — and it is what the refusal below has to remove.
    assert!(
        control_castable.len() > 1,
        "reach guard: an unbounded paid batch grant makes the WHOLE batch \
         castable ({} of 4 here) — that is the uncapped permission a printed cap \
         of one must never decay into",
        control_castable.len()
    );

    // The refusal: a printed cap of one over a batch of four. The lingering
    // permission cannot stop the second cast, so the clause is refused and NO
    // probe is castable.
    let (zones, castable) = probe_zones_and_castability("a spell");
    assert!(
        zones.iter().all(|zone| *zone == Zone::Exile),
        "the exile step preceding the refused cast must still run — a probe left \
         in the library is uncastable for the wrong reason, got {zones:?}"
    );
    assert!(
        castable.is_empty(),
        "CR 608.2c: a paid batch cast printing a cap of one must grant NO casting \
         permission rather than an uncapped one over the whole batch; still \
         castable = {castable:?}"
    );
}

/// CR 611.2a: the SENTENCE-leading duration seam refuses a bound it would drop.
///
/// `split_clause_sequence` cuts "Until end of turn, you may play lands **and**
/// cast … from among cards exiled this way …" into two chunks and only the first
/// carries the stripped prefix, so the cast half is reconciled by
/// `apply_sentence_duration_to_coordinated_casts` → `reconcile_coordinated_cast`
/// — a structurally distinct seam from the single-chunk `with_clause_duration`
/// one, with its own copy of the silent degrade.
///
/// Magus of the Mind is the real card on this seam and prints a PLURAL head, so
/// it degrades cleanly and is the reach guard. The capped fixture is synthetic:
/// no printed card coordinates a land play with a capped cast, which is exactly
/// why this seam was never exercised with a bound and kept silently dropping one.
#[test]
fn sentence_leading_duration_over_a_capped_coordinated_cast_refuses() {
    // REACH GUARD: the real card on this seam still reconciles to a durational
    // lingering permission rather than refusing.
    let magus = parse(MAGUS_OF_THE_MIND, "Magus of the Mind", &["Creature"]);
    let Effect::CastFromZone {
        driver, duration, ..
    } = parsed_cast_from_zone(&magus)
    else {
        unreachable!("helper returns CastFromZone")
    };
    assert_eq!(
        *driver,
        CastFromZoneDriver::LingeringPermission,
        "reach guard: Magus prints an unbounded plural head, so the coordinated \
         duration must still degrade it cleanly"
    );
    assert_eq!(
        *duration,
        Some(Duration::UntilEndOfTurn),
        "reach guard: the sentence-leading duration must still reach the cast half"
    );

    // The same coordinated grammar with a printed cap. The duration selects the
    // lingering mechanism, which has no shared budget for the cap, so the clause
    // refuses instead of becoming an uncapped end-of-turn free-cast permission.
    //
    // The cap is stated by the SINGULAR head noun rather than by "up to two" on
    // purpose: an explicit "up to N" on this surface is claimed earlier by
    // `try_parse_counted_free_cast_from_exiled_this_way`, which lowers to
    // `Effect::FreeCastFromZones` (its own count channel) and never reaches the
    // `CastFromZone` reconciliation seam under test here. The singular form
    // reaches the seam carrying `max_casts: Some(1)`.
    let gaps = all_gap_names(
        "{U}, {T}, Sacrifice this creature: Shuffle your library, then exile the top four cards. \
         Until end of turn, you may play lands and cast a spell from among cards exiled \
         this way without paying its mana cost.",
        "Capped Coordinated Probe",
        &["Creature"],
    );
    assert!(
        gaps.iter()
            .any(|gap| gap == engine::types::ability::CAST_BOUND_LOST_TO_DURATION_GAP),
        "the coordinated-duration seam must refuse the printed cap through the \
         shared duration-scoped gap, got gaps {gaps:?}"
    );
}
