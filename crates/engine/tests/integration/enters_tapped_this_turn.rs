//! "Permanents/Lands enter tapped this turn" — a turn-duration floating
//! enters-tapped replacement (CR 614.1d + CR 514.2 + CR 611.2a).
//!
//! Cards under test (verbatim Oracle text):
//!   - Due Respect (Instant): "Permanents enter tapped this turn.\nDraw a card."
//!   - Nahiri's Lithoforming (Sorcery): "Sacrifice X lands. For each land
//!     sacrificed this way, draw a card. You may play X additional lands this
//!     turn. Lands you control enter tapped this turn."
//!
//! Both previously left the enter-tapped clause as `Effect::Unimplemented`.
//! The fix lifts it to `Effect::AddTargetReplacement { target: None }` carrying
//! a `ChangeZone` replacement whose `execute` is a SelfRef tap, `expiry` is
//! end-of-turn, and whose `source_controller` is anchored to the resolving
//! ability's controller at install time.
//!
//! Tests 1–2 assert parser SHAPE on the verbatim Oracle text. Tests R1–R3 drive
//! the real replacement pipeline (production entry via cast / play-land /
//! `replace_event`) and assert measured tap-state.

use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    ControllerRef, Effect, RestrictionExpiry, TapStateChange, TargetFilter, TypeFilter,
};
use engine::types::actions::{GameAction, ResolutionOptionalPaymentChoice};
use engine::types::game_state::{CostResume, PayCostKind, WaitingFor};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::{EtbTapState, Zone};
use engine::types::ObjectId;

const P1: PlayerId = PlayerId(1);

const DUE_RESPECT: &str = "Permanents enter tapped this turn.\nDraw a card.";
const NAHIRI: &str = "Sacrifice X lands. For each land sacrificed this way, draw a card. \
     You may play X additional lands this turn. Lands you control enter tapped this turn.";
// The load-bearing clause of Nahiri's Lithoforming, cast standalone to install
// the "Lands you control" floating replacement through the real cast pipeline
// (the full X-cost land-sacrifice cast is hard to drive; the plan permits a
// minimal install as long as the ENTRY runs through production).
const LANDS_YOU_CONTROL: &str = "Lands you control enter tapped this turn.";

// --- helpers ---------------------------------------------------------------

/// Collect every `Effect` across all abilities' `sub_ability` chains.
fn all_effects(parsed: &engine::parser::oracle::ParsedAbilities) -> Vec<&Effect> {
    let mut out = Vec::new();
    for ability in &parsed.abilities {
        let mut cur = Some(ability);
        while let Some(d) = cur {
            out.push(&*d.effect);
            cur = d.sub_ability.as_deref();
        }
    }
    out
}

/// The single lifted enters-tapped replacement carried by the parsed abilities.
/// Panics with a clear message if the `AddTargetReplacement { target: None }`
/// shape is absent (positive reach-guard: proves the clause parsed).
fn lifted_replacement(
    parsed: &engine::parser::oracle::ParsedAbilities,
) -> engine::types::ability::ReplacementDefinition {
    for effect in all_effects(parsed) {
        if let Effect::AddTargetReplacement {
            replacement,
            target,
        } = effect
        {
            assert_eq!(
                *target,
                TargetFilter::None,
                "the lift must use the no-per-target-binding mode (CR 611.2a)"
            );
            return (**replacement).clone();
        }
    }
    panic!("expected an AddTargetReplacement in the parsed abilities, found none");
}

fn assert_no_unimplemented(parsed: &engine::parser::oracle::ParsedAbilities) {
    for effect in all_effects(parsed) {
        assert!(
            !matches!(effect, Effect::Unimplemented { .. }),
            "no clause may parse to Effect::Unimplemented, found {effect:?}"
        );
    }
}

/// Assert the carried replacement is the enters-tapped `ChangeZone` shape.
fn assert_enter_tapped_shape(def: &engine::types::ability::ReplacementDefinition) {
    // CR 614.1d: the event is a battlefield-entry replacement.
    assert_eq!(
        def.event,
        ReplacementEvent::ChangeZone,
        "event must be ChangeZone"
    );
    // The execute is a SelfRef single Tap (the enters-tapped modifier class).
    let execute_effect = def
        .execute
        .as_ref()
        .map(|ability| (*ability.effect).clone())
        .expect("replacement must carry an execute effect");
    assert!(
        matches!(
            execute_effect,
            Effect::SetTapState {
                state: TapStateChange::Tap,
                ..
            }
        ),
        "execute must be SetTapState{{Tap}}, got {execute_effect:?}"
    );
    // CR 514.2: expires at end of turn.
    assert_eq!(
        def.expiry,
        Some(RestrictionExpiry::EndOfTurn),
        "replacement must expire at end of turn"
    );
}

/// Extract the `TypedFilter` from a `valid_card` filter.
fn valid_typed(
    def: &engine::types::ability::ReplacementDefinition,
) -> engine::types::ability::TypedFilter {
    match def.valid_card.clone() {
        Some(TargetFilter::Typed(tf)) => tf,
        other => panic!("expected valid_card = Typed(..), got {other:?}"),
    }
}

// --- Test 1: parser SHAPE (Due Respect) ------------------------------------

#[test]
fn due_respect_parses_permanents_enter_tapped_shape() {
    let parsed = parse_oracle_text(
        DUE_RESPECT,
        "Due Respect",
        &[],
        &["Instant".to_string()],
        &[],
    );

    // Positive reach-guard: zero Unimplemented anywhere.
    assert_no_unimplemented(&parsed);

    let def = lifted_replacement(&parsed);
    assert_enter_tapped_shape(&def);

    // "Permanents" → Typed { [Permanent] }, no controller scope.
    let tf = valid_typed(&def);
    assert!(
        matches!(tf.type_filters.as_slice(), [TypeFilter::Permanent]),
        "valid_card must be Typed{{[Permanent]}}, got {:?}",
        tf.type_filters
    );
    assert_eq!(tf.controller, None, "Due Respect's clause is uncontrolled");

    // The "Draw a card." sub-clause must still be present.
    assert!(
        all_effects(&parsed)
            .iter()
            .any(|e| matches!(e, Effect::Draw { .. })),
        "the Draw sub-effect must survive alongside the lifted replacement"
    );
}

// --- Test 2: parser SHAPE (Nahiri's Lithoforming) --------------------------

#[test]
fn nahiris_lithoforming_parses_lands_you_control_shape() {
    let parsed = parse_oracle_text(
        NAHIRI,
        "Nahiri's Lithoforming",
        &[],
        &["Sorcery".to_string()],
        &[],
    );

    // Positive reach-guard: the entire chain now parses (no Unimplemented).
    assert_no_unimplemented(&parsed);

    let def = lifted_replacement(&parsed);
    assert_enter_tapped_shape(&def);

    // "Lands you control" → Typed { [Land], controller: You }.
    let tf = valid_typed(&def);
    assert!(
        matches!(tf.type_filters.as_slice(), [TypeFilter::Land]),
        "valid_card must be Typed{{[Land]}}, got {:?}",
        tf.type_filters
    );
    assert_eq!(
        tf.controller,
        Some(ControllerRef::You),
        "Nahiri's clause is scoped to lands YOU control"
    );

    // The other clauses are unchanged and still present.
    let effects = all_effects(&parsed);
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::Sacrifice { .. })),
        "the Sacrifice clause must remain"
    );
    assert!(
        effects.iter().any(|e| matches!(e, Effect::Draw { .. })),
        "the per-land Draw clause must remain"
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::CastFromZone { .. })),
        "the play-additional-lands clause must remain"
    );
}

// --- Runtime helpers -------------------------------------------------------

fn zone_change_tap_state(runner: &mut GameRunner, object_id: ObjectId, from: Zone) -> EtbTapState {
    let mut events = Vec::new();
    let proposed = ProposedEvent::zone_change(object_id, from, Zone::Battlefield, None);
    match replace_event(runner.state_mut(), proposed, &mut events) {
        ReplacementResult::Execute(ProposedEvent::ZoneChange { enter_tapped, .. }) => enter_tapped,
        other => panic!("expected an Execute(ZoneChange) result, got {other:?}"),
    }
}

// --- Test R1: runtime — Due Respect taps later entries ---------------------

#[test]
fn r1_due_respect_taps_only_entries_after_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A card for Due Respect's own "Draw a card".
    scenario.add_spell_to_library_top(P0, "Filler", true);
    // A creature already on the battlefield BEFORE Due Respect resolves.
    let old_bear = scenario.add_creature(P0, "Old Bear", 2, 2).id();
    // The creature that will ENTER after Due Respect resolves.
    let new_bear = scenario.add_creature_to_hand(P0, "New Bear", 2, 2).id();
    let due_respect = scenario
        .add_spell_to_hand_from_oracle(P0, "Due Respect", true, DUE_RESPECT)
        .id();

    let mut runner = scenario.build();

    // Cast + resolve Due Respect: installs the replacement and draws 1.
    let outcome = runner.cast(due_respect).resolve();
    outcome.assert_hand_drawn(P0, 1);

    // A creature entering LATER this turn (production cast) enters tapped.
    runner.cast(new_bear).resolve();
    assert!(
        runner.state().objects[&new_bear].tapped,
        "a creature entering after Due Respect resolved must enter tapped"
    );

    // The creature that entered BEFORE the spell resolved is untouched.
    assert!(
        !runner.state().objects[&old_bear].tapped,
        "a creature already on the battlefield must not be tapped retroactively"
    );
}

// --- Test R2: runtime — expiry at cleanup ----------------------------------

#[test]
fn r2_due_respect_replacement_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_spell_to_library_top(P0, "Filler", true);
    // A land in P0's hand whose proposed entry we probe next turn.
    let probe_land = scenario.add_land_to_hand(P0, "Probe Land").id();
    let due_respect = scenario
        .add_spell_to_hand_from_oracle(P0, "Due Respect", true, DUE_RESPECT)
        .id();

    let mut runner = scenario.build();
    runner.cast(due_respect).resolve();

    // Sanity: the floating replacement is installed this turn.
    assert_eq!(
        runner.state().pending_damage_replacements.len(),
        1,
        "Due Respect must install exactly one floating replacement this turn"
    );

    // Advance past this turn's cleanup (CR 514.2) into the next turn.
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);

    // The floating store is empty after cleanup pruned the EndOfTurn expiry.
    assert!(
        runner.state().pending_damage_replacements.is_empty(),
        "the end-of-turn replacement must be pruned at cleanup"
    );

    // A permanent proposed to enter now is UNTAPPED — the duration expired.
    // (If expiry were not set, the surviving replacement would tap it.)
    let tap_state = zone_change_tap_state(&mut runner, probe_land, Zone::Hand);
    assert_eq!(
        tap_state,
        EtbTapState::Unspecified,
        "after the replacement expires, a new entry must be untapped"
    );
}

// --- Test R3: runtime — Nahiri multi-authority -----------------------------

