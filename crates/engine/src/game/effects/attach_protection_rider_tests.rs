//! CR 702.16n + CR 702.16p: protection-rider provenance at the attachment
//! boundary. Hosts and Auras are printed cards (Grizzly Bears, Black Knight,
//! Flickering Ward, White Ward, Benevolent Blessing, Pacifism, Humility,
//! Demystify) cast through the production pipeline. Every grant no printed
//! card produces is built by hand, named "Synthetic …", and its comment names
//! the nearest printed analogue. The "doesn't remove Auras" rider is synthetic
//! wherever it appears: its one printed card, Spectra Ward, grants protection
//! from "each color", a quality the engine does not yet match against colored
//! sources, so it cannot isolate the rider.

use super::tests::{
    apply_protection_grant, evaluate_protection_layers, printed_snapshot_ids,
    protection_snapshot_ids, spawn_equipment, spawn_grant_source,
};
use super::*;
use crate::database::card_db::CardDatabase;
use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
use crate::game::scenario_db::GameScenarioDbExt;
use crate::game::zones::create_object;
use crate::types::ability::{
    ChosenAttribute, ContinuousModification, Duration, ProtectionDoesNotRemove, StaticDefinition,
    TargetFilter, TypedFilter,
};
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::{PersistedGameState, PersistedRestoreFinalization, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{Keyword, ProtectionTarget};
use crate::types::layers::ProtectionGrantOccurrence;
use crate::types::mana::{ManaColor, ManaType, ManaUnit};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn db() -> &'static CardDatabase {
    crate::test_support::shared_card_db()
}

fn scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Give `player` priority and enough mana of every type to cast the printed
/// spells below.
fn prepare(runner: &mut GameRunner, player: PlayerId) {
    assert!(runner.state().stack.is_empty());
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
    for mana in [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ] {
        for _ in 0..6 {
            state
                .add_mana_to_pool(player, ManaUnit::new(mana, ObjectId(0), false, vec![]))
                .unwrap();
        }
    }
}

/// Cast a printed spell at `target` and let the stack settle. `color` is the
/// answer to the spell's "As this Aura enters, choose a color" prompt: when it
/// is given the prompt is required, and the chosen color must be recorded on
/// the permanent; when it is not, no prompt may appear.
fn cast(
    runner: &mut GameRunner,
    player: PlayerId,
    spell: ObjectId,
    target: Option<ObjectId>,
    color: Option<&str>,
) {
    prepare(runner, player);
    let cast = runner.cast(spell);
    let cast = match target {
        Some(target) => cast.target_object(target),
        None => cast,
    };
    let outcome = cast.resolve();
    match (color, outcome.final_waiting_for()) {
        (
            Some(wanted),
            WaitingFor::NamedChoice {
                player: chooser,
                options,
                ..
            },
        ) => {
            assert_eq!(*chooser, player, "the caster chooses the color");
            let choice = options
                .iter()
                .find(|option| option.eq_ignore_ascii_case(wanted))
                .expect("the color is offered")
                .clone();
            runner
                .act(GameAction::ChooseOption { choice })
                .expect("the color choice resolves");
        }
        (Some(_), other) => panic!("the spell must ask a color, got {other:?}"),
        (None, WaitingFor::NamedChoice { .. }) => panic!("unexpected color prompt"),
        (None, _) => {}
    }
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
    if let Some(wanted) = color {
        let chosen = runner.state().objects[&spell]
            .chosen_color()
            .expect("the chosen color is recorded");
        assert!(format!("{chosen:?}").eq_ignore_ascii_case(wanted));
    }
}

fn attached(runner: &GameRunner, aura: ObjectId, host: ObjectId) {
    assert_eq!(runner.state().objects[&aura].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&aura].attached_to, Some(host.into()));
}

/// Printed Grizzly Bears ({1}{G}, 2/2, no rules text): a host with no
/// protection, hexproof, shroud or ward of its own.
fn grizzly_bears(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    let bears = scenario.add_real_card(player, "Grizzly Bears", Zone::Battlefield, db());
    let face = db().get_face_by_name("Grizzly Bears").unwrap();
    assert_eq!(face.oracle_text, None, "reach guard: a vanilla host");
    bears
}

/// A white Aura with no printed counterpart, for grants no printed card makes.
fn spawn_synthetic_white_aura(
    state: &mut GameState,
    name: &str,
    card_id: u64,
    controller: PlayerId,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    obj.color.push(ManaColor::White);
    id
}

fn spawn_synthetic_white_equipment(state: &mut GameState, card_id: u64) -> ObjectId {
    let id = spawn_equipment(state, "Synthetic white Equipment", card_id);
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .color
        .push(ManaColor::White);
    id
}

/// Unit fixtures push a static onto `static_definitions` directly; a full
/// pass reseeds that set from `base_static_definitions`, so mirror it when
/// the fixture is created after the source was already initialized.
fn persist_last_static(state: &mut GameState, source_id: ObjectId) {
    let defs: Vec<_> = state.objects[&source_id]
        .static_definitions
        .iter_all()
        .cloned()
        .collect();
    state
        .objects
        .get_mut(&source_id)
        .unwrap()
        .base_static_definitions = std::sync::Arc::new(defs);
}

/// Synthetic quoted static "`host_id` has protection from white" with
/// `rider`. No printed card quotes a ridered protection static; its printed
/// analogues are Flickering Ward (`Source`), Spectra Ward (`Auras`) and
/// Benevolent Blessing (`ControlledAttachmentsAlreadyAttached`).
fn quoted_white_protection(host_id: ObjectId, rider: ProtectionDoesNotRemove) -> StaticDefinition {
    StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: host_id })
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Protection(ProtectionTarget::Color(ManaColor::White)),
        }])
        .protection_does_not_remove(rider)
}

/// `granter`'s synthetic static grants `inner` to every object matching
/// `recipients`.
fn grant_static_to(
    state: &mut GameState,
    granter: ObjectId,
    recipients: TargetFilter,
    inner: StaticDefinition,
) {
    state
        .objects
        .get_mut(&granter)
        .unwrap()
        .static_definitions
        .push(
            StaticDefinition::continuous()
                .affected(recipients)
                .modifications(vec![ContinuousModification::GrantStaticAbility {
                    definition: Box::new(inner),
                }]),
        );
    persist_last_static(state, granter);
}

fn white_riders(
    state: &GameState,
    host_id: ObjectId,
) -> Vec<Option<crate::game::game_object::ProtectionRiderGrant>> {
    state.objects[&host_id]
        .protection_provenance
        .iter()
        .filter(|entry| entry.quality == ProtectionTarget::Color(ManaColor::White))
        .map(|entry| entry.rider.clone())
        .collect()
}

/// Index of the printed static that carries `object`'s rider.
fn ridered_static_index(state: &GameState, object: ObjectId) -> usize {
    state.objects[&object]
        .static_definitions
        .iter_all()
        .position(|def| def.protection_does_not_remove.is_some())
        .expect("reach guard: the printed rider static")
}

