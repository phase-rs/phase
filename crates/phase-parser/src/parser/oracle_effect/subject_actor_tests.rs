//! CR 608.2c: the player named as the subject of a zone-move verb performs the
//! move, so the parser binds that subject as the `actor` of every
//! `ChangeZone` / `ChangeZoneAll` the verb produced (issue #10414). A clause
//! with no subject keeps the `Controller` default, whatever player its filter
//! names.

use super::*;
use crate::parser::parse_oracle_text;
use crate::types::ability::LibraryInstructionActor;

/// Every zone move in the card's parsed abilities and trigger bodies, in walk
/// order, as `(depth, actor)`. Depth 0 is an ability's root effect.
fn zone_move_actors(
    name: &str,
    types: &[&str],
    oracle: &str,
) -> Vec<(usize, LibraryInstructionActor)> {
    fn walk(
        def: &AbilityDefinition,
        depth: usize,
        out: &mut Vec<(usize, LibraryInstructionActor)>,
    ) {
        if let Effect::ChangeZone { actor, .. } | Effect::ChangeZoneAll { actor, .. } =
            def.effect.as_ref()
        {
            out.push((depth, actor.clone()));
        }
        for mode in &def.mode_abilities {
            walk(mode, depth + 1, out);
        }
        if let Some(else_branch) = def.else_ability.as_deref() {
            walk(else_branch, depth + 1, out);
        }
        if let Some(sub) = def.sub_ability.as_deref() {
            walk(sub, depth + 1, out);
        }
    }
    let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
    let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
    let mut out = Vec::new();
    for ability in &parsed.abilities {
        walk(ability, 0, &mut out);
    }
    for trigger in &parsed.triggers {
        if let Some(execute) = trigger.execute.as_deref() {
            walk(execute, 0, &mut out);
        }
    }
    for replacement in &parsed.replacements {
        if let Some(execute) = replacement.execute.as_deref() {
            walk(execute, 0, &mut out);
        }
    }
    assert!(!out.is_empty(), "{name} parses a zone move: {parsed:#?}");
    out
}

fn subject(player: TargetFilter) -> LibraryInstructionActor {
    LibraryInstructionActor::Player { player }
}

// --- Binding site: the targeted-subject `TargetOnly` wrapper ---------------

/// "Target opponent exiles a creature they control and their graveyard." The
/// wrapper announces the player; both moves beneath it (depth 1 and 2) are that
/// player's, referenced as the parent target.
#[test]
fn targeted_subject_wrapper_binds_every_nested_move() {
    assert_eq!(
        zone_move_actors(
            "Strategic Betrayal",
            &["Sorcery"],
            "Target opponent exiles a creature they control and their graveyard.",
        ),
        vec![
            (1, subject(TargetFilter::ParentTarget)),
            (2, subject(TargetFilter::ParentTarget)),
        ]
    );
    assert_eq!(
        zone_move_actors(
            "Mudhole",
            &["Instant"],
            "Target player exiles all land cards from their graveyard.",
        ),
        vec![(1, subject(TargetFilter::ParentTarget))]
    );
    assert_eq!(
        zone_move_actors(
            "Skullcap Snail",
            &["Creature"],
            "When this creature enters, target opponent exiles a card from their hand.",
        ),
        vec![(1, subject(TargetFilter::ParentTarget))]
    );
}

// --- Binding site: a leading subject on the predicate ----------------------

/// "Return target nonland permanent to its owner's hand, then that player
/// exiles a card from their hand." The returning clause names the permanent's
/// owner, so "that player" is the owner (CR 608.2c + CR 400.3), also when
/// another player controls the permanent; the move sits one clause down the
/// chain.
#[test]
fn leading_subject_binds_its_player_reference() {
    assert_eq!(
        zone_move_actors(
            "Ashiok, Nightmare Muse",
            &["Planeswalker"],
            "[\u{2212}3]: Return target nonland permanent to its owner's hand, then that player exiles a card from their hand.",
        ),
        vec![(1, subject(TargetFilter::ParentTargetOwner))]
    );
    assert_eq!(
        zone_move_actors(
            "Thought Lash",
            &["Enchantment"],
            "When a player doesn't pay this enchantment's cumulative upkeep, that player exiles all cards from their library.",
        ),
        vec![(0, subject(TargetFilter::TriggeringPlayer))]
    );
}

