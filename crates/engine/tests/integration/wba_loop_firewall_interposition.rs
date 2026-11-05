//! CR 732.2a INTERPOSITION acceptance: an observer on the combo board does not refuse the
//! loop-shortcut offer, since CR 732.2b gives every other player the deviation mechanism.

use std::sync::Arc;

use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityKind, Effect, ReplacementCondition, ReplacementDefinition, TargetFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

use super::sprout_inalla_realistic_offer::{drive_sprout_cast, load_realistic_dump};

const P0: PlayerId = PlayerId(0);

/// The three census lands this row drives, each with its VERBATIM Oracle text and its printed
/// subtypes. All three parse to one `UnlessControlsMatching` entry replacement — a live
/// battlefield census.
///
/// They are deliberately three DIFFERENT censuses — a supertype+type census, a colour census
/// and a subtype census — so the row is about the class of card and not about one filter shape.
const CENSUS_LANDS: [(&str, &str, &[&str]); 3] = [
    (
        "Barad-dûr",
        "Barad-dûr enters tapped unless you control a legendary creature.\n{T}: Add {B}.\n\
         {X}{X}{B}, {T}: Amass Orcs X. Activate only if a creature died this turn.",
        &[],
    ),
    (
        "Taiga Stadium",
        "Taiga Stadium enters tapped unless you control a white, blue, or black permanent.\n\
         {T}: Add {R} or {G}.",
        &[],
    ),
    (
        "Country Roads",
        "This land enters tapped unless you control a Mount or Vehicle.\n{T}: Add {W}.\n\
         {1}{W}, {T}, Sacrifice this land: Create a 1/1 colorless Pilot creature token with \
         \"This token saddles Mounts and crews Vehicles as though its power were 2 greater.\" \
         Activate only as a sorcery.",
        &[],
    ),
];

/// Parse a census land's real Oracle text and hand back its single replacement definition.
///
/// `expected` is the condition arm the CALLER's row reads, and it is required: every row this
/// helper feeds separates on which arm its fixture reaches, so no call site can reach one
/// without declaring which arm it depends on. Only the discriminant is compared — `expected`'s
/// payload is a placeholder.
fn census_land_def(
    name: &str,
    oracle: &str,
    subtypes: &[&str],
    expected: ReplacementCondition,
) -> ReplacementDefinition {
    let subs: Vec<String> = subtypes.iter().map(|s| (*s).to_string()).collect();
    let parsed = engine::parser::parse_oracle_text(oracle, name, &[], &["Land".to_string()], &subs);
    assert_eq!(
        parsed.replacements.len(),
        1,
        "fixture pin: {name} parses to exactly ONE replacement definition; a parser change that \
         splits or merges it re-points every arm of every row this helper feeds"
    );
    let def = parsed.replacements[0].clone();
    // The CR 614.1d self-entry triple, asserted on the real parse.
    assert_eq!(
        (
            def.event.clone(),
            def.valid_card.clone(),
            def.destination_zone
        ),
        (
            ReplacementEvent::Moved,
            Some(TargetFilter::SelfRef),
            Some(Zone::Battlefield)
        ),
        "fixture pin: {name} carries the CR 614.1d self-entry triple (Moved / SelfRef / \
         Battlefield)"
    );
    // Debug's leading identifier is the variant, and the variant is the whole of what this
    // assert compares; `expected`'s payload is a placeholder and must never print as a fixture
    // value.
    let expected_arm: String = format!("{expected:?}")
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    assert_eq!(
        def.condition.as_ref().map(std::mem::discriminant),
        Some(std::mem::discriminant(&expected)),
        "fixture pin: {name} must parse to {expected_arm}, the arm its row reads, not {:?}",
        def.condition
    );
    def
}

/// Put ONE census land on P0's battlefield, carrying `def` and NOTHING else — no abilities, no
/// triggers, no statics.
///
/// BOTH `base_replacement_definitions` AND `replacement_definitions` are written, or
/// `game/layers.rs`'s per-pass reset drops the definition and every arm silently reads an empty
/// store (shipped fixture precedent: `wba_fodder_multiset::graft_doubler`).
fn graft_census_land(state: &mut GameState, name: &str, def: ReplacementDefinition) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let host = create_object(state, card_id, P0, name.to_string(), Zone::Battlefield);
    let obj = state
        .objects
        .get_mut(&host)
        .expect("the just-created census land is in `objects`");
    obj.card_types.core_types = vec![CoreType::Land];
    obj.base_replacement_definitions = Arc::new(vec![def.clone()]);
    obj.replacement_definitions = vec![def].into();
    host
}

/// Rewrite the grafted definition's `valid_card` from `SelfRef` to `Typed{Land}` — CR 614.1d's
/// OTHER half, "[Objects] enter [the battlefield] . . .".
///
/// Written through `Arc::make_mut` on `base_replacement_definitions` and mirrored into the live
/// store, because `game/layers.rs` re-seeds the live store from the base store on every pass: a
/// mutation applied to the live vector alone is erased before the drive sees it.
fn make_it_watch_every_land(state: &mut GameState, host: ObjectId) {
    let obj = state
        .objects
        .get_mut(&host)
        .expect("the census land is live");
    let base = Arc::make_mut(&mut obj.base_replacement_definitions);
    assert_eq!(
        base.len(),
        1,
        "reach-guard: exactly one grafted definition to rewrite"
    );
    base[0].valid_card = Some(TargetFilter::Typed(TypedFilter::land()));
    obj.replacement_definitions = base.clone().into();
}

/// Count the battlefield Saprolings `who` controls — the cast-resolved reach-guard oracle.
fn count_saprolings(state: &GameState, who: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.controller == who && o.name == "Saproling")
        })
        .count()
}

/// Drive one live Sprout Swarm recast on `state` and report `(offered, final_waiting_for_label)`,
/// asserting the cast-resolved reach-guards that hold in BOTH directions first — without them a
/// "no offer" could mean "the harness never drove anything".
fn drive_and_report(state: GameState, why: &str) -> bool {
    let before = count_saprolings(&state, P0);
    let outcome = drive_sprout_cast(state);
    assert_eq!(
        outcome.zone_of(ObjectId(405)),
        Zone::Hand,
        "{why} reach-guard: Buyback must return Sprout Swarm to P0's hand, i.e. the cast really \
         resolved"
    );
    assert_eq!(
        count_saprolings(outcome.state(), P0),
        before + 1,
        "{why} reach-guard: the iteration created exactly one more Saproling"
    );
    match outcome.final_waiting_for() {
        WaitingFor::LoopShortcut { proposer, .. } if *proposer == P0 => true,
        // A POSITIVE pin, not merely `!LoopShortcut`: the drive must land on ordinary priority
        // for P0 — the no-offer state — so a refusing arm cannot be satisfied by a wedge.
        WaitingFor::Priority { player } if *player == P0 => false,
        other => panic!("{why}: unexpected terminal prompt {other:?}"),
    }
}