/// CR 702.16p + CR 613.7: Benevolent Blessing ("This effect doesn't remove
/// Auras and Equipment you control that are already attached to it.")
/// snapshots its controller's attachments when its effect starts. Humility
/// ("All creatures lose all abilities…"), cast later, removes the resulting
/// protection, but the effect never stopped applying, so the snapshot survives.
/// A second Pacifism attaches while Humility is out; once Demystify ("Destroy
/// target enchantment.") destroys Humility, protection from white returns and
/// exempts only the Pacifism attached when the effect started, so the late one
/// is put into its owner's graveyard (CR 704.5m).
#[test]
fn cr_702_16p_snapshot_survives_later_ability_removal() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let pacifism = s.add_real_card(P0, "Pacifism", Zone::Hand, db());
    let blessing = s.add_real_card(P0, "Benevolent Blessing", Zone::Hand, db());
    let humility = s.add_real_card(P0, "Humility", Zone::Hand, db());
    let late_pacifism = s.add_real_card(P0, "Pacifism", Zone::Hand, db());
    let demystify = s.add_real_card(P0, "Demystify", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, pacifism, Some(host), None);
    cast(&mut runner, P0, blessing, Some(host), Some("white"));
    attached(&runner, pacifism, host);
    attached(&runner, blessing, host);
    let def_index = ridered_static_index(runner.state(), blessing);
    let snapshot = printed_snapshot_ids(runner.state(), blessing, def_index, 0, host);
    assert!(
        snapshot.contains(&pacifism),
        "reach guard: the white Aura P0 already controlled is snapshotted at effect start"
    );
    assert_eq!(
        attachment_illegality(runner.state(), pacifism, host),
        None,
        "the snapshotted Aura stays legal"
    );

    cast(&mut runner, P0, humility, None, None);
    assert!(runner.state().objects[&host]
        .keywords
        .iter()
        .all(|k| !matches!(k, Keyword::Protection(_))));
    assert!(
        runner.state().objects[&host]
            .protection_provenance
            .is_empty(),
        "the surviving layer result carries no protection instance"
    );
    assert_eq!(
        printed_snapshot_ids(runner.state(), blessing, def_index, 0, host),
        snapshot,
        "the effect never stopped applying, so its start snapshot must survive"
    );

    cast(&mut runner, P0, late_pacifism, Some(host), None);
    attached(&runner, late_pacifism, host);
    cast(&mut runner, P0, demystify, Some(humility), None);
    assert_eq!(runner.state().objects[&humility].zone, Zone::Graveyard);
    assert!(
        runner.state().objects[&host]
            .keywords
            .contains(&Keyword::Protection(ProtectionTarget::Color(
                ManaColor::White
            ))),
        "reach guard: protection from white returns once Humility is gone"
    );
    attached(&runner, pacifism, host);
    assert_eq!(
        runner.state().objects[&late_pacifism].zone,
        Zone::Graveyard,
        "the Pacifism attached after the effect started is not exempt"
    );
}

/// CR 109.5 + CR 702.16p: "you" in Benevolent Blessing's rider ("This effect
/// doesn't remove Auras and Equipment you control that are already attached
/// to it.") is Blessing's current controller. P1 casts Steal Enchantment
/// ("Enchant enchantment / You control enchanted enchantment.") on P0's
/// Blessing, then Blessing moves to another Grizzly Bears, where its effect
/// starts applying anew and takes P1's attachments, not P0's. Synthetic: the
/// move is a direct attach standing in for Enchantment Alteration ("Attach
/// target Aura attached to a creature or land to another permanent of that
/// type."), which is not in the test card fixture.
#[test]
fn stolen_benevolent_blessing_snapshots_its_new_controllers_attachments() {
    let mut s = scenario();
    let first_host = grizzly_bears(&mut s, P0);
    let second_host = grizzly_bears(&mut s, P1);
    let p0_pacifism = s.add_real_card(P0, "Pacifism", Zone::Hand, db());
    let p1_pacifism = s.add_real_card(P1, "Pacifism", Zone::Hand, db());
    let blessing = s.add_real_card(P0, "Benevolent Blessing", Zone::Hand, db());
    let steal = s.add_real_card(P1, "Steal Enchantment", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, p0_pacifism, Some(second_host), None);
    cast(&mut runner, P1, p1_pacifism, Some(second_host), None);
    cast(&mut runner, P0, blessing, Some(first_host), Some("white"));
    cast(&mut runner, P1, steal, Some(blessing), None);
    assert_eq!(
        runner.state().objects[&blessing].controller,
        P1,
        "reach guard: Steal Enchantment took Blessing"
    );

    attach_to(runner.state_mut(), blessing, second_host);
    attached(&runner, blessing, second_host);
    let state = runner.state();
    assert_eq!(
        attachment_illegality(state, p1_pacifism, second_host),
        None,
        "the new controller's Aura was already attached"
    );
    assert_eq!(
        attachment_illegality(state, blessing, second_host),
        None,
        "Blessing is the new controller's own Aura"
    );
    assert_eq!(
        attachment_illegality(state, p0_pacifism, second_host),
        Some(AttachIllegality::Protection),
        "the former controller's Aura is not exempt"
    );
}

/// CR 702.26b + CR 702.26c + CR 702.16p: a phased-out permanent is treated as
/// though it does not exist, so Benevolent Blessing's effect stops applying
/// while it and its creature are phased out, and starts again (with a new
/// snapshot) when they phase in. Printed cards: Benevolent Blessing (white
/// chosen) and Bonesplitter (colorless, so not snapshotted) on Grizzly Bears;
/// Reality Ripple ("Target artifact, creature, or land phases out.") phases them
/// out; Painter's Servant ("…permanents are the chosen color…", white) makes
/// Bonesplitter white. When they phase in, Bonesplitter is white and already
/// attached, so the new snapshot keeps it. Synthetic: the phase-in is called
/// directly, standing in for P0's next untap step.
#[test]
fn phasing_in_starts_a_new_start_snapshot() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let bonesplitter = s.add_real_card(P0, "Bonesplitter", Zone::Battlefield, db());
    let blessing = s.add_real_card(P0, "Benevolent Blessing", Zone::Hand, db());
    let ripple = s.add_real_card(P0, "Reality Ripple", Zone::Hand, db());
    let painter = s.add_real_card(P0, "Painter's Servant", Zone::Hand, db());
    let mut runner = s.build();
    prepare(&mut runner, P0);
    runner
        .activate(bonesplitter, 0)
        .target_object(host)
        .resolve();
    runner.advance_until_stack_empty();
    attached(&runner, bonesplitter, host);
    cast(&mut runner, P0, blessing, Some(host), Some("white"));
    attached(&runner, blessing, host);

    cast(&mut runner, P0, ripple, Some(host), None);
    assert!(
        !runner.state().objects[&host].is_phased_in_permanent(),
        "reach guard: Reality Ripple phased the Bears out"
    );
    cast(&mut runner, P0, painter, None, Some("white"));

    let mut events = Vec::new();
    crate::game::phasing::phase_in_object(runner.state_mut(), host, &mut events);
    crate::game::layers::mark_layers_full(runner.state_mut());
    crate::game::layers::flush_layers(runner.state_mut());
    let state = runner.state();
    assert!(state.objects[&host].is_phased_in_permanent());
    attached(&runner, bonesplitter, host);
    assert!(
        state.objects[&bonesplitter]
            .color
            .contains(&ManaColor::White),
        "reach guard: Painter's Servant made Bonesplitter white"
    );
    assert_eq!(
        attachment_illegality(state, bonesplitter, host),
        None,
        "Blessing's effect started again with Bonesplitter already attached"
    );
}