/// Rona's subject governs only its own sentence: the following "you may put it
/// onto the battlefield" is the controller's move.
#[test]
fn leading_subject_stops_at_the_next_sentence() {
    assert_eq!(
        zone_move_actors(
            "Rona, Tolarian Obliterator",
            &["Creature"],
            "Whenever a source deals damage to Rona, that source's controller exiles a card from their hand at random. If it's a land card, you may put it onto the battlefield under your control. Otherwise, you may cast it without paying its mana cost.",
        ),
        vec![
            (0, subject(TargetFilter::ParentTargetController)),
            (1, LibraryInstructionActor::Controller),
        ]
    );
}

/// The actor shape with a declared player group's id elided, for subjects the
/// chunk loop announces under a fresh group.
fn declared_subject_at(depth: usize) -> impl Fn(&(usize, LibraryInstructionActor)) -> bool {
    move |(at, actor)| {
        *at == depth
            && matches!(
                actor,
                LibraryInstructionActor::Player {
                    player: TargetFilter::DeclaredPlayer { .. }
                }
            )
    }
}

// --- Binding site: an elided subject carried to the next conjunct -----------

/// "Target player draws a card, then exiles a card from their hand." The
/// second verb has no subject of its own; the carried subject performs it.
#[test]
fn carried_subject_binds_the_conjugated_continuation() {
    let actors = zone_move_actors(
        "Arcane Artisan",
        &["Creature"],
        "{2}{U}, {T}: Target player draws a card, then exiles a card from their hand. If a creature card is exiled this way, that player creates a token that's a copy of that card.",
    );
    assert_eq!(actors.len(), 1, "{actors:?}");
    assert!(declared_subject_at(1)(&actors[0]), "{actors:?}");
}

// --- Binding site: a leading subject restated as a pronoun ------------------

/// "…, then they exile a nonland card from their graveyard." The continuation
/// restates its subject as a pronoun; the chain's anaphor rewrite binds it to
/// the announced opponent, and the performer follows, so both moves are the
/// opponent's.
#[test]
fn pronoun_subject_binds_its_continuation() {
    let actors = zone_move_actors(
        "Azula, Cunning Usurper",
        &["Creature"],
        "When Azula enters, target opponent exiles a nontoken creature they control, then they exile a nonland card from their graveyard.",
    );
    assert_eq!(actors.len(), 2, "{actors:?}");
    assert_eq!(actors[0], (1, subject(TargetFilter::ParentTarget)));
    assert!(declared_subject_at(2)(&actors[1]), "{actors:?}");
}

/// "That player exiles a card from it, then you exile a card from it." An
/// explicit "you" keeps its own move on the controller inside the same chain.
#[test]
fn an_explicit_you_continuation_keeps_the_controller() {
    let actors = zone_move_actors(
        "Struggle for Sanity",
        &["Sorcery"],
        "Target opponent reveals their hand. That player exiles a card from it, then you exile a card from it. Repeat this process until all cards in that hand have been exiled. That player returns the cards they exiled this way to their hand and puts the rest into their graveyard.",
    );
    assert!(declared_subject_at(1)(&actors[0]), "{actors:?}");
    assert_eq!(
        actors[1],
        (2, LibraryInstructionActor::Controller),
        "{actors:?}"
    );
}

// --- Binding site: a subject's list of exiled objects ------------------------

/// "Target opponent exiles the top card of their library, a card at random
/// from their graveyard, and a card at random from their hand." The single
/// exile at the end of the gather chain is the opponent's.
#[test]
fn subject_object_list_binds_the_closing_exile() {
    let actors = zone_move_actors(
        "Crabomination",
        &["Creature"],
        "When this creature enters, target opponent exiles the top card of their library, a card at random from their graveyard, and a card at random from their hand. You may cast a spell from among cards exiled this way without paying its mana cost.",
    );
    assert_eq!(actors.len(), 1, "{actors:?}");
    assert!(declared_subject_at(4)(&actors[0]), "{actors:?}");
}

// --- Modes -------------------------------------------------------------------

/// Each mode's targeted subject binds its own move.
#[test]
fn each_modal_subject_binds_its_move() {
    assert_eq!(
        zone_move_actors(
            "Debt to the Kami",
            &["Instant"],
            "Choose one \u{2014}\n\u{2022} Target opponent exiles a creature they control.\n\u{2022} Target opponent exiles an enchantment they control.",
        ),
        vec![
            (1, subject(TargetFilter::ParentTarget)),
            (1, subject(TargetFilter::ParentTarget)),
        ]
    );
}