/// Three real entry-census lands offer on the combo board whether their entry replacement watches
/// only themselves or every land: the observer is the replay's to judge (CR 732.2a).
#[test]
fn spent_self_entry_relief_offers_on_three_real_entry_census_lands() {
    for (name, oracle, subtypes) in CENSUS_LANDS {
        let def = census_land_def(
            name,
            oracle,
            subtypes,
            ReplacementCondition::UnlessControlsMatching {
                filter: TargetFilter::None,
            },
        );
        for watches_every_land in [false, true] {
            let mut board = load_realistic_dump();
            let host = graft_census_land(&mut board, name, def.clone());
            if watches_every_land {
                make_it_watch_every_land(&mut board, host);
            }
            assert!(
                drive_and_report(board, name),
                "{name}, watches every land: {watches_every_land}"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// CR 732.2a PROPOSAL-ABSENCE acceptance: census lands whose activated ability the proposed
// sequence never activates.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// The three census lands [`unactivated_ability_relief_offers_on_three_real_census_lands`] drives,
/// each with its VERBATIM Oracle text from the pinned card-data export. Each carries TWO
/// activated abilities: a mana ability (`{T}: Add ..`) and a second, non-mana ability whose body reads the
/// board. They are deliberately three DIFFERENT reads — a counter sweep over every creature
/// you control, a token mint with a board-scaled cost reduction, and a P/T sweep over every
/// creature you control — so the row is about the class of card and not about one effect shape.
const PROPOSAL_LANDS: [(&str, &str, &[&str]); 3] = [
    (
        "Abandoned Air Temple",
        "This land enters tapped unless you control a basic land.\n{T}: Add {W}.\n\
         {3}{W}, {T}: Put a +1/+1 counter on each creature you control.",
        &[],
    ),
    (
        "The Lonely Mountain",
        "({T}: Add {R}.)\nThis land enters tapped unless you control an Equipment.\n\
         {4}{R}, {T}: Create a 2/2 red Dwarf creature token. This ability costs {1} less to \
         activate for each Equipment you control. Activate only as a sorcery.",
        &["Mountain"],
    ),
    (
        "Castle Embereth",
        "This land enters tapped unless you control a Mountain.\n{T}: Add {R}.\n\
         {1}{R}{R}, {T}: Creatures you control get +1/+0 until end of turn.",
        &[],
    ),
];

/// Fire Nation Palace's VERBATIM Oracle text, from the same export. Its non-mana ability grants a
/// keyword to its one target, which reads nothing the loop grows.
const FIRE_NATION_PALACE: (&str, &str, &[&str]) = (
    "Fire Nation Palace",
    "This land enters tapped unless you control a basic land.\n{T}: Add {R}.\n\
     {1}{R}, {T}: Target creature you control gains firebending 4 until end of turn. \
     (Whenever it attacks, add {R}{R}{R}{R}. This mana lasts until end of combat.)",
    &[],
);

/// Chocobo Camp's VERBATIM Oracle text, from the same export.
const CHOCOBO_CAMP: (&str, &str, &[&str]) = (
    "Chocobo Camp",
    "This land enters tapped unless you control a legendary creature.\n\
     {T}: Add {G}. When you next cast a Bird creature spell this turn, it enters with an \
     additional +1/+1 counter on it.\n\
     {2}{G}{G}, {T}: Create a 2/2 green Bird creature token with \"Whenever a land you control \
     enters, this token gets +1/+0 until end of turn.\"",
    &[],
);

/// Put ONE land on P0's battlefield carrying its REAL parsed abilities AND its real entry
/// replacement, and nothing else. Both replacement stores are written for the same reason
/// [`graft_census_land`] writes both: `game/layers.rs` re-seeds the live store from the base
/// store on every pass.
fn graft_full_land(state: &mut GameState, card: (&str, &str, &[&str])) -> ObjectId {
    let (name, oracle, subtypes) = card;
    let subs: Vec<String> = subtypes.iter().map(|s| (*s).to_string()).collect();
    let parsed = engine::parser::parse_oracle_text(oracle, name, &[], &["Land".to_string()], &subs);
    assert_eq!(
        parsed.replacements.len(),
        1,
        "fixture pin: {name} parses to exactly ONE replacement definition (the CR 614.1d entry \
         condition)"
    );
    assert_eq!(
        nonmana_ability_index(&parsed.abilities).len(),
        1,
        "fixture pin: {name} parses to exactly ONE NON-mana activated ability"
    );
    assert!(
        parsed.triggers.is_empty() && parsed.statics.is_empty(),
        "fixture pin: {name} carries NO triggers and NO static abilities"
    );

    let card_id = CardId(state.next_object_id);
    let host = create_object(state, card_id, P0, name.to_string(), Zone::Battlefield);
    let obj = state
        .objects
        .get_mut(&host)
        .expect("the just-created land is in `objects`");
    obj.card_types.core_types = vec![CoreType::Land];
    obj.abilities = Arc::new(parsed.abilities.clone());
    obj.base_replacement_definitions = Arc::new(parsed.replacements.clone());
    obj.replacement_definitions = parsed.replacements.into();
    host
}

/// The indices of the parsed abilities that are NOT CR 605.1a mana abilities.
///
/// A PREDICATE rather than a positional pin, because a land's parsed ability list is not its
/// exported one: intrinsic basic-land-type mana abilities are attached by the database loader,
/// so The Lonely Mountain exports two abilities and parses to one, while Chocobo Camp exports
/// and parses two.
fn nonmana_ability_index(abilities: &[engine::types::ability::AbilityDefinition]) -> Vec<usize> {
    abilities
        .iter()
        .enumerate()
        .filter(|(_, a)| !engine::game::mana_abilities::is_mana_ability(a))
        .map(|(i, _)| i)
        .collect()
}

/// Rewrite the grafted land's sole NON-mana ability from `Activated` to `Spell` kind —
/// CR 117.1b's other side.
fn spellify_the_nonmana_ability(state: &mut GameState, host: ObjectId) {
    let obj = state.objects.get_mut(&host).expect("the land is live");
    let abilities = Arc::make_mut(&mut obj.abilities);
    let targets = nonmana_ability_index(abilities);
    assert_eq!(
        targets.len(),
        1,
        "reach-guard: exactly one non-mana ability to rewrite"
    );
    assert_eq!(
        abilities[targets[0]].kind,
        AbilityKind::Activated,
        "reach-guard: the non-mana ability really is an ACTIVATED one"
    );
    abilities[targets[0]].kind = AbilityKind::Spell;
}

/// Flip `uses_tracked_set` on the CR 603.7 delayed triggered ability the grafted land's MANA
/// ability creates, so the payload resolves against the parent ability's tracked object set.
fn track_the_delayed_payload(state: &mut GameState, host: ObjectId) {
    let obj = state.objects.get_mut(&host).expect("the land is live");
    let abilities = Arc::make_mut(&mut obj.abilities);
    assert!(
        !nonmana_ability_index(abilities).contains(&0),
        "reach-guard: `abilities[0]` really is the CR 605.1a mana ability that carries the \
         delayed trigger"
    );
    let sub = abilities[0]
        .sub_ability
        .as_mut()
        .expect("reach-guard: the mana ability carries the delayed-trigger sub-ability");
    let Effect::CreateDelayedTrigger {
        uses_tracked_set, ..
    } = sub.effect.as_mut()
    else {
        panic!(
            "reach-guard: that sub-ability's effect is the `Effect::CreateDelayedTrigger` \
             this row is about"
        );
    };
    *uses_tracked_set = true;
}

/// Three real census lands whose activated ability reads the growing class offer on the combo
/// board, whether that ability is activated or spell-kind: the observer is the replay's to judge
/// (CR 732.2a).
#[test]
fn unactivated_ability_relief_offers_on_three_real_census_lands() {
    for card in PROPOSAL_LANDS {
        let name = card.0;
        for spellified in [false, true] {
            let mut board = load_realistic_dump();
            let host = graft_full_land(&mut board, card);
            if spellified {
                spellify_the_nonmana_ability(&mut board, host);
            }
            assert!(
                drive_and_report(board, name),
                "{name}, spell-kind: {spellified}"
            );
        }
    }
}

/// **Fire Nation Palace offers even when its ability is not one the proposal leaves out.** CR
/// 611.2c fixes the keyword grant to the one creature it targets, so the body reads nothing the
/// loop grows, and with its `kind` rewritten to `Spell` the ability still does not veto.
#[test]
fn a_single_target_keyword_grant_does_not_veto_the_offer() {
    assert!(
        drive_and_report(load_realistic_dump(), "baseline"),
        "BASELINE positive control: the untouched combo board OFFERS"
    );
    let name = FIRE_NATION_PALACE.0;

    let mut with_land = load_realistic_dump();
    graft_full_land(&mut with_land, FIRE_NATION_PALACE);
    assert!(
        drive_and_report(with_land, name),
        "ARM A ({name}): the proposal contains no activation of this ability"
    );

    let mut spellified = load_realistic_dump();
    let host = graft_full_land(&mut spellified, FIRE_NATION_PALACE);
    spellify_the_nonmana_ability(&mut spellified, host);
    assert!(
        drive_and_report(spellified, name),
        "ARM B ({name}): the proposal-absence relief no longer applies, and a keyword granted to \
         one target reads nothing the loop grows"
    );
}

/// Chocobo Camp offers on the combo board untapped and tapped, and with its mana ability's delayed
/// payload resolving against a tracked set: the observer is the replay's to judge (CR 732.2a).
#[test]
fn chocobo_camp_offers_untapped_and_tapped() {
    for tapped in [false, true] {
        let mut board = load_realistic_dump();
        let host = graft_full_land(&mut board, CHOCOBO_CAMP);
        board
            .objects
            .get_mut(&host)
            .expect("Chocobo Camp is live")
            .tapped = tapped;
        assert!(drive_and_report(board, "chocobo camp"), "tapped = {tapped}");
    }
    let mut tracked = load_realistic_dump();
    let host = graft_full_land(&mut tracked, CHOCOBO_CAMP);
    track_the_delayed_payload(&mut tracked, host);
    assert!(drive_and_report(tracked, "tracked-set chocobo camp"));
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// CR 732.2a SUBTYPE-CENSUS acceptance: the `UnlessControlsSubtype` check lands beside an
// `UnlessControlsOtherLeq` sibling.
// ─────────────────────────────────────────────────────────────────────────────────────────

/// The two `UnlessControlsSubtype` check lands, VERBATIM Oracle text from the pinned export.
/// Both are `core_types: [Land]` with no printed subtypes and one parsed replacement each.
const CHECK_LANDS: [(&str, &str, &[&str]); 2] = [
    (
        "Dragonskull Summit",
        "This land enters tapped unless you control a Swamp or a Mountain.\n{T}: Add {B} or {R}.",
        &[],
    ),
    (
        "Hinterland Harbor",
        "This land enters tapped unless you control a Forest or an Island.\n{T}: Add {G} or {U}.",
        &[],
    ),
];

/// The untouched cluster sibling: `UnlessControlsOtherLeq`, whose scan arm is
/// `Axes::CONSERVATIVE` before and after this change. Same Oracle-text discipline.
const OTHER_LEQ_CONTROL: (&str, &str, &[&str]) = (
    "Blackcleave Cliffs",
    "This land enters tapped unless you control two or fewer other lands.\n{T}: Add {B} or {R}.",
    &[],
);

/// Two real subtype-census lands and their `UnlessControlsOtherLeq` sibling offer on the combo
/// board whether their entry replacement watches only themselves or every land: the observer is
/// the replay's to judge (CR 732.2a).
#[test]
fn check_lands_still_offer_with_the_subtype_arm_repaired() {
    let (control_name, control_oracle, control_subtypes) = OTHER_LEQ_CONTROL;
    let mut lands = vec![(
        control_name,
        census_land_def(
            control_name,
            control_oracle,
            control_subtypes,
            ReplacementCondition::UnlessControlsOtherLeq {
                count: 0,
                filter: TypedFilter::default(),
            },
        ),
    )];
    for (name, oracle, subtypes) in CHECK_LANDS {
        lands.push((
            name,
            census_land_def(
                name,
                oracle,
                subtypes,
                ReplacementCondition::UnlessControlsSubtype {
                    subtypes: Vec::new(),
                },
            ),
        ));
    }
    for (name, def) in lands {
        for watches_every_land in [false, true] {
            let mut board = load_realistic_dump();
            let host = graft_census_land(&mut board, name, def.clone());
            if watches_every_land {
                make_it_watch_every_land(&mut board, host);
            }
            assert!(
                drive_and_report(board, name),
                "{name}, watches every land: {watches_every_land}"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// CR 701.17 INSTRUCTED-MILL acceptance, and the shapes that must still veto.
//
// The arms above are about a loop whose period touches only the battlefield. This half adds
// the mill: `mill_base` is the same combo board plus a real parsed Altar of the Brood, so each
// period also declines every opponent's library. `certify_instructed_opponent_library_departure`
// establishes that decline by exclusion and the cover's four growth obligations consume it;
// C2 is the public-information veto that keeps the certificate honest, and every row below is
// the base plus ONE object that either reaches C2 or provably does not.
//
// CR 400.2 scopes the relief: the detection drive reads the board through the proposer's own
// hidden view, so an observer the proposer may not see cannot carry a veto, while the same
// observer in the proposer's OWN hand still does. That trio is what attributes the hand
// collection, and the interposer/descend rows at the end are the library one.
// ─────────────────────────────────────────────────────────────────────────────────────────

use engine::types::ability::{PlayerFilter, TriggerDefinition};
use engine::types::triggers::TriggerMode;

use super::witherbloom_altar_probe::graft_altar;

const P1: PlayerId = PlayerId(1);

/// Narcomoeba, VERBATIM Oracle text from the pinned `client/public/card-data.json` export.
/// Its library→graveyard trigger is the interposer row's whole subject.
const NARCOMOEBA: (&str, &str, &[&str]) = (
    "Narcomoeba",
    "Flying\nWhen this card is put into your graveyard from your library, you may put it onto \
     the battlefield.",
    &["Illusion"],
);

/// Gaea's Blessing, VERBATIM Oracle text from the same export. Its library→graveyard trigger is
/// MANDATORY and choice-free — `optional: false`, a `ChangeZoneAll` execute with no target
/// selection and no mode — which is what makes it the non-prompting half of the interposer pair.
const GAEAS_BLESSING: (&str, &str, &[&str]) = (
    "Gaea's Blessing",
    "Target player shuffles up to three target cards from their graveyard into their library.\n\
     Draw a card.\nWhen this card is put into your graveyard from your library, shuffle your \
     graveyard into your library.",
    &[],
);

/// The shared mill base: this module's own combo board plus a real Altar of the Brood, so the
/// loop's period mills every opponent. ONE object away from `load_realistic_dump()`, which is
/// why a hostile row built on it is one object away from an offering board and its silence has
/// exactly one cause.
pub(crate) fn mill_base() -> GameState {
    let mut state = load_realistic_dump();
    graft_altar(&mut state, P0);
    state
}

/// Graft a battlefield permanent under `seat` carrying `def` and nothing else — the
/// attributability control every trigger-shaped row below rests on.
///
/// `push_printed_trigger` is the single authority that keeps `base_trigger_definitions` and the
/// live list in lockstep; a bare `objects.insert` leaves the definition out of the base store,
/// `game/layers.rs`'s per-pass reset drops the live entry, and the arm reads an empty store.
fn graft_observer(
    state: &mut GameState,
    seat: PlayerId,
    zone: Zone,
    def: TriggerDefinition,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let host = create_object(state, card_id, seat, "Departure Observer".to_string(), zone);
    let obj = state
        .objects
        .get_mut(&host)
        .expect("the just-created observer is in `objects`");
    obj.card_types.core_types = vec![CoreType::Artifact];
    obj.push_printed_trigger(def);
    host
}

/// A second Altar of the Brood on P0's battlefield with `mutate` applied to the mill it
/// executes. The rows that use it flip exactly one field, so the pair separates on that field
/// and not on the extra permanent.
fn graft_mutated_altar(
    state: &mut GameState,
    mutate: impl FnOnce(&mut engine::types::ability::AbilityDefinition),
) -> ObjectId {
    let host = graft_altar(state, P0);
    let obj = state
        .objects
        .get_mut(&host)
        .expect("the second Altar is live");
    let base = Arc::make_mut(&mut obj.base_trigger_definitions);
    assert_eq!(
        base.len(),
        1,
        "reach-guard: exactly one grafted trigger to rewrite"
    );
    let execute = base[0]
        .execute
        .as_mut()
        .expect("reach-guard: Altar's trigger carries the mill it executes");
    mutate(execute);
    let rewritten = base[0].clone();
    obj.trigger_definitions.clear();
    obj.base_trigger_definitions = Arc::new(Vec::new());
    obj.push_printed_trigger(rewritten);
    host
}

/// Every non-caster library card rewritten NON-PERMANENT and surface-free.
///
/// On this board a milled card can neither flip `descended_this_turn` (CR 700.11 counts only a
/// permanent card) nor open a window nor carry a definition C2 could read, and it satisfies the
/// cover's own inertness obligation WITHOUT the proposer view having to blank it. That is what
/// makes each row below one live channel with the other provably absent: the pinned control
/// keeps offering when the redaction is taken away, so a row that reddens there reddens on its
/// own object.
fn pinned_mill_base() -> GameState {
    let mut state = mill_base();
    let pinned: Vec<ObjectId> = state
        .players
        .iter()
        .filter(|p| p.id != P0)
        .flat_map(|p| p.library.iter().copied())
        .collect();
    assert!(
        !pinned.is_empty(),
        "reach-guard: the non-caster libraries are non-empty, so the pin below is a rewrite \
         and not an empty walk"
    );
    for id in pinned {
        let obj = state.objects.get_mut(&id).expect("library card is keyed");
        obj.card_types.core_types = vec![CoreType::Instant];
        obj.card_types.supertypes.clear();
        obj.card_types.subtypes.clear();
        obj.trigger_definitions.clear();
        obj.base_trigger_definitions = Arc::new(Vec::new());
        obj.replacement_definitions.clear();
        obj.base_replacement_definitions = Arc::new(Vec::new());
        obj.static_definitions.clear();
        obj.base_static_definitions = Arc::new(Vec::new());
        obj.abilities = Arc::new(Vec::new());
        obj.base_abilities = Arc::new(Vec::new());
        obj.keywords.clear();
        obj.counters.clear();
    }
    state
}

/// The id at `depth` of `victim`'s library AS IT STANDS AFTER the loop-priming cast.
///
/// MEASURED LAW: that cast mills the top of every victim library before the detection drive
/// runs, so a card the drive must mill is placed by the POST-cast order, never the pre-cast
/// one — at pre-cast depth it is consumed by the priming cast and the row asserts against a
/// board that no longer holds the thing it checks. Driving a throwaway copy of the same cast
/// is what says which id that is; its own decline is this helper's reach-guard.
fn post_cast_library_anchor(base: &GameState, victim: PlayerId, depth: usize) -> ObjectId {
    let before = library_ids(base, victim).len();
    let outcome = drive_sprout_cast(base.clone());
    let after = library_ids(outcome.state(), victim);
    assert!(
        after.len() < before,
        "reach-guard: the priming cast must decline {victim:?}'s library, or 'post-cast depth' \
         names the same card as 'pre-cast depth' and the law this helper encodes is untested"
    );
    *after
        .get(depth)
        .expect("the post-cast library is deeper than the requested depth")
}

fn library_ids(state: &GameState, who: PlayerId) -> Vec<ObjectId> {
    state
        .players
        .iter()
        .find(|p| p.id == who)
        .expect("seat exists")
        .library
        .iter()
        .copied()
        .collect()
}

/// Move `card` to sit immediately before `anchor` in `victim`'s library.
fn place_before(state: &mut GameState, victim: PlayerId, anchor: ObjectId, card: ObjectId) {
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == victim)
        .expect("seat exists");
    let from = player
        .library
        .iter()
        .position(|id| *id == card)
        .expect("the card was created into this library");
    player.library.remove(from);
    let at = player
        .library
        .iter()
        .position(|id| *id == anchor)
        .expect("the anchor is still in this library");
    player.library.insert(at, card);
}

/// Put a real parsed library→graveyard card into `victim`'s library immediately before
/// `anchor`, carrying its own parsed trigger and its own core type.
///
/// Parameterized on the card because the interposition rows separate on WHICH card is milled
/// mid-collapse — one that asks its controller a question, one that does not — and every other
/// variable has to stay fixed for that separation to attribute. `core` feeds the parse as well
/// as the object: `CoreType`'s `Display` is exactly the word the parser takes.
fn graft_milled_card(
    state: &mut GameState,
    victim: PlayerId,
    anchor: ObjectId,
    card: (&str, &str, &[&str]),
    core: CoreType,
) -> ObjectId {
    let (name, oracle, subtypes) = card;
    let subs: Vec<String> = subtypes.iter().map(|s| (*s).to_string()).collect();
    let parsed = engine::parser::parse_oracle_text(oracle, name, &[], &[core.to_string()], &subs);
    assert_eq!(
        parsed.triggers.len(),
        1,
        "fixture pin: {name} parses to exactly ONE trigger — the library→graveyard window \
         this row is about; a parser change that splits or drops it un-points the row"
    );
    let card_id = CardId(state.next_object_id);
    let host = create_object(state, card_id, victim, name.to_string(), Zone::Library);
    {
        let obj = state
            .objects
            .get_mut(&host)
            .expect("the just-created interposer is in `objects`");
        obj.card_types.core_types = vec![core];
        obj.push_printed_trigger(parsed.triggers[0].clone());
    }
    place_before(state, victim, anchor, host);
    host
}

/// Put a real parsed Narcomoeba into `victim`'s library immediately before `anchor`.
fn graft_narcomoeba(state: &mut GameState, victim: PlayerId, anchor: ObjectId) -> ObjectId {
    graft_milled_card(state, victim, anchor, NARCOMOEBA, CoreType::Creature)
}

/// Put a real parsed Gaea's Blessing into `victim`'s library immediately before `anchor` — the
/// interposer whose milled trigger is mandatory and choice-free.
fn graft_gaeas_blessing(state: &mut GameState, victim: PlayerId, anchor: ObjectId) -> ObjectId {
    graft_milled_card(state, victim, anchor, GAEAS_BLESSING, CoreType::Sorcery)
}

/// One row: what it adds to its base, and whether the base's offer survives it.
struct MillRow {
    what: &'static str,
    board: fn() -> GameState,
    expect_offer: bool,
}

/// Drive EVERY row before asserting, so one disagreeing row does not hide the verdicts of the
/// rows after it — the control and its paired hostile arms are only evidence together.
fn run_rows(rows: &[MillRow]) {
    let observed: Vec<(&str, bool, bool)> = rows
        .iter()
        .map(|row| {
            (
                row.what,
                row.expect_offer,
                drive_and_report((row.board)(), row.what),
            )
        })
        .collect();
    let disagreed: Vec<String> = observed
        .iter()
        .filter(|(_, expected, actual)| expected != actual)
        .map(|(what, expected, actual)| {
            format!("  offer expected {expected}, got {actual} — {what}")
        })
        .collect();
    assert!(
        disagreed.is_empty(),
        "{} of {} rows disagreed with their expectation:\n{}",
        disagreed.len(),
        observed.len(),
        disagreed.join("\n")
    );
}

// ── Board suppliers: each is one row's ONE object ──

fn milled_mode_observer() -> GameState {
    let mut state = mill_base();
    graft_observer(
        &mut state,
        P1,
        Zone::Battlefield,
        TriggerDefinition::new(TriggerMode::Milled),
    );
    state
}

fn from_anywhere_graveyard_observer() -> GameState {
    let mut state = mill_base();
    graft_observer(
        &mut state,
        P1,
        Zone::Battlefield,
        TriggerDefinition::new(TriggerMode::ChangesZone).destination(Zone::Graveyard),
    );
    state
}

fn battlefield_origin_with_library_in_origin_zones() -> GameState {
    let mut state = mill_base();
    let mut def = TriggerDefinition::new(TriggerMode::ChangesZone).origin(Zone::Battlefield);
    def.origin_zones = vec![Zone::Library, Zone::Battlefield];
    graft_observer(&mut state, P1, Zone::Battlefield, def);
    state
}

fn battlefield_origin_alone() -> GameState {
    let mut state = mill_base();
    graft_observer(
        &mut state,
        P1,
        Zone::Battlefield,
        TriggerDefinition::new(TriggerMode::ChangesZone).origin(Zone::Battlefield),
    );
    state
}

fn hand_mill_observer(seat: PlayerId, functions_in: Zone) -> GameState {
    let mut state = mill_base();
    graft_observer(
        &mut state,
        seat,
        Zone::Hand,
        TriggerDefinition::new(TriggerMode::Milled).trigger_zones(vec![functions_in]),
    );
    state
}
fn mill_observer_in_an_opponents_hand() -> GameState {
    hand_mill_observer(P1, Zone::Hand)
}
fn mill_observer_in_the_proposers_own_hand() -> GameState {
    hand_mill_observer(P0, Zone::Hand)
}
fn battlefield_scoped_mill_observer_in_the_proposers_own_hand() -> GameState {
    hand_mill_observer(P0, Zone::Battlefield)
}

fn graveyard_diverting_replacement() -> GameState {
    let mut state = mill_base();
    let def = ReplacementDefinition::new(ReplacementEvent::Moved).destination_zone(Zone::Graveyard);
    graft_census_land(&mut state, "Graveyard Diverter", def);
    state
}

fn real_tapland_self_entry() -> GameState {
    let mut state = mill_base();
    let (name, oracle, subtypes) = CENSUS_LANDS[0];
    let def = census_land_def(
        name,
        oracle,
        subtypes,
        ReplacementCondition::UnlessControlsMatching {
            filter: TargetFilter::None,
        },
    );
    graft_census_land(&mut state, name, def);
    state
}

fn second_altar_opponent_facing() -> GameState {
    let mut state = mill_base();
    graft_mutated_altar(&mut state, |_| {});
    state
}
fn second_altar_each_player() -> GameState {
    let mut state = mill_base();
    graft_mutated_altar(&mut state, |execute| {
        execute.player_scope = Some(PlayerFilter::All);
    });
    state
}
fn second_altar_may_mill() -> GameState {
    let mut state = mill_base();
    graft_mutated_altar(&mut state, |execute| {
        execute.optional = true;
    });
    state
}

fn pinned_with_narcomoeba() -> GameState {
    let mut state = pinned_mill_base();
    let anchor = post_cast_library_anchor(&state, P1, 1);
    graft_narcomoeba(&mut state, P1, anchor);
    state
}

fn pinned_with_vanilla_permanent_at_depth_one() -> GameState {
    let mut state = pinned_mill_base();
    let anchor = post_cast_library_anchor(&state, P1, 1);
    let obj = state
        .objects
        .get_mut(&anchor)
        .expect("the anchored library card is keyed");
    assert!(
        obj.trigger_definitions.is_empty() && obj.base_trigger_definitions.is_empty(),
        "reach-guard: the pin left this card definition-free, so the ONLY variable this row \
         adds is its permanent type"
    );
    obj.card_types.core_types = vec![CoreType::Creature];
    state
}

/// Every seat's library size after one driven cycle — the reading that separates the two
/// silent rows below from the offering control they are each one field away from.
fn library_sizes_after_one_cycle(state: GameState) -> Vec<(PlayerId, usize)> {
    drive_sprout_cast(state)
        .state()
        .players
        .iter()
        .map(|p| (p.id, p.library.len()))
        .collect()
}

/// **The mill base OFFERS.** The positive control every row below rests on: one real Altar of
/// the Brood away from the module's own combo board, the loop's period now declines every
/// opponent's library, and `certify_instructed_opponent_library_departure` establishes that
/// decline so the cover's growth obligations can account it.
///
/// REVERT / MUTATION PROBE: make `certify_instructed_opponent_library_departure` return `None`
/// unconditionally ⇒ this **FAILS** while the module's Altar-free rows above stay green.
#[test]
fn mill_base_offers_the_shortcut() {
    assert!(
        drive_and_report(load_realistic_dump(), "baseline"),
        "BASELINE positive control: the untouched combo board OFFERS, so a failure below is \
         about the Altar and not about the harness"
    );
    assert!(
        drive_and_report(mill_base(), "mill base"),
        "CR 701.17a + CR 701.17b: a period that mills each opponent is still an unbounded \
         object-growth loop — an empty library neither stops the mill nor ends the game — so \
         the certified departure must let the cover account the decline and the offer stand"
    );
}

/// **A functioning observer of the certified departure keeps the veto.** C2 is the certificate's
/// public-information conjunct: the proposal may not promise a library decline that something on
/// the board would act on. Each row is the mill base plus ONE battlefield permanent carrying ONE
/// trigger definition and nothing else, so the verdict is that definition's.
///
/// The last pair is the fail-closed one: `origin: Battlefield` alone PROVES exclusion (a
/// certified id left a LIBRARY), but a non-empty `origin_zones` makes the matcher ignore
/// `origin` entirely and require `from_zone` to be in that set instead — so the same pin stops
/// proving anything and the veto must return. Its paired positive is the same definition with
/// `origin_zones` empty.
///
/// REVERT / MUTATION PROBE: delete the `!def.origin_zones.is_empty()` fail-closed conjunct in
/// `game::triggers::departure_observer_provably_excludes` ⇒ the third row OFFERS ⇒ **FAILS**.
#[test]
fn departure_observing_triggers_keep_the_veto() {
    run_rows(&[
        MillRow {
            what: "PAIRED POSITIVE: the mill base with nothing added OFFERS",
            board: mill_base,
            expect_offer: true,
        },
        MillRow {
            what: "CR 701.17a: a TriggerMode::Milled permanent fires on the certified mill by \
                   definition, so C2 must refuse",
            board: milled_mode_observer,
            expect_offer: false,
        },
        MillRow {
            what: "a 'from anywhere' ChangesZone trigger with destination Graveyard pins no \
                   origin and lands in the zone CR 701.17a puts a milled card in, so neither \
                   half of C2's exclusion holds",
            board: from_anywhere_graveyard_observer,
            expect_offer: false,
        },
        MillRow {
            what: "origin: Battlefield with a non-empty origin_zones including Library — the \
                   matcher ignores `origin`, so the origin pin proves nothing and C2 fails \
                   closed",
            board: battlefield_origin_with_library_in_origin_zones,
            expect_offer: false,
        },
        MillRow {
            what: "PAIRED POSITIVE for the row above: the SAME definition with origin_zones \
                   EMPTY provably cannot match a library departure, so the offer stands",
            board: battlefield_origin_alone,
            expect_offer: true,
        },
    ]);
}

/// **The hidden-zone relief is scoped to what the proposer may not see.** CR 400.2 makes an
/// opponent's hand a hidden zone, so the detection drive reads it through the proposer's own
/// hidden view and a `Milled` observer sitting there carries no veto — the proposer could not
/// have known about it at proposal time (CR 732.2a), and CR 732.2b gives its controller the
/// deviation point where that information re-enters. The SAME definition in the PROPOSER'S own
/// hand is information the proposer does hold, so it still vetoes.
///
/// One field separates each row from the next — the seat that owns the hand, then the zone the
/// definition declares (CR 113.6b) — so the relief is attributed to the hand collection of the
/// proposer-view redaction and to nothing else. The middle row is also the outer two's
/// reach-guard: C2 demonstrably walks hand-functioning definitions, so their offers are the
/// redaction and the zone-of-function gate rather than a scan that never looked.
#[test]
fn a_hand_mill_observer_vetoes_only_in_the_proposers_own_hand() {
    run_rows(&[
        MillRow {
            what: "a hand-functioning Milled observer in an OPPONENT'S hand is outside the \
                   proposer's information and must not veto",
            board: mill_observer_in_an_opponents_hand,
            expect_offer: true,
        },
        MillRow {
            what: "the SAME definition in the PROPOSER'S OWN hand is information the proposer \
                   holds, so C2 must refuse",
            board: mill_observer_in_the_proposers_own_hand,
            expect_offer: false,
        },
        MillRow {
            what: "PAIRED POSITIVE for the row above: the same card in the same hand carrying \
                   the same Milled definition DECLARED to function on the battlefield instead \
                   (CR 113.6b) still OFFERS — so what refuses above is the definition \
                   functioning where the proposer can see it, not an extra card in P0's hand",
            board: battlefield_scoped_mill_observer_in_the_proposers_own_hand,
            expect_offer: true,
        },
    ]);
}

/// **A replacement that could divert the certified departure keeps the veto, and an ordinary
/// tapland does not.** CR 614.6 lets a replacement send a card that would go to a graveyard
/// somewhere else, so a `Moved` definition pinned to Graveyard — or pinned nowhere — observes
/// the very move the certificate promises. The `destination_zone` narrowing is what keeps the
/// measured corpus signature `(Moved, SelfRef, Battlefield)` from vetoing every real board, and
/// the paired row is a REAL card carrying exactly that triple.
#[test]
fn a_graveyard_diverting_replacement_vetoes_and_a_real_tapland_does_not() {
    run_rows(&[
        MillRow {
            what: "CR 614.6: a Moved replacement pinned to the graveyard could divert the \
                   certified departure, so the certificate must refuse",
            board: graveyard_diverting_replacement,
            expect_offer: false,
        },
        MillRow {
            what: "PAIRED POSITIVE: a real entry-census tapland's own CR 614.1d self-entry \
                   replacement is pinned to Battlefield and observes no departure",
            board: real_tapland_self_entry,
            expect_offer: true,
        },
    ]);
}

/// **A caster-side mill is not certified, and neither is a mill the proposer may decline.**
/// Both rows are a SECOND Altar of the Brood with exactly one field of its executed mill
/// rewritten, so each separates from the unmutated second Altar on that field alone.
///
/// `player_scope: All` makes the caster's own library decline too. That movement is not a
/// candidate — the certificate ranges over NON-caster libraries — so C1b's residual finds it
/// unaccounted and refuses. `optional: true` is CR 603.5's "you may", a choice made as the
/// ability resolves and therefore a choice the proposed sequence would have to contain. The
/// library reading in the row below is what tells the two apart, and it is what makes the
/// second one isolated: its period declines exactly the libraries the offering control's does.
#[test]
fn caster_side_and_declinable_mills_are_not_certified() {
    run_rows(&[
        MillRow {
            what: "PAIRED POSITIVE: a second, unmutated opponent-facing Altar still OFFERS, so \
                   the two rows below separate on the rewritten field and not on the permanent",
            board: second_altar_opponent_facing,
            expect_offer: true,
        },
        MillRow {
            what: "an 'each player mills' period leaves the CASTER's own library decline \
                   unaccounted, and C1b's residual refuses it",
            board: second_altar_each_player,
            expect_offer: false,
        },
        MillRow {
            what: "CR 603.5: a 'may' mill is a resolution-time choice, so the proposed \
                   sequence would have to contain an answer the proposer never gave",
            board: second_altar_may_mill,
            expect_offer: false,
        },
    ]);
}

/// The mechanism behind the two silent rows above, read off the boards rather than asserted by
/// name — and the may-mill row's isolation is total: it produces the SAME library outcome as
/// the offering control, so nothing about what the period does to any library can explain why
/// one offers and the other does not.
#[test]
fn the_each_player_mill_declines_the_casters_library_and_the_may_mill_declines_nothing_extra() {
    let offering = library_sizes_after_one_cycle(second_altar_opponent_facing());
    assert_ne!(
        offering,
        library_sizes_after_one_cycle(mill_base()),
        "REACH-GUARD: the second Altar really mills — its board's libraries differ from the \
         one-Altar base, so the two rows below are not comparing a permanent that does nothing"
    );
    assert_eq!(
        library_sizes_after_one_cycle(second_altar_may_mill()),
        offering,
        "the may-mill row's period declines exactly the libraries the OFFERING control's does, \
         so its refusal is the declinability itself (CR 603.5) and not anything the mill did"
    );

    let each_player = library_sizes_after_one_cycle(second_altar_each_player());
    let caster_of = |sizes: &[(PlayerId, usize)]| {
        sizes
            .iter()
            .find(|(id, _)| *id == P0)
            .expect("the caster has a seat")
            .1
    };
    assert!(
        caster_of(&each_player) < caster_of(&offering),
        "the each-player row's refusal IS a caster-side decline: only there does P0's own \
         library shrink, and that movement is what C1b's residual finds unaccounted"
    );
}

/// **The two channels a milled card can reach the drive through, one live per row, the other
/// pinned dead.** Both rows OFFER, and each agrees with the bare pinned control it is measured
/// against, so what they assert is that the proposer-view redaction has taken that channel out
/// of the detection drive before it can act.
///
/// On the pinned base every non-caster library card is non-permanent and definition-free, so a
/// milled card can neither flip `descended_this_turn` (CR 700.11) nor open a window. The
/// interposer row adds ONE real parsed Narcomoeba at post-cast depth 1 of a victim's library —
/// its own library→graveyard window is the live channel. The descend row instead rewrites the
/// card already at that depth to a vanilla PERMANENT carrying no definitions — no window
/// anywhere, so the live channel is the arriving card's permanent type alone.
///
/// REVERT / MUTATION PROBE, RUN rather than reasoned: make
/// `game::visibility::proposer_hidden_view` return its clone unprojected, so the drive reads
/// unredacted library cards ⇒ the pinned control still OFFERS and BOTH rows go silent ⇒ both
/// **FAIL**. The control holding is what makes each failure the row's own object; which
/// conjunct refuses it is not this row's claim.
#[test]
fn a_milled_cards_own_window_and_permanent_type_reach_the_drive_through_neither_channel() {
    run_rows(&[
        MillRow {
            what: "PAIRED CONTROL: the bare pinned mill base OFFERS, so both rows below are \
                   measured against a board whose only difference is their own object",
            board: pinned_mill_base,
            expect_offer: true,
        },
        MillRow {
            what: "INTERPOSER: a real Narcomoeba the drive mills carries its own \
                   library→graveyard window, which the proposer cannot see and the drive \
                   therefore never opens",
            board: pinned_with_narcomoeba,
            expect_offer: true,
        },
        MillRow {
            what: "DESCEND: a vanilla permanent card the drive mills would flip CR 700.11's \
                   descended_this_turn on its owner, a Player field no certified-departure \
                   relief reaches",
            board: pinned_with_vanilla_permanent_at_depth_one,
            expect_offer: true,
        },
    ]);
}

// ─────────────────────────────────────────────────────────────────────────────────────────
// THE ACCEPT PATH
//
// Every row above stops at the OFFER. These go past it: the route the mill period takes once
// accepted, what the collapse at the CR 500.5 boundary delivers, and how much of it.
//
// Magnitude is pinned on `pinned_mill_base()`, whose non-caster libraries carry no definition
// that could interpose, so a decline shorter than the accepted count has exactly one cause.
// ─────────────────────────────────────────────────────────────────────────────────────────

use engine::ai_support::legal_actions_for_viewer;
use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::analysis::resource::ResourceAxis;
use engine::game::engine::apply;
use engine::game::scenario::GameRunner;
use engine::types::actions::GameAction;
use engine::types::game_state::PersistentAxisMaterialization;

/// Everything P0's accept registered. The stash is the observable of the route from outside the
/// engine crate: the batched route registers its items, a performed take registers nothing.
fn registered(state: &GameState) -> &[PersistentAxisMaterialization] {
    state
        .pending_unbounded_materialization
        .get(&P0)
        .map_or(&[], Vec::as_slice)
}

/// Whether P0's take performed the period rather than standing on the batched mark.
fn took_the_replay(state: &GameState) -> bool {
    registered(state).is_empty()
}

/// The board driven by one real buyback+convoke recast to its CR 732.2a offer.
pub(crate) fn offer_state(state: GameState) -> GameState {
    let state = drive_sprout_cast(state).state().clone();
    assert!(
        matches!(state.waiting_for, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0),
        "reach-guard: the live recast must surface P0's offer, got {:?}",
        state.waiting_for
    );
    state
}

/// P0 declares `Fixed(n)`; every living opponent accepts in APNAP order.
fn declare_and_accept_all(state: &mut GameState, n: u32) {
    apply(
        state,
        P0,
        GameAction::DeclareShortcut {
            count: IterationCount::Fixed(n),
            template: None,
        },
    )
    .expect("the proposer declares the object-growth shortcut");
    while let WaitingFor::RespondToShortcut { player, .. } = state.waiting_for.clone() {
        apply(
            state,
            player,
            GameAction::RespondToShortcut {
                response: ShortcutResponse::Accept,
            },
        )
        .expect("each living opponent accepts");
    }
}

/// Every seat's library size, in seat order.
fn library_sizes(state: &GameState) -> Vec<(PlayerId, usize)> {
    state
        .players
        .iter()
        .map(|p| (p.id, p.library.len()))
        .collect()
}

/// **Adding the mill flips the accepted route to the concrete replay, on a board whose
/// batched payload is NOT empty.** Written as ONE test so the arms are inseparable: the
/// Altar arm alone is satisfiable by a blanket route change, the Altar-free arm alone by
/// never wiring the mill conjunct at all.
///
/// ALTAR-FREE (the discriminator): the untouched combo board grows only Saprolings, so its
/// accept keeps the O(1) batched mint. ALTAR: the same board plus one real Altar of the Brood
/// publishes a `LibraryDelta` axis for which no batched item exists, and the accept switches
/// to the replay — while STILL registering a token axis on the batched arm, which is what
/// makes this the non-empty-`batched` population rather than the pure-mill one.
///
/// REVERT PROBE: delete the `unbatchable_deferred` disjunct at the `game::engine` route seam
/// ⇒ the Altar arm falls back to the batched mint and reds; the Altar-free arm stays green.
#[test]
fn the_mill_flips_a_non_empty_batched_period_onto_the_replay() {
    let mut altar_free = offer_state(load_realistic_dump());
    declare_and_accept_all(&mut altar_free, 2);
    assert!(
        !took_the_replay(&altar_free),
        "the Altar-free board's only growth is batchable, so its accept keeps the batched mint"
    );
    assert!(
        registered(&altar_free)
            .iter()
            .any(|m| matches!(m, PersistentAxisMaterialization::Tokens(_))),
        "reach-guard: the batched payload carries a token axis, so the Altar arm below \
         differs from this one by the MILL and not by an empty payload"
    );

    let mut milling = offer_state(mill_base());
    let before = library_sizes(&milling);
    declare_and_accept_all(&mut milling, 2);
    assert!(
        took_the_replay(&milling),
        "CR 732.2c: the accepted proposal promises a library decline no batched item can \
         deliver, so the same period — token growth included — must route to the replay"
    );
    assert!(
        library_sizes(&milling)
            .iter()
            .zip(&before)
            .any(|((id, after), (_, before))| *id != P0 && after < before),
        "reach-guard: the take milled an opponent"
    );
}

/// **An accepted mill take MOVES cards and leaves no mark.** The first row in this module that
/// goes past the offer, and the one the offer rows cannot substitute for: a Replay arm that failed
/// to deliver the `LibraryDelta` would move zero cards, and every assertion above would still
/// pass.
///
/// Direction only — a NONZERO decline per victim and an absent mark. This row stays
/// direction-only BY DESIGN: it runs on `mill_base()`, whose library cards keep their real
/// definitions, so a magnitude asserted here would be asserting the absence of an interposer
/// nobody enumerated. Magnitude is pinned on the interposer-free `pinned_mill_base()` by
/// `an_interposer_free_mill_collapse_declines_in_proportion_to_the_accepted_count`.
///
/// REVERT PROBE: delete the `unbatchable_deferred` disjunct at the `game::engine` route seam
/// ⇒ the accept routes to the batched mint, which carries no `LibraryDelta`, so no opponent
/// library declines and P0 keeps its `LibraryDelta` marks ⇒ **FAILS**.
#[test]
fn an_accepted_mill_take_moves_cards_and_leaves_no_mark() {
    const N: u32 = 3;

    let mut state = offer_state(mill_base());
    let before = library_sizes(&state);
    let victims: Vec<PlayerId> = state
        .players
        .iter()
        .map(|p| p.id)
        .filter(|&id| id != P0)
        .collect();
    assert!(
        !victims.is_empty(),
        "reach-guard: the dump seats opponents for the Altar to mill"
    );

    declare_and_accept_all(&mut state, N);

    let after = library_sizes(&state);
    for victim in &victims {
        let lookup = |sizes: &[(PlayerId, usize)]| {
            sizes
                .iter()
                .find(|(id, _)| id == victim)
                .map(|(_, n)| *n)
                .expect("every seat is in both readings")
        };
        assert!(
            lookup(&after) < lookup(&before),
            "CR 701.17a: the take must actually mill {victim:?} — library went {} -> {}",
            lookup(&before),
            lookup(&after)
        );
    }

    let surviving = state
        .unbounded_resources
        .get(&P0)
        .cloned()
        .unwrap_or_default();
    for victim in &victims {
        assert!(
            !surviving.contains(&ResourceAxis::LibraryDelta(*victim)),
            "CR 732.2c: the take DELIVERED {victim:?}'s library decline, so no infinite-mill \
             badge stands"
        );
    }
}

/// **An interposer-free mill collapse declines IN PROPORTION to the accepted count.** The
/// magnitude row the direction-only row above cannot substitute for: a drive that truncated at
/// a fixed prefix, or one that silently fell back to the O(1) batched mint, moves cards and
/// retires marks exactly as that row asserts.
///
/// No magic constant: the same board is run at two counts and the declines are compared to each
/// other. A fixed-prefix truncation makes the two declines EQUAL; a batched fallback makes both
/// ZERO; only a drive that replays the accepted number of periods makes the larger run decline
/// strictly more, in the ratio of the counts.
///
/// `pinned_mill_base()` is the right base and `mill_base()` is not — the pin rewrites every
/// non-caster library card to a definition-free Instant, so the only thing that could shorten a
/// decline here is the drive itself.
#[test]
fn an_interposer_free_mill_collapse_declines_in_proportion_to_the_accepted_count() {
    const SMALL: u32 = 2;
    const LARGE: u32 = 4;

    // One (count -> per-victim decline) reading on a fresh copy of the pinned board.
    let run = |n: u32| -> Vec<(PlayerId, usize)> {
        let mut state = offer_state(pinned_mill_base());
        let before = library_sizes(&state);
        declare_and_accept_all(&mut state, n);
        assert!(
            took_the_replay(&state),
            "reach-guard: the accepted period must take the REPLAY, or a decline of zero is \
             the batched mint's silence rather than a delivered count"
        );
        let after = library_sizes(&state);
        for (id, size) in &after {
            if *id != P0 {
                assert!(
                    *size > 0,
                    "stated precondition: {id:?}'s library is still NON-EMPTY after {n} \
                     cycles, so the decline measured a delivered count and not an exhausted \
                     library"
                );
            }
        }
        before
            .iter()
            .zip(after.iter())
            .filter(|((id, _), _)| *id != P0)
            .map(|((id, b), (_, a))| (*id, b - a))
            .collect()
    };

    let small = run(SMALL);
    let large = run(LARGE);
    assert!(
        !small.is_empty(),
        "reach-guard: the dump seats opponents for the Altar to mill"
    );

    for ((victim, small_decline), (_, large_decline)) in small.iter().zip(large.iter()) {
        assert!(
            *small_decline > 0,
            "CR 701.17a: {victim:?} must actually be milled at the smaller count"
        );
        // CR 732.2c: the shortcut advances with every choice the proposal contained, so an
        // interposer-free replay delivers the count it was given — the decline scales with it.
        assert_eq!(
            large_decline * (SMALL as usize),
            small_decline * (LARGE as usize),
            "{victim:?}: declines must be in the ratio of the accepted counts \
             ({SMALL} -> {small_decline}, {LARGE} -> {large_decline}); equal declines mean a \
             fixed-prefix truncation"
        );
    }
}

/// The beat budget every hand-stepped drive below runs under. Exhausting it is reported as
/// exhaustion — naming the bound and the last beat — so "the decision never arrived" can never be
/// read off a drive that simply ran out of steps.
const BEAT_BUDGET: usize = 64;

/// Cast ONE real Sprout Swarm recast — buyback accepted, one untapped Saproling convoked for the
/// `{G}`. Both objects are found BY SCAN because the collapse taps and mints fodder, so a pinned
/// `ObjectId` names a different object every period.
fn cast_one_sprout(runner: &mut GameRunner, why: &str) {
    let (sprout, fodder) = {
        let state = runner.state();
        let sprout = state
            .players
            .iter()
            .find(|p| p.id == P0)
            .expect("the proposer is seated")
            .hand
            .iter()
            .copied()
            .find(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.name == "Sprout Swarm")
            })
            .unwrap_or_else(|| {
                panic!("{why}: reach-guard — a Sprout Swarm in P0's hand to recast")
            });
        let fodder: Vec<ObjectId> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.controller == P0 && o.name == "Saproling" && !o.tapped)
            })
            .take(1)
            .collect();
        assert!(
            !fodder.is_empty(),
            "{why}: reach-guard — an untapped P0 Saproling to convoke for the {{G}}"
        );
        (sprout, fodder)
    };
    let _commit = runner
        .cast(sprout)
        .accept_optional()
        .convoke_with(&fodder)
        .commit();
}

/// Hand-step priority until the drive reaches a beat that is not a priority pass, or until the
/// period settles at an empty-stack `Priority`. Returns that beat.
///
/// NEVER `.resolve()`: `game::scenario::drive_resolution`'s CR 608.2d arm answers
/// `OptionalEffectChoice` on its own, so an assertion layered on it cannot fail in the direction
/// it guards. Because this stepper stops at the FIRST such beat, a prompt raised anywhere inside
/// the period is the value it returns — which is what lets a caller assert a period raised none.
fn step_to_decision(runner: &mut GameRunner, why: &str) -> WaitingFor {
    for _ in 0..BEAT_BUDGET {
        let beat = runner.state().waiting_for.clone();
        match beat {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return beat,
            WaitingFor::Priority { .. } => {
                if let Err(e) = runner.act(GameAction::PassPriority) {
                    panic!("{why}: passing toward the next decision was refused — {e:?}");
                }
            }
            other => return other,
        }
    }
    panic!(
        "{why}: {BEAT_BUDGET} beats exhausted before any decision — the BOUND ran out, which is \
         not the same as the decision never arriving; last beat {:?}",
        runner.state().waiting_for
    )
}

/// Build the interposed collapse board every interposition row below shares and accept `n`,
/// returning the board after the take, the grafted interposer's `ObjectId`, and the PRE-ACCEPT
/// `library_sizes` reading.
///
/// `depth` IS the committed prefix. `post_cast_library_anchor` returns the id AT post-cast index
/// `depth` and `place_before` inserts the graft AT that index, so `depth` cards sit above the
/// interposer and the replay commits `depth` whole periods before reaching it.
///
/// The graft is a parameter because the rows separate on the interposer, not on the board.
fn collapse_with_interposer(
    n: u32,
    depth: usize,
    graft: fn(&mut GameState, PlayerId, ObjectId) -> ObjectId,
) -> (GameState, ObjectId, Vec<(PlayerId, usize)>) {
    let mut base = pinned_mill_base();
    let anchor = post_cast_library_anchor(&base, P1, depth);
    let interposer = graft(&mut base, P1, anchor);

    let mut state = offer_state(base);
    let before = library_sizes(&state);
    assert!(
        state.may_trigger_auto_choices.is_empty(),
        "reach-guard: no recorded auto-answer stands in for the interposer's decision — the \
         abort below is the undetermined choice, not a replayed one"
    );
    declare_and_accept_all(&mut state, n);
    assert!(
        took_the_replay(&state),
        "reach-guard: the accepted period took the REPLAY, so a short decline below is a \
         truncation rather than the batched mint never milling at all"
    );
    (state, interposer, before)
}

/// [`collapse_with_interposer`] with the PROMPTING interposer — the one that truncates.
fn truncated_by_interposer(n: u32, depth: usize) -> (GameState, ObjectId, Vec<(PlayerId, usize)>) {
    collapse_with_interposer(n, depth, graft_narcomoeba)
}

/// The uniform CR 732.2a terminal-beat assertion every `LoopCollapse` collapse row shares: some
/// seat's production legal-action surface is non-empty at the beat the submit arm returned, that
/// seat's FIRST candidate is accepted through `apply()`, and the game is at `Priority` afterwards.
///
/// Shape-agnostic deliberately. The entered phase's CR 703.1 turn-based action can stand ahead of
/// the CR 117.3a priority grant — CR 508.1's declare-attackers does, on the r6a witherbloom board
/// — so a `Priority` matcher on the returned beat itself would red there while CR 732.2a is
/// satisfied.
///
/// Driven on a CLONE and the post-action clone is returned, so a caller can assert further
/// without re-driving.
pub fn answer_terminal_beat(state: &GameState, why: &str) -> GameState {
    let surface: Vec<(PlayerId, usize)> = state
        .players
        .iter()
        .map(|p| (p.id, legal_actions_for_viewer(state, p.id).0.len()))
        .collect();
    let (seat, actions) = state
        .players
        .iter()
        .map(|p| (p.id, legal_actions_for_viewer(state, p.id).0))
        .find(|(_, acts)| !acts.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "{why}: CR 732.2a — no seat can act at the terminal beat {:?}, surface {surface:?}",
                state.waiting_for
            )
        });
    let mut answered = state.clone();
    apply(&mut answered, seat, actions[0].clone()).unwrap_or_else(|e| {
        panic!(
            "{why}: {seat:?}'s first production candidate {:?} must be accepted at the terminal \
             beat {:?}, got {e:?}",
            actions[0], state.waiting_for
        )
    });
    assert!(
        matches!(answered.waiting_for, WaitingFor::Priority { .. }),
        "{why}: CR 732.2a — answering the terminal beat reaches a priority window, got {:?}",
        answered.waiting_for
    );
    answered
}

/// **An interposer the replay reaches truncates the collapse to a WHOLE-PERIOD PREFIX, rolls
/// the aborted iteration back entire, and leaves no stale ∞ mark.**
///
/// CR 732.2a: a shortcut proposal may not "include conditional actions, where the outcome of a
/// game event determines the next action a player takes", and its ending point "must be a place
/// where a player has priority". A real Narcomoeba grafted into a victim's library is such a
/// point: its library→graveyard trigger asks its controller a question the accepted proposal
/// never contained, so the sequence was legal only up to it.
///
/// The graft sits at POST-cast depth 1, so the replay reaches it only after one complete
/// period — which is what makes the delivered prefix strictly interior to `[0, N]` rather than
/// the degenerate 0 that a depth-0 interposer would produce.
#[test]
fn an_interposer_truncates_the_collapse_to_a_whole_period_prefix() {
    const N: u32 = 3;

    let (state, narcomoeba, before) = truncated_by_interposer(N, 1);

    // The delivered prefix is strictly interior to [0, N] and identical on every victim,
    // because the drive commits WHOLE periods and the mill is one period-wide event.
    let after = library_sizes(&state);
    let declines: Vec<(PlayerId, usize)> = before
        .iter()
        .zip(after.iter())
        .filter(|((id, _), _)| *id != P0)
        .map(|((id, b), (_, a))| (*id, b - a))
        .collect();
    for (victim, decline) in &declines {
        assert!(
            *decline > 0 && (*decline as u32) < N,
            "CR 732.2a: the accepted sequence was legal only up to the interposer, so \
             {victim:?}'s decline must be strictly between 0 and {N}, got {decline}"
        );
    }
    let first = declines[0].1;
    assert!(
        declines.iter().all(|(_, d)| *d == first),
        "the drive commits WHOLE periods, so every victim declines by the same prefix: \
         {declines:?}"
    );

    // The aborted iteration is rolled back ENTIRE: same object, same zone, by id.
    assert_eq!(
        state.objects.get(&narcomoeba).map(|o| o.zone),
        Some(Zone::Library),
        "the iteration that reached the interposer is rolled back whole, so the Narcomoeba is \
         still in a library"
    );
    assert!(
        library_ids(&state, P1).contains(&narcomoeba),
        "rolled back to ITS OWN library, identified by ObjectId — the drive works on a clone, \
         so identity is preserved by construction rather than by a re-find"
    );

    // CR 732.2a: the ending point is a place where a player has priority.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: the truncated collapse ends at a priority window, got {:?}",
        state.waiting_for
    );

    // A truncated collapse still RETIRES the axis it partly delivered — no stale ∞ badge.
    assert!(
        !state
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.contains(&ResourceAxis::LibraryDelta(P1))),
        "the truncated collapse is a DRIVEN materialization, so it retires {P1:?}'s library \
         axis rather than leaving an infinite-mill badge standing"
    );
}