#[test]
fn r3_nahiri_taps_your_lands_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Objects that will be proposed to enter through the replacement pipeline.
    let my_land = scenario.add_land_to_hand(P0, "My Land").id();
    let their_land = scenario.add_land_to_hand(P1, "Their Land").id();
    let my_creature = scenario.add_creature_to_hand(P0, "My Bear", 2, 2).id();

    // Install the "Lands you control enter tapped this turn" replacement through
    // the real cast pipeline (P0 is the resolving controller / anchor).
    let install = scenario
        .add_spell_to_hand_from_oracle(P0, "Litho Clause", true, LANDS_YOU_CONTROL)
        .id();

    let mut runner = scenario.build();
    runner.cast(install).resolve();

    // CR 109.4: the install anchors source_controller to P0 so the
    // controller-relative "you control" filter can resolve for a floating def.
    {
        let pending = &runner.state().pending_damage_replacements;
        assert_eq!(
            pending.len(),
            1,
            "exactly one floating replacement installed"
        );
        assert_eq!(
            pending[0].source_controller,
            Some(P0),
            "the resolver must anchor source_controller to the installing player"
        );
        assert_eq!(pending[0].event, ReplacementEvent::ChangeZone);
        assert_eq!(pending[0].expiry, Some(RestrictionExpiry::EndOfTurn));
    }

    // Production entry (CR 614.1d) via the real replacement pipeline:
    //   - P0's land matches [Land] + controller You  → enters TAPPED.
    assert_eq!(
        zone_change_tap_state(&mut runner, my_land, Zone::Hand),
        EtbTapState::Tapped,
        "a land YOU control must enter tapped"
    );
    //   - P1's land fails the controller:You gate      → enters UNTAPPED.
    assert_eq!(
        zone_change_tap_state(&mut runner, their_land, Zone::Hand),
        EtbTapState::Unspecified,
        "an opponent's land must not be tapped by 'Lands you control'"
    );
    //   - P0's creature fails the [Land] type gate      → enters UNTAPPED.
    assert_eq!(
        zone_change_tap_state(&mut runner, my_creature, Zone::Hand),
        EtbTapState::Unspecified,
        "a non-land you control must not be tapped by a Land-scoped replacement"
    );
}