// --- "that player" in a trigger effect ----------------------------------------

/// After the effect declares a target player, "that player" is that player, not
/// the trigger event's: Salt Vampire's target opponent exiles the card (and
/// draws, the carried subject).
#[test]
fn trigger_that_player_binds_the_declared_target() {
    let actors = zone_move_actors(
        "Salt Vampire",
        &["Creature"],
        "When this creature enters, look at target opponent's hand. You may choose a nonland card from it. If you do, that player exiles that card, then draws a card.",
    );
    assert_eq!(actors.len(), 1, "{actors:?}");
    assert!(declared_subject_at(1)(&actors[0]), "{actors:?}");
}

/// "That player" opening the trigger effect is the trigger event's player
/// ("a player casts", "a player doesn't pay"), so it is stamped.
#[test]
fn trigger_that_player_opening_the_effect_is_the_event_player() {
    assert_eq!(
        zone_move_actors(
            "Possibility Storm",
            &["Enchantment"],
            "Whenever a player casts a spell from their hand, that player exiles it, then exiles cards from the top of their library until they exile a card that shares a card type with it. That player may cast that card without paying its mana cost. Then they put all cards exiled with this enchantment on the bottom of their library in a random order.",
        ),
        vec![(0, subject(TargetFilter::TriggeringPlayer))]
    );
}

/// CR 608.2c: the controller's own choice among the revealed cards ("You
/// choose one of those cards", "You may choose one of those cards") names no
/// other player, so "that player" still reads the revealing target player
/// (Taster of Wares, Juggernaut Peddler).
#[test]
fn trigger_that_player_reads_past_the_controllers_choice() {
    let cases: [(&str, &str); 2] = [
        (
            "Taster of Wares",
            "When this creature enters, target opponent reveals X cards from their hand, where X is the number of Goblins you control. You choose one of those cards. That player exiles it. If an instant or sorcery card is exiled this way, you may cast it for as long as you control this creature, and mana of any type can be spent to cast that spell.",
        ),
        (
            "Juggernaut Peddler",
            "Vigilance\nWhen Juggernaut Peddler enters, target player reveals all nonland cards in their hand. You may choose one of those cards. If you do, that player exiles it and conjures a card named Juggernaut into their hand.",
        ),
    ];
    for (name, oracle) in cases {
        let actors = zone_move_actors(name, &["Creature"], oracle);
        assert_eq!(actors.len(), 1, "{name}: {actors:?}");
        assert!(declared_subject_at(2)(&actors[0]), "{name}: {actors:?}");
    }
}

/// CR 506.2 + CR 608.2c: with no announced target, "that player" names the
/// player the earlier clause acted through: the defending player (Goblin Guide;
/// Trepanation Blade, past the creature's power change) or the player a
/// per-player scope iterates (Conundrum Sphinx).
#[test]
fn trigger_that_player_names_the_earlier_player_reference() {
    let cases: [(&str, &str, &str, usize, TargetFilter); 3] = [
        (
            "Goblin Guide",
            "Creature",
            "Haste\nWhenever this creature attacks, defending player reveals the top card of their library. If it's a land card, that player puts it into their hand.",
            1,
            TargetFilter::DefendingPlayer,
        ),
        (
            "Trepanation Blade",
            "Artifact",
            "Whenever equipped creature attacks, defending player reveals cards from the top of their library until they reveal a land card. The creature gets +1/+0 until end of turn for each card revealed this way. That player puts the revealed cards into their graveyard.\nEquip {2}",
            2,
            TargetFilter::DefendingPlayer,
        ),
        (
            "Conundrum Sphinx",
            "Creature",
            "Flying\nWhenever this creature attacks, each player chooses a card name. Then each player reveals the top card of their library. If the card a player revealed has the name they chose, that player puts it into their hand. If it doesn't, that player puts it on the bottom of their library.",
            2,
            TargetFilter::ScopedPlayer,
        ),
    ];
    for (name, card_type, oracle, depth, player) in cases {
        assert_eq!(
            zone_move_actors(name, &[card_type], oracle),
            vec![(depth, subject(player))],
            "{name}"
        );
    }
}