/// CR 702.16m + CR 702.16n ("If the creature has other instances of
/// protection from the same quality, those instances affect Auras as
/// normal."): Black Knight's printed protection from white and Flickering
/// Ward's ridered grant of white are separate instances. Ward is cast while
/// Humility removes the printed instance; once Demystify destroys Humility,
/// the printed instance returns and the state-based action removes Ward, which
/// its own rider exempts only from its own instance.
#[test]
fn printed_protection_blocks_own_rider_aura_after_layers() {
    let mut s = scenario();
    let knight = s.add_real_card(P0, "Black Knight", Zone::Battlefield, db());
    let humility = s.add_real_card(P0, "Humility", Zone::Battlefield, db());
    let ward = s.add_real_card(P0, "Flickering Ward", Zone::Hand, db());
    let demystify = s.add_real_card(P0, "Demystify", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, ward, Some(knight), Some("white"));
    attached(&runner, ward, knight);
    // Reach guard: under Humility the only white instance is Ward's own grant,
    // carrying Ward's own rider; Black Knight's printed protection is gone.
    assert_eq!(
        white_riders(runner.state(), knight),
        vec![Some(crate::game::game_object::ProtectionRiderGrant {
            source: ward,
            occurrence: ProtectionGrantOccurrence::Static {
                definition_index: ridered_static_index(runner.state(), ward),
            },
            mod_index: 0,
            does_not_remove: ProtectionDoesNotRemove::Source,
        })],
        "Ward-only white protection before Humility leaves"
    );
    assert_eq!(
        attachment_illegality(runner.state(), ward, knight),
        None,
        "Ward's rider exempts Ward from its own instance"
    );

    cast(&mut runner, P0, demystify, Some(humility), None);
    assert_eq!(runner.state().objects[&humility].zone, Zone::Graveyard);
    assert!(crate::game::keywords::has_keyword(
        &runner.state().objects[&knight],
        &Keyword::Protection(ProtectionTarget::Color(ManaColor::White))
    ));
    // REVERT-PROBE: drop the rider-less synthesis from
    // `record_protection_provenance` ⇒ only Ward's exempting instance is
    // recorded, Ward stays attached, and this fails.
    assert_eq!(
        runner.state().objects[&ward].zone,
        Zone::Graveyard,
        "the printed instance must remove the ridered Aura"
    );
}

/// CR 702.16n: a quoted granted static's protection carries its own
/// "doesn't remove Auras" rider through the synthesized child. The quoted
/// grant is synthetic (no printed card quotes it; Spectra Ward prints the
/// rider directly); the host is Grizzly Bears and the Aura is Pacifism.
#[test]
fn synthesized_granted_protection_keeps_its_auras_rider() {
    let (runner, host, pacifism) = synthesized_grant_board(Some(ProtectionDoesNotRemove::Auras));
    assert_eq!(
        attachment_illegality(runner.state(), pacifism, host),
        None,
        "the synthesized child's Auras rider must exempt the attachment"
    );
}

/// Baseline for the synthesized-child test: without the rider the same quoted
/// protection blocks Pacifism.
#[test]
fn synthesized_granted_protection_without_rider_blocks_aura() {
    let (runner, host, pacifism) = synthesized_grant_board(None);
    assert_eq!(
        attachment_illegality(runner.state(), pacifism, host),
        Some(AttachIllegality::Protection),
        "the riderless baseline must block the Aura"
    );
}

/// Grizzly Bears enchanted by an opponent's Pacifism, then a synthetic quoted
/// grant "Grizzly Bears has protection from white" with `rider`.
fn synthesized_grant_board(
    rider: Option<ProtectionDoesNotRemove>,
) -> (GameRunner, ObjectId, ObjectId) {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let pacifism = s.add_real_card(P1, "Pacifism", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P1, pacifism, Some(host), None);
    attached(&runner, pacifism, host);

    let state = runner.state_mut();
    let source = spawn_grant_source(state, "Synthetic quoted-grant source", 80);
    let mut inner = StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: host })
        .modifications(vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Protection(ProtectionTarget::Color(ManaColor::White)),
        }]);
    if let Some(rider) = rider {
        inner = inner.protection_does_not_remove(rider);
    }
    grant_static_to(
        state,
        source,
        TargetFilter::SpecificObject { id: host },
        inner,
    );
    evaluate_protection_layers(state);
    assert!(
        crate::game::keywords::has_keyword(
            &state.objects[&host],
            &Keyword::Protection(ProtectionTarget::Color(ManaColor::White))
        ),
        "reach guard: the synthesized child is the only producer"
    );
    (runner, host, pacifism)
}

/// How a quoted ridered static reaches its recipient.
#[derive(Clone, Copy, Debug)]
enum GrantPath {
    /// The granter's own static grants it at gather time.
    Static,
    /// A resolution-created effect grants it (CR 611.2c); the parent has no
    /// static-definition index.
    Transient,
    /// The recipient joins the granter's affected set only after layer 4
    /// (an Aura creature), so its child comes from the late-grant path.
    Late,
}

/// The objects of [`granted_rider_board`].
struct RiderBoard {
    host: ObjectId,
    recipient: ObjectId,
    granter: ObjectId,
    equipment: ObjectId,
    opposing_aura: ObjectId,
}

/// Grizzly Bears enchanted first by an opponent's Pacifism, then by two
/// synthetic white Auras and a synthetic white Equipment of P0, all attached
/// before the grant starts. The recipient carries its own synthetic static
/// first, so the granted copy's slot on the recipient differs from the
/// granter's static's slot; the snapshot follows neither, only the grant
/// occurrence. `path` grants the recipient the synthetic quoted static
/// "Grizzly Bears has protection from white" with `rider`.
fn granted_rider_board(
    path: GrantPath,
    rider: ProtectionDoesNotRemove,
) -> (GameRunner, RiderBoard) {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let opposing_aura = s.add_real_card(P1, "Pacifism", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P1, opposing_aura, Some(host), None);
    attached(&runner, opposing_aura, host);

    let state = runner.state_mut();
    let recipient = spawn_synthetic_white_aura(state, "Synthetic recipient Aura", 86, P0);
    let granter = spawn_synthetic_white_aura(state, "Synthetic granting Aura", 87, P0);
    let equipment = spawn_synthetic_white_equipment(state, 88);
    for attachment in [recipient, granter, equipment] {
        attach_to(state, attachment, host);
    }
    state
        .objects
        .get_mut(&recipient)
        .unwrap()
        .static_definitions
        .push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: recipient })
                .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
        );
    persist_last_static(state, recipient);

    let inner = quoted_white_protection(host, rider);
    match path {
        GrantPath::Static => grant_static_to(
            state,
            granter,
            TargetFilter::SpecificObject { id: recipient },
            inner,
        ),
        GrantPath::Transient => {
            state
                .add_transient_continuous_effect(
                    granter,
                    P0,
                    Duration::UntilEndOfTurn,
                    TargetFilter::SpecificObject { id: recipient },
                    vec![ContinuousModification::GrantStaticAbility {
                        definition: Box::new(inner),
                    }],
                    None,
                )
                .expect("transient grant must register");
        }
        GrantPath::Late => {
            grant_static_to(
                state,
                granter,
                TargetFilter::Typed(TypedFilter::creature().subtype("Aura".to_string())),
                inner,
            );
            state
                .add_transient_continuous_effect(
                    granter,
                    P0,
                    Duration::UntilEndOfTurn,
                    TargetFilter::SpecificObject { id: recipient },
                    vec![ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    }],
                    None,
                )
                .expect("animation must register");
        }
    }
    evaluate_protection_layers(state);
    (
        runner,
        RiderBoard {
            host,
            recipient,
            granter,
            equipment,
            opposing_aura,
        },
    )
}