/// **The delivered prefix tracks the interposer's DEPTH.** Written as one row with two arms so
/// neither is green alone: a drive that never ran makes both declines zero and reds the
/// `depth = 1` arm; a drive that ignores interposers makes them equal and reds the `depth = 0`
/// arm.
///
/// **The `depth = 0` arm pins CR 732.2a's ending point at ZERO delivery**: a taken shortcut's
/// ending point must "be a place where a player has priority", and the granted seat can act there.
#[test]
fn the_delivered_prefix_tracks_the_interposers_depth() {
    const N: u32 = 3;

    // Signed, because a `usize` decline PANICS where an assertion should RED, and the sign is
    // not guaranteed on a seat whose interposer rewrites its own library.
    let declines =
        |before: &[(PlayerId, usize)], after: &[(PlayerId, usize)]| -> Vec<(PlayerId, i64)> {
            before
                .iter()
                .zip(after.iter())
                .filter(|((id, _), _)| *id != P0)
                .map(|((id, b), (_, a))| (*id, *b as i64 - *a as i64))
                .collect()
        };

    // ── depth 0: the empty prefix ──
    let (empty, narcomoeba, before) = collapse_with_interposer(N, 0, graft_narcomoeba);
    let zero = declines(&before, &library_sizes(&empty));
    assert!(
        !zero.is_empty(),
        "reach-guard: the dump seats opponents for the Altar to mill"
    );
    for (victim, decline) in &zero {
        assert_eq!(
            *decline, 0,
            "CR 732.2a: an interposer on TOP aborts before the first iteration commits, so \
             {victim:?} declines nothing"
        );
    }
    assert!(
        library_ids(&empty, P1).contains(&narcomoeba),
        "the aborted iteration is rolled back whole, so the interposer is still in P1's own \
         library, by ObjectId"
    );
    let granted = empty.priority_player;
    assert!(
        matches!(empty.waiting_for, WaitingFor::Priority { .. })
            && !legal_actions_for_viewer(&empty, granted).0.is_empty(),
        "CR 732.2a: the zero-delivery take ends where the granted seat {granted:?} can act, got {:?}",
        empty.waiting_for
    );
    let answered = answer_terminal_beat(&empty, "CR 732.2a zero-delivery ending point");
    assert_ne!(
        answered.waiting_for, empty.waiting_for,
        "CR 732.2a: answering the zero-delivery beat ADVANCES it"
    );
    assert!(
        !empty
            .unbounded_resources
            .get(&P0)
            .is_some_and(|axes| axes.contains(&ResourceAxis::LibraryDelta(P1))),
        "no infinite-mill badge is left standing behind an empty prefix"
    );

    // ── depth 1: a strictly larger prefix, still strictly under N ──
    let (one, _, before_one) = truncated_by_interposer(N, 1);
    let prefix = declines(&before_one, &library_sizes(&one));
    let first = prefix[0].1;
    for (victim, decline) in &prefix {
        assert!(
            *decline > zero[0].1 && *decline < i64::from(N),
            "CR 732.2a: the sequence was legal up to the interposer, so {victim:?}'s decline is \
             strictly between the depth-0 arm's {} and {N}, got {decline}",
            zero[0].1
        );
        assert_eq!(
            *decline, first,
            "the drive commits WHOLE periods, so every victim declines by the same prefix: \
             {prefix:?}"
        );
    }
    assert!(
        matches!(one.waiting_for, WaitingFor::Priority { .. }),
        "CR 732.2a: a NON-empty prefix does end at a priority window, got {:?}",
        one.waiting_for
    );
}