/// CR 614.1a + CR 608.2c: in a draw replacement, "that player" is the player
/// who would draw (Shared Fate), and the replacement's referent rewrite
/// carries the performer with the moved object.
#[test]
fn draw_replacement_that_player_performs_as_the_drawer() {
    assert_eq!(
        zone_move_actors(
            "Shared Fate",
            &["Enchantment"],
            "If a player would draw a card, that player exiles the top card of one of their opponents' libraries face down instead.\nEach player may look at cards they exiled with this enchantment, and they may play lands and cast spells from among those cards.",
        ),
        vec![(0, subject(TargetFilter::PostReplacementDamageTarget))]
    );
}

/// CR 608.2c + CR 400.3: after "return … to its owner's hand", every player
/// slot of the following "that player" clause is the owner: the discarding
/// player (Recoil, Compelling Deterrence, Dinrova Horror's trigger) and, for
/// Ashiok, the hand the exiled card comes from. A clause that follows any other
/// instruction keeps the controller reading ("Destroy target creature. Then
/// that player discards a card" — synthetic control, no printed card).
#[test]
fn that_player_after_a_return_to_owners_hand_is_the_owner() {
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{ControllerRef, FilterProp, TypedFilter};
    let second_clause = |name: &str, types: &[&str], oracle: &str| -> Effect {
        let types: Vec<String> = types.iter().map(|t| (*t).to_string()).collect();
        let parsed = parse_oracle_text(oracle, name, &[], &types, &[]);
        let root = parsed
            .abilities
            .first()
            .or_else(|| parsed.triggers.first().and_then(|t| t.execute.as_deref()))
            .unwrap_or_else(|| panic!("{name} parses an ability"));
        assert!(matches!(
            root.effect.as_ref(),
            Effect::Bounce { .. } | Effect::Destroy { .. }
        ));
        root.sub_ability
            .as_deref()
            .unwrap_or_else(|| panic!("{name} has a second clause"))
            .effect
            .as_ref()
            .clone()
    };
    for (name, types, oracle) in [
        (
            "Recoil",
            &["Instant"][..],
            "Return target permanent to its owner's hand. Then that player discards a card.",
        ),
        (
            "Compelling Deterrence",
            &["Instant"][..],
            "Return target nonland permanent to its owner's hand. Then that player discards a card if you control a Zombie.",
        ),
        (
            "Dinrova Horror",
            &["Creature"][..],
            "When this creature enters, return target permanent to its owner's hand, then that player discards a card.",
        ),
    ] {
        assert!(
            matches!(
                second_clause(name, types, oracle),
                Effect::Discard {
                    target: TargetFilter::ParentTargetOwner,
                    ..
                }
            ),
            "{name}"
        );
    }
    let Effect::ChangeZone {
        target: TargetFilter::Typed(TypedFilter {
            controller,
            properties,
            ..
        }),
        ..
    } = second_clause(
        "Ashiok, Nightmare Muse",
        &["Planeswalker"],
        "[\u{2212}3]: Return target nonland permanent to its owner's hand, then that player exiles a card from their hand.",
    )
    else {
        panic!("Ashiok's second clause is the exile pick");
    };
    assert_eq!(controller, Some(ControllerRef::ParentTargetOwner));
    assert!(properties.contains(&FilterProp::Owned {
        controller: ControllerRef::ParentTargetOwner
    }));
    assert!(matches!(
        second_clause(
            "Gorbleflux Edict",
            &["Sorcery"],
            "Destroy target creature. Then that player discards a card.",
        ),
        Effect::Discard {
            target: TargetFilter::ParentTargetController,
            ..
        }
    ));
}

// --- Negative rows: no subject, so the controller performs -----------------