/// CR 702.16n + CR 702.16p + CR 113.7: a quoted granted static's rider
/// belongs to the object that has the static (the recipient), not to the
/// permanent that granted it, on every grant path. Printed analogues of the
/// riders: Flickering Ward ("This effect doesn't remove this Aura."), Spectra
/// Ward ("This effect doesn't remove Auras.") and Benevolent Blessing ("This
/// effect doesn't remove Auras and Equipment you control that are already
/// attached to it."). The synthesized child is the only producer of the
/// protection in each board.
fn assert_granted_rider_identity(path: GrantPath) {
    let protection = Some(AttachIllegality::Protection);
    for rider in [
        ProtectionDoesNotRemove::Source,
        ProtectionDoesNotRemove::Auras,
        ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached,
    ] {
        let (runner, board) = granted_rider_board(path, rider.clone());
        let state = runner.state();
        let riders = white_riders(state, board.host);
        assert_eq!(riders.len(), 1, "{path:?} {rider:?}: one granted instance");
        let grant = riders[0].as_ref().expect("the granted rider survives");
        assert_eq!(grant.does_not_remove, rider);
        assert_eq!(
            grant.source, board.recipient,
            "{path:?} {rider:?}: the granted static's source is its recipient"
        );
        let legality = |attachment| attachment_illegality(state, attachment, board.host);
        let expected = match &rider {
            // Only "this Aura": the recipient, never the granting Aura.
            ProtectionDoesNotRemove::Source => [None, protection, protection, protection],
            // Every Aura, including the opponent's Pacifism, but no Equipment.
            ProtectionDoesNotRemove::Auras => [None, None, protection, None],
            // The recipient's start snapshot for this grant occurrence: what
            // its controller (P0) had attached, not P1's Pacifism.
            ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached => {
                assert!(
                    matches!(grant.occurrence, ProtectionGrantOccurrence::Granted { .. }),
                    "{path:?}: a granted static is identified by its grant occurrence"
                );
                assert_eq!(
                    protection_snapshot_ids(
                        state,
                        board.recipient,
                        grant.occurrence.clone(),
                        grant.mod_index,
                        board.host
                    ),
                    vec![board.recipient, board.granter, board.equipment],
                    "{path:?}: the recipient holds the grant's start snapshot"
                );
                [None, None, None, protection]
            }
        };
        assert_eq!(
            [
                legality(board.recipient),
                legality(board.granter),
                legality(board.equipment),
                legality(board.opposing_aura),
            ],
            expected,
            "{path:?} {rider:?}: [recipient, granter, equipment, Pacifism]"
        );
    }
}

#[test]
fn statically_granted_rider_names_its_recipient() {
    assert_granted_rider_identity(GrantPath::Static);
}

#[test]
fn transiently_granted_rider_names_its_recipient() {
    assert_granted_rider_identity(GrantPath::Transient);
}

#[test]
fn late_granted_rider_names_its_recipient() {
    assert_granted_rider_identity(GrantPath::Late);
}

/// CR 702.16n + CR 702.16p: a ridered static on the protected permanent itself
/// keys its rider to that permanent's own definition. The static is synthetic:
/// printed riders appear only on Auras (Flickering Ward, Spectra Ward,
/// Benevolent Blessing). "This Aura" then names the creature, which no
/// attachment can be, so Pacifism is still blocked; the other riders exempt as
/// they do from an Aura.
#[test]
fn own_static_rider_uses_its_own_identity() {
    let protection = Some(AttachIllegality::Protection);
    for rider in [
        ProtectionDoesNotRemove::Source,
        ProtectionDoesNotRemove::Auras,
        ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached,
    ] {
        let mut s = scenario();
        let host = grizzly_bears(&mut s, P0);
        let pacifism = s.add_real_card(P1, "Pacifism", Zone::Hand, db());
        let mut runner = s.build();
        cast(&mut runner, P1, pacifism, Some(host), None);
        let state = runner.state_mut();
        let equipment = spawn_synthetic_white_equipment(state, 103);
        attach_to(state, equipment, host);
        apply_protection_grant(
            state,
            host,
            host,
            ProtectionTarget::Color(ManaColor::White),
            Some(rider.clone()),
        );
        persist_last_static(state, host);
        evaluate_protection_layers(state);

        let riders = white_riders(state, host);
        assert_eq!(riders.len(), 1, "{rider:?}: one instance");
        let grant = riders[0].as_ref().expect("the rider survives");
        assert_eq!(grant.source, host);
        assert_eq!(
            grant.occurrence,
            ProtectionGrantOccurrence::Static {
                definition_index: 0
            }
        );
        let expected = match &rider {
            ProtectionDoesNotRemove::Source => [protection, protection],
            ProtectionDoesNotRemove::Auras => [None, protection],
            // P1's Pacifism is not controlled by the static's controller (P0).
            ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached => [protection, None],
        };
        assert_eq!(
            [
                attachment_illegality(state, pacifism, host),
                attachment_illegality(state, equipment, host),
            ],
            expected,
            "{rider:?}: [Pacifism, Equipment]"
        );
    }
}

/// CR 707.2 + CR 702.16n: a copy of Flickering Ward acquires its ridered static
/// as its own, so "this Aura" is the copy. The copy effect is synthetic (the
/// printed analogue is an enchantment entering as a copy, e.g. Copy
/// Enchantment); a copy doesn't acquire the original's as-enters choice, so it
/// records its own (white). The copy on one Grizzly Bears stays legal through
/// its own rider, independent of the original on another.
#[test]
fn copy_of_ridered_aura_carries_its_own_rider() {
    let mut s = scenario();
    let original_host = grizzly_bears(&mut s, P0);
    let copy_host = grizzly_bears(&mut s, P0);
    let ward = s.add_real_card(P0, "Flickering Ward", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, ward, Some(original_host), Some("white"));
    attached(&runner, ward, original_host);

    let state = runner.state_mut();
    let ward_values = crate::game::printed_cards::intrinsic_copiable_values(&state.objects[&ward]);
    let copy = spawn_synthetic_white_aura(state, "Synthetic copy", 99, P0);
    attach_to(state, copy, copy_host);
    state
        .objects
        .get_mut(&copy)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::White));
    state
        .add_transient_continuous_effect(
            copy,
            P0,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: copy },
            vec![ContinuousModification::CopyValues {
                values: Box::new(ward_values),
                display: Default::default(),
            }],
            None,
        )
        .expect("copy effect must register");
    evaluate_protection_layers(state);

    assert_eq!(state.objects[&copy].name, "Flickering Ward", "reach guard");
    let riders = white_riders(state, copy_host);
    assert_eq!(riders.len(), 1, "reach guard: the copy's own instance");
    assert_eq!(riders[0].as_ref().map(|rider| rider.source), Some(copy));
    assert_eq!(attachment_illegality(state, copy, copy_host), None);
    assert_eq!(attachment_illegality(state, ward, original_host), None);
}