/// **A MANDATORY, CHOICE-FREE interposer does not truncate: the collapse still delivers its full
/// `N`.** This is the class statement's pin — what aborts the replay is an unscripted PROMPT, not
/// a trigger, and not the fact that a non-proposer's ability spoke. Gaea's Blessing's milled
/// trigger fires, shuffles its controller's graveyard back into its library mid-drive, and asks
/// nobody anything.
///
/// The one-variable pairing is [`an_interposer_truncates_the_collapse_to_a_whole_period_prefix`]:
/// the same helper, the same `depth`, the same `N`, prompting interposer against non-prompting.
/// Neither leg is green alone — a drive that aborts on any interposer reds the `N`-decline legs,
/// and a fixture whose interposer never fired reds the short-decline leg.
///
/// Every decline is written as an ADDITION: the interposer's own seat ends net-unchanged, so a
/// `usize` subtraction there panics where an assertion should red. No terminal-beat leg: both
/// twins leave `Priority` after the take, so a beat here discriminates nothing and
/// [`an_interposer_truncates_the_collapse_to_a_whole_period_prefix`] owns the CR 732.2a ending
/// point. The graft's final zone is deliberately not asserted — its own trigger shuffles it into
/// a randomized library.
#[test]
fn a_mandatory_choice_free_interposer_delivers_the_whole_count() {
    const N: u32 = 3;

    let (state, graft, before) = collapse_with_interposer(N, 1, graft_gaeas_blessing);
    let after = library_sizes(&state);
    let lookup = |sizes: &[(PlayerId, usize)], who: PlayerId| {
        sizes
            .iter()
            .find(|(id, _)| *id == who)
            .map(|(_, n)| *n)
            .expect("every seat is in both readings")
    };

    let host = state
        .objects
        .get(&graft)
        .map(|o| o.owner)
        .expect("the grafted interposer is keyed");
    let untouched: Vec<PlayerId> = state
        .players
        .iter()
        .map(|p| p.id)
        .filter(|id| *id != P0 && *id != host)
        .collect();
    assert!(
        !untouched.is_empty(),
        "reach-guard: the dump seats victims the interposer's trigger does not touch"
    );

    for victim in &untouched {
        assert_eq!(
            lookup(&after, *victim) + N as usize,
            lookup(&before, *victim),
            "CR 732.2c: a trigger that asks nothing does not stop the replay, so {victim:?} \
             takes the whole accepted count"
        );
    }
    // The graft's own seat is the reach-guard: its trigger really fired and really rewrote its
    // library, which is what keeps the full-count legs above from being an inert fixture.
    assert!(
        lookup(&after, host) + N as usize > lookup(&before, host),
        "reach-guard: {host:?}'s milled trigger shuffled its graveyard back, so its library is \
         SHORT of a full {N}-card decline — without that the graft never spoke"
    );
}