// Complete pinned English printed records. Native synthesis happens inside this
// fixture, so these tests consume this parser rather than a stale export.
fn u5_database() -> &'static engine::database::CardDatabase {
    static DATABASE: std::sync::OnceLock<engine::database::CardDatabase> =
        std::sync::OnceLock::new();
    DATABASE.get_or_init(|| {
        let records: Vec<engine::database::mtgjson::AtomicCard> = serde_json::from_str(r###"[{"colorIdentity":["B","U"],"colors":["B","U"],"identifiers":{"scryfallOracleId":"a2aa2c8f-7058-4020-9881-663b104ea051"},"layout":"normal","manaCost":"{3}{U}{B}","manaValue":5.0,"name":"Grimgrin, Corpse-Born","power":"5","toughness":"5","rulings":[{"date":"2011-09-22","text":"If the defending player controls no creatures when Grimgrin attacks, the last ability will be removed from the stack and have no effect."},{"date":"2011-09-22","text":"If the targeted creature is an illegal target by the time Grimgrin's last ability resolves, the entire ability doesn't resolve and none of its effects will occur. You won't put a +1/+1 counter on Grimgrin."},{"date":"2013-07-01","text":"If Grimgrin's last ability resolves, but the targeted creature isn't destroyed (perhaps because it regenerated or has indestructible), you'll still put a +1/+1 on Grimgrin."}],"subtypes":["Zombie","Warrior"],"supertypes":["Legendary"],"type":"Legendary Creature — Zombie Warrior","types":["Creature"],"text":"Grimgrin enters tapped and doesn't untap during your untap step.\nSacrifice another creature: Untap Grimgrin and put a +1/+1 counter on it.\nWhenever Grimgrin attacks, destroy target creature defending player controls, then put a +1/+1 counter on Grimgrin."},{"colorIdentity":["U"],"colors":["U"],"identifiers":{"scryfallOracleId":"d8c49932-9b4f-4cb0-a7e9-4b7eb12203ee"},"keywords":["Trample"],"layout":"normal","manaCost":"{5}{U}{U}{U}{U}","manaValue":9.0,"name":"Leviathan","power":"10","toughness":"10","rulings":[{"date":"2004-10-04","text":"You don't have to pay the untap cost if it is untapped outside the upkeep phase or is untapped by an effect."}],"subtypes":["Leviathan"],"supertypes":[],"type":"Creature — Leviathan","types":["Creature"],"text":"Trample\nThis creature enters tapped and doesn't untap during your untap step.\nAt the beginning of your upkeep, you may sacrifice two Islands. If you do, untap this creature.\nThis creature can't attack unless you sacrifice two Islands. (This cost is paid as attackers are declared.)"},{"colorIdentity":[],"colors":[],"identifiers":{"scryfallOracleId":"c1c78144-b335-4d22-a668-9173ab6a0d04"},"keywords":["Trample"],"layout":"normal","manaCost":"{4}","manaValue":4.0,"name":"Traxos, Scourge of Kroog","power":"7","toughness":"7","rulings":[{"date":"2018-04-27","text":"A card, spell, or permanent is historic if it has the legendary supertype, the artifact card type, or the Saga subtype. Having two of those qualities doesn't make an object more historic than another or provide an additional bonus—an object either is historic or it isn't."},{"date":"2018-04-27","text":"An ability that triggers \"whenever you cast a historic spell\" doesn't trigger if a historic card is put onto the battlefield without being cast."},{"date":"2018-04-27","text":"Lands are never cast, so abilities that trigger \"whenever you cast a historic spell\" won't trigger if you play a legendary land. They also won't trigger if a card on the battlefield transforms into a legendary land, as the Ixalan and Rivals of Ixalan double-faced cards do."},{"date":"2018-04-27","text":"Some abilities trigger \"whenever you cast a historic spell.\" Such an ability resolves before the spell that caused it to trigger. It resolves even if that spell is countered."}],"subtypes":["Dragon","Construct"],"supertypes":["Legendary"],"type":"Legendary Artifact Creature — Dragon Construct","types":["Artifact","Creature"],"text":"Trample\nTraxos enters tapped and doesn't untap during your untap step.\nWhenever you cast a historic spell, untap Traxos. (Artifacts, legendaries, and Sagas are historic.)"},{"colorIdentity":["B","G","R"],"colors":["B","G","R"],"identifiers":{"scryfallOracleId":"d46c3fb6-f1c7-4a96-ae42-5bce17fc7c1d"},"keywords":["Flying","Treasure"],"layout":"normal","manaCost":"{3}{B}{R}{G}","manaValue":6.0,"name":"Ziatora, the Incinerator","power":"6","toughness":"6","rulings":[{"date":"2022-04-29","text":"Use the creature's power as it last existed on the battlefield to determine how much damage Ziatora deals."}],"subtypes":["Demon","Dragon"],"supertypes":["Legendary"],"type":"Legendary Creature — Demon Dragon","types":["Creature"],"text":"Flying\nAt the beginning of your end step, you may sacrifice another creature. When you do, Ziatora deals damage equal to that creature's power to any target and you create three Treasure tokens."},{"colorIdentity":["W"],"colors":["W"],"identifiers":{"scryfallOracleId":"087f9ad7-e74f-40e2-8102-1ed2925d0418"},"layout":"normal","manaCost":"{1}{W}","manaValue":2.0,"name":"Rest in Peace","rulings":[{"date":"2018-03-16","text":"If Rest in Peace is destroyed by a spell, Rest in Peace will be exiled and then the spell will be put into its owner's graveyard."},{"date":"2018-03-16","text":"If a card is discarded while Rest in Peace is on the battlefield, abilities that function when a card is discarded (such as madness) still work, even though that card never reaches a graveyard. In addition, spells or abilities that check the characteristics of a discarded card (such as Chandra Ablaze's first ability) can find that card in exile."},{"date":"2018-03-16","text":"While Rest in Peace is on the battlefield, abilities that trigger whenever a creature dies won't trigger because cards and tokens are never put into a player's graveyard."}],"subtypes":[],"supertypes":[],"type":"Enchantment","types":["Enchantment"],"text":"When this enchantment enters, exile all graveyards.\nIf a card or token would be put into a graveyard from anywhere, exile it instead."},{"colorIdentity":["U"],"colors":["U"],"identifiers":{"scryfallOracleId":"32452ccf-88c7-41d6-93e5-15d31f29726d"},"layout":"normal","manaCost":"{3}{U}","manaValue":4.0,"name":"Frozen Aether","rulings":[{"date":"2007-02-01","text":"Does not affect cards that phase in."},{"date":"2007-02-01","text":"It affects all opponents."},{"date":"2007-02-01","text":"It applies to cards and tokens that are put onto the battlefield by an effect, as well as ones that are played from the player's hand."},{"date":"2007-02-01","text":"The appropriate permanents enter tapped. They do not enter untapped and then immediately tap, therefore they do not trigger any effects due to tapping."}],"subtypes":[],"supertypes":[],"type":"Enchantment","types":["Enchantment"],"text":"Artifacts, creatures, and lands your opponents control enter tapped."},{"colorIdentity":["U","W"],"colors":[],"identifiers":{"scryfallOracleId":"f1750962-a87c-49f6-b731-02ae971ac6ea"},"layout":"normal","manaValue":0.0,"name":"Hallowed Fountain","rulings":[{"date":"2018-10-05","text":"Unlike most dual lands, this land has two basic land types. It's not basic, so cards such as District Guide can't find it, but it does have the appropriate land types for effects such as that of Drowned Catacomb (from the Ixalan set)."},{"date":"2025-11-17","text":"If an effect puts this land onto the battlefield tapped, you may pay 2 life, but it still enters tapped."},{"date":"2025-11-17","text":"This land has two basic land types. It's not basic, so cards such as Tend the Sprigs can't find it, but it does have the appropriate land types for effects such as that of Kishla Village (from the Tarkir: Dragonstorm release)."}],"subtypes":["Plains","Island"],"supertypes":[],"type":"Land — Plains Island","types":["Land"],"text":"({T}: Add {W} or {U}.)\nAs this land enters, you may pay 2 life. If you don't, it enters tapped."},{"colorIdentity":["U","W"],"colors":[],"identifiers":{"scryfallOracleId":"027dd013-baa7-4111-b3c9-f4d1414e9c45"},"layout":"normal","manaValue":0.0,"name":"Glacial Fortress","rulings":[{"date":"2009-10-01","text":"As this is entering, it checks for lands that are already on the battlefield. It won't see lands that are entering at the same time (due to Warp World, for example)."},{"date":"2009-10-01","text":"This checks for lands you control with the land type Plains or Island, not for lands named Plains or Island. The lands it checks for don't have to be basic lands. For example, if you control Watery Grave (a nonbasic land with the land types Island and Swamp), Glacial Fortress will enter untapped."}],"subtypes":[],"supertypes":[],"type":"Land","types":["Land"],"text":"This land enters tapped unless you control a Plains or an Island.\n{T}: Add {W} or {U}."},{"colorIdentity":["U"],"colors":[],"identifiers":{"scryfallOracleId":"2da7c49f-cc1e-45d9-9cbf-067e92b0daef"},"layout":"normal","manaValue":0.0,"name":"Vivid Creek","subtypes":[],"supertypes":[],"type":"Land","types":["Land"],"text":"This land enters tapped with two charge counters on it.\n{T}: Add {U}.\n{T}, Remove a charge counter from this land: Add one mana of any color."},{"colorIdentity":["U"],"colors":["U"],"identifiers":{"scryfallOracleId":"837182db-1bf3-4a2c-bd01-1af9d9873561"},"layout":"normal","manaCost":"{U}","manaValue":1.0,"name":"Unsummon","subtypes":[],"supertypes":[],"type":"Instant","types":["Instant"],"text":"Return target creature to its owner's hand."},{"colorIdentity":["U"],"colors":["U"],"identifiers":{"scryfallOracleId":"77db8db4-9fe4-478e-b919-5348b0b87b0d"},"layout":"normal","manaCost":"{X}{1}{U}{U}","manaValue":3.0,"name":"Dominate","subtypes":[],"supertypes":[],"type":"Instant","types":["Instant"],"text":"Gain control of target creature with mana value X or less."},{"colorIdentity":["U","W"],"colors":["W"],"identifiers":{"scryfallOracleId":"a3ec6b5d-08ec-4ae0-b1db-c4b87a1849c7"},"keywords":["Flashback"],"layout":"normal","manaCost":"{1}{W}","manaValue":2.0,"name":"Momentary Blink","rulings":[{"date":"2021-03-19","text":"\"Flashback [cost]\" means \"You may cast this card from your graveyard by paying [cost] rather than paying its mana cost\" and \"If the flashback cost was paid, exile this card instead of putting it anywhere else any time it would leave the stack.\""},{"date":"2021-03-19","text":"A spell cast using flashback will always be exiled afterward, whether it resolves, is countered, or leaves the stack in some other way."},{"date":"2021-03-19","text":"If a card with flashback is put into your graveyard during your turn, you can cast it if it's legal to do so before any other player can take any actions."},{"date":"2021-03-19","text":"To determine the total cost of a spell, start with the mana cost or alternative cost (such as a flashback cost) you're paying, add any cost increases, then apply any cost reductions. The mana value of the spell is determined only by its mana cost, no matter what the total cost to cast the spell was."},{"date":"2021-03-19","text":"You can cast a spell using flashback even if it was somehow put into your graveyard without having been cast."},{"date":"2021-03-19","text":"You must still follow any timing restrictions and permissions, including those based on the card's type. For instance, you can cast a sorcery using flashback only when you could normally cast a sorcery."},{"date":"2022-12-08","text":"If a token is exiled this way, it will cease to exist and won't return to the battlefield."},{"date":"2022-12-08","text":"When the card returns to the battlefield, it will be a new object with no connection to the card that was exiled. Auras attached to the exiled creature will be put into their owners' graveyards. Any Equipment will become unattached and remain on the battlefield. Any counters on the exiled creature will cease to exist."}],"subtypes":[],"supertypes":[],"type":"Instant","types":["Instant"],"text":"Exile target creature you control, then return it to the battlefield under its owner's control.\nFlashback {3}{U} (You may cast this card from your graveyard for its flashback cost. Then exile it.)"},{"colorIdentity":["U"],"colors":["U"],"identifiers":{"scryfallOracleId":"cc187110-1148-4090-bbb8-e205694a39f5"},"layout":"normal","manaCost":"{U}{U}","manaValue":2.0,"name":"Counterspell","subtypes":[],"supertypes":[],"type":"Instant","types":["Instant"],"text":"Counter target spell."},{"colorIdentity":[],"colors":[],"identifiers":{"scryfallOracleId":"7663ac7c-1de3-4250-b96a-fae9dbd66a27"},"layout":"normal","manaCost":"{0}","manaValue":0.0,"name":"Memnite","power":"1","toughness":"1","subtypes":["Construct"],"supertypes":[],"type":"Artifact Creature — Construct","types":["Artifact","Creature"]},{"colorIdentity":["W"],"colors":["W"],"identifiers":{"scryfallOracleId":"cfb756dd-6fe1-430a-8f04-8b10cac62a86"},"layout":"normal","manaCost":"{W}","manaValue":1.0,"name":"Isamaru, Hound of Konda","power":"2","toughness":"2","subtypes":["Dog"],"supertypes":["Legendary"],"type":"Legendary Creature — Dog","types":["Creature"]},{"colorIdentity":[],"colors":[],"identifiers":{"scryfallOracleId":"1a20e805-c80b-4710-917b-706d29ec395e"},"keywords":["Flying"],"layout":"normal","manaCost":"{1}","manaValue":1.0,"name":"Hope of Ghirapur","power":"1","toughness":"1","rulings":[{"date":"2017-02-09","text":"If you skip your turn, the affected player will be unable to cast noncreature spells until you actually start a turn."},{"date":"2017-02-09","text":"The affected player may still activate abilities, play lands, and cast creature spells."},{"date":"2017-02-09","text":"The affected player won’t be able to cast any noncreature spells until after the time that your “beginning of upkeep” triggered abilities are put onto the stack on your next turn."},{"date":"2017-02-09","text":"The target player (and any other player) may cast spells in response to the activated ability of Hope of Ghirapur. The ability won’t affect those spells and it won’t affect spells that the target player cast before you activated it. (In other words, the ability can’t be used to counter a spell.)"},{"date":"2017-02-09","text":"When a player leaves a multiplayer game, any continuous effects with durations that last until that player’s next turn will last until that turn would have begun. They neither expire immediately nor last indefinitely."}],"subtypes":["Thopter"],"supertypes":["Legendary"],"type":"Legendary Artifact Creature — Thopter","types":["Artifact","Creature"],"text":"Flying\nSacrifice Hope of Ghirapur: Until your next turn, target player who was dealt combat damage by Hope of Ghirapur this turn can't cast noncreature spells."},{"colorIdentity":["W"],"colors":["W"],"identifiers":{"scryfallOracleId":"c15bb7eb-aaaa-4468-9641-8f706d6137e8"},"layout":"saga","manaCost":"{1}{W}{W}","manaValue":3.0,"name":"History of Benalia","rulings":[{"date":"2018-04-27","text":"A chapter ability doesn’t trigger if a lore counter is put on a Saga that already had a number of lore counters greater than or equal to that chapter’s number. For example, the third lore counter put on a Saga causes the III chapter ability to trigger, but I and II won’t trigger again."},{"date":"2018-04-27","text":"As a Saga enters the battlefield, its controller puts a lore counter on it. As your precombat main phase begins (immediately after your draw step), you put another lore counter on each Saga you control. Putting a lore counter on a Saga in either of these ways doesn’t use the stack."},{"date":"2018-04-27","text":"Each symbol on the left of a Saga’s text box represents a chapter ability. A chapter ability is a triggered ability that triggers when a lore counter that is put on the Saga causes the number of lore counters on the Saga to become equal to or greater than the ability’s chapter number. Chapter abilities are put onto the stack and may be responded to."},{"date":"2018-04-27","text":"If counters are removed from a Saga, the appropriate chapter abilities will trigger again when the Saga receives lore counters. Removing lore counters won’t cause a previous chapter ability to trigger."},{"date":"2018-04-27","text":"If multiple chapter abilities trigger at the same time, their controller puts them on the stack in any order. If any of them require targets, those targets are chosen as you put the abilities on the stack, before any of those abilities resolve."},{"date":"2018-04-27","text":"Once a chapter ability has triggered, the ability on the stack won’t be affected if the Saga gains or loses counters, or if it leaves the battlefield."},{"date":"2018-04-27","text":"Once the number of lore counters on a Saga is greater than or equal to the greatest number among its chapter abilities—in the Dominaria set, this is always three—the Saga’s controller sacrifices it as soon as its chapter ability has left the stack, most likely by resolving or being countered. This state-based action doesn’t use the stack."},{"date":"2018-04-27","text":"The final chapter ability of History of Benalia affects only Knights you control at the time it resolves. Creatures you begin to control later in the turn or that become Knights later in the turn won’t get +2/+1."}],"subtypes":["Saga"],"supertypes":[],"type":"Enchantment — Saga","types":["Enchantment"],"text":"(As this Saga enters and after your draw step, add a lore counter. Sacrifice after III.)\nI, II — Create a 2/2 white Knight creature token with vigilance.\nIII — Knights you control get +2/+1 until end of turn."},{"colorIdentity":["W"],"colors":[],"identifiers":{"scryfallOracleId":"f73979bb-91a5-4388-b70b-0cd7a4e14291"},"layout":"normal","manaValue":0.0,"name":"Flagstones of Trokair","subtypes":[],"supertypes":["Legendary"],"type":"Legendary Land","types":["Land"],"text":"{T}: Add {W}.\nWhen Flagstones of Trokair is put into a graveyard from the battlefield, you may search your library for a Plains card, put it onto the battlefield tapped, then shuffle."},{"colorIdentity":["U"],"colors":["U"],"identifiers":{"scryfallOracleId":"b83a3ba0-249e-4c39-bbf0-cb005413f7d2"},"layout":"normal","manaCost":"{2}{U}","manaValue":3.0,"name":"Show and Tell","rulings":[{"date":"2004-10-04","text":"If the cards being put onto the battlefield also require choices, those choices are made after all players choose their card. The active player makes choices for their card (if any), then the other players (if any) in turn order."},{"date":"2004-10-04","text":"Players choose cards during resolution, not announcement."},{"date":"2008-04-01","text":"The current player chooses first, then each other player chooses in turn order. A player does not have to reveal the chosen card, so long as it is clear *which* card was chosen. After all choices are made, the cards are put onto the battlefield simultaneously."}],"subtypes":[],"supertypes":[],"type":"Sorcery","types":["Sorcery"],"text":"Each player may put an artifact, creature, enchantment, or land card from their hand onto the battlefield."},{"colorIdentity":["G"],"colors":["G"],"identifiers":{"scryfallOracleId":"14c8f55d-d177-4c25-a931-ebeb9e6062a0"},"layout":"normal","manaCost":"{1}{G}","manaValue":2.0,"name":"Grizzly Bears","power":"2","toughness":"2","subtypes":["Bear"],"supertypes":[],"type":"Creature — Bear","types":["Creature"]},{"colorIdentity":["W"],"colors":["W"],"identifiers":{"scryfallOracleId":"1c367fa2-844a-4083-9484-c6def1dee8a8"},"layout":"normal","manaCost":"{1}{W}","manaValue":2.0,"name":"Due Respect","rulings":[{"date":"2011-06-01","text":"Due Respect won't tap any permanents that have entered before it resolved."}],"subtypes":[],"supertypes":[],"type":"Instant","types":["Instant"],"text":"Permanents enter tapped this turn.\nDraw a card."},{"colorIdentity":["U"],"colors":[],"identifiers":{"scryfallOracleId":"b2c6aa39-2d2a-459c-a555-fb48ba993373"},"layout":"normal","manaValue":0.0,"name":"Island","subtypes":["Island"],"supertypes":["Basic"],"type":"Basic Land — Island","types":["Land"],"text":"({T}: Add {U}.)"},{"colorIdentity":["W"],"colors":[],"identifiers":{"scryfallOracleId":"bc71ebf6-2056-41f7-be35-b2e5c34afa99"},"layout":"normal","manaValue":0.0,"name":"Plains","subtypes":["Plains"],"supertypes":["Basic"],"type":"Basic Land — Plains","types":["Land"],"text":"({T}: Add {W}.)"},{"colorIdentity":["B"],"colors":[],"identifiers":{"scryfallOracleId":"56719f6a-1a6c-4c0a-8d21-18f7d7350b68"},"layout":"normal","manaValue":0.0,"name":"Swamp","subtypes":["Swamp"],"supertypes":["Basic"],"type":"Basic Land — Swamp","types":["Land"],"text":"({T}: Add {B}.)"},{"colorIdentity":["G"],"colors":["G"],"identifiers":{"scryfallOracleId":"68954295-54e3-4303-a6bc-fc4547a4e3a3"},"layout":"normal","manaCost":"{G}","manaValue":1.0,"name":"Llanowar Elves","power":"1","toughness":"1","subtypes":["Elf","Druid"],"supertypes":[],"type":"Creature — Elf Druid","types":["Creature"],"text":"{T}: Add {G}."}]"###).unwrap();
        let faces = records.into_iter().map(|record| {
            let face = engine::database::synthesis::build_oracle_face(&record, None);
            (face.name.to_lowercase(), face)
        }).collect::<std::collections::BTreeMap<_, _>>();
        engine::database::CardDatabase::from_json_str(&serde_json::to_string(&faces).unwrap()).unwrap()
    })
}