/// The provenance list is not serialized. Every persisted restore, immediate
/// or deferred until runtime rehydration (WASM `restore_game_state`,
/// `resume_multiplayer_host_state` and the server session restore all defer),
/// finalizes with a layer flush, so the restored state carries the exact
/// ridered instance before any action reads attachment legality. White Ward:
/// "Enchanted creature has protection from white. This effect doesn't remove
/// this Aura." On Grizzly Bears its own instance is the only one, so the rider
/// alone keeps the white Aura attached.
#[test]
fn persisted_restore_rebuilds_the_rider_before_any_action() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let ward = s.add_real_card(P0, "White Ward", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, ward, Some(host), None);
    attached(&runner, ward, host);
    let state = runner.state();
    let expected = state.objects[&host].protection_provenance.clone();
    let riders = white_riders(state, host);
    assert_eq!(riders.len(), 1, "reach guard: White Ward's instance only");
    let rider = riders[0].as_ref().expect("White Ward's rider");
    assert_eq!(rider.source, ward);
    assert_eq!(rider.does_not_remove, ProtectionDoesNotRemove::Source);
    assert_eq!(
        attachment_illegality(state, ward, host),
        None,
        "rider-only reach: White Ward's own rider keeps it attached"
    );

    let raw: GameState = serde_json::from_str(&serde_json::to_string(state).unwrap()).unwrap();
    assert!(
        raw.objects[&host].protection_provenance.is_empty(),
        "the list is not part of the wire"
    );

    for policy in [
        PersistedRestoreFinalization::Immediate,
        PersistedRestoreFinalization::DeferUntilRehydrated,
    ] {
        let wire = serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap();
        let prepared = serde_json::from_value::<PersistedGameState>(wire)
            .unwrap()
            .prepare_for_restore(policy)
            .unwrap();
        let restored = match policy {
            PersistedRestoreFinalization::Immediate => prepared.finalize_immediately(),
            PersistedRestoreFinalization::DeferUntilRehydrated => {
                prepared.finalize_after_rehydration(|_| Ok(()))
            }
        }
        .unwrap();
        assert_eq!(
            restored.objects[&host].protection_provenance, expected,
            "{policy:?}: the restore boundary rebuilds the exact rider"
        );
        assert_eq!(attachment_illegality(&restored, ward, host), None);
    }
}

/// CR 702.16m + CR 702.16n: a later chosen-keyword grant of the same quality is
/// its own surviving instance, recorded even though Flickering Ward's grant
/// already put protection from white in the keyword list. With only Ward's
/// grant, Ward is legal on Grizzly Bears; the independent grant changes that.
/// The chosen-keyword grant is synthetic: no printed card grants a chosen
/// protection keyword (`AddChosenKeyword`'s printed user is Greymond, Avacyn's
/// Stalwart, with first strike, vigilance or lifelink).
#[test]
fn chosen_grant_changes_rider_only_attachment_legality() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let ward = s.add_real_card(P0, "Flickering Ward", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, ward, Some(host), Some("white"));
    attached(&runner, ward, host);
    assert_eq!(
        attachment_illegality(runner.state(), ward, host),
        None,
        "rider-only reach: Ward's grant exempts Ward"
    );

    let state = runner.state_mut();
    let chooser = spawn_grant_source(state, "Synthetic chosen-protection source", 78);
    {
        let obj = state.objects.get_mut(&chooser).unwrap();
        obj.chosen_attributes
            .push(ChosenAttribute::Keyword(Keyword::Protection(
                ProtectionTarget::Color(ManaColor::White),
            )));
        obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: host })
                .modifications(vec![ContinuousModification::AddChosenKeyword]),
        );
    }
    persist_last_static(state, chooser);
    evaluate_protection_layers(state);
    assert_eq!(
        state.objects[&host].protection_provenance.len(),
        2,
        "the chosen grant adds an independent rider-less instance"
    );
    assert_eq!(
        attachment_illegality(state, ward, host),
        Some(AttachIllegality::Protection),
        "the independent instance changes attachment legality"
    );
}

/// The objects of [`start_identity_board`].
struct StartIdentityBoard {
    host: ObjectId,
    recipient: ObjectId,
    white_equipment: ObjectId,
    later_white_equipment: ObjectId,
}

/// Grizzly Bears with two synthetic Equipment of P0 attached: one white, one
/// colorless (it turns white only after a grant starts), and a synthetic
/// colorless Aura of P0 that later grants carry their quoted statics to (an
/// Aura, so the late-grant path can select it once it becomes a creature).
fn start_identity_board() -> (GameRunner, StartIdentityBoard) {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let mut runner = s.build();
    let state = runner.state_mut();
    let white_equipment = spawn_synthetic_white_equipment(state, 110);
    let later_white_equipment = spawn_equipment(state, "Synthetic colorless Equipment", 111);
    for equipment in [white_equipment, later_white_equipment] {
        attach_to(state, equipment, host);
    }
    let recipient = create_object(
        state,
        CardId(112),
        P0,
        "Synthetic recipient Aura".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&recipient).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    (
        runner,
        StartIdentityBoard {
            host,
            recipient,
            white_equipment,
            later_white_equipment,
        },
    )
}

/// A synthetic granter whose static gives `board.recipient` the synthetic
/// quoted Benevolent Blessing analogue: "Grizzly Bears has protection from
/// white. This effect doesn't remove Auras and Equipment you control that are
/// already attached to it."
fn start_blessing_grant(
    state: &mut GameState,
    board: &StartIdentityBoard,
    card_id: u64,
) -> ObjectId {
    let granter = spawn_grant_source(state, "Synthetic granting source", card_id);
    grant_static_to(
        state,
        granter,
        TargetFilter::SpecificObject {
            id: board.recipient,
        },
        quoted_white_protection(
            board.host,
            ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached,
        ),
    );
    granter
}

/// [`start_blessing_grant`] by `path`: a static's grant, a resolution-created
/// grant, or a late grant the recipient joins in layer 4 (it becomes an Aura
/// creature through a synthetic animation).
fn start_blessing_grant_by(
    state: &mut GameState,
    board: &StartIdentityBoard,
    card_id: u64,
    path: GrantPath,
) {
    let inner = quoted_white_protection(
        board.host,
        ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached,
    );
    match path {
        GrantPath::Static => {
            start_blessing_grant(state, board, card_id);
        }
        GrantPath::Transient => {
            let granter = spawn_grant_source(state, "Synthetic granting source", card_id);
            state
                .add_transient_continuous_effect(
                    granter,
                    P0,
                    Duration::UntilEndOfTurn,
                    TargetFilter::SpecificObject {
                        id: board.recipient,
                    },
                    vec![ContinuousModification::GrantStaticAbility {
                        definition: Box::new(inner),
                    }],
                    None,
                )
                .expect("transient grant must register");
        }
        GrantPath::Late => {
            let granter = spawn_grant_source(state, "Synthetic granting source", card_id);
            grant_static_to(
                state,
                granter,
                TargetFilter::Typed(TypedFilter::creature().subtype("Aura".to_string())),
                inner,
            );
            state
                .add_transient_continuous_effect(
                    granter,
                    P0,
                    Duration::UntilEndOfTurn,
                    TargetFilter::SpecificObject {
                        id: board.recipient,
                    },
                    vec![ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    }],
                    None,
                )
                .expect("animation must register");
        }
    }
}

/// A synthetic static with no protection: `recipient` gets +1/+0.
fn unrelated_static(recipient: ObjectId) -> StaticDefinition {
    StaticDefinition::continuous()
        .affected(TargetFilter::SpecificObject { id: recipient })
        .modifications(vec![ContinuousModification::AddPower { value: 1 }])
}

/// Synthetic stand-in for an effect that changes an attachment's color after
/// a grant started (the case `cr_702_16p_does_not_exempt_attachment_that_becomes_matching_after_grant_start`
/// covers for a fixed slot).
fn turn_white(state: &mut GameState, object: ObjectId) {
    let obj = state.objects.get_mut(&object).unwrap();
    obj.base_color = vec![ManaColor::White];
    obj.color = vec![ManaColor::White];
}

/// The grant occurrences of the "already attached" protection on `host`.
fn already_attached_occurrences(
    state: &GameState,
    host: ObjectId,
) -> Vec<ProtectionGrantOccurrence> {
    white_riders(state, host)
        .into_iter()
        .flatten()
        .filter(|rider| {
            rider.does_not_remove == ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached
        })
        .map(|rider| rider.occurrence)
        .collect()
}

/// The slot the ridered protection static occupies on `recipient`.
fn ridered_slot(state: &GameState, recipient: ObjectId) -> usize {
    ridered_static_index(state, recipient)
}