/// Imperative zone moves keep the controller as their performer however the
/// filter names another player's objects or zone: a player-valued filter
/// (Tormod's Crypt), a library owner (Rahilda), announced card targets
/// (Froghemoth), a sweep scoped to the target's controller (Declaration in
/// Stone, Eradicate). A quantified subject ("each opponent", Yarok's
/// Fenlurker) is lowered to a per-player fan-out whose seat is the
/// controller, so it keeps the default too.
#[test]
fn subjectless_and_fanned_out_moves_keep_the_controller() {
    let cases: [(&str, &str, &str); 6] = [
        (
            "Tormod's Crypt",
            "Artifact",
            "{T}, Sacrifice this artifact: Exile target player's graveyard.",
        ),
        (
            "Rahilda, Wanted Cutthroat",
            "Creature",
            "When Rahilda, Wanted Cutthroat deals combat damage to a player, exile a nonland card from their library at random. During any turn you attacked with a Wolf or Werewolf, you may cast that card and you may spend mana as though it were mana of any color to cast that spell.",
        ),
        (
            "Froghemoth",
            "Creature",
            "Whenever this creature deals combat damage to a player, exile up to that many target cards from their graveyard. Put a +1/+1 counter on this creature for each creature card exiled this way. You gain 1 life for each noncreature card exiled this way.",
        ),
        (
            "Yarok's Fenlurker",
            "Creature",
            "When this creature enters, each opponent exiles a card from their hand.",
        ),
        (
            "Declaration in Stone",
            "Sorcery",
            "Exile target creature and all other creatures its controller controls with the same name as that creature. That player investigates for each nontoken creature exiled this way.",
        ),
        (
            "Eradicate",
            "Sorcery",
            "Exile target nonblack creature. Search its controller's graveyard, hand, and library for all cards with the same name as that creature and exile them. Then that player shuffles.",
        ),
    ];
    for (name, card_type, oracle) in cases {
        let actors = zone_move_actors(name, &[card_type], oracle);
        assert!(
            actors
                .iter()
                .all(|(_, actor)| *actor == LibraryInstructionActor::Controller),
            "{name}: {actors:?}"
        );
    }
}

// --- The walk -----------------------------------------------------------------

fn change_zone(destination: Zone) -> Effect {
    Effect::ChangeZone {
        actor: LibraryInstructionActor::Controller,
        origin: None,
        destination,
        target: TargetFilter::Any,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
        library_shuffle: Default::default(),
    }
}

fn change_zone_all(destination: Zone) -> Effect {
    Effect::ChangeZoneAll {
        actor: LibraryInstructionActor::Controller,
        origin: None,
        destination,
        target: TargetFilter::Any,
        enters_under: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enters_attacking: false,
        enter_with_counters: vec![],
        face_down_profile: None,
        library_position: None,
        library_shuffle: Default::default(),
        random_order: false,
    }
}

fn actor_of(effect: &Effect) -> &LibraryInstructionActor {
    match effect {
        Effect::ChangeZone { actor, .. } | Effect::ChangeZoneAll { actor, .. } => actor,
        other => panic!("expected a zone move, got {other:?}"),
    }
}

/// The stamp reaches the root, each `sub_ability` depth, every mode and the
/// else branch, for both zone-move variants; it leaves an actor an inner
/// clause already bound, and a "you" subject restates nothing.
#[test]
fn stamp_walks_the_whole_clause_chain() {
    let bound = subject(TargetFilter::TriggeringPlayer);
    let mut inner_bound = AbilityDefinition::new(AbilityKind::Spell, change_zone(Zone::Hand));
    if let Effect::ChangeZone { actor, .. } = inner_bound.effect.as_mut() {
        *actor = bound.clone();
    }
    let mut modal = AbilityDefinition::new(AbilityKind::Spell, Effect::Investigate);
    modal.mode_abilities = vec![
        AbilityDefinition::new(AbilityKind::Spell, change_zone(Zone::Exile)),
        AbilityDefinition::new(AbilityKind::Spell, change_zone_all(Zone::Graveyard)),
    ];
    modal.else_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        change_zone_all(Zone::Library),
    )));
    modal.sub_ability = Some(Box::new(inner_bound));
    let mut depth_one = AbilityDefinition::new(AbilityKind::Spell, change_zone_all(Zone::Exile));
    depth_one.sub_ability = Some(Box::new(modal));

    let mut root = change_zone(Zone::Exile);
    let mut chain = depth_one.clone();
    stamp_subject_actor(&mut root, Some(&mut chain), &TargetFilter::ParentTarget);
    let expected = subject(TargetFilter::ParentTarget);
    assert_eq!(actor_of(&root), &expected, "root");
    assert_eq!(actor_of(&chain.effect), &expected, "depth 1");
    let modal = chain.sub_ability.as_deref().expect("depth 2");
    for mode in &modal.mode_abilities {
        assert_eq!(actor_of(&mode.effect), &expected, "mode");
    }
    let else_branch = modal.else_ability.as_deref().expect("else");
    assert_eq!(actor_of(&else_branch.effect), &expected, "else");
    let inner = modal.sub_ability.as_deref().expect("depth 3");
    assert_eq!(actor_of(&inner.effect), &bound, "an inner binding is kept");

    // CR 603.12: a reflexive "when you do" ability is a separate ability; its
    // moves keep their own actor.
    let mut reflexive = AbilityDefinition::new(AbilityKind::Spell, change_zone(Zone::Exile));
    reflexive.condition = Some(crate::types::ability::AbilityCondition::WhenYouDo);
    let mut root = change_zone(Zone::Exile);
    stamp_subject_actor(&mut root, Some(&mut reflexive), &TargetFilter::ParentTarget);
    assert_eq!(actor_of(&root), &expected, "root");
    assert_eq!(
        actor_of(&reflexive.effect),
        &LibraryInstructionActor::Controller,
        "a reflexive ability is not stamped"
    );

    let mut root = change_zone(Zone::Exile);
    let mut chain = depth_one;
    stamp_subject_actor(&mut root, Some(&mut chain), &TargetFilter::Controller);
    assert_eq!(actor_of(&root), &LibraryInstructionActor::Controller);
    assert_eq!(
        actor_of(&chain.effect),
        &LibraryInstructionActor::Controller
    );
}