fn u5_complete(runner: &GameRunner) {
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().stack.is_empty());
    assert!(runner.state().resolution_stack.is_empty());
}

fn u5_fund(scenario: &mut GameScenario, player: PlayerId, mana: &[ManaType]) {
    scenario.with_mana_pool(
        player,
        mana.iter()
            .map(|kind| ManaUnit::new(*kind, ObjectId(0), false, vec![]))
            .collect(),
    );
}

#[test]
fn u5_full_sources_paid_entries_and_actual_own_untap() {
    for (name, mana) in [
        (
            "Grimgrin, Corpse-Born",
            vec![
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Blue,
                ManaType::Black,
            ],
        ),
        (
            "Leviathan",
            vec![
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Blue,
                ManaType::Blue,
                ManaType::Blue,
                ManaType::Blue,
            ],
        ),
        ("Traxos, Scourge of Kroog", vec![ManaType::Colorless; 4]),
    ] {
        for player in [P0, P1] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_library_top(P0, &["A", "B", "C", "D"]);
            scenario.with_library_top(P1, &["E", "F", "G", "H"]);
            let source = scenario.add_real_card(player, name, Zone::Hand, u5_database());
            let upkeep_pool = if name == "Leviathan" {
                vec![
                    scenario.add_real_card(player, "Island", Zone::Battlefield, u5_database()),
                    scenario.add_real_card(player, "Island", Zone::Battlefield, u5_database()),
                ]
            } else {
                vec![]
            };
            let ordinary =
                scenario.add_real_card(player, "Llanowar Elves", Zone::Battlefield, u5_database());
            let foreign_player = if player == P0 { P1 } else { P0 };
            let foreign = scenario.add_real_card(
                foreign_player,
                "Grizzly Bears",
                Zone::Battlefield,
                u5_database(),
            );
            let foreign_entrant =
                scenario.add_real_card(foreign_player, "Grizzly Bears", Zone::Hand, u5_database());
            let foreign_mana_sources = [
                scenario.add_real_card(
                    foreign_player,
                    "Llanowar Elves",
                    Zone::Battlefield,
                    u5_database(),
                ),
                scenario.add_real_card(
                    foreign_player,
                    "Llanowar Elves",
                    Zone::Battlefield,
                    u5_database(),
                ),
            ];
            u5_fund(&mut scenario, player, &mana);
            let mut runner = scenario.build();
            // Initial player/phase setup precedes every public action.
            runner.state_mut().active_player = player;
            runner.state_mut().priority_player = player;
            runner.state_mut().waiting_for = WaitingFor::Priority { player };
            let hand_epoch = runner.state().objects[&source].incarnation;
            let cast = runner.cast(source).commit();
            assert_eq!(cast.state().objects[&source].zone, Zone::Stack);
            assert_eq!(cast.state().stack.len(), 1);
            assert!(cast.state().players[player.0 as usize].mana_pool.is_empty());
            cast.resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert!(runner.state().objects[&source].incarnation > hand_epoch);
            assert!(runner.state().objects[&source].tapped, "{name}: paid entry");
            assert_eq!(runner.state().objects[&source].owner, player);
            assert_eq!(runner.state().objects[&source].controller, player);
            assert!(!runner.state().objects[&foreign].tapped);
            u5_complete(&runner);
            let entered_epoch = runner.state().objects[&source].incarnation;
            runner.activate(ordinary, 0).resolve();
            assert!(runner.state().objects[&ordinary].tapped);
            u5_complete(&runner);
            // CR 502.3: reach this source's next actual own untap through both
            // turns, retaining the same incarnation and an ordinary positive.
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.advance_to_upkeep();
            assert_ne!(runner.state().active_player, player);
            assert!(runner.state().objects[&source].tapped);
            // CR 302.1: the foreign creature is cast in its controller's own
            // main phase, after the tested source has entered and is functioning.
            runner.advance_to_phase(Phase::PreCombatMain);
            assert_eq!(runner.state().active_player, foreign_player);
            assert_eq!(runner.state().phase, Phase::PreCombatMain);
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { player: actor } if actor == foreign_player)
            );
            u5_complete(&runner);
            for mana_source in foreign_mana_sources {
                runner.activate(mana_source, 0).resolve();
            }
            let foreign_hand_epoch = runner.state().objects[&foreign_entrant].incarnation;
            let foreign_cast = runner.cast(foreign_entrant).commit();
            assert_eq!(
                foreign_cast.state().objects[&foreign_entrant].zone,
                Zone::Stack
            );
            assert_eq!(foreign_cast.state().stack.len(), 1);
            assert_eq!(foreign_cast.state().stack[0].source_id, foreign_entrant);
            assert!(foreign_cast.state().players[foreign_player.0 as usize]
                .mana_pool
                .is_empty());
            foreign_cast.resolve();
            assert_eq!(
                runner.state().objects[&foreign_entrant].zone,
                Zone::Battlefield
            );
            assert!(runner.state().objects[&foreign_entrant].incarnation > foreign_hand_epoch);
            assert_eq!(
                runner.state().objects[&foreign_entrant].owner,
                foreign_player
            );
            assert_eq!(
                runner.state().objects[&foreign_entrant].controller,
                foreign_player
            );
            // CR 614.12: each self-entry replacement affects only its source;
            // the unrelated entrant stays untapped beside that tapped source.
            assert!(
                !runner.state().objects[&foreign_entrant].tapped,
                "{name}: foreign paid entry"
            );
            assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&source].incarnation, entered_epoch);
            assert!(runner.state().objects[&source].tapped);
            u5_complete(&runner);
            runner.advance_to_combat();
            runner.declare_attackers(&[]).unwrap();
            runner.advance_to_end_step();
            runner.advance_to_upkeep();
            assert_eq!(runner.state().active_player, player);
            runner.advance_until_stack_empty();
            assert_eq!(runner.state().objects[&source].incarnation, entered_epoch);
            assert!(
                runner.state().objects[&source].tapped,
                "{name}: own untap restriction"
            );
            assert!(
                !runner.state().objects[&ordinary].tapped,
                "ordinary own creature really untaps"
            );
            if name == "Leviathan" {
                assert!(
                    matches!(runner.state().waiting_for, WaitingFor::ResolutionOptionalPaymentChoice { player: actor, source_id, ref costs } if actor == player && source_id == source && costs.len() == 1 && costs[0].index == 0)
                );
                runner
                    .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                        choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
                    })
                    .unwrap();
                assert!(matches!(
                    runner.state().waiting_for,
                    WaitingFor::PayCost {
                        kind: PayCostKind::Sacrifice,
                        count: 2,
                        min_count: 2,
                        ..
                    }
                ));
                runner
                    .act(GameAction::SelectCards {
                        cards: upkeep_pool.clone(),
                    })
                    .unwrap();
                for id in upkeep_pool {
                    assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
                }
                assert!(
                    !runner.state().objects[&source].tapped,
                    "the full paid source's printed upkeep consumer completes"
                );
            }
            u5_complete(&runner);
        }
        for payment in [vec![], vec![ManaType::Red; mana.len()]] {
            // Traxos accepts any four mana, so its wrong-funding twin is short.
            let payment = if name == "Traxos, Scourge of Kroog" && !payment.is_empty() {
                vec![ManaType::Red; 3]
            } else {
                payment
            };
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source = scenario.add_real_card(P0, name, Zone::Hand, u5_database());
            u5_fund(&mut scenario, P0, &payment);
            let mut runner = scenario.build();
            assert!(
                runner.cast(source).try_resolve().is_err(),
                "{name}: insufficient payment"
            );
            assert_ne!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert!(!runner.state().objects[&source].tapped);
            match runner.state().waiting_for {
                WaitingFor::ManaPayment { .. } => {
                    runner.act(GameAction::CancelCast).unwrap();
                }
                WaitingFor::Priority { .. } => {}
                ref other => panic!("unexpected refused-cast boundary: {other:?}"),
            }
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
            u5_complete(&runner);
        }
    }
}

#[test]
fn u5_leviathan_real_upkeep_exact_singleton_and_invalid_public_payments() {
    for population in [0usize, 1, 2, 4] {
        for decline in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::Untap);
            let source = scenario.add_real_card(P0, "Leviathan", Zone::Battlefield, u5_database());
            let islands = (0..population)
                .map(|_| scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()))
                .collect::<Vec<_>>();
            let foreign = scenario.add_real_card(P1, "Island", Zone::Battlefield, u5_database());
            let non_island = scenario.add_real_card(P0, "Plains", Zone::Battlefield, u5_database());
            let off_pool = scenario.add_real_card(P0, "Island", Zone::Hand, u5_database());
            let mut runner = scenario.build();
            assert!(runner.state().objects[&source].tapped);
            runner.advance_to_upkeep();
            assert_eq!(runner.state().phase, Phase::Upkeep);
            assert_eq!(
                runner.state().stack.len(),
                1,
                "actual phase-generated trigger"
            );
            assert_eq!(runner.state().stack[0].source_id, source);
            assert_eq!(runner.state().stack[0].controller, P0);
            runner.resolve_top();
            if population < 2 {
                assert!(runner.state().objects[&source].tapped);
                for id in &islands {
                    assert_eq!(runner.state().objects[id].zone, Zone::Battlefield);
                }
                u5_complete(&runner);
                continue;
            }
            let WaitingFor::ResolutionOptionalPaymentChoice {
                player,
                source_id,
                costs: options,
            } = &runner.state().waiting_for
            else {
                panic!(
                    "full upkeep must offer its singleton exact cost: {:?}",
                    runner.state().waiting_for
                )
            };
            assert_eq!((*player, *source_id), (P0, source));
            assert_eq!(options.len(), 1);
            assert_eq!(options[0].index, 0);
            assert!(
                matches!(&options[0].cost, engine::types::ability::AbilityCost::Sacrifice(cost)
                if cost.requirement.fixed_count() == Some(2) && matches!(&cost.target, TargetFilter::Typed(tf) if tf.type_filters == vec![TypeFilter::Subtype("Island".into())]))
            );
            assert!(runner.state().active_optional_effect_frame().is_some());
            assert!(!runner.state().resolution_stack.is_empty());
            let before = serde_json::to_string(runner.state()).unwrap();
            assert!(engine::game::engine::apply(
                runner.state_mut(),
                P1,
                GameAction::ChooseResolutionOptionalPaymentBranch {
                    choice: ResolutionOptionalPaymentChoice::Pay { index: 0 }
                }
            )
            .is_err());
            assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
            assert!(runner
                .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                    choice: ResolutionOptionalPaymentChoice::Pay { index: 1 }
                })
                .is_err());
            assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
            if decline {
                runner
                    .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                        choice: ResolutionOptionalPaymentChoice::Decline,
                    })
                    .unwrap();
                for id in &islands {
                    assert_eq!(runner.state().objects[id].zone, Zone::Battlefield);
                }
                assert!(runner.state().objects[&source].tapped);
                u5_complete(&runner);
                continue;
            }
            runner
                .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                    choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
                })
                .unwrap();
            let WaitingFor::PayCost {
                player,
                kind,
                choices,
                count,
                min_count,
                resume,
            } = &runner.state().waiting_for
            else {
                panic!("exact payment selection")
            };
            assert_eq!((*player, *count, *min_count), (P0, 2, 2));
            assert_eq!(*kind, PayCostKind::Sacrifice);
            assert_eq!(*resume, CostResume::Resolution);
            assert_eq!(
                choices
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                islands.iter().copied().collect()
            );
            for invalid in [
                vec![islands[0]],
                vec![islands[0], islands[0]],
                vec![islands[0], foreign],
                vec![islands[0], non_island],
                vec![islands[0], off_pool],
            ] {
                let before = serde_json::to_string(runner.state()).unwrap();
                assert!(runner
                    .act(GameAction::SelectCards { cards: invalid })
                    .is_err());
                assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
                assert!(runner.state().objects[&source].tapped);
            }
            let selected = vec![islands[population - 1], islands[population - 2]];
            runner
                .act(GameAction::SelectCards {
                    cards: selected.clone(),
                })
                .unwrap();
            for id in &islands {
                assert_eq!(
                    runner.state().objects[id].zone,
                    if selected.contains(id) {
                        Zone::Graveyard
                    } else {
                        Zone::Battlefield
                    }
                );
            }
            for id in [foreign, non_island] {
                assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
            }
            assert!(!runner.state().objects[&source].tapped);
            assert!(runner.state().active_optional_effect_frame().is_none());
            assert!(runner.state().pending_cost_move_resume.is_none());
            u5_complete(&runner);
        }
    }
}