/// CR 702.16p ("when the protection effect starts to apply"): an unrelated
/// grant that installed on the recipient first ends, so the ridered static
/// moves to a lower slot. The effect never stopped applying, so it keeps its
/// start snapshot and does not exempt an Equipment that turned white later.
/// Constructed control (synthetic): the unrelated grant is a static on an
/// older source, removed directly to stand in for the grant ending; the printed
/// Benevolent Blessing (`cr_702_16p_snapshot_survives_later_ability_removal`)
/// and the fixed-slot witness (`cr_702_16p_does_not_exempt_attachment_that_becomes_matching_after_grant_start`)
/// sit beside it. Run for each path that produces a granted static.
fn assert_earlier_grant_ending_keeps_the_start_snapshot(path: GrantPath) {
    let (mut runner, board) = start_identity_board();
    let state = runner.state_mut();
    let older = spawn_grant_source(state, "Synthetic older source", 121);
    grant_static_to(
        state,
        older,
        TargetFilter::SpecificObject {
            id: board.recipient,
        },
        unrelated_static(board.recipient),
    );
    start_blessing_grant_by(state, &board, 113, path);
    evaluate_protection_layers(state);
    assert_eq!(
        ridered_slot(state, board.recipient),
        1,
        "{path:?} reach guard: shifted slot"
    );
    let [occurrence] = <[_; 1]>::try_from(already_attached_occurrences(state, board.host))
        .expect("one ridered grant");
    let start = protection_snapshot_ids(state, board.recipient, occurrence.clone(), 0, board.host);
    turn_white(state, board.later_white_equipment);
    evaluate_protection_layers(state);
    assert_eq!(
        attachment_illegality(state, board.later_white_equipment, board.host),
        Some(AttachIllegality::Protection),
        "{path:?} reach guard: white only after the effect started"
    );

    let older = state.objects.get_mut(&older).unwrap();
    older.static_definitions = Vec::new().into();
    older.base_static_definitions = std::sync::Arc::new(Vec::new());
    evaluate_protection_layers(state);
    assert_eq!(
        ridered_slot(state, board.recipient),
        0,
        "{path:?} reach guard: the slot moved"
    );
    assert_eq!(
        attachment_illegality(state, board.later_white_equipment, board.host),
        Some(AttachIllegality::Protection),
        "{path:?}: a new snapshot at the moved slot would exempt the later-white Equipment"
    );
    assert_eq!(
        attachment_illegality(state, board.white_equipment, board.host),
        None
    );
    assert_eq!(start, vec![board.white_equipment]);
    assert_eq!(
        protection_snapshot_ids(state, board.recipient, occurrence, 0, board.host),
        start,
        "{path:?}: the start snapshot is kept under the grant occurrence"
    );
}

#[test]
fn earlier_grant_ending_keeps_the_start_snapshot() {
    assert_earlier_grant_ending_keeps_the_start_snapshot(GrantPath::Static);
}

#[test]
fn earlier_grant_ending_keeps_a_transient_grants_start_snapshot() {
    assert_earlier_grant_ending_keeps_the_start_snapshot(GrantPath::Transient);
}

#[test]
fn earlier_grant_ending_keeps_a_late_grants_start_snapshot() {
    assert_earlier_grant_ending_keeps_the_start_snapshot(GrantPath::Late);
}

/// CR 702.16p: an unrelated grant from an older source starts and installs on
/// the recipient first, so the ridered static moves to a higher slot. The
/// effect keeps its start snapshot. Constructed control (synthetic), the
/// mirror of `assert_earlier_grant_ending_keeps_the_start_snapshot`, run for
/// each path that produces a granted static.
fn assert_earlier_grant_starting_keeps_the_start_snapshot(path: GrantPath) {
    let (mut runner, board) = start_identity_board();
    let state = runner.state_mut();
    let older = spawn_grant_source(state, "Synthetic older source", 114);
    start_blessing_grant_by(state, &board, 115, path);
    evaluate_protection_layers(state);
    assert_eq!(
        ridered_slot(state, board.recipient),
        0,
        "{path:?} reach guard: first slot"
    );
    let [occurrence] = <[_; 1]>::try_from(already_attached_occurrences(state, board.host))
        .expect("one ridered grant");
    let start = protection_snapshot_ids(state, board.recipient, occurrence.clone(), 0, board.host);
    turn_white(state, board.later_white_equipment);

    grant_static_to(
        state,
        older,
        TargetFilter::SpecificObject {
            id: board.recipient,
        },
        unrelated_static(board.recipient),
    );
    evaluate_protection_layers(state);
    assert_eq!(
        ridered_slot(state, board.recipient),
        1,
        "{path:?} reach guard: the slot moved"
    );
    assert_eq!(
        attachment_illegality(state, board.later_white_equipment, board.host),
        Some(AttachIllegality::Protection),
        "{path:?}: a new snapshot at the moved slot would exempt the later-white Equipment"
    );
    assert_eq!(
        attachment_illegality(state, board.white_equipment, board.host),
        None
    );
    assert_eq!(start, vec![board.white_equipment]);
    assert_eq!(
        protection_snapshot_ids(state, board.recipient, occurrence, 0, board.host),
        start,
        "{path:?}: the start snapshot is kept under the grant occurrence"
    );
}

#[test]
fn earlier_grant_starting_keeps_the_start_snapshot() {
    assert_earlier_grant_starting_keeps_the_start_snapshot(GrantPath::Static);
}

#[test]
fn earlier_grant_starting_keeps_a_transient_grants_start_snapshot() {
    assert_earlier_grant_starting_keeps_the_start_snapshot(GrantPath::Transient);
}

#[test]
fn earlier_grant_starting_keeps_a_late_grants_start_snapshot() {
    assert_earlier_grant_starting_keeps_the_start_snapshot(GrantPath::Late);
}

/// CR 702.16p + CR 113.2c: two sources grant the recipient byte-identical
/// ridered statics, so layer 6 installs one copy. Each grant is its own effect
/// with its own start: the second started after an Equipment turned white, so
/// only the second exempts it. While the first applies, its instance still
/// blocks that Equipment (CR 702.16n); once the first grant ends, the second's
/// own snapshot keeps it. Constructed (synthetic): no printed card grants this
/// static; the first grant ending is its static removed directly.
#[test]
fn identical_grants_from_different_sources_keep_separate_snapshots() {
    let (mut runner, board) = start_identity_board();
    let state = runner.state_mut();
    let first_granter = start_blessing_grant(state, &board, 116);
    evaluate_protection_layers(state);
    turn_white(state, board.later_white_equipment);
    evaluate_protection_layers(state);
    start_blessing_grant(state, &board, 117);
    evaluate_protection_layers(state);

    assert_eq!(
        state.objects[&board.recipient]
            .static_definitions
            .iter_all()
            .filter(|def| def.protection_does_not_remove.is_some())
            .count(),
        1,
        "reach guard: one installed copy for both grants"
    );
    let [first, second] = <[_; 2]>::try_from(already_attached_occurrences(state, board.host))
        .expect("two ridered grants");
    assert_ne!(first, second);
    let mut snapshots = [first, second].map(|occurrence| {
        protection_snapshot_ids(state, board.recipient, occurrence, 0, board.host)
    });
    assert_eq!(
        attachment_illegality(state, board.white_equipment, board.host),
        None
    );
    assert_eq!(
        attachment_illegality(state, board.later_white_equipment, board.host),
        Some(AttachIllegality::Protection),
        "the first grant's instance blocks it"
    );

    let first_granter = state.objects.get_mut(&first_granter).unwrap();
    first_granter.static_definitions = Vec::new().into();
    first_granter.base_static_definitions = std::sync::Arc::new(Vec::new());
    evaluate_protection_layers(state);
    assert_eq!(
        already_attached_occurrences(state, board.host).len(),
        1,
        "reach guard: only the second grant applies"
    );
    assert_eq!(
        attachment_illegality(state, board.later_white_equipment, board.host),
        None,
        "the second grant's own start snapshot exempts it"
    );
    assert_eq!(
        attachment_illegality(state, board.white_equipment, board.host),
        None
    );
    snapshots.sort();
    assert_eq!(
        snapshots,
        [
            vec![board.white_equipment],
            vec![board.white_equipment, board.later_white_equipment],
        ]
    );
}