// --- Promotions keep the performer -------------------------------------------

/// CR 701.24c + CR 608.2c: "they shuffle them into their library" promotes the
/// pronoun's singular move to a mass move over the chain's set; the subject
/// that performs it survives the promotion. Synthetic actor on the promoted
/// shape.
#[test]
fn plural_library_recall_promotion_keeps_the_actor() {
    let performer = subject(TargetFilter::ParentTargetController);
    let mut effect = change_zone(Zone::Library);
    if let Effect::ChangeZone { actor, target, .. } = &mut effect {
        *actor = performer.clone();
        *target = TargetFilter::ParentTarget;
    }
    rewrite_plural_library_recall_to_tracked_set(&mut effect);
    assert!(
        matches!(effect, Effect::ChangeZoneAll { .. }),
        "promoted: {effect:?}"
    );
    assert_eq!(actor_of(&effect), &performer);
}

/// CR 608.2c: the shared reference visitor reaches a zone move's performer as
/// well as its moved objects, so a rebind of the anaphor (the replaced draw's
/// drawer, a declared player) rewrites both alike.
#[test]
fn reference_visitor_reaches_the_zone_move_performer() {
    for mut effect in [change_zone(Zone::Exile), change_zone_all(Zone::Exile)] {
        match &mut effect {
            Effect::ChangeZone { actor, .. } | Effect::ChangeZoneAll { actor, .. } => {
                *actor = subject(TargetFilter::TriggeringPlayer);
            }
            _ => unreachable!(),
        }
        each_target_filter_mut(&mut effect, &mut |filter| {
            if *filter == TargetFilter::TriggeringPlayer {
                *filter = TargetFilter::PostReplacementDamageTarget;
            }
        });
        assert_eq!(
            actor_of(&effect),
            &subject(TargetFilter::PostReplacementDamageTarget)
        );
    }
    // The controller default carries no filter and is left alone.
    let mut effect = change_zone(Zone::Exile);
    each_target_filter_mut(&mut effect, &mut |filter| {
        *filter = TargetFilter::PostReplacementDamageTarget;
    });
    assert_eq!(actor_of(&effect), &LibraryInstructionActor::Controller);
}

/// CR 608.2c: inside a "for each opponent who doesn't" body, "that
/// player" is the scoped opponent, so a zone move's performer is rebound with
/// the body's other player references. Synthetic actor: no printed decline
/// body parses to a subject-form zone move today.
#[test]
fn decline_body_rebind_reaches_the_zone_move_performer() {
    for mut effect in [change_zone(Zone::Exile), change_zone_all(Zone::Exile)] {
        match &mut effect {
            Effect::ChangeZone { actor, .. } | Effect::ChangeZoneAll { actor, .. } => {
                *actor = subject(TargetFilter::TriggeringPlayer);
            }
            _ => unreachable!(),
        }
        super::lower::rebind_decline_body_recipient(&mut effect);
        assert_eq!(actor_of(&effect), &subject(TargetFilter::ScopedPlayer));
    }
}