#[test]
fn u5_grimgrin_actual_activation_pays_another_own_nondefault_creature() {
    for player in [P0, P1] {
        let opponent = if player == P0 { P1 } else { P0 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source =
            scenario.add_real_card(player, "Grimgrin, Corpse-Born", Zone::Hand, u5_database());
        let other_source = scenario.add_real_card(
            opponent,
            "Grimgrin, Corpse-Born",
            Zone::Battlefield,
            u5_database(),
        );
        let first =
            scenario.add_real_card(player, "Grizzly Bears", Zone::Battlefield, u5_database());
        let chosen =
            scenario.add_real_card(player, "Llanowar Elves", Zone::Battlefield, u5_database());
        let foreign =
            scenario.add_real_card(opponent, "Grizzly Bears", Zone::Battlefield, u5_database());
        u5_fund(
            &mut scenario,
            player,
            &[
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Blue,
                ManaType::Black,
            ],
        );
        let mut runner = scenario.build();
        runner.state_mut().active_player = player;
        runner.state_mut().priority_player = player;
        runner.state_mut().waiting_for = WaitingFor::Priority { player };
        runner.cast(source).resolve();
        assert!(runner.state().objects[&source].tapped);
        u5_complete(&runner);
        let epoch = runner.state().objects[&source].incarnation;
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        let WaitingFor::PayCost {
            player: actor,
            kind,
            choices,
            count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("activation must ask for its real sacrifice cost")
        };
        assert_eq!((*actor, *count), (player, 1));
        assert_eq!(*kind, PayCostKind::Sacrifice);
        assert_eq!(
            choices
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            [first, chosen].into_iter().collect()
        );
        for illegal in [source, other_source, foreign] {
            let before = serde_json::to_string(runner.state()).unwrap();
            assert!(runner
                .act(GameAction::SelectCards {
                    cards: vec![illegal]
                })
                .is_err());
            assert_eq!(serde_json::to_string(runner.state()).unwrap(), before);
        }
        runner
            .act(GameAction::SelectCards {
                cards: vec![chosen],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&chosen].zone, Zone::Graveyard);
        assert_eq!(runner.state().objects[&first].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&source].incarnation, epoch);
        assert!(!runner.state().objects[&source].tapped);
        assert_eq!(
            runner.state().objects[&source]
                .counters
                .get(&engine::types::counter::CounterType::Plus1Plus1)
                .copied(),
            Some(1)
        );
        assert!(runner.state().objects[&other_source].tapped);
        assert!(runner.state().objects[&other_source].counters.is_empty());
        u5_complete(&runner);
    }
}

#[test]
fn u5_leviathan_replacement_modified_exact_payment_settles_every_member() {
    for replacements in [0usize, 1, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        let source = scenario.add_real_card(P0, "Leviathan", Zone::Battlefield, u5_database());
        let islands = (0..3)
            .map(|_| scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()))
            .collect::<Vec<_>>();
        let authorities = (0..replacements)
            .map(|_| scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, u5_database()))
            .collect::<Vec<_>>();
        for id in &authorities {
            assert_eq!(
                u5_database()
                    .get_face_by_name("Rest in Peace")
                    .unwrap()
                    .replacements
                    .len(),
                1
            );
            assert_ne!(*id, source);
        }
        let mut runner = scenario.build();
        runner.advance_to_upkeep();
        runner.resolve_top();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ResolutionOptionalPaymentChoice { player: P0, source_id, ref costs } if source_id == source && costs.len() == 1 && costs[0].index == 0)
        );
        runner
            .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
            })
            .unwrap();
        let selected = vec![islands[2], islands[1]];
        let mut settled_events = runner
            .act(GameAction::SelectCards {
                cards: selected.clone(),
            })
            .unwrap()
            .events;
        if replacements == 2 {
            for (member, remaining) in [(islands[2], islands[1]), (islands[1], islands[0])] {
                let WaitingFor::ReplacementChoice {
                    player, candidates, ..
                } = &runner.state().waiting_for
                else {
                    panic!("two real Rest in Peace sources require order")
                };
                assert_eq!(*player, P0);
                assert_eq!(
                    candidates
                        .iter()
                        .map(|candidate| candidate.source_id)
                        .collect::<std::collections::BTreeSet<_>>(),
                    authorities.iter().copied().collect()
                );
                let pending = runner.state().pending_replacement.as_ref().unwrap();
                assert_eq!(pending.candidates.len(), 2);
                let selected_replacement = pending.candidates[1];
                assert_eq!(selected_replacement.source, candidates[1].source_id);
                assert!(!pending.proposed.already_applied(&selected_replacement));
                assert!(pending.proposed.applied_set().is_empty());
                assert!(
                    runner.state().objects[&source].tapped,
                    "no payoff before complete batch settlement"
                );
                assert_eq!(runner.state().objects[&member].zone, Zone::Battlefield);
                assert_eq!(runner.state().objects[&remaining].zone, Zone::Battlefield);
                assert!(runner.state().pending_cost_move_resume.is_some());
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                settled_events.extend(result.events.iter().cloned());
                assert_eq!(result.events.iter().filter(|event| matches!(event, engine::types::events::GameEvent::ReplacementApplied { source_id, .. } if *source_id == selected_replacement.source)).count(), 1);
                assert_eq!(runner.state().objects[&member].zone, Zone::Exile);
            }
        }
        let expected = if replacements == 0 {
            Zone::Graveyard
        } else {
            Zone::Exile
        };
        for id in selected {
            assert_eq!(runner.state().objects[&id].zone, expected);
            assert_eq!(settled_events.iter().filter(|event| matches!(event, engine::types::events::GameEvent::PermanentSacrificed { object_id, player_id: P0 } if *object_id == id)).count(), 1, "selected member settles exactly once");
        }
        assert_eq!(settled_events.iter().filter(|event| matches!(event, engine::types::events::GameEvent::PermanentUntapped { object_id } if *object_id == source)).count(), 1);
        assert_eq!(runner.state().objects[&islands[0]].zone, Zone::Battlefield);
        assert!(
            !runner.state().objects[&source].tapped,
            "replacement-modified payment is still paid"
        );
        assert!(runner.state().pending_cost_move_resume.is_none());
        assert!(runner.state().active_optional_effect_frame().is_none());
        assert!(runner.state().pending_replacement.is_none());
        u5_complete(&runner);
    }
}

#[test]
fn u5_traxos_actual_historic_cast_union_resolves_above_paid_spell_and_survives_counter() {
    for (name, mana) in [
        ("Memnite", vec![]),
        ("Isamaru, Hound of Konda", vec![ManaType::White]),
        ("Hope of Ghirapur", vec![ManaType::Colorless]),
        (
            "History of Benalia",
            vec![ManaType::Colorless, ManaType::White, ManaType::White],
        ),
    ] {
        for counter in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let source =
                scenario.add_real_card(P0, "Traxos, Scourge of Kroog", Zone::Hand, u5_database());
            let spell = scenario.add_real_card(P0, name, Zone::Hand, u5_database());
            let counterspell =
                scenario.add_real_card(P1, "Counterspell", Zone::Hand, u5_database());
            let mut source_and_spell_mana = vec![ManaType::Colorless; 4];
            source_and_spell_mana.extend(mana.iter().copied());
            u5_fund(&mut scenario, P0, &source_and_spell_mana);
            u5_fund(&mut scenario, P1, &[ManaType::Blue, ManaType::Blue]);
            let mut runner = scenario.build();
            runner.cast(source).resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert!(runner.state().objects[&source].tapped);
            u5_complete(&runner);
            runner.cast(spell).commit();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
            assert!(runner.state().players[0].mana_pool.is_empty());
            assert_eq!(
                runner.state().stack.len(),
                2,
                "historic union triggers exactly once: {name}"
            );
            assert_eq!(runner.state().stack[0].source_id, spell);
            assert_eq!(runner.state().stack[1].source_id, source);
            assert!(matches!(
                runner.state().stack[1].kind,
                engine::types::game_state::StackEntryKind::TriggeredAbility { .. }
            ));
            runner.resolve_top();
            assert!(!runner.state().objects[&source].tapped);
            assert_eq!(
                runner.state().stack.len(),
                1,
                "the paid spell is deliberately parked"
            );
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(runner.state().resolution_stack.is_empty());
            if counter {
                runner.act(GameAction::PassPriority).unwrap();
                runner.cast(counterspell).target_object(spell).resolve();
                assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
                assert_eq!(runner.state().objects[&counterspell].zone, Zone::Graveyard);
                assert!(runner.state().players[1].mana_pool.is_empty());
            } else {
                runner.advance_until_stack_empty();
                assert_eq!(runner.state().objects[&spell].zone, Zone::Battlefield);
                if name == "History of Benalia" {
                    assert_eq!(
                        runner.state().objects[&spell]
                            .counters
                            .get(&engine::types::counter::CounterType::Lore)
                            .copied(),
                        Some(1)
                    );
                    assert!(runner
                        .state()
                        .objects
                        .values()
                        .any(|object| object.controller == P0
                            && object.zone == Zone::Battlefield
                            && object
                                .card_types
                                .subtypes
                                .iter()
                                .any(|subtype| subtype == "Knight")));
                }
            }
            assert!(
                !runner.state().objects[&source].tapped,
                "cast trigger is independent of countered spell"
            );
            u5_complete(&runner);
        }
    }
    // Nonhistoric actual CAST and legendary LAND PLAY share the same live
    // Traxos producer as the successful historic cases above.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_real_card(
        P0,
        "Traxos, Scourge of Kroog",
        Zone::Battlefield,
        u5_database(),
    );
    let bear = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, u5_database());
    let land = scenario.add_real_card(P0, "Flagstones of Trokair", Zone::Hand, u5_database());
    let historic = scenario.add_real_card(P0, "Memnite", Zone::Hand, u5_database());
    u5_fund(&mut scenario, P0, &[ManaType::Colorless, ManaType::Green]);
    let mut runner = scenario.build();
    runner.cast(bear).commit();
    assert_eq!(runner.state().stack.len(), 1);
    runner.advance_until_stack_empty();
    assert!(runner.state().objects[&source].tapped);
    u5_complete(&runner);
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .unwrap();
    assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
    assert!(runner.state().objects[&source].tapped);
    u5_complete(&runner);
    runner.cast(historic).commit();
    assert_eq!(runner.state().stack.len(), 2);
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&source].tapped);
    u5_complete(&runner);
}