/// CR 400.7 + CR 702.16p: a host that leaves and returns is a new object, so
/// the grant starts applying to it anew and takes a new snapshot. Constructed
/// control (synthetic): the incarnation is bumped and the Equipment unattached
/// directly, standing in for the host leaving and returning between two layer
/// passes; then the white Equipment is attached to the new object.
#[test]
fn host_reentering_starts_a_new_snapshot() {
    let (mut runner, board) = start_identity_board();
    let state = runner.state_mut();
    start_blessing_grant(state, &board, 118);
    evaluate_protection_layers(state);
    assert_eq!(
        attachment_illegality(state, board.white_equipment, board.host),
        None,
        "reach guard: the first incarnation's snapshot exempts it"
    );
    let [occurrence] = <[_; 1]>::try_from(already_attached_occurrences(state, board.host))
        .expect("one ridered grant");
    let first_snapshot =
        protection_snapshot_ids(state, board.recipient, occurrence.clone(), 0, board.host);

    for equipment in [board.white_equipment, board.later_white_equipment] {
        state.objects.get_mut(&equipment).unwrap().attached_to = None;
    }
    let host = state.objects.get_mut(&board.host).unwrap();
    host.attachments.clear();
    host.bump_incarnation();
    let incarnation = host.incarnation;
    evaluate_protection_layers(state);
    state
        .objects
        .get_mut(&board.white_equipment)
        .unwrap()
        .attached_to = Some(board.host.into());
    state
        .objects
        .get_mut(&board.host)
        .unwrap()
        .attachments
        .push(board.white_equipment);
    assert_eq!(
        attachment_illegality(state, board.white_equipment, board.host),
        Some(AttachIllegality::Protection),
        "the departed incarnation's snapshot must not exempt it"
    );

    assert_eq!(first_snapshot, vec![board.white_equipment]);
    assert!(
        state.objects[&board.recipient]
            .protection_start_snapshots
            .keys()
            .all(|key| key.host.incarnation == incarnation),
        "the departed incarnation's snapshot is pruned"
    );
    assert_eq!(
        protection_snapshot_ids(state, board.recipient, occurrence, 0, board.host),
        Vec::<ObjectId>::new(),
        "nothing was attached when the effect started on the new object"
    );
}

/// CR 702.16p: the start snapshot is game state that cannot be re-derived, so
/// it is saved, and a restore keeps it under the same identity. Printed
/// Benevolent Blessing (white chosen) joins P0's Pacifism on Grizzly Bears,
/// beside a synthetic Equipment that turns white only after Blessing's effect
/// started. After a persisted restore (immediate and deferred), Blessing holds
/// the same snapshot: Pacifism stays legal and the later-white Equipment stays
/// blocked, which a snapshot re-taken at restore would get wrong. A save written
/// with the older slot key restores the same way: preparing the restore
/// migrates its entry before any layer flush.
#[test]
fn persisted_restore_keeps_the_start_snapshot_identity() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let pacifism = s.add_real_card(P0, "Pacifism", Zone::Hand, db());
    let blessing = s.add_real_card(P0, "Benevolent Blessing", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, pacifism, Some(host), None);
    let later_white = spawn_equipment(runner.state_mut(), "Synthetic colorless Equipment", 122);
    attach_to(runner.state_mut(), later_white, host);
    cast(&mut runner, P0, blessing, Some(host), Some("white"));
    turn_white(runner.state_mut(), later_white);
    evaluate_protection_layers(runner.state_mut());
    let state = runner.state();
    assert_eq!(
        attachment_illegality(state, later_white, host),
        Some(AttachIllegality::Protection),
        "reach guard: white only after Blessing's effect started"
    );
    let snapshots = state.objects[&blessing].protection_start_snapshots.clone();
    let def_index = ridered_static_index(state, blessing);
    assert!(
        printed_snapshot_ids(state, blessing, def_index, 0, host).contains(&pacifism),
        "reach guard: Pacifism is in Blessing's start snapshot"
    );

    let restore = |wire: serde_json::Value, policy| {
        let prepared = serde_json::from_value::<PersistedGameState>(wire)
            .unwrap()
            .prepare_for_restore(policy)
            .unwrap();
        match policy {
            PersistedRestoreFinalization::Immediate => prepared.finalize_immediately(),
            PersistedRestoreFinalization::DeferUntilRehydrated => {
                prepared.finalize_after_rehydration(|_| Ok(()))
            }
        }
        .unwrap()
    };
    for policy in [
        PersistedRestoreFinalization::Immediate,
        PersistedRestoreFinalization::DeferUntilRehydrated,
    ] {
        let wire = serde_json::to_value(PersistedGameState::capture(state.clone())).unwrap();
        let restored = restore(wire, policy);
        assert_eq!(attachment_illegality(&restored, pacifism, host), None);
        assert_eq!(
            attachment_illegality(&restored, later_white, host),
            Some(AttachIllegality::Protection),
            "{policy:?}: a snapshot re-taken at restore would exempt it"
        );
        assert_eq!(
            restored.objects[&blessing].protection_start_snapshots, snapshots,
            "{policy:?}: the snapshot keeps its identity"
        );
    }

    // The same game saved with the older slot key.
    let mut legacy =
        serde_json::to_value(PersistedGameState::Raw(Box::new(state.clone()))).unwrap();
    let root = if legacy.get("state").is_some() {
        &mut legacy["state"]
    } else {
        &mut legacy
    };
    let object = &mut root["objects"][blessing.0.to_string()];
    let entries = object
        .as_object_mut()
        .unwrap()
        .remove("protection_start_snapshots")
        .expect("reach guard: the snapshot is saved");
    let [entry] = entries.as_array().unwrap().as_slice() else {
        panic!("one snapshot");
    };
    object["protection_start_exempt_attachments"] =
        serde_json::json!([[[def_index, 0, host.0], entry[1].clone()]]);
    let restored = restore(legacy, PersistedRestoreFinalization::Immediate);
    assert_eq!(attachment_illegality(&restored, pacifism, host), None);
    assert_eq!(
        attachment_illegality(&restored, later_white, host),
        Some(AttachIllegality::Protection),
        "the migrated snapshot, not one re-taken at restore"
    );
    assert_eq!(
        restored.objects[&blessing].protection_start_snapshots, snapshots,
        "the older slot key migrates to the same identity"
    );
    assert!(restored.objects[&blessing]
        .legacy_protection_start_exempt_attachments
        .is_empty());
}

