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
    apply_protection_grant, evaluate_protection_layers, protection_snapshot_ids, spawn_equipment,
    spawn_grant_source,
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
use crate::types::game_state::WaitingFor;
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::{Keyword, ProtectionTarget};
use crate::types::layers::ProtectionRiderDefinition;
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

/// Cast a printed spell at `target`, answering an "As this Aura enters, choose
/// a color" prompt with `color`, and let the stack settle.
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
    if let WaitingFor::NamedChoice { options, .. } = outcome.final_waiting_for() {
        let wanted = color.expect("a color prompt needs an answer");
        let choice = options
            .iter()
            .find(|option| option.eq_ignore_ascii_case(wanted))
            .expect("the color is offered")
            .clone();
        runner
            .act(GameAction::ChooseOption { choice })
            .expect("the color choice resolves");
    }
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        runner.state().waiting_for
    );
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
#[test]
fn cr_702_16p_snapshot_survives_later_ability_removal() {
    let mut s = scenario();
    let host = grizzly_bears(&mut s, P0);
    let pacifism = s.add_real_card(P0, "Pacifism", Zone::Hand, db());
    let blessing = s.add_real_card(P0, "Benevolent Blessing", Zone::Hand, db());
    let humility = s.add_real_card(P0, "Humility", Zone::Hand, db());
    let mut runner = s.build();
    cast(&mut runner, P0, pacifism, Some(host), None);
    cast(&mut runner, P0, blessing, Some(host), Some("white"));
    attached(&runner, pacifism, host);
    attached(&runner, blessing, host);
    let def_index = ridered_static_index(runner.state(), blessing);
    let snapshot = protection_snapshot_ids(runner.state(), blessing, def_index, 0, host);
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
        protection_snapshot_ids(runner.state(), blessing, def_index, 0, host),
        snapshot,
        "the effect never stopped applying, so its start snapshot must survive"
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
    assert_eq!(
        attachment_illegality(runner.state(), ward, knight),
        None,
        "reach guard: under Humility only Ward's own instance is live"
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
/// first, so the granted copy installs at its index 1 while the granter's
/// static is at its index 0. `path` grants the recipient the synthetic quoted
/// static "Grizzly Bears has protection from white" with `rider`.
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
            // The start snapshot written for the recipient's installed copy:
            // what its controller (P0) had attached, not P1's Pacifism.
            ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached => {
                assert_eq!(
                    protection_snapshot_ids(state, board.recipient, 1, 0, board.host),
                    vec![board.recipient, board.granter, board.equipment],
                    "{path:?}: the snapshot is the recipient's installed copy's"
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
        assert_eq!(grant.definition, ProtectionRiderDefinition::Live(0));
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
    use crate::types::game_state::{PersistedGameState, PersistedRestoreFinalization};

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