#[test]
fn u5_traxos_show_and_tell_placement_is_not_a_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_real_card(
        P0,
        "Traxos, Scourge of Kroog",
        Zone::Battlefield,
        u5_database(),
    );
    let decoy = scenario.add_real_card(P0, "Grizzly Bears", Zone::Hand, u5_database());
    let placed = scenario.add_real_card(P0, "Memnite", Zone::Hand, u5_database());
    let cast_later = scenario.add_real_card(P0, "Hope of Ghirapur", Zone::Hand, u5_database());
    let show = scenario.add_real_card(P0, "Show and Tell", Zone::Hand, u5_database());
    scenario.add_real_card(P1, "Plains", Zone::Hand, u5_database());
    u5_fund(
        &mut scenario,
        P0,
        &[
            ManaType::Colorless,
            ManaType::Colorless,
            ManaType::Blue,
            ManaType::Colorless,
        ],
    );
    let mut runner = scenario.build();
    runner.cast(show).commit().stop_at_optional().resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    assert!(runner.state().objects[&source].tapped);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    let WaitingFor::EffectZoneChoice {
        player,
        cards,
        count,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("Show and Tell's real hand selection")
    };
    assert_eq!((*player, *count), (P0, 1));
    assert_eq!(
        cards
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [decoy, placed, cast_later].into_iter().collect()
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![placed],
        })
        .unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert_eq!(runner.state().objects[&placed].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&decoy].zone, Zone::Hand);
    assert!(runner.state().objects[&source].tapped);
    u5_complete(&runner);
    runner.cast(cast_later).commit();
    assert_eq!(runner.state().stack.len(), 2);
    runner.advance_until_stack_empty();
    assert!(!runner.state().objects[&source].tapped);
    u5_complete(&runner);
}

#[test]
fn u5_source_produced_if_continues_and_when_creates_a_real_reflexive_stack() {
    // Constructed grammar controls supplement the full printed Leviathan and
    // Ziatora sources. No cost, trigger, result or frame is installed by hand.
    for when in [false, true] {
        for population in [0usize, 1] {
            for accept in [false, true] {
                let connector = if when { "When" } else { "If" };
                let oracle = format!("At the beginning of your upkeep, you may sacrifice an Island. {connector} you do, draw a card.");
                let mut scenario = GameScenario::new();
                scenario.at_phase(Phase::Untap);
                let source = scenario
                    .add_enchantment_from_oracle(P0, "Constructed Payment Grammar", &oracle)
                    .id();
                let island = if population == 1 {
                    Some(scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()))
                } else {
                    None
                };
                scenario.with_library_top(P0, &["Drawn Grammar Card", "Library Guard"]);
                let mut runner = scenario.build();
                let hand_before = runner.state().players[0].hand.len();
                runner.advance_to_upkeep();
                assert_eq!(runner.state().stack.len(), 1);
                assert_eq!(runner.state().stack[0].source_id, source);
                runner.resolve_top();
                if let Some(island) = island {
                    assert!(
                        matches!(runner.state().waiting_for, WaitingFor::ResolutionOptionalPaymentChoice { player: P0, ref costs, .. } if costs.len() == 1 && costs[0].index == 0)
                    );
                    runner
                        .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                            choice: if accept {
                                ResolutionOptionalPaymentChoice::Pay { index: 0 }
                            } else {
                                ResolutionOptionalPaymentChoice::Decline
                            },
                        })
                        .unwrap();
                    if accept {
                        assert!(matches!(
                            runner.state().waiting_for,
                            WaitingFor::PayCost {
                                player: P0,
                                kind: PayCostKind::Sacrifice,
                                count: 1,
                                min_count: 1,
                                ..
                            }
                        ));
                        runner
                            .act(GameAction::SelectCards {
                                cards: vec![island],
                            })
                            .unwrap();
                        assert_eq!(runner.state().objects[&island].zone, Zone::Graveyard);
                        if when {
                            assert!(matches!(
                                runner.state().waiting_for,
                                WaitingFor::Priority { .. }
                            ));
                            assert_eq!(
                                runner.state().stack.len(),
                                1,
                                "When creates an actual separate triggered ability"
                            );
                            assert!(runner.state().resolution_stack.is_empty());
                            assert_eq!(
                                runner.state().players[0].hand.len(),
                                hand_before,
                                "reflexive child has not resolved"
                            );
                            runner.resolve_top();
                        }
                        assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
                    } else {
                        assert_eq!(runner.state().objects[&island].zone, Zone::Battlefield);
                    }
                }
                if population == 0 || !accept {
                    assert_eq!(runner.state().players[0].hand.len(), hand_before);
                }
                u5_complete(&runner);
            }
        }
    }
}

#[test]
fn u5_constructed_when_targets_are_selected_only_after_real_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario.add_enchantment_from_oracle(P0, "Constructed Reflexive Target Grammar", "At the beginning of your upkeep, you may sacrifice an Island. When you do, put a +1/+1 counter on target creature.").id();
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database());
    let first = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, u5_database());
    let chosen = scenario.add_real_card(P1, "Llanowar Elves", Zone::Battlefield, u5_database());
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().stack.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    runner.resolve_top();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ResolutionOptionalPaymentChoice { source_id, .. } if source_id == source)
    );
    runner
        .act(GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        })
        .unwrap();
    runner
        .act(GameAction::SelectCards {
            cards: vec![island],
        })
        .unwrap();
    let WaitingFor::TriggerTargetSelection {
        player,
        source_id,
        target_slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("actual reflexive target announcement")
    };
    assert_eq!((*player, *source_id), (P0, Some(source)));
    assert_eq!(target_slots.len(), 1);
    assert_eq!(
        target_slots[0].legal_targets,
        vec![
            engine::types::ability::TargetRef::Object(first),
            engine::types::ability::TargetRef::Object(chosen)
        ]
    );
    assert!(runner.state().resolution_stack.is_empty());
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(chosen)),
        })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().objects[&chosen].counters.is_empty());
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&chosen]
            .counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .copied(),
        Some(1)
    );
    assert!(runner.state().objects[&first].counters.is_empty());
    u5_complete(&runner);
}

#[test]
fn u5_leviathan_real_bounce_blink_and_control_change_preserve_original_trigger_authority() {
    for interaction in ["Unsummon", "Momentary Blink", "Dominate"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        let source = scenario.add_real_card(P0, "Leviathan", Zone::Battlefield, u5_database());
        let islands = [
            scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()),
            scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()),
        ];
        let foreign = [
            scenario.add_real_card(P1, "Island", Zone::Battlefield, u5_database()),
            scenario.add_real_card(P1, "Island", Zone::Battlefield, u5_database()),
        ];
        let caster = if interaction == "Dominate" { P1 } else { P0 };
        let spell = scenario.add_real_card(caster, interaction, Zone::Hand, u5_database());
        let mut runner = scenario.build();
        runner.advance_to_upkeep();
        assert_eq!(runner.state().stack.len(), 1);
        let trigger_id = runner.state().stack[0].id;
        let old_epoch = runner.state().objects[&source].incarnation;
        // Mana is funded after the actual untap boundary, before announcement.
        let mana = match interaction {
            "Unsummon" => vec![ManaType::Blue],
            "Momentary Blink" => vec![ManaType::Colorless, ManaType::White],
            _ => {
                let mut mana = vec![ManaType::Colorless; 10];
                mana.extend([ManaType::Blue, ManaType::Blue]);
                mana
            }
        };
        for kind in mana {
            runner.state_mut().players[caster.0 as usize]
                .mana_pool
                .add(ManaUnit::new(kind, source, false, vec![]));
        }
        if caster == P1 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        if interaction == "Dominate" {
            runner.cast(spell).x(9).target_object(source).commit();
        } else {
            runner.cast(spell).target_object(source).commit();
        }
        assert_eq!(runner.state().stack.len(), 2);
        runner.resolve_top();
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack[0].id, trigger_id);
        assert_eq!(runner.state().stack[0].controller, P0);
        assert!(runner.state().players[caster.0 as usize]
            .mana_pool
            .is_empty());
        if interaction == "Unsummon" {
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&source].owner, P0);
        } else {
            assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
            assert!(runner.state().objects[&source].tapped);
            if interaction == "Momentary Blink" {
                assert!(runner.state().objects[&source].incarnation > old_epoch);
            } else {
                assert_eq!(runner.state().objects[&source].incarnation, old_epoch);
                assert_eq!(runner.state().objects[&source].controller, P1);
                assert_eq!(runner.state().objects[&source].owner, P0);
            }
        }
        runner.resolve_top();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ResolutionOptionalPaymentChoice { player: P0, source_id, ref costs } if source_id == source && costs.len() == 1)
        );
        runner
            .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
            })
            .unwrap();
        let WaitingFor::PayCost {
            player,
            choices,
            count,
            min_count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("original payer's pool")
        };
        assert_eq!((*player, *count, *min_count), (P0, 2, 2));
        assert_eq!(
            choices
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            islands.into_iter().collect()
        );
        assert!(runner
            .act(GameAction::SelectCards {
                cards: foreign.to_vec()
            })
            .is_err());
        runner
            .act(GameAction::SelectCards {
                cards: islands.to_vec(),
            })
            .unwrap();
        for id in islands {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        for id in foreign {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        match interaction {
            "Momentary Blink" => assert!(
                runner.state().objects[&source].tapped,
                "old trigger cannot untap a new incarnation"
            ),
            "Dominate" => assert!(
                !runner.state().objects[&source].tapped,
                "same incarnation still untaps under new controller"
            ),
            _ => assert_eq!(runner.state().objects[&source].zone, Zone::Hand),
        }
        u5_complete(&runner);
    }
}