/// **The interposition reaches the milled player, and the offer re-arms behind it.** Beats 3 and
/// 4 of the truncation pair: after the collapse stops at the interposer, one real driving period
/// opens that interposer's window, the window is the VICTIM's, answering it is honoured, and the
/// detector re-offers the remainder on the post-trigger board.
///
/// CR 603.3a + CR 108.4a: the loop's controller is P0 while the milled card's controller is its
/// owner P1, so the row pins the prompt's `player` and `source_id` and a window opened to the
/// wrong seat reds it.
///
/// **A recast, not a priority pass** — on this board the mill is Altar of the Brood's ENTRY
/// trigger, so nothing is milled unless a permanent enters and a loop of bare passes opens no
/// window at all. Both objects are found by scan: the collapse taps and mints fodder, so a pinned
/// `ObjectId` names a different object every period.
///
/// **The negative arm is an arm of this row, not a second row**, so neither is green alone: the
/// positive arm alone is satisfiable by a harness that prompts on everything, the negative arm
/// alone by one that prompts on nothing. It asserts the VARIANT, never "no optional prompt" — the
/// period's own optional prompt is P0's buyback `OptionalCostChoice`, which the driver answers,
/// so a variant-blind assertion would be false on a correct engine. It is deliberately NOT scoped
/// by `source_id`: that board holds no interposer, and such a scope would make the arm vacuous.
#[test]
fn the_interposers_window_reaches_the_milled_player_and_the_offer_re_arms() {
    const N: u32 = 3;
    /// Measured: the offer returns after ONE further period. The bound is headroom, and
    /// exhausting it is reported as exhaustion rather than as "no re-offer".
    const FURTHER_PERIODS: usize = 4;

    let lookup = |sizes: &[(PlayerId, usize)], who: PlayerId| {
        sizes
            .iter()
            .find(|(id, _)| *id == who)
            .map(|(_, n)| *n as i64)
            .expect("every seat is in both readings")
    };

    // ── Beat 3: the window, its seat, and its answer ──
    let (state, narcomoeba, _) = truncated_by_interposer(N, 1);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the truncation beat carries NO offer — that is the absent half of the re-arm pair, \
         got {:?}",
        state.waiting_for
    );
    let victims: Vec<PlayerId> = state
        .players
        .iter()
        .map(|p| p.id)
        .filter(|id| *id != P0)
        .collect();
    let before = library_sizes(&state);

    let mut runner = GameRunner::from_state(state);
    cast_one_sprout(&mut runner, "the interposer's window");
    let window = step_to_decision(&mut runner, "the interposer's window");

    let WaitingFor::OptionalEffectChoice {
        player, source_id, ..
    } = window
    else {
        panic!("CR 603.3a: one real period must open the interposer's window, got {window:?}")
    };
    assert_eq!(
        (player, source_id),
        (P1, narcomoeba),
        "CR 108.4a: the window belongs to the MILLED player and to the grafted interposer, \
         not to the loop's proposer"
    );
    let opened = library_sizes(runner.state());
    assert!(
        lookup(&opened, P1) < lookup(&before, P1),
        "reach-guard: the period actually milled {P1:?} ({} -> {}), so a window that opened is \
         a decision and not an artefact of a board where nothing was milled",
        lookup(&before, P1),
        lookup(&opened, P1)
    );
    assert!(
        runner
            .state()
            .players
            .iter()
            .any(|p| p.id == P1 && p.graveyard.contains(&narcomoeba)),
        "the interposer is in ITS OWN controller's graveyard at the beat its window opens, \
         by ObjectId"
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the milled player answers its own may-trigger");
    assert!(
        runner.state().battlefield.contains(&narcomoeba),
        "CR 608.2d: the answer is HONOURED — an accepted 'you may put it onto the battlefield' \
         puts the card there, which is what separates a live decision from an inert prompt"
    );

    // ── Beat 4: absent, then earned ──
    let settled = step_to_decision(&mut runner, "the period that consumes the interposer");
    assert!(
        matches!(settled, WaitingFor::Priority { .. }) && runner.state().stack.is_empty(),
        "the period that CONSUMES the interposer still ends with no offer, at an empty-stack \
         priority beat — the second absent half of the pair, got {settled:?}"
    );
    let consumed = library_sizes(runner.state());
    let per_period: Vec<(PlayerId, i64)> = victims
        .iter()
        .map(|v| (*v, lookup(&before, *v) - lookup(&consumed, *v)))
        .collect();
    for (victim, per) in &per_period {
        assert!(
            *per > 0,
            "reach-guard: the driven period mills {victim:?}, so the relation the second \
             collapse is measured against is not zero"
        );
    }

    let mut offer = None;
    for _ in 0..FURTHER_PERIODS {
        cast_one_sprout(&mut runner, "the re-arm drive");
        match step_to_decision(&mut runner, "the re-arm drive") {
            WaitingFor::LoopShortcut { certificate, .. } => {
                offer = Some(certificate);
                break;
            }
            WaitingFor::Priority { .. } => continue,
            other => panic!("the re-arm drive stopped at an unscripted beat: {other:?}"),
        }
    }
    let Some(certificate) = offer else {
        panic!(
            "the offer did not re-arm within {FURTHER_PERIODS} driven periods — exhaustion, \
             NOT an absent re-offer; last beat {:?}",
            runner.state().waiting_for
        )
    };
    assert!(
        certificate
            .unbounded
            .contains(&ResourceAxis::LibraryDelta(P1)),
        "CR 732.2a: the truncation left a RESUMABLE loop, so the re-offer's certificate carries \
         {P1:?}'s library axis again, got {:?}",
        certificate.unbounded
    );

    let before_second = library_sizes(runner.state());
    declare_and_accept_all(runner.state_mut(), N);
    let after_second = library_sizes(runner.state());
    for (victim, per) in &per_period {
        assert_eq!(
            lookup(&after_second, *victim) + i64::from(N) * per,
            lookup(&before_second, *victim),
            "the resumed take delivers all {N} periods at the per-period rate this row \
             measured for {victim:?}, so the truncation left a loop that can finish"
        );
    }

    // ── Negative arm: the identical driver, on a board with no interposer ──
    let mut clean = offer_state(pinned_mill_base());
    declare_and_accept_all(&mut clean, N);
    assert!(
        took_the_replay(&clean),
        "reach-guard: the interposer-free accept takes the SAME replay route, so the arms differ \
         only by the graft"
    );
    let mut runner = GameRunner::from_state(clean);
    cast_one_sprout(&mut runner, "the interposer-free period");
    let end = step_to_decision(&mut runner, "the interposer-free period");
    assert!(
        matches!(end, WaitingFor::LoopShortcut { proposer, .. } if proposer == P0)
            && runner.state().stack.is_empty(),
        "the interposer-free period raises no OptionalEffectChoice at ANY beat — the stepper \
         stops at the first beat that is neither a pass nor a trigger ordering, so a prompt \
         anywhere would land here — and finishes a whole period into a fresh empty-stack offer, \
         got {end:?}"
    );
}