/// CR 707.2 + CR 702.16p: a copy of Benevolent Blessing has the ridered static
/// as its own, so its effect, start snapshot and identity are the copy's. The
/// copy effect is synthetic (the printed analogue is an enchantment entering as
/// a copy, e.g. Copy Enchantment), and the copy records its own as-enters
/// color. It snapshots the white Aura its controller had on its own Grizzly
/// Bears, keeping that Aura legal; the original's snapshot is separate.
#[test]
fn copy_of_benevolent_blessing_takes_its_own_start_snapshot() {
    let mut s = scenario();
    let original_host = grizzly_bears(&mut s, P0);
    let copy_host = grizzly_bears(&mut s, P0);
    let blessing = s.add_real_card(P0, "Benevolent Blessing", Zone::Hand, db());
    let mut runner = s.build();
    cast(
        &mut runner,
        P0,
        blessing,
        Some(original_host),
        Some("white"),
    );

    let state = runner.state_mut();
    let earlier_aura = spawn_synthetic_white_aura(state, "Synthetic white Aura", 119, P0);
    attach_to(state, earlier_aura, copy_host);
    let blessing_values =
        crate::game::printed_cards::intrinsic_copiable_values(&state.objects[&blessing]);
    let copy = spawn_synthetic_white_aura(state, "Synthetic copy", 120, P0);
    attach_to(state, copy, copy_host);
    state
        .objects
        .get_mut(&copy)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::Color(ManaColor::White));
    state
        .add_transient_continuous_effect(
            copy,
            P0,
            Duration::Permanent,
            TargetFilter::SpecificObject { id: copy },
            vec![ContinuousModification::CopyValues {
                values: Box::new(blessing_values),
                display: Default::default(),
            }],
            None,
        )
        .expect("copy effect must register");
    evaluate_protection_layers(state);

    assert_eq!(
        state.objects[&copy].name, "Benevolent Blessing",
        "reach guard"
    );
    let [occurrence] = <[_; 1]>::try_from(already_attached_occurrences(state, copy_host))
        .expect("the copy's own ridered grant");
    let def_index = ridered_static_index(state, copy);
    assert_eq!(
        occurrence,
        ProtectionGrantOccurrence::Static {
            definition_index: def_index
        }
    );
    assert!(
        printed_snapshot_ids(state, copy, def_index, 0, copy_host).contains(&earlier_aura),
        "the copy holds its own start snapshot"
    );
    assert!(printed_snapshot_ids(state, blessing, def_index, 0, copy_host).is_empty());
    assert_eq!(attachment_illegality(state, earlier_aura, copy_host), None);
}

/// CR 702.16m + CR 702.16n: a static that grants a chosen protection keyword
/// carries that static's rider, as a fixed protection grant does. Synthetic:
/// no printed card grants a chosen protection keyword (`AddChosenKeyword`'s
/// printed user is Greymond, Avacyn's Stalwart, with first strike, vigilance
/// or lifelink), and the rider is Flickering Ward's "This effect doesn't
/// remove this Aura."
#[test]
fn chosen_protection_grant_carries_its_statics_rider() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let mut runner = s.build();
    let state = runner.state_mut();
    let aura = spawn_synthetic_white_aura(state, "Synthetic chosen-protection Aura", 123, P0);
    attach_to(state, aura, host);
    {
        let obj = state.objects.get_mut(&aura).unwrap();
        obj.chosen_attributes
            .push(ChosenAttribute::Keyword(Keyword::Protection(
                ProtectionTarget::Color(ManaColor::White),
            )));
        obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SpecificObject { id: host })
                .modifications(vec![ContinuousModification::AddChosenKeyword])
                .protection_does_not_remove(ProtectionDoesNotRemove::Source),
        );
    }
    persist_last_static(state, aura);
    evaluate_protection_layers(state);

    let riders = white_riders(state, host);
    assert_eq!(riders.len(), 1, "reach guard: one chosen instance");
    let rider = riders[0].as_ref().expect("the static's rider is carried");
    assert_eq!(rider.source, aura);
    assert_eq!(rider.does_not_remove, ProtectionDoesNotRemove::Source);
    assert_eq!(
        attachment_illegality(state, aura, host),
        None,
        "the chosen grant's own rider exempts its Aura"
    );
}

/// CR 611.3a + CR 702.16p: a creature entering where a granted ridered static
/// reaches it takes the full layer pass (`prepare_incremental_flush` escalates
/// when a granted-static child of an outside carrier can reach an entrant), so
/// the entrant's rider and start grant name the grant occurrence, never the
/// slot of the carrier's installed copy. Synthetic: no printed card quotes a
/// ridered static, and the entrant is created directly as a creature entering
/// the battlefield.
#[test]
fn entrant_reached_by_a_granted_rider_takes_the_full_pass() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let mut runner = s.build();
    let state = runner.state_mut();
    let carrier = spawn_grant_source(state, "Synthetic carrier", 124);
    let granter = spawn_grant_source(state, "Synthetic granting source", 125);
    grant_static_to(
        state,
        granter,
        TargetFilter::SpecificObject { id: carrier },
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::creature()))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Color(ManaColor::White)),
            }])
            .protection_does_not_remove(
                ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached,
            ),
    );
    evaluate_protection_layers(state);
    assert_eq!(
        white_riders(state, host).len(),
        1,
        "reach guard: the grant applies"
    );

    let entrant = create_object(
        state,
        CardId(126),
        P0,
        "Synthetic entrant".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&entrant).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
    }
    crate::game::layers::mark_layers_entered(state, entrant);
    crate::game::perf_counters::reset();
    crate::game::layers::flush_layers(state);
    let counters = crate::game::perf_counters::snapshot();
    assert_eq!(
        (counters.layers_incremental, counters.layers_full_eval),
        (0, 1),
        "the entrant's flush escalates to the full pass"
    );
    let riders = white_riders(state, entrant);
    assert_eq!(riders.len(), 1, "reach guard: the entrant is protected");
    let rider = riders[0].as_ref().expect("the granted rider");
    assert!(matches!(
        rider.occurrence,
        ProtectionGrantOccurrence::Granted { .. }
    ));
    assert!(state.objects[&entrant]
        .protection_start_grants
        .iter()
        .all(|grant| matches!(
            grant.key.occurrence,
            ProtectionGrantOccurrence::Granted { .. }
        )));
}

/// CR 702.16p: only a source on the battlefield holds a start snapshot: the
/// prune and the legacy migration visit battlefield sources only, and leaving
/// the battlefield clears the map (CR 400.7). Synthetic: a
/// graveyard object whose ridered static functions from the graveyard (no
/// printed card has one; the printed rider is Benevolent Blessing's, on an
/// Aura on the battlefield) gives Grizzly Bears protection from white. It holds
/// no snapshot, so the white Equipment already attached is not exempt.
#[test]
fn non_battlefield_source_holds_no_start_snapshot() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let mut runner = s.build();
    let state = runner.state_mut();
    let equipment = spawn_synthetic_white_equipment(state, 127);
    attach_to(state, equipment, host);
    let source = create_object(
        state,
        CardId(128),
        P0,
        "Synthetic graveyard source".to_string(),
        Zone::Graveyard,
    );
    state
        .objects
        .get_mut(&source)
        .unwrap()
        .static_definitions
        .push(
            quoted_white_protection(
                host,
                ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached,
            )
            .active_zones(vec![Zone::Graveyard]),
        );
    persist_last_static(state, source);
    evaluate_protection_layers(state);

    let riders = white_riders(state, host);
    assert_eq!(riders.len(), 1, "reach guard: the graveyard static applies");
    assert_eq!(riders[0].as_ref().map(|rider| rider.source), Some(source));
    assert!(
        state.objects[&source].protection_start_snapshots.is_empty(),
        "a non-battlefield source holds no snapshot"
    );
    assert_eq!(
        attachment_illegality(state, equipment, host),
        Some(AttachIllegality::Protection)
    );
}