#[test]
fn u5_paid_entry_zero_one_and_multiple_replacement_authorities() {
    for population in [0usize, 1, 2, 3] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Due Respect Draw", "Library Guard"]);
        let name = if population == 0 {
            "Memnite"
        } else {
            "Traxos, Scourge of Kroog"
        };
        let entrant = scenario.add_real_card(P0, name, Zone::Hand, u5_database());
        let external = if population >= 2 {
            Some(scenario.add_real_card(P1, "Frozen Aether", Zone::Battlefield, u5_database()))
        } else {
            None
        };
        let floating = if population == 3 {
            Some(scenario.add_real_card(P0, "Due Respect", Zone::Hand, u5_database()))
        } else {
            None
        };
        let mut mana = vec![ManaType::Colorless; if population == 0 { 0 } else { 4 }];
        if floating.is_some() {
            mana.extend([ManaType::Colorless, ManaType::White]);
        }
        u5_fund(&mut scenario, P0, &mana);
        let mut runner = scenario.build();
        if let Some(spell) = floating {
            runner.cast(spell).resolve();
            assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
            assert_eq!(runner.state().pending_damage_replacements.len(), 1);
            u5_complete(&runner);
        }
        runner.cast(entrant).commit();
        runner.resolve_top();
        let mut applied = Vec::new();
        if population >= 2 {
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = &runner.state().waiting_for
            else {
                panic!("real entry replacement ordering")
            };
            assert_eq!(*player, P0);
            let expected = [Some(entrant), external, floating]
                .into_iter()
                .flatten()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                candidates
                    .iter()
                    .map(|candidate| candidate.source_id)
                    .collect::<std::collections::BTreeSet<_>>(),
                expected
            );
            assert_ne!(runner.state().objects[&entrant].zone, Zone::Battlefield);
            // The nondefault last candidate comes from the actual stored offer.
            let chosen_index = candidates.len() - 1;
            let pending = runner.state().pending_replacement.as_ref().unwrap();
            let selected = pending.candidates[chosen_index];
            if selected.source == ObjectId(0) {
                let definition = &runner.state().pending_damage_replacements[selected.index];
                assert_eq!(
                    definition.source_object,
                    Some(candidates[chosen_index].source_id)
                );
                assert_eq!(definition.source_controller, Some(P0));
            } else {
                assert_eq!(selected.source, candidates[chosen_index].source_id);
            }
            let expected_storage = [Some(entrant), external, floating.map(|_| ObjectId(0))]
                .into_iter()
                .flatten()
                .collect::<std::collections::BTreeSet<_>>();
            assert!(!pending.proposed.already_applied(&selected));
            let result = runner
                .act(GameAction::ChooseReplacement {
                    index: chosen_index,
                })
                .unwrap();
            applied.extend(result.events.iter().filter_map(|event| {
                if let engine::types::events::GameEvent::ReplacementApplied { source_id, .. } =
                    event
                {
                    Some(*source_id)
                } else {
                    None
                }
            }));
            if population == 3 {
                let WaitingFor::ReplacementChoice {
                    player, candidates, ..
                } = &runner.state().waiting_for
                else {
                    panic!("remaining entry order")
                };
                assert_eq!(*player, P0);
                assert_eq!(candidates.len(), 2);
                let pending = runner.state().pending_replacement.as_ref().unwrap();
                assert!(pending.proposed.already_applied(&selected));
                assert!(!pending.candidates.contains(&selected));
                assert_eq!(pending.proposed.applied_set().len(), 1);
                assert!(pending
                    .candidates
                    .iter()
                    .all(|id| !pending.proposed.already_applied(id)));
                let result = runner
                    .act(GameAction::ChooseReplacement { index: 1 })
                    .unwrap();
                applied.extend(result.events.iter().filter_map(|event| {
                    if let engine::types::events::GameEvent::ReplacementApplied {
                        source_id, ..
                    } = event
                    {
                        Some(*source_id)
                    } else {
                        None
                    }
                }));
            }
            assert_eq!(applied.len(), population);
            assert_eq!(
                applied
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                expected_storage
            );
        }
        assert_eq!(runner.state().objects[&entrant].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&entrant].tapped, population != 0);
        assert!(runner.state().pending_replacement.is_none());
        u5_complete(&runner);
    }
}

#[test]
fn u5_neighboring_full_lands_keep_shock_conditional_and_counter_entry_semantics() {
    for external in [false, true] {
        for accept in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let land = scenario.add_real_card(P0, "Hallowed Fountain", Zone::Hand, u5_database());
            if external {
                scenario.add_real_card(P1, "Frozen Aether", Zone::Battlefield, u5_database());
            }
            let mut runner = scenario.build();
            let card_id = runner.state().objects[&land].card_id;
            runner
                .act(GameAction::PlayLand {
                    object_id: land,
                    card_id,
                })
                .unwrap();
            if external {
                let WaitingFor::ReplacementChoice {
                    player, candidates, ..
                } = &runner.state().waiting_for
                else {
                    panic!("shock plus external order")
                };
                assert_eq!(*player, P0);
                assert_eq!(candidates.len(), 2);
                let index = candidates
                    .iter()
                    .position(|candidate| candidate.source_id == land)
                    .unwrap();
                runner.act(GameAction::ChooseReplacement { index }).unwrap();
            }
            let WaitingFor::ReplacementChoice {
                player, candidates, ..
            } = &runner.state().waiting_for
            else {
                panic!("actual shock-land life choice")
            };
            assert_eq!(*player, P0);
            assert_eq!(candidates.len(), 2);
            runner
                .act(GameAction::ChooseReplacement {
                    index: if accept { 0 } else { 1 },
                })
                .unwrap();
            assert_eq!(runner.state().objects[&land].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&land].tapped, external || !accept);
            assert_eq!(runner.life(P0), if accept { 18 } else { 20 });
            u5_complete(&runner);
        }
    }
    for prerequisite in [None, Some("Plains"), Some("Island"), Some("Swamp")] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        if let Some(name) = prerequisite {
            scenario.add_real_card(P0, name, Zone::Battlefield, u5_database());
        }
        let land = scenario.add_real_card(P0, "Glacial Fortress", Zone::Hand, u5_database());
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&land].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .unwrap();
        assert_eq!(
            runner.state().objects[&land].tapped,
            !matches!(prerequisite, Some("Plains" | "Island"))
        );
        u5_complete(&runner);
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land = scenario.add_real_card(P0, "Vivid Creek", Zone::Hand, u5_database());
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&land].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: land,
            card_id,
        })
        .unwrap();
    assert!(runner.state().objects[&land].tapped);
    assert_eq!(
        runner.state().objects[&land]
            .counters
            .get(&engine::types::counter::CounterType::Generic(
                "charge".into()
            ))
            .copied(),
        Some(2)
    );
    u5_complete(&runner);
}

#[test]
fn u5_source_removed_before_upkeep_suppresses_only_its_own_trigger() {
    for remove in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::End);
        let source = scenario.add_real_card(P0, "Leviathan", Zone::Battlefield, u5_database());
        let bounce = scenario.add_real_card(P1, "Unsummon", Zone::Hand, u5_database());
        scenario.with_library_top(P0, &["Next Draw", "Guard"]);
        u5_fund(&mut scenario, P1, &[ManaType::Blue]);
        let mut runner = scenario.build();
        runner.state_mut().active_player = P1;
        runner.state_mut().priority_player = P1;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
        if remove {
            runner.cast(bounce).target_object(source).resolve();
            assert_eq!(runner.state().objects[&source].zone, Zone::Hand);
            u5_complete(&runner);
        }
        runner.advance_to_upkeep();
        assert_eq!(runner.state().active_player, P0);
        assert_eq!(runner.state().phase, Phase::Upkeep);
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .filter(|entry| entry.source_id == source)
                .count(),
            usize::from(!remove)
        );
        runner.advance_until_stack_empty();
        assert_eq!(
            runner.state().objects[&source].zone,
            if remove {
                Zone::Hand
            } else {
                Zone::Battlefield
            }
        );
        u5_complete(&runner);
    }
}

#[test]
fn u5_grimgrin_and_traxos_old_abilities_cannot_affect_blinked_incarnations() {
    for grimgrin in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let name = if grimgrin {
            "Grimgrin, Corpse-Born"
        } else {
            "Traxos, Scourge of Kroog"
        };
        let source = scenario.add_real_card(P0, name, Zone::Battlefield, u5_database());
        let fodder = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, u5_database());
        let memnite = scenario.add_real_card(P0, "Memnite", Zone::Hand, u5_database());
        let blink = scenario.add_real_card(P0, "Momentary Blink", Zone::Hand, u5_database());
        u5_fund(&mut scenario, P0, &[ManaType::Colorless, ManaType::White]);
        let mut runner = scenario.build();
        let old_epoch = runner.state().objects[&source].incarnation;
        if grimgrin {
            runner
                .act(GameAction::ActivateAbility {
                    payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                    source_id: source,
                    ability_index: 0,
                })
                .unwrap();
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::PayCost {
                    kind: PayCostKind::Sacrifice,
                    ..
                }
            ));
            runner
                .act(GameAction::SelectCards {
                    cards: vec![fodder],
                })
                .unwrap();
            assert_eq!(runner.state().stack.len(), 1);
        } else {
            runner.cast(memnite).commit();
            assert_eq!(runner.state().stack.len(), 2);
        }
        runner.cast(blink).target_object(source).commit();
        runner.resolve_top();
        assert!(runner.state().objects[&source].incarnation > old_epoch);
        assert!(runner.state().objects[&source].tapped);
        let new_epoch = runner.state().objects[&source].incarnation;
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&source].incarnation, new_epoch);
        assert!(runner.state().objects[&source].tapped);
        assert!(runner.state().objects[&source].counters.is_empty());
        u5_complete(&runner);
    }
}

#[test]
fn u5_two_real_leviathan_triggers_keep_distinct_sources_and_single_payment_occurrences() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let first = scenario.add_real_card(P0, "Leviathan", Zone::Battlefield, u5_database());
    let second = scenario.add_real_card(P0, "Leviathan", Zone::Battlefield, u5_database());
    let islands = [
        scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()),
        scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()),
    ];
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!("two real upkeep sources must preserve their ordering authority")
    };
    assert_eq!(*player, P0);
    assert_eq!(triggers.len(), 2);
    runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .unwrap();
    assert_eq!(runner.state().stack.len(), 2);
    let paid_source = runner.state().stack.last().unwrap().source_id;
    let other_source = if paid_source == first { second } else { first };
    runner.resolve_top();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ResolutionOptionalPaymentChoice { source_id, .. } if source_id == paid_source)
    );
    runner
        .act(GameAction::ChooseResolutionOptionalPaymentBranch {
            choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
        })
        .unwrap();
    runner
        .act(GameAction::SelectCards {
            cards: islands.to_vec(),
        })
        .unwrap();
    assert!(!runner.state().objects[&paid_source].tapped);
    assert!(runner.state().objects[&other_source].tapped);
    assert_eq!(runner.state().stack.len(), 1);
    assert!(runner.state().resolution_stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    runner.resolve_top();
    assert!(
        runner.state().objects[&other_source].tapped,
        "the other occurrence cannot reuse already paid Islands"
    );
    u5_complete(&runner);
}

#[test]
fn u5_real_dominate_before_upkeep_uses_current_controller_turn_and_owned_payment_pool() {
    for blink in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::End);
        scenario.with_library_top(P0, &["P0 Draw", "Guard"]);
        scenario.with_library_top(P1, &["P1 Draw", "Guard"]);
        let source = scenario.add_real_card(P0, "Leviathan", Zone::Battlefield, u5_database());
        let control = scenario.add_real_card(P1, "Dominate", Zone::Hand, u5_database());
        let blink_spell = scenario.add_real_card(P1, "Momentary Blink", Zone::Hand, u5_database());
        let old_controller_pool = [
            scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()),
            scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database()),
        ];
        let actual_pool = [
            scenario.add_real_card(P1, "Island", Zone::Battlefield, u5_database()),
            scenario.add_real_card(P1, "Island", Zone::Battlefield, u5_database()),
        ];
        let mut mana = vec![ManaType::Colorless; 10];
        mana.extend([ManaType::Blue, ManaType::Blue]);
        u5_fund(&mut scenario, P1, &mana);
        let mut runner = scenario.build();
        runner.state_mut().active_player = P1;
        runner.state_mut().priority_player = P1;
        runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
        runner.cast(control).x(9).target_object(source).resolve();
        assert_eq!(runner.state().objects[&source].owner, P0);
        assert_eq!(runner.state().objects[&source].controller, P1);
        u5_complete(&runner);
        runner.advance_to_upkeep();
        assert_eq!(runner.state().active_player, P0);
        assert!(runner.state().stack.is_empty());
        u5_complete(&runner);
        runner.advance_to_combat();
        runner.declare_attackers(&[]).unwrap();
        runner.advance_to_end_step();
        runner.advance_to_upkeep();
        assert_eq!(runner.state().active_player, P1);
        assert_eq!(runner.state().stack.len(), 1);
        assert_eq!(runner.state().stack[0].controller, P1);
        let trigger_epoch = runner.state().objects[&source].incarnation;
        if blink {
            for kind in [ManaType::Colorless, ManaType::White] {
                runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
                    kind,
                    source,
                    false,
                    vec![],
                ));
            }
            runner.cast(blink_spell).target_object(source).commit();
            runner.resolve_top();
            assert_eq!(runner.state().stack.len(), 1);
            assert_eq!(runner.state().stack[0].controller, P1);
            assert_eq!(
                runner.state().objects[&source].controller,
                P0,
                "Blink returns under its owner's control"
            );
            assert_eq!(runner.state().objects[&source].owner, P0);
            assert!(runner.state().objects[&source].incarnation > trigger_epoch);
            assert!(runner.state().objects[&source].tapped);
        }
        runner.resolve_top();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ResolutionOptionalPaymentChoice { player: P1, source_id, .. } if source_id == source)
        );
        runner
            .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
            })
            .unwrap();
        let WaitingFor::PayCost {
            choices, player, ..
        } = &runner.state().waiting_for
        else {
            panic!("new controller's cost pool")
        };
        assert_eq!(*player, P1);
        assert_eq!(
            choices
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            actual_pool.into_iter().collect()
        );
        runner
            .act(GameAction::SelectCards {
                cards: actual_pool.to_vec(),
            })
            .unwrap();
        for id in old_controller_pool {
            assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        }
        assert_eq!(
            runner.state().objects[&source].tapped,
            blink,
            "only the original incarnation may be untapped"
        );
        u5_complete(&runner);
    }
}

#[test]
fn u5_same_body_if_reads_the_actual_outer_dies_snapshot_after_its_own_cost() {
    for counters in [0, 2] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let observer = scenario.add_enchantment_from_oracle(P0, "Constructed Same Body Authority", "Whenever a creature dies, you may sacrifice an Island. If you do, draw a card. You gain 3 life if it had a +1/+1 counter on it.").id();
        let grimgrin = scenario.add_real_card(
            P0,
            "Grimgrin, Corpse-Born",
            Zone::Battlefield,
            u5_database(),
        );
        let victim = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, u5_database());
        let decoy = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, u5_database());
        let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database());
        scenario.with_counter(
            victim,
            engine::types::counter::CounterType::Plus1Plus1,
            counters,
        );
        scenario.with_library_top(P0, &["Actual Draw", "Library Guard"]);
        let mut runner = scenario.build();
        let hand = runner.state().players[0].hand.len();
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: grimgrin,
                ability_index: 0,
            })
            .unwrap();
        let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for else {
            panic!("real initial sacrifice")
        };
        assert!(choices.contains(&victim) && choices.contains(&decoy));
        runner
            .act(GameAction::SelectCards {
                cards: vec![victim],
            })
            .unwrap();
        assert_eq!(runner.state().stack.len(), 2);
        assert_eq!(runner.state().stack.last().unwrap().source_id, observer);
        runner.resolve_top();
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ResolutionOptionalPaymentChoice { source_id, .. } if source_id == observer)
        );
        runner
            .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Pay { index: 0 },
            })
            .unwrap();
        runner
            .act(GameAction::SelectCards {
                cards: vec![island],
            })
            .unwrap();
        assert_eq!(runner.state().players[0].hand.len(), hand + 1);
        assert_eq!(
            runner.life(P0),
            if counters == 0 { 20 } else { 23 },
            "the outer creature's snapshot, not the newly paid Island, owns the predicate"
        );
        assert_eq!(runner.state().stack.len(), 1);
        assert!(runner.state().resolution_stack.is_empty());
        runner.advance_until_stack_empty();
        u5_complete(&runner);
    }
}

#[test]
fn u5_genuine_oneof_original_indexes_and_ordinary_nonpayment_sacrifice_remain_distinct() {
    for index in [0usize, 1] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        scenario.add_enchantment_from_oracle(P0, "Constructed Genuine Alternatives", "At the beginning of your upkeep, you may sacrifice an Island or pay {2}. If you do, draw a card.");
        let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database());
        scenario.with_library_top(P0, &["Alternative Draw", "Guard"]);
        let mut runner = scenario.build();
        runner.advance_to_upkeep();
        for _ in 0..2 {
            runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
                ManaType::Colorless,
                island,
                false,
                vec![],
            ));
        }
        runner.resolve_top();
        let WaitingFor::ResolutionOptionalPaymentChoice { costs, .. } = &runner.state().waiting_for
        else {
            panic!("old genuine OneOf")
        };
        assert_eq!(
            costs.iter().map(|cost| cost.index).collect::<Vec<_>>(),
            vec![0, 1]
        );
        runner
            .act(GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: ResolutionOptionalPaymentChoice::Pay { index },
            })
            .unwrap();
        if index == 0 {
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::PayCost {
                    kind: PayCostKind::Sacrifice,
                    ..
                }
            ));
            runner
                .act(GameAction::SelectCards {
                    cards: vec![island],
                })
                .unwrap();
        }
        assert_eq!(
            runner.state().objects[&island].zone,
            if index == 0 {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            }
        );
        assert_eq!(runner.state().players[0].hand.len(), 1);
        u5_complete(&runner);
    }
    // Constructed ordinary instruction, with no affirmative connector: CR 609.3
    // still permits doing as much as possible. It must not enter exact payment.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_enchantment_from_oracle(
        P0,
        "Constructed Ordinary Sacrifice",
        "At the beginning of your upkeep, you may sacrifice two Islands.",
    );
    let island = scenario.add_real_card(P0, "Island", Zone::Battlefield, u5_database());
    let mut runner = scenario.build();
    runner.advance_to_upkeep();
    runner.resolve_top();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    assert_eq!(runner.state().objects[&island].zone, Zone::Graveyard);
    u5_complete(&runner);
}

#[test]
fn u5_full_grimgrin_attack_retains_defending_targets_and_counter_rulings() {
    for disposition in ["none", "legal", "illegal", "indestructible"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario.add_real_card(
            P0,
            "Grimgrin, Corpse-Born",
            Zone::Battlefield,
            u5_database(),
        );
        let fodder = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, u5_database());
        let own_decoy =
            scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, u5_database());
        let victim = if disposition == "none" {
            None
        } else {
            scenario.add_real_card(P1, "Llanowar Elves", Zone::Battlefield, u5_database());
            Some(if disposition == "indestructible" {
                // A constructed rules fixture, rather than altered printed Oracle.
                scenario
                    .add_creature(P1, "Constructed Indestructible Defender", 2, 2)
                    .indestructible()
                    .id()
            } else {
                scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, u5_database())
            })
        };
        let bounce = scenario.add_real_card(P1, "Unsummon", Zone::Hand, u5_database());
        let mut runner = scenario.build();
        runner
            .act(GameAction::ActivateAbility {
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                source_id: source,
                ability_index: 0,
            })
            .unwrap();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                ..
            }
        ));
        runner
            .act(GameAction::SelectCards {
                cards: vec![fodder],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert!(!runner.state().objects[&source].tapped);
        assert_eq!(
            runner.state().objects[&source]
                .counters
                .get(&engine::types::counter::CounterType::Plus1Plus1)
                .copied(),
            Some(1)
        );
        u5_complete(&runner);
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(source, engine::game::combat::AttackTarget::Player(P1))])
            .unwrap();
        if let Some(victim) = victim {
            let WaitingFor::TriggerTargetSelection {
                player,
                source_id,
                target_slots,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("full Grimgrin's defending-player target")
            };
            assert_eq!((*player, *source_id), (P0, Some(source)));
            assert_eq!(target_slots.len(), 1);
            assert_eq!(target_slots[0].legal_targets.len(), 2);
            assert!(target_slots[0]
                .legal_targets
                .contains(&engine::types::ability::TargetRef::Object(victim)));
            assert!(!target_slots[0]
                .legal_targets
                .contains(&engine::types::ability::TargetRef::Object(own_decoy)));
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Object(victim)),
                })
                .unwrap();
            assert_eq!(runner.state().stack.len(), 1);
            if disposition == "illegal" {
                runner.state_mut().players[1].mana_pool.add(ManaUnit::new(
                    ManaType::Blue,
                    bounce,
                    false,
                    vec![],
                ));
                runner.act(GameAction::PassPriority).unwrap();
                runner.cast(bounce).target_object(victim).commit();
                runner.resolve_top();
                assert_eq!(runner.state().objects[&victim].zone, Zone::Hand);
                assert_eq!(runner.state().stack.len(), 1);
            }
            runner.resolve_top();
            assert_eq!(
                runner.state().objects[&victim].zone,
                match disposition {
                    "illegal" => Zone::Hand,
                    "indestructible" => Zone::Battlefield,
                    _ => Zone::Graveyard,
                }
            );
        } else {
            assert!(runner.state().stack.is_empty());
        }
        assert_eq!(
            runner.state().objects[&source]
                .counters
                .get(&engine::types::counter::CounterType::Plus1Plus1)
                .copied(),
            Some(if matches!(disposition, "legal" | "indestructible") {
                2
            } else {
                1
            })
        );
        assert_eq!(runner.state().objects[&own_decoy].zone, Zone::Battlefield);
        u5_complete(&runner);
    }
}

#[test]
fn u5_existing_tap_and_recursive_composite_payments_keep_their_public_paths() {
    for composite in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Untap);
        let cost = if composite {
            "tap an untapped creature you control, tap an untapped artifact you control"
        } else {
            "tap two untapped creatures you control"
        };
        let oracle =
            format!("At the beginning of your upkeep, you may {cost}. If you do, draw a card.");
        scenario.add_enchantment_from_oracle(P0, "Constructed Old Tap Grammar", &oracle);
        let first = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, u5_database());
        let second = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, u5_database());
        let artifact = scenario.add_real_card(P0, "Memnite", Zone::Battlefield, u5_database());
        scenario.with_library_top(P0, &["Old Tap Draw", "Guard"]);
        let mut runner = scenario.build();
        runner.advance_to_upkeep();
        runner.resolve_top();
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ));
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .unwrap();
        let WaitingFor::PayCost {
            player,
            kind,
            choices,
            count,
            min_count,
            ..
        } = &runner.state().waiting_for
        else {
            panic!("old tap payment selector")
        };
        assert_eq!(*player, P0);
        assert!(matches!(kind, PayCostKind::TapCreatures { .. }));
        assert_eq!(
            (*count, *min_count),
            if composite { (1, 1) } else { (2, 2) }
        );
        assert!(choices.contains(&second) && choices.contains(&artifact));
        runner
            .act(GameAction::SelectCards {
                cards: if composite {
                    vec![second]
                } else {
                    vec![artifact, second]
                },
            })
            .unwrap();
        if composite {
            let WaitingFor::PayCost {
                player,
                kind,
                choices,
                count,
                min_count,
                ..
            } = &runner.state().waiting_for
            else {
                panic!("second old composite leaf")
            };
            assert_eq!((*player, *count, *min_count), (P0, 1, 1));
            assert!(matches!(kind, PayCostKind::TapCreatures { .. }));
            assert_eq!(*choices, vec![artifact]);
            runner
                .act(GameAction::SelectCards {
                    cards: vec![artifact],
                })
                .unwrap();
        }
        assert!(runner.state().objects[&second].tapped && runner.state().objects[&artifact].tapped);
        assert!(!runner.state().objects[&first].tapped);
        assert_eq!(runner.state().players[0].hand.len(), 1);
        u5_complete(&runner);
    }
}
