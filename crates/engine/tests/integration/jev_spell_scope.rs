//! Ordinary public spell ownership with genuine host-cost and regeneration controls.

use super::jev_token_followup::{assert_created, board, ORDINARY_CONSTRUCTION};
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::{oracle_nom::primitives::parse_mana_cost, parse_oracle_text};
use engine::types::ability::{Effect, EffectKind, PerpetualModification, ShieldKind};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;
use nom::{combinator::all_consuming, Parser};

const FEUDKILLER: &str = "You gain 10 life. Then if you have more life than an opponent, create a 5/5 white Giant Warrior creature token.";
const PULSE: &str = "You gain 4 life. Then if an opponent has more life than you, return Pulse of the Fields to its owner's hand.";
const SURVIVAL: &str = "You gain 2 life. Then if you have more life than an opponent, draw a card.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
const WHITE_SUN: &str = "You gain X life. Create X 1/1 colorless Phyrexian Mite artifact creature tokens with toxic 1 and \"This token can't block.\" If X is 5 or more, destroy all other creatures. (Players dealt combat damage by a creature with toxic 1 also get a poison counter.)";
const CHOICE: &str = "Seek two cards. You may shuffle them into your library. If you do, seek two cards.\nYou have no maximum hand size for the rest of the game.";
const DISCOVER: &str = "Seek three nonland cards, then nonland cards in your hand perpetually gain \"This spell costs {1} less to cast.\"";
const TEFERI: &str = "Counter target spell. Each card in its controller's graveyard, hand, and library with the same name as that spell perpetually gains \"This spell costs {2} more to cast.\"";

#[test]
fn refused_standing_cast_permission_preserves_its_whole_rarity_restriction() {
    const ORACLE: &str = "You may look at the top card of your library any time.\nYou may cast common spells from the top of your library.\nGuests you control get +1/+1.";
    let parsed = parse_oracle_text(
        ORACLE,
        "Lila, Hospitality Hostess",
        &[],
        &["Creature".into()],
        &["Elf".into(), "Employee".into()],
    );
    assert_eq!(
        parsed.statics.len(),
        2,
        "independent complete standing abilities survive: {parsed:#?}"
    );
    assert_eq!(parsed.abilities.len(), 1, "{parsed:#?}");
    assert_eq!(
        parsed.abilities[0].effect.unimplemented_description(),
        Some("You may cast common spells from the top of your library.")
    );
    assert!(
        parsed.abilities[0].sub_ability.is_none(),
        "a partial cast permission must not survive beneath the whole refusal"
    );
}

#[test]
fn preserved_numeric_block_limit_allows_one_blocker_and_refuses_two() {
    const ROCKSTEADY: &str = "Rocksteady can't be blocked by more than one creature.\nBoars you control can't be blocked by more than one creature.\nForestcycling {2} ({2}, Discard this card: Search your library for a Forest card, reveal it, put it into your hand, then shuffle.)";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rocksteady = scenario
        .add_creature_from_oracle(P0, "Rocksteady, Crash Courser", 7, 7, ROCKSTEADY)
        .with_subtypes(vec!["Boar", "Mutant"])
        .id();
    let boar = scenario
        .add_creature(P0, "Other Boar", 2, 2)
        .with_subtypes(vec!["Boar"])
        .id();
    let first = scenario.add_creature(P1, "First legal blocker", 1, 3).id();
    let second = scenario.add_creature(P1, "Second legal blocker", 1, 3).id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (rocksteady, AttackTarget::Player(P1)),
            (boar, AttackTarget::Player(P1)),
        ])
        .unwrap();
    runner.advance_to_phase(Phase::DeclareBlockers);
    // The numeric restriction is neither a broad creature-type exclusion nor
    // unblockable: one ordinary creature can legally block each attacker.
    assert!(runner
        .declare_blockers(&[(first, rocksteady), (second, rocksteady)])
        .is_err());
    runner
        .declare_blockers(&[(first, rocksteady), (second, boar)])
        .unwrap();
    runner.combat_damage();
    assert_eq!(runner.state().players[1].life, 20);
    assert_spell_complete(runner.state());
}

#[test]
fn preserved_definite_creature_slot_pumps_only_the_announced_fighter() {
    for (name, oracle, cost, colors, generic, power, instant) in [
        ("Tail Swipe", "Choose target creature you control and target creature you don't control. If you cast this spell during your main phase, the creature you control gets +1/+1 until end of turn. Then those creatures fight each other. (Each deals damage equal to its power to the other.)", "{G}", vec![ManaType::Green], 0, 3, true),
        ("Joust", "Choose target creature you control and target creature you don't control. The creature you control gets +2/+1 until end of turn if it's a Knight. Then those creatures fight each other. (Each deals damage equal to its power to the other.)", "{1}{R}", vec![ManaType::Red], 1, 4, false),
    ] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, spell_pool(generic, &colors));
        let chosen = scenario.add_creature(P0, "Chosen Knight", 2, 5).with_subtypes(vec!["Knight"]).id();
        let bystander = scenario.add_creature(P0, "Other Knight", 2, 5).with_subtypes(vec!["Knight"]).id();
        let opponent = scenario.add_creature(P1, "Other fighter", 3, 8).id();
        let spell = scenario.add_spell_to_hand_from_oracle(P0, name, instant, oracle).with_mana_cost(printed_cost(cost)).id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).target_objects(&[chosen, opponent]).resolve();
        // CR 608.2c: the definite description reads the declared friendly slot.
        assert_eq!(outcome.state().objects[&chosen].power, Some(power));
        assert_eq!(outcome.state().objects[&opponent].damage_marked, power as u32);
        assert_eq!(outcome.state().objects[&bystander].power, Some(2));
        assert_eq!(outcome.state().objects[&bystander].damage_marked, 0);
        assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
        assert_spell_complete(outcome.state());
    }
}

#[test]
fn preserved_singular_damage_keeps_its_source_when_the_recipient_is_announced() {
    const AMBUSCADE: &str = "Target creature you control gets +1/+0 until end of turn. It deals damage equal to its power to target creature an opponent controls.";
    for remove_source in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, spell_pool(2, &[ManaType::Green]));
        scenario.with_mana_pool(P1, spell_pool(0, &[ManaType::Blue]));
        let source = scenario
            .add_creature(P0, "Lifelink source", 2, 4)
            .with_keyword(Keyword::Lifelink)
            .id();
        let recipient = scenario.add_creature(P1, "Damage recipient", 7, 8).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Ambuscade", true, AMBUSCADE)
            .with_mana_cost(printed_cost("{2}{G}"))
            .id();
        let response = scenario
            .add_spell_to_hand_from_oracle(
                P1,
                "Unsummon",
                true,
                "Return target creature to its owner's hand.",
            )
            .with_mana_cost(printed_cost("{U}"))
            .id();
        let mut runner = scenario.build();
        runner
            .cast(spell)
            .target_objects(&[source, recipient])
            .commit();
        if remove_source {
            if runner.state().priority_player == P0 {
                runner.act(GameAction::PassPriority).unwrap();
            }
            runner.cast(response).target_object(source).resolve();
        } else {
            runner.advance_until_stack_empty();
        }
        // CR 120.1 + CR 608.2b: the recipient never becomes the damage source;
        // a source removed in response cannot deal damage using its old power.
        assert_eq!(
            runner.state().objects[&recipient].damage_marked,
            if remove_source { 0 } else { 3 }
        );
        assert_eq!(
            runner.state().players[0].life,
            if remove_source { 20 } else { 23 }
        );
        assert_eq!(runner.state().objects[&source].damage_marked, 0);
        assert_eq!(
            runner.state().objects[&source].zone,
            if remove_source {
                Zone::Hand
            } else {
                Zone::Battlefield
            }
        );
        assert_spell_complete(runner.state());
    }
}

#[test]
fn preserved_named_permanent_damage_uses_the_countered_permanent_not_the_cast_spell() {
    const RIPLEY: &str = "Whenever you cast your third spell each turn, put a +1/+1 counter on Captain Ripley Vance, then it deals damage equal to its power to any target.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let captain = scenario
        .add_creature_from_oracle(P0, "Captain Ripley Vance", 3, 2, RIPLEY)
        .id();
    let recipient = scenario.add_creature(P1, "Ripley recipient", 9, 10).id();
    let spells: Vec<_> = (0..3)
        .map(|_| {
            scenario
                .add_creature_to_hand_from_oracle(P0, "Ornithopter", 0, 2, "Flying")
                .as_artifact_creature()
                .with_mana_cost(printed_cost("{0}"))
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for spell in &spells[..2] {
        runner.cast(*spell).resolve();
        assert_eq!(runner.state().objects[&recipient].damage_marked, 0);
    }
    let outcome = runner.cast(spells[2]).target_object(recipient).resolve();
    assert_eq!(outcome.state().objects[&captain].power, Some(4));
    assert_eq!(outcome.state().objects[&recipient].damage_marked, 4);
    assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::DamageDealt { source_id, target: engine::types::ability::TargetRef::Object(id), amount: 4, .. } if *source_id == captain && *id == recipient)));
    assert!(outcome.events().iter().all(|event| !matches!(event, GameEvent::DamageDealt { source_id, .. } if spells.contains(source_id))));
    assert_spell_complete(outcome.state());
}

#[test]
fn preserved_counter_threshold_damage_keeps_the_artifact_source_across_the_cast_event() {
    const RECORDING: &str = "Whenever you cast an instant or sorcery spell, put a time counter on this artifact. Then if there are seven or more time counters on it, remove those counters and it deals 20 damage to you.\n{T}: When you next cast an instant or sorcery spell this turn, copy that spell. You may choose new targets for the copy.";
    let mut scenario = GameScenario::new();
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_life(P0, 40)
        .with_life(P1, 40);
    scenario.with_mana_pool(P0, spell_pool(0, &[ManaType::Red; 7]));
    let recording = scenario
        .add_artifact_from_oracle(P0, "Cursed Recording", RECORDING)
        .id();
    let spells: Vec<_> = (0..7)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(
                    P0,
                    "Shock",
                    true,
                    "Shock deals 2 damage to any target.",
                )
                .with_mana_cost(printed_cost("{R}"))
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    for spell in &spells[..6] {
        runner.cast(*spell).target_player(P1).resolve();
        assert_eq!(runner.state().players[0].life, 40);
    }
    let outcome = runner.cast(spells[6]).target_player(P1).resolve();
    assert_eq!(outcome.state().players[0].life, 20);
    assert_eq!(outcome.state().players[1].life, 26);
    assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::DamageDealt { source_id, target: engine::types::ability::TargetRef::Player(P0), amount: 20, .. } if *source_id == recording)));
    assert!(outcome.state().objects[&recording]
        .counters
        .get(&engine::types::counter::CounterType::Time)
        .is_none_or(|count| *count == 0));
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(outcome.state());
}

#[test]
fn preserved_spell_self_damage_keeps_the_spell_as_source_after_the_victim_reference() {
    const LESSON: &str = "Kicker {4} (You may pay an additional {4} as you cast this spell.)\nFirebending Lesson deals 2 damage to target creature. If this spell was kicked, it deals 5 damage to that creature instead.";
    for kicked in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, spell_pool(if kicked { 4 } else { 0 }, &[ManaType::Red]));
        let recipient = scenario.add_creature(P1, "Lesson recipient", 7, 10).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Firebending Lesson", true, LESSON)
            .with_mana_cost(printed_cost("{R}"))
            .id();
        let mut runner = scenario.build();
        let cast = runner.cast(spell).target_object(recipient);
        let outcome = if kicked {
            cast.accept_optional().resolve()
        } else {
            cast.decline_optional().resolve()
        };
        let expected = if kicked { 5 } else { 2 };
        assert_eq!(outcome.state().objects[&recipient].damage_marked, expected);
        assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::DamageDealt { source_id, target: engine::types::ability::TargetRef::Object(id), amount, .. } if *source_id == spell && *id == recipient && *amount == expected)));
        assert!(outcome.events().iter().all(|event| !matches!(event, GameEvent::DamageDealt { source_id, .. } if *source_id == recipient)));
        assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
        outcome.assert_zone(&[spell], Zone::Graveyard);
        assert_spell_complete(outcome.state());
    }
}

#[test]
fn preserved_keyword_replication_checks_each_keyword_independently_at_resolution() {
    use engine::game::keywords::object_has_effective_keyword_kind;
    use engine::types::keywords::KeywordKind;
    const ORATOR: &str = "Vigilance\nWhenever this creature attacks, it gains flying until end of turn if you control a creature with flying. The same is true for first strike, double strike, deathtouch, indestructible, lifelink, menace, and trample.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let orator = scenario
        .add_creature_from_oracle(P0, "Thunderous Orator", 2, 2, ORATOR)
        .id();
    scenario
        .add_creature(P0, "Trample witness", 3, 3)
        .with_keyword(Keyword::Trample);
    scenario
        .add_creature(P1, "Opponent flying witness", 1, 1)
        .with_keyword(Keyword::Flying);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(orator, AttackTarget::Player(P1))])
        .unwrap();
    runner.advance_until_stack_empty();
    // CR 608.2c + CR 611.2c: the missing flying witness cannot suppress trample,
    // and the opponent's flyer cannot grant flying to the attacking Orator.
    assert!(object_has_effective_keyword_kind(
        runner.state(),
        orator,
        KeywordKind::Trample
    ));
    assert!(!object_has_effective_keyword_kind(
        runner.state(),
        orator,
        KeywordKind::Flying
    ));
    assert!(!object_has_effective_keyword_kind(
        runner.state(),
        orator,
        KeywordKind::DoubleStrike
    ));
    assert_spell_complete(runner.state());
}

#[test]
fn preserved_plural_affected_subject_reads_the_nearer_mass_population() {
    use engine::game::keywords::object_has_effective_keyword_kind;
    use engine::types::keywords::KeywordKind;
    const ARDBERT: &str = "Whenever you cast a white spell, put a +1/+1 counter on each legendary creature you control. They gain vigilance until end of turn.\nWhenever you cast a black spell, put a +1/+1 counter on each legendary creature you control. They gain menace until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, spell_pool(0, &[ManaType::White]));
    let ardbert = scenario
        .add_creature_from_oracle(P0, "Ardbert, Warrior of Darkness", 2, 2, ARDBERT)
        .as_legendary()
        .id();
    let other = scenario
        .add_creature(P0, "Own legendary recipient", 3, 3)
        .as_legendary()
        .id();
    let own_decoy = scenario.add_creature(P0, "Own ordinary decoy", 3, 3).id();
    let foreign = scenario
        .add_creature(P1, "Foreign legendary decoy", 3, 3)
        .as_legendary()
        .id();
    let spell = scenario.add_spell_to_hand_from_oracle(P0, "Healing Salve", true, "Choose one —\n• Target player gains 3 life.\n• Prevent the next 3 damage that would be dealt to any target this turn.").with_color(vec![ManaColor::White]).with_mana_cost(printed_cost("{W}")).id();
    let mut runner = scenario.build();
    runner.cast(spell).modes(&[0]).target_player(P0).resolve();
    for (id, power) in [(ardbert, 3), (other, 4)] {
        assert_eq!(runner.state().objects[&id].power, Some(power));
        assert!(object_has_effective_keyword_kind(
            runner.state(),
            id,
            KeywordKind::Vigilance
        ));
        assert!(!object_has_effective_keyword_kind(
            runner.state(),
            id,
            KeywordKind::Menace
        ));
    }
    for decoy in [own_decoy, foreign] {
        assert_eq!(runner.state().objects[&decoy].power, Some(3));
        assert!(!object_has_effective_keyword_kind(
            runner.state(),
            decoy,
            KeywordKind::Vigilance
        ));
    }
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(runner.state());
}

#[test]
fn preserved_cohort_count_selects_two_spells_and_disposes_only_the_other_revealed_cards() {
    const PIECES: &str = "Reveal the top five cards of your library. Put up to two instant and/or sorcery cards from among them into your hand and the rest into your graveyard.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, spell_pool(2, &[ManaType::Blue]));
    let outside = scenario.add_land_to_library_top(P0, "Unrevealed land").id();
    let land_a = scenario.add_land_to_library_top(P0, "Revealed land A").id();
    let land_b = scenario.add_land_to_library_top(P0, "Revealed land B").id();
    let sorcery = scenario
        .add_spell_to_library_top(P0, "Selected sorcery", false)
        .id();
    let instant = scenario
        .add_spell_to_library_top(P0, "Selected instant", true)
        .id();
    let unselected = scenario
        .add_spell_to_library_top(P0, "Unselected instant", true)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Pieces of the Puzzle", false, PIECES)
        .with_mana_cost(printed_cost("{2}{U}"))
        .id();
    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .effect_zone(&[instant, sorcery])
        .resolve();
    // CR 608.2c: selection is capped at two within this revealed cohort.
    outcome.assert_zone(&[instant, sorcery], Zone::Hand);
    outcome.assert_zone(&[unselected, land_a, land_b], Zone::Graveyard);
    outcome.assert_zone(&[outside], Zone::Library);
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(outcome.state());
}

#[test]
fn preserved_conjunct_duration_expires_the_inherited_first_strike() {
    use engine::game::keywords::object_has_effective_keyword_kind;
    use engine::types::keywords::KeywordKind;
    const LEGION: &str =
        "Until end of turn, double target creature's power and it gains first strike.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    scenario.with_library_top(P0, &["Next draw P0", "Reserve P0"]);
    scenario.with_library_top(P1, &["Next draw P1", "Reserve P1"]);
    scenario.with_mana_pool(P0, spell_pool(1, &[ManaType::White]));
    let target = scenario.add_creature(P0, "Leadership target", 3, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Legion Leadership", true, LEGION)
        .with_mana_cost(printed_cost("{1}{R/W}"))
        .id();
    let mut runner = scenario.build();
    runner.cast(spell).target_object(target).resolve();
    assert_eq!(runner.state().objects[&target].power, Some(6));
    assert!(object_has_effective_keyword_kind(
        runner.state(),
        target,
        KeywordKind::FirstStrike
    ));
    assert_spell_complete(runner.state());
    runner.advance_to_upkeep();
    // CR 611.2a: both conjuncts end at the same printed window.
    assert_eq!(runner.state().objects[&target].power, Some(3));
    assert!(!object_has_effective_keyword_kind(
        runner.state(),
        target,
        KeywordKind::FirstStrike
    ));
    assert_spell_complete(runner.state());
}

#[test]
fn preserved_granted_conditional_trigger_uses_its_otherwise_branch_before_the_monarch_pump() {
    const REGAL: &str = "Sliver creatures you control have \"When this creature enters, Slivers you control get +1/+1 until end of turn if you're the monarch. Otherwise, you become the monarch.\"";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, spell_pool(4, &[ManaType::White]));
    let own_decoy = scenario.add_creature(P0, "Own nonsliver", 3, 3).id();
    let opponent_decoy = scenario
        .add_creature(P1, "Opponent Sliver", 2, 2)
        .with_subtypes(vec!["Sliver"])
        .id();
    let regal = scenario
        .add_creature_to_hand_from_oracle(P0, "Regal Sliver", 3, 3, REGAL)
        .with_subtypes(vec!["Sliver"])
        .with_mana_cost(printed_cost("{3}{W}"))
        .id();
    let metallic = scenario
        .add_creature_to_hand_from_oracle(P0, "Metallic Sliver", 1, 1, "")
        .as_artifact_creature()
        .with_subtypes(vec!["Sliver"])
        .with_mana_cost(printed_cost("{1}"))
        .id();
    let mut runner = scenario.build();
    runner.cast(regal).resolve();
    assert_eq!(runner.state().monarch, Some(P0));
    assert_eq!(runner.state().objects[&regal].power, Some(3));
    assert_spell_complete(runner.state());
    runner.cast(metallic).resolve();
    assert_eq!(runner.state().objects[&regal].power, Some(4));
    assert_eq!(runner.state().objects[&metallic].power, Some(2));
    assert_eq!(runner.state().objects[&own_decoy].power, Some(3));
    assert_eq!(runner.state().objects[&opponent_decoy].power, Some(2));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(runner.state());
}

#[test]
fn preserved_temporary_cost_grant_snapshots_life_and_retains_its_spell_color_scope() {
    const WILL: &str = "Vigilance\n{T}: Spells you cast this turn that are white and/or blue cost {X} less to cast, where X is the amount of life you gained this turn. Activate only as a sorcery.";
    const SALVE: &str = "Choose one —\n• Target player gains 3 life.\n• Prevent the next 3 damage that would be dealt to any target this turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        spell_pool(
            5,
            &[
                ManaType::White,
                ManaType::White,
                ManaType::Blue,
                ManaType::Blue,
                ManaType::Red,
            ],
        ),
    );
    scenario.with_library_top(
        P0,
        &[
            "Opportunity A",
            "Opportunity B",
            "Opportunity C",
            "Opportunity D",
            "Reserve",
        ],
    );
    let will = scenario
        .add_creature_from_oracle(P0, "Will, Scion of Peace", 2, 4, WILL)
        .with_color(vec![ManaColor::White, ManaColor::Blue])
        .id();
    let first_salve = scenario
        .add_spell_to_hand_from_oracle(P0, "Healing Salve", true, SALVE)
        .with_color(vec![ManaColor::White])
        .with_mana_cost(printed_cost("{W}"))
        .id();
    let later_salve = scenario
        .add_spell_to_hand_from_oracle(P0, "Healing Salve", true, SALVE)
        .with_color(vec![ManaColor::White])
        .with_mana_cost(printed_cost("{W}"))
        .id();
    let outside_color = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Lava Axe",
            false,
            "Lava Axe deals 5 damage to target player or planeswalker.",
        )
        .with_color(vec![ManaColor::Red])
        .with_mana_cost(printed_cost("{4}{R}"))
        .id();
    let reduced = scenario
        .add_spell_to_hand_from_oracle(P0, "Opportunity", true, "Target player draws four cards.")
        .with_color(vec![ManaColor::Blue])
        .with_mana_cost(printed_cost("{4}{U}{U}"))
        .id();
    let mut runner = scenario.build();
    runner
        .cast(first_salve)
        .modes(&[0])
        .target_player(P0)
        .resolve();
    runner.activate(will, 0).resolve();
    assert!(runner.state().objects[&will].tapped);
    runner
        .cast(later_salve)
        .modes(&[0])
        .target_player(P0)
        .resolve();
    assert_eq!(runner.state().players[0].life, 26);
    // Will's ruling fixes X at resolution (three), and red is outside the
    // printed white/blue union. Later life gain cannot make X six.
    runner.cast(outside_color).target_player(P1).resolve();
    assert_eq!(runner.state().players[1].life, 15);
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);
    let outcome = runner.cast(reduced).target_player(P0).resolve();
    outcome.assert_hand_drawn(P0, 4);
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(outcome.state());
}

#[test]
fn preserved_hand_choice_keeps_the_targeted_opponents_hand_and_chosen_identity() {
    const PRISON: &str = "Imprint — When this artifact enters, you may have target player reveal their hand. If you do, choose a nonland card from it and exile that card.\nWhenever a player casts a spell that shares a color or mana value with the exiled card, this artifact deals 2 damage to that player.";
    for accept in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, spell_pool(5, &[]));
        let own = scenario
            .add_spell_to_hand(P0, "Own nonland decoy", true)
            .id();
        let first = scenario
            .add_spell_to_hand(P1, "Opponent first nonland", true)
            .id();
        let second = scenario
            .add_spell_to_hand(P1, "Opponent chosen nonland", false)
            .id();
        let land = scenario.add_land_to_hand(P1, "Opponent land decoy").id();
        let prison = scenario
            .add_artifact_to_hand_from_oracle(P0, "Thought Prison", PRISON)
            .with_mana_cost(printed_cost("{5}"))
            .id();
        let mut runner = scenario.build();
        let cast = runner.cast(prison).target_player(P1).commit();
        if accept {
            cast.accept_optional().resolve();
        } else {
            cast.decline_optional().resolve();
        }
        if accept {
            let WaitingFor::RevealChoice {
                player,
                cards,
                optional,
                ..
            } = &runner.state().waiting_for
            else {
                panic!(
                    "required opponent hand choice: {:?}",
                    runner.state().waiting_for
                )
            };
            assert_eq!(*player, P0);
            assert!(!*optional);
            assert!(cards.contains(&first) && cards.contains(&second));
            assert!(!cards.contains(&own) && !cards.contains(&land));
            runner
                .act(GameAction::SelectCards {
                    cards: vec![second],
                })
                .unwrap();
            runner.advance_until_stack_empty();
        }
        // CR 608.2c: only the chosen card from the targeted hand is exiled.
        assert_eq!(
            runner.state().objects[&second].zone,
            if accept { Zone::Exile } else { Zone::Hand }
        );
        for decoy in [own, first, land] {
            assert_eq!(runner.state().objects[&decoy].zone, Zone::Hand);
        }
        assert_eq!(runner.state().objects[&prison].zone, Zone::Battlefield);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_spell_complete(runner.state());
    }
}

#[test]
fn preserved_delayed_zone_gate_reads_the_sought_card_at_the_later_end_step() {
    const CALL: &str = "Choose one —\n• Seek a Kithkin card. At the beginning of the next end step, if that card is in your hand, exile it.\n• Creature cards in your hand perpetually get +1/+1.";
    for discard_before_end in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PostCombatMain);
        scenario.with_mana_pool(P0, spell_pool(0, &[ManaType::White]));
        scenario.with_mana_pool(P1, spell_pool(0, &[ManaType::Black]));
        let sought = scenario
            .add_spell_to_library_top(P0, "Unique Kithkin", false)
            .as_creature()
            .with_subtypes(vec!["Kithkin"])
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Thoughtweft's Call", false, CALL)
            .with_mana_cost(printed_cost("{W}"))
            .id();
        let discard = scenario.add_spell_to_hand_from_oracle(P1, "Funeral Charm", true, "Choose one —\n• Target player discards a card.\n• Target creature gets +2/-1 until end of turn.\n• Target creature gains swampwalk until end of turn. (It can\'t be blocked as long as defending player controls a Swamp.)").with_mana_cost(printed_cost("{B}")).id();
        let mut runner = scenario.build();
        runner.cast(spell).modes(&[0]).resolve();
        assert_eq!(runner.state().objects[&sought].zone, Zone::Hand);
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        // This reach guard distinguishes the printed live hand condition from
        // the separate object-occurrence protection exercised by the move below.
        let gate = runner.state().delayed_triggers[0]
            .ability
            .condition
            .as_ref()
            .expect("the later payload owns the live zone condition");
        assert!(
            matches!(gate, engine::types::ability::AbilityCondition::TargetMatchesFilter { filter: engine::types::ability::TargetFilter::Typed(filter), use_lki: false, subject_slot: None } if filter.controller == Some(engine::types::ability::ControllerRef::You) && filter.properties.contains(&engine::types::ability::FilterProp::InZone { zone: Zone::Hand })),
            "{gate:#?}"
        );
        assert_spell_complete(runner.state());
        if discard_before_end {
            if runner.state().priority_player == P0 {
                runner.act(GameAction::PassPriority).unwrap();
            }
            runner
                .cast(discard)
                .modes(&[0])
                .target_player(P0)
                .discard(&[sought])
                .resolve();
            assert_eq!(runner.state().objects[&sought].zone, Zone::Graveyard);
        }
        runner.advance_to_end_step();
        runner.advance_until_stack_empty();
        // CR 603.7c + CR 400.7: a moved object is outside the printed hand gate.
        assert_eq!(
            runner.state().objects[&sought].zone,
            if discard_before_end {
                Zone::Graveyard
            } else {
                Zone::Exile
            }
        );
        assert_eq!(
            runner.state().objects[&discard].zone,
            if discard_before_end {
                Zone::Graveyard
            } else {
                Zone::Hand
            }
        );
        assert_spell_complete(runner.state());
    }
}

fn printed_cost(text: &str) -> ManaCost {
    all_consuming(parse_mana_cost).parse(text).unwrap().1
}

fn spell_pool(generic: usize, colors: &[ManaType]) -> Vec<ManaUnit> {
    colors
        .iter()
        .copied()
        .chain(std::iter::repeat_n(ManaType::Colorless, generic))
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn assert_spell_complete(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "{:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "ordinary stack remains");
    assert!(
        state.resolution_stack.is_empty(),
        "suspended resolution remains"
    );
}

#[test]
fn numeric_life_chains_compare_after_gain() {
    let p2 = PlayerId(2);
    for opponents in [[15, 100], [20, 100], [21, 100]] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_life(P0, 10)
            .with_life(P1, opponents[0])
            .with_life(p2, opponents[1]);
        scenario.with_mana_pool(P0, spell_pool(4, &[ManaType::White, ManaType::White]));
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Feudkiller's Verdict", false, FEUDKILLER)
            .with_mana_cost(printed_cost("{4}{W}{W}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        // CR 119.3 + CR 608.2c/h: the strict live Min comparison follows gain10.
        outcome.assert_life_delta(P0, 10);
        outcome.assert_life_delta(P1, 0);
        outcome.assert_life_delta(p2, 0);
        let tokens: Vec<_> = outcome
            .state()
            .objects
            .values()
            .filter(|object| object.is_token && object.zone == Zone::Battlefield)
            .collect();
        assert_eq!(tokens.len(), usize::from(opponents[0] < 20));
        for token in tokens {
            assert_eq!(token.controller, P0);
            assert_eq!((token.power, token.toughness), (Some(5), Some(5)));
            assert_eq!(token.color, vec![ManaColor::White]);
            assert!(token.card_types.core_types.contains(&CoreType::Creature));
            for subtype in ["Giant", "Warrior"] {
                assert!(token
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(subtype)));
            }
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        outcome.assert_zone(&[spell], Zone::Graveyard);
        assert_spell_complete(runner.state());
    }
    for opponents in [[13, 15], [13, 14], [12, 13]] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_life(P0, 10)
            .with_life(P1, opponents[0])
            .with_life(p2, opponents[1]);
        scenario.with_mana_pool(P0, spell_pool(1, &[ManaType::White, ManaType::White]));
        let decoy = scenario
            .add_spell_to_hand_from_oracle(P0, "Pulse of the Fields", true, PULSE)
            .id();
        let grave_decoy = scenario
            .add_spell_to_graveyard(P0, "Pulse of the Fields", true)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Pulse of the Fields", true, PULSE)
            .with_mana_cost(printed_cost("{1}{W}{W}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        // CR 608.2c/h + CR 608.2n: only this resolving source returns if Max>14.
        outcome.assert_life_delta(P0, 4);
        outcome.assert_life_delta(P1, 0);
        outcome.assert_life_delta(p2, 0);
        outcome.assert_zone(
            &[spell],
            if opponents[1] > 14 {
                Zone::Hand
            } else {
                Zone::Graveyard
            },
        );
        outcome.assert_zone(&[decoy], Zone::Hand);
        outcome.assert_zone(&[grave_decoy], Zone::Graveyard);
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_spell_complete(runner.state());
    }
}

#[test]
fn survival_cache_full_rebound_retains_ordered_gain_draw() {
    // Direct strict/equality Min controls retain the complete Rebound source.
    for opponent in [11, 12] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_life(P0, 10)
            .with_life(P1, opponent)
            .with_life(PlayerId(2), 30);
        scenario.with_library_top(P0, &["Draw witness", "Later witness"]);
        scenario.with_mana_pool(P0, spell_pool(2, &[ManaType::White]));
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Survival Cache", false, SURVIVAL)
            .with_keyword(Keyword::Rebound)
            .with_mana_cost(printed_cost("{2}{W}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        // CR 119.3 + CR 608.2c/h: equality is not more life.
        outcome.assert_life_delta(P0, 2);
        outcome.assert_hand_drawn(P0, i64::from(opponent == 11));
        outcome.assert_zone(&[spell], Zone::Exile);
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        assert_spell_complete(runner.state());
    }
    for accept in [true, false] {
        let mut scenario = GameScenario::new_n_player(3, 42);
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_life(P0, 10)
            .with_life(P1, 13)
            .with_life(PlayerId(2), 30);
        for player in [P0, P1, PlayerId(2)] {
            scenario.with_library_top(
                player,
                &[
                    "Upkeep draw witness",
                    "Rebound draw witness",
                    "Reserve draw witness",
                ],
            );
        }
        scenario.with_mana_pool(P0, spell_pool(2, &[ManaType::White]));
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Survival Cache", false, SURVIVAL)
            .with_keyword(Keyword::Rebound)
            .with_mana_cost(printed_cost("{2}{W}"))
            .id();
        let mut runner = scenario.build();
        let first = runner.cast(spell).resolve();
        first.assert_life_delta(P0, 2);
        first.assert_hand_drawn(P0, 0);
        first.assert_zone(&[spell], Zone::Exile);
        assert_eq!(runner.state().delayed_triggers.len(), 1);
        assert!(runner.state().objects[&spell]
            .casting_permissions
            .is_empty());
        assert_spell_complete(runner.state());
        let first_turn = runner.state().turn_number;
        // CR 702.88a/b + CR 608.2g: ordinary turn actions produce the real offer.
        for _ in 0..300 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ) {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                ref other => panic!("unexpected turn prompt: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        assert!(runner.state().turn_number > first_turn);
        assert_eq!(runner.state().active_player, P0);
        assert_eq!(runner.state().phase, Phase::Upkeep);
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == spell)
        );
        let options = engine::ai_support::legal_actions(runner.state());
        for decision in [true, false] {
            assert!(options.iter().any(|action| matches!(action, GameAction::DecideOptionalEffect { accept } if *accept == decision)));
        }
        let hand_before = runner.state().players[0].hand.len();
        let draw_witness = runner.state().players[0].library[0];
        let mut events = runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap()
            .events;
        assert_eq!(
            runner.state().objects[&spell].zone,
            if accept { Zone::Stack } else { Zone::Exile }
        );
        for _ in 0..20 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_eq!(runner.state().players[0].life, if accept { 14 } else { 12 });
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + usize::from(accept)
        );
        assert_eq!(
            runner.state().objects[&draw_witness].zone,
            if accept { Zone::Hand } else { Zone::Library }
        );
        assert_eq!(
            runner.state().objects[&spell].zone,
            if accept { Zone::Graveyard } else { Zone::Exile }
        );
        assert!(runner.state().delayed_triggers.is_empty());
        assert!(runner.state().objects[&spell]
            .casting_permissions
            .is_empty());
        assert_eq!(events.iter().filter(|event| matches!(event, GameEvent::EffectResolved { kind: EffectKind::GainLife, source_id, .. } if *source_id == spell)).count(), usize::from(accept));
        assert_spell_complete(runner.state());
    }
}

#[test]
fn white_suns_twilight_full_source_x0_x3_x5() {
    for x in [0, 3, 5] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, spell_pool(x, &[ManaType::White, ManaType::White]));
        let victim = scenario.add_creature(P1, "Other creature", 2, 2).id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "White Sun's Twilight", false, WHITE_SUN)
            .with_mana_cost(printed_cost("{X}{W}{W}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).x(x as u32).resolve();
        // CR 601.2b + CR 111.3 + CR 119.3: paid X and token-owned text survive.
        outcome.assert_life_delta(P0, x as i32);
        outcome.assert_zone(
            &[victim],
            if x >= 5 {
                Zone::Graveyard
            } else {
                Zone::Battlefield
            },
        );
        let mites: Vec<_> = runner
            .state()
            .objects
            .values()
            .filter(|object| object.is_token && object.zone == Zone::Battlefield)
            .collect();
        assert_eq!(mites.len(), x);
        for mite in mites {
            assert_eq!(mite.controller, P0);
            assert_eq!((mite.power, mite.toughness), (Some(1), Some(1)));
            assert!(mite.color.is_empty());
            for core in [CoreType::Artifact, CoreType::Creature] {
                assert!(mite.card_types.core_types.contains(&core));
            }
            for subtype in ["Phyrexian", "Mite"] {
                assert!(mite
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(subtype)));
            }
            assert!(mite.keywords.contains(&Keyword::Toxic(1)));
            assert!(mite
                .static_definitions
                .iter_unchecked()
                .any(|definition| definition.mode == StaticMode::CantBlock));
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_spell_complete(runner.state());
    }
}

#[test]
fn choice_of_fortunes_complete_optional_and_persistent_rule() {
    for (available, accept) in [(6, true), (6, false), (1, true), (0, false)] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, spell_pool(2, &[ManaType::Blue]));
        let existing: Vec<_> = (0..9)
            .map(|index| scenario.add_card_to_hand(P0, &format!("Preexisting {index}")))
            .collect();
        let foreign: Vec<_> = (0..9)
            .map(|index| scenario.add_card_to_hand(P1, &format!("Foreign {index}")))
            .collect();
        let library: Vec<_> = (0..available)
            .map(|index| {
                scenario
                    .add_spell_to_library_top(P0, &format!("Seek member {index}"), true)
                    .id()
            })
            .collect();
        scenario.with_library_top(P1, &["Opponent turn draw", "Opponent reserve"]);
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Choice of Fortunes", false, CHOICE)
            .with_mana_cost(printed_cost("{2}{U}"))
            .id();
        let mut runner = scenario.build();
        let initial = runner.cast(spell).commit().stop_at_optional().resolve();
        assert!(
            matches!(initial.final_waiting_for(), WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if *source_id == spell),
            "{:?}",
            initial.final_waiting_for()
        );
        let options = engine::ai_support::legal_actions(runner.state());
        for decision in [true, false] {
            assert!(options.iter().any(|action| matches!(action, GameAction::DecideOptionalEffect { accept } if *accept == decision)));
        }
        let first_set: Vec<_> = library
            .iter()
            .copied()
            .filter(|id| runner.state().objects[id].zone == Zone::Hand)
            .collect();
        assert_eq!(first_set.len(), available.min(2));
        for id in &existing {
            assert_eq!(runner.state().objects[id].zone, Zone::Hand);
        }
        let mut events = initial.events().to_vec();
        events.extend(
            runner
                .act(GameAction::DecideOptionalEffect { accept })
                .unwrap()
                .events,
        );
        for _ in 0..20 {
            if runner.state().stack.is_empty()
                && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            {
                break;
            }
            events.extend(runner.act(GameAction::PassPriority).unwrap().events);
        }
        assert_spell_complete(runner.state());
        assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
        let resolved: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::EffectResolved {
                    kind, source_id, ..
                } if *source_id == spell => Some(*kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            resolved,
            if accept {
                vec![
                    EffectKind::Seek,
                    EffectKind::ChangeZoneAll,
                    EffectKind::Shuffle,
                    EffectKind::Seek,
                    EffectKind::CreateEmblem,
                ]
            } else {
                vec![EffectKind::Seek, EffectKind::CreateEmblem]
            }
        );
        // Digital Seek retains exactly its first set; optional movement has one shuffle.
        for id in &first_set {
            let moved_back = events.iter().any(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Hand), to: Zone::Library, .. } if object_id == id));
            assert_eq!(moved_back, accept);
        }
        let shuffles: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| {
                matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        player_id: P0,
                        action: PlayerActionKind::ShuffledLibrary,
                        ..
                    }
                )
                .then_some(index)
            })
            .collect();
        assert_eq!(shuffles.len(), usize::from(accept));
        let seeks: Vec<_> = events.iter().enumerate().filter_map(|(index, event)| matches!(event, GameEvent::EffectResolved { kind: EffectKind::Seek, source_id, .. } if *source_id == spell).then_some(index)).collect();
        if accept {
            assert!(seeks[0] < shuffles[0] && shuffles[0] < seeks[1]);
            assert!(events.iter().enumerate().filter(|(_, event)| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Hand), to: Zone::Library, .. } if first_set.contains(object_id))).all(|(index, _)| index < shuffles[0]));
        } else {
            for id in &first_set {
                assert_eq!(runner.state().objects[id].zone, Zone::Hand);
            }
        }
        for id in existing.iter().chain(&foreign) {
            assert!(events.iter().all(|event| !matches!(event, GameEvent::ZoneChanged { object_id, .. } if object_id == id)));
        }
        let emblems: Vec<_> = runner
            .state()
            .objects
            .values()
            .filter(|object| object.is_emblem)
            .collect();
        assert_eq!(emblems.len(), 1);
        assert_eq!(emblems[0].controller, P0);
        assert_eq!(emblems[0].zone, Zone::Command);
        let caster_hand = runner.state().players[0].hand.len();
        assert!(caster_hand > 7);
        let casting_turn = runner.state().turn_number;
        // CR 109.5 + CR 402.2 + CR 514.1: actual cleanup distinguishes recipients.
        // No discard may stop P0; P1 must receive its ordinary required choice.
        for _ in 0..180 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DiscardToHandSize { .. }
            ) {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                ref other => panic!("unexpected cleanup route: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        let WaitingFor::DiscardToHandSize {
            player,
            count,
            cards,
        } = runner.state().waiting_for.clone()
        else {
            panic!("opponent must discard: {:?}", runner.state().waiting_for)
        };
        assert_eq!(
            player, P1,
            "constructor-only revert grants every player no maximum"
        );
        assert_eq!(runner.state().active_player, P1);
        assert_eq!(runner.state().phase, Phase::Cleanup);
        assert_eq!(
            cards,
            runner.state().players[1]
                .hand
                .iter()
                .copied()
                .collect::<Vec<_>>()
        );
        assert_eq!(count, cards.len() - 7);
        assert!(count > 0);
        assert_eq!(
            runner.state().players[0].hand.len(),
            caster_hand,
            "caster crossed cleanup without discarding"
        );
        let discarded = cards[..count].to_vec();
        runner
            .act(GameAction::SelectCards {
                cards: discarded.clone(),
            })
            .unwrap();
        assert_eq!(runner.state().players[1].hand.len(), 7);
        for id in discarded {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        assert_spell_complete(runner.state());
        assert!(runner.state().turn_number > casting_turn);
        assert_eq!(
            runner
                .state()
                .objects
                .values()
                .filter(|object| object.is_emblem && object.controller == P0)
                .count(),
            1
        );
    }
}

#[test]
fn hosted_no_max_ends_on_source_departure_and_tower_keeps_mana() {
    for remove in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for index in 0..9 {
            scenario.add_card_to_hand(P0, &format!("Hand member {index}"));
        }
        scenario.with_library_top(P0, &["Next turn draw", "Reserve"]);
        scenario.with_library_top(P1, &["Opponent draw", "Reserve"]);
        let book = scenario
            .add_artifact_to_hand_from_oracle(P0, "Spellbook", "You have no maximum hand size.")
            .with_mana_cost(printed_cost("{0}"))
            .id();
        let bounce = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Labelled synthetic bounce",
                true,
                "Return target artifact to its owner's hand.",
            )
            .with_mana_cost(printed_cost("{0}"))
            .id();
        let mut runner = scenario.build();
        runner
            .cast(book)
            .resolve()
            .assert_zone(&[book], Zone::Battlefield);
        assert_spell_complete(runner.state());
        if remove {
            if runner.state().priority_player != P0 {
                runner.act(GameAction::PassPriority).unwrap();
            }
            runner
                .cast(bounce)
                .target_object(book)
                .resolve()
                .assert_zone(&[book], Zone::Hand);
            assert_spell_complete(runner.state());
        }
        let hand = runner.state().players[0].hand.len();
        for _ in 0..140 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DiscardToHandSize { .. }
            ) || (runner.state().active_player == P1 && runner.state().phase == Phase::Upkeep)
            {
                break;
            }
            let action = match runner.state().waiting_for {
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                    attacks: vec![],
                    bands: vec![],
                },
                WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                    assignments: vec![],
                },
                ref other => panic!("unexpected hosted cleanup: {other:?}"),
            };
            runner.act(action).unwrap();
        }
        if remove {
            let WaitingFor::DiscardToHandSize {
                player: P0,
                count,
                cards,
            } = runner.state().waiting_for.clone()
            else {
                panic!(
                    "source departure must restore cleanup: {:?}",
                    runner.state().waiting_for
                )
            };
            assert_eq!(count, hand - 7);
            assert_eq!(
                cards,
                runner.state().players[0]
                    .hand
                    .iter()
                    .copied()
                    .collect::<Vec<_>>()
            );
            runner
                .act(GameAction::SelectCards {
                    cards: cards[..count].to_vec(),
                })
                .unwrap();
            assert_eq!(runner.state().players[0].hand.len(), 7);
        } else {
            assert_eq!(
                runner.state().active_player,
                P1,
                "live host allowed P0 through cleanup"
            );
            assert_eq!(runner.state().players[0].hand.len(), hand);
        }
        // CR 611.3b: the live hosted rule and independently persistent spell differ.
        assert_spell_complete(runner.state());
    }
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tower = scenario
        .add_land_from_oracle(
            P0,
            "Reliquary Tower",
            "You have no maximum hand size.\n{T}: Add {C}.",
        )
        .id();
    let mut runner = scenario.build();
    runner.activate(tower, 0).resolve();
    assert_eq!(runner.state().players[0].mana_pool.total(), 1);
    assert!(runner.state().objects[&tower].tapped);
    assert_spell_complete(runner.state());
}

#[test]
fn discover_formula_grants_unsought_hand_and_future_paid_cast() {
    for available in [3, 1, 0] {
        let mut scenario = GameScenario::new();
        scenario
            .at_phase(Phase::PreCombatMain)
            .with_mana_pool(P0, spell_pool(4, &[ManaType::Blue, ManaType::Blue]));
        let existing = scenario
            .add_spell_to_hand_from_oracle(P0, "Labelled unsought", true, "You gain 1 life.")
            .with_mana_cost(printed_cost("{2}{U}"))
            .id();
        let foreign = scenario
            .add_spell_to_hand_from_oracle(P1, "Labelled unsought", true, "You gain 1 life.")
            .with_mana_cost(printed_cost("{2}{U}"))
            .id();
        let land = scenario.add_land_to_hand(P0, "Island").id();
        let sought: Vec<_> = (0..available)
            .map(|index| {
                scenario
                    .add_spell_to_library_top(P0, &format!("Seek nonland {index}"), true)
                    .id()
            })
            .collect();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Discover the Formula", true, DISCOVER)
            .with_mana_cost(printed_cost("{4}{U}{U}"))
            .id();
        let mut runner = scenario.build();
        let outcome = runner.cast(spell).resolve();
        outcome.assert_hand_drawn(P0, available as i64);
        assert_spell_complete(runner.state());
        for id in sought.iter().copied().chain([existing]) {
            assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
            assert_eq!(runner.state().objects[&id].perpetual_mods.iter().filter(|modification| matches!(modification, PerpetualModification::ModifyCost { mode: CostModifyMode::Reduce, amount } if *amount == ManaCost::generic(1))).count(), 1);
        }
        for id in [land, foreign, spell] {
            assert!(runner.state().objects[&id].perpetual_mods.is_empty());
        }
        assert!(outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Seek,
                ..
            }
        )));
        if runner.state().priority_player != P0 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        runner.state_mut().players[0].mana_pool =
            engine::types::mana::ManaPool::from_units(spell_pool(1, &[ManaType::Blue]));
        // CR 601.2f + CR 118.7a: unsought {2}{U} now costs exactly {1}{U}.
        let committed = runner.cast(existing).commit();
        assert_eq!(committed.state().objects[&existing].zone, Zone::Stack);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        committed.resolve().assert_life_delta(P0, 1);
        assert_spell_complete(runner.state());
    }
    // Same-name unmodified control requires the unreduced printed cost.
    let mut scenario = GameScenario::new();
    scenario
        .at_phase(Phase::PreCombatMain)
        .with_mana_pool(P0, spell_pool(2, &[ManaType::Blue]));
    let control = scenario
        .add_spell_to_hand_from_oracle(P0, "Labelled unsought", true, "You gain 1 life.")
        .with_mana_cost(printed_cost("{2}{U}"))
        .id();
    let mut runner = scenario.build();
    let commit = runner.cast(control).commit();
    assert_eq!(commit.state().players[0].mana_pool.total(), 0);
    commit.resolve().assert_life_delta(P0, 1);
    assert_spell_complete(runner.state());
}

#[test]
fn teferi_contingency_counters_real_spell_and_keeps_unmodeled_tail_honest() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        spell_pool(0, &[ManaType::White, ManaType::Blue, ManaType::Blue]),
    );
    scenario.with_mana_pool(P1, spell_pool(2, &[]));
    let target = scenario
        .add_spell_to_hand_from_oracle(P1, "Labelled opposing spell", true, "You gain 3 life.")
        .with_mana_cost(printed_cost("{1}"))
        .id();
    let unrelated = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Labelled independent stack spell",
            true,
            "You gain 2 life.",
        )
        .with_mana_cost(printed_cost("{1}"))
        .id();
    let hand_decoy = scenario
        .add_spell_to_hand_from_oracle(P1, "Labelled opposing spell", true, "You gain 3 life.")
        .id();
    let gy_decoy = scenario
        .add_spell_to_graveyard(P1, "Labelled opposing spell", true)
        .id();
    let library_decoy = scenario
        .add_spell_to_library_top(P1, "Labelled opposing spell", true)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Teferi's Contingency", true, TEFERI)
        .with_mana_cost(printed_cost("{W}{U}{U}"))
        .id();
    let mut runner = scenario.build();
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner.cast(target).commit();
    assert_eq!(runner.state().objects[&target].zone, Zone::Stack);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    runner.cast(unrelated).commit();
    assert_eq!(runner.state().objects[&unrelated].zone, Zone::Stack);
    if runner.state().priority_player != P0 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert!(outcome.events().iter().any(|event| matches!(event, GameEvent::SpellCountered { object_id, countered_by, .. } if *object_id == target && *countered_by == spell)));
    outcome.assert_zone(&[target, spell, gy_decoy, unrelated], Zone::Graveyard);
    outcome.assert_zone(&[hand_decoy], Zone::Hand);
    outcome.assert_zone(&[library_decoy], Zone::Library);
    outcome.assert_life_delta(P1, 2);
    assert!(outcome.events().iter().all(|event| !matches!(event, GameEvent::SpellCountered { object_id, .. } if *object_id == unrelated)));
    assert!(runner
        .state()
        .objects
        .values()
        .all(|object| object.perpetual_mods.is_empty()));
    assert!(runner
        .state()
        .objects
        .values()
        .all(|object| !object.is_emblem));
    assert!(runner.state().transient_continuous_effects.is_empty());
    assert_spell_complete(runner.state());
    let parsed = parse_oracle_text(
        TEFERI,
        "Teferi's Contingency",
        &[],
        &["Instant".into()],
        &[],
    );
    let tail = parsed.abilities[0].sub_ability.as_deref().unwrap();
    assert!(
        matches!(tail.effect.as_ref(), Effect::Unimplemented { description: Some(fragment), .. } if fragment == TEFERI.strip_prefix("Counter target spell. ").unwrap()),
        "{parsed:#?}"
    );
}

#[test]
fn construction_no_regeneration_rider_reaches_destroy() {
    // Explicitly synthetic control differs only by omitting the rider.
    let regenerable = "Destroy target nonartifact attacking creature. Create a black Spirit creature token. Its power is equal to that creature's power and its toughness is equal to that creature's toughness.";
    for (oracle, expected_zone) in [
        (ORDINARY_CONSTRUCTION, Zone::Graveyard),
        (regenerable, Zone::Battlefield),
    ] {
        let mut b = board(
            oracle,
            (7, 8),
            false,
            false,
            "Return target creature to its owner's hand.",
        );
        b.runner.cast(b.regen).target_object(b.b).resolve();
        // CR 117.3b + CR 117.3d: finish preparation before P0 casts again.
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        b.runner.act(GameAction::PassPriority).unwrap();
        assert!(matches!(
            b.runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        assert!(b.runner.state().objects[&b.b]
            .replacement_definitions
            .iter_unchecked()
            .any(|r| r.shield_kind == ShieldKind::Regeneration));
        assert!(b
            .runner
            .state()
            .combat
            .as_ref()
            .unwrap()
            .attackers
            .iter()
            .any(|a| a.object_id == b.b));
        let outcome = b.runner.cast(b.spell).target_object(b.b).resolve();
        // CR 701.19a/c + CR 608.2c: the rider suppresses the shield, not creation.
        outcome.assert_zone(&[b.b], expected_zone);
        assert_created(outcome.state(), outcome.events(), b.spell, 1, (7, 8));
        if expected_zone == Zone::Battlefield {
            assert!(outcome.state().objects[&b.b].tapped);
            assert!(outcome
                .state()
                .combat
                .as_ref()
                .is_none_or(|c| c.attackers.iter().all(|a| a.object_id != b.b)));
        }
    }
}

#[test]
fn ordinary_host_cost_and_token_definition_keep_distinct_owners() {
    for reduced in [false, true] {
        let text = if reduced {
            format!("This spell costs {{1}} less to cast.\n{ORDINARY_CONSTRUCTION}")
        } else {
            ORDINARY_CONSTRUCTION.into()
        };
        // SHAPE accompanies actual payment and resolution; it is not the runtime proof.
        let parsed = parse_oracle_text(
            &text,
            "Synthetic self-cost Spirit",
            &[],
            &["Instant".into()],
            &[],
        );
        assert_eq!(parsed.statics.len(), usize::from(reduced));
        if reduced {
            assert!(
                matches!(&parsed.statics[0].mode, StaticMode::ModifyCost { mode: CostModifyMode::Reduce, amount, .. } if amount == &ManaCost::generic(1))
            );
        }
        assert_eq!(parsed.abilities.len(), 1);
        let chain: Vec<_> =
            std::iter::successors(Some(&parsed.abilities[0]), |d| d.sub_ability.as_deref())
                .collect();
        assert!(chain
            .iter()
            .all(|d| !matches!(*d.effect, Effect::Unimplemented { .. })));
        assert!(chain
            .iter()
            .any(|d| matches!(*d.effect, Effect::Token { .. })));
        assert_eq!(
            chain
                .iter()
                .filter(|d| matches!(*d.effect, Effect::Destroy { .. }))
                .count(),
            1
        );
        let paid = if reduced { 1 } else { 2 };
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let victim = scenario
            .add_creature(P1, "Opponent's ordinary B", 7, 8)
            .id();
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Synthetic self-cost Spirit", true, &text)
            .with_mana_cost(ManaCost::generic(2))
            .id();
        let mut runner = scenario.build();
        runner.state_mut().active_player = P1;
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(victim, AttackTarget::Player(P0))])
            .unwrap();
        runner.advance_to_priority_window();
        if runner.state().priority_player == P1 {
            runner.act(GameAction::PassPriority).unwrap();
        }
        for _ in 0..paid {
            runner
                .state_mut()
                .add_mana_to_pool(
                    P0,
                    ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
                )
                .unwrap();
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), paid);
        let committed = runner.cast(spell).target_object(victim).commit();
        // CR 601.2a/f: one available mana pays printed {2} only with the host reduction.
        assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
        assert_eq!(committed.state().players[0].mana_pool.total(), 0);
        let outcome = committed.resolve();
        outcome.assert_zone(&[victim], Zone::Graveyard);
        assert_created(outcome.state(), outcome.events(), spell, 1, (7, 8));
    }
}

#[test]
fn divine_sacrament_paid_source_tracks_both_graveyard_threshold_crossings() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = export_route_repair_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        spell_pool(1, &[ManaType::White, ManaType::White, ManaType::Green]),
    );
    scenario.with_library_top(P1, &["Observed card", "Reserve"]);
    for _ in 0..6 {
        scenario.add_spell_to_graveyard(P0, "Graveyard card", true);
    }
    let white = scenario
        .add_creature(P0, "White beneficiary", 2, 2)
        .with_color(vec![ManaColor::White])
        .id();
    let opposing_white = scenario
        .add_creature(P1, "Opposing white beneficiary", 2, 2)
        .with_color(vec![ManaColor::White])
        .id();
    let nonwhite = scenario
        .add_creature(P0, "Nonwhite decoy", 2, 2)
        .with_color(vec![ManaColor::Green])
        .id();
    let sacrament = scenario.add_real_card(P0, "Divine Sacrament", Zone::Hand, &db);
    let bauble = scenario.add_real_card(P0, "Mishra's Bauble", Zone::Battlefield, &db);
    let ooze = scenario.add_real_card(P0, "Scavenging Ooze", Zone::Battlefield, &db);
    let mut runner = scenario.build();
    runner.cast(sacrament).resolve();
    for id in [white, opposing_white] {
        assert_eq!(runner.state().objects[&id].power, Some(3));
    }
    runner.activate(bauble, 0).target_player(P1).resolve();
    assert_eq!(runner.state().objects[&bauble].zone, Zone::Graveyard);
    for id in [white, opposing_white] {
        assert_eq!(
            runner.state().objects[&id].power,
            Some(4),
            "the seventh card changes a live static predicate"
        );
    }
    runner.activate(ooze, 0).target_object(bauble).resolve();
    assert_eq!(runner.state().objects[&bauble].zone, Zone::Exile);
    for id in [white, opposing_white] {
        assert_eq!(
            runner.state().objects[&id].power,
            Some(3),
            "leaving threshold removes only the additional bonus"
        );
    }
    assert_eq!(runner.state().objects[&nonwhite].power, Some(2));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(runner.state());
}

#[test]
fn diplomacy_paid_spell_keeps_the_revealed_opponent_as_the_life_loss_recipient() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = export_route_repair_db();
    for warrior in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, spell_pool(2, &[ManaType::Black]));
        if warrior {
            scenario
                .add_creature(P0, "Warrior reach guard", 2, 2)
                .with_subtypes(vec!["Warrior"]);
        }
        let own = scenario.add_spell_to_hand(P0, "Own hand decoy", true).id();
        let selected = scenario
            .add_spell_to_hand(P1, "Chosen opposing card", true)
            .id();
        let other = scenario
            .add_spell_to_hand(P1, "Other opposing card", false)
            .id();
        let land = scenario.add_land_to_hand(P1, "Opposing land decoy").id();
        let spell = scenario.add_real_card(P0, "Diplomacy of the Wastes", Zone::Hand, &db);
        let mut runner = scenario.build();
        runner.cast(spell).target_player(P1).resolve();
        let cards = match &runner.state().waiting_for {
            WaitingFor::ChooseFromZoneChoice { player, cards, .. }
            | WaitingFor::EffectZoneChoice { player, cards, .. }
            | WaitingFor::RevealChoice { player, cards, .. } => {
                assert_eq!(*player, P0);
                cards
            }
            other => panic!(
                "the revealed opponent hand requires the controller's real selection: {other:?}"
            ),
        };
        assert!(cards.contains(&selected) && cards.contains(&other));
        assert!(!cards.contains(&own) && !cards.contains(&land));
        runner
            .act(GameAction::SelectCards {
                cards: vec![selected],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&selected].zone, Zone::Graveyard);
        for id in [own, other, land] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Hand);
        }
        assert_eq!(runner.state().players[0].life, 20);
        assert_eq!(
            runner.state().players[1].life,
            if warrior { 18 } else { 20 }
        );
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_spell_complete(runner.state());
    }
}

#[test]
fn sword_of_the_meek_returns_and_attaches_to_the_entering_creature() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = export_route_repair_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, spell_pool(1, &[]));
    let sword = scenario.add_real_card(P0, "Sword of the Meek", Zone::Graveyard, &db);
    let decoy = scenario.add_creature(P0, "Earlier 1/1", 1, 1).id();
    let entering = scenario
        .add_creature_to_hand(P0, "Paid entering 1/1", 1, 1)
        .with_mana_cost(printed_cost("{1}"))
        .id();
    let mut runner = scenario.build();
    runner.cast(entering).accept_optional().resolve();
    assert_eq!(runner.state().objects[&sword].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&entering].power, Some(2));
    assert_eq!(runner.state().objects[&entering].toughness, Some(3));
    assert_eq!(runner.state().objects[&decoy].power, Some(1));
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(runner.state());
}

#[test]
fn swamp_kitten_animation_and_counter_keep_the_same_declared_swamp() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::counter::CounterType;
    let db = export_route_repair_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kitten = scenario.add_real_card(P0, "The Swamp Kitten", Zone::Battlefield, &db);
    let swamp = scenario
        .add_land_from_oracle(P0, "Selected Swamp", "{T}: Add {B}.")
        .with_subtypes(vec!["Swamp"])
        .as_artifact_land()
        .id();
    let other = scenario
        .add_land_from_oracle(P0, "Non-Swamp decoy", "{T}: Add {G}.")
        .with_subtypes(vec!["Forest"])
        .id();
    let mut runner = scenario.build();
    runner.advance_to_combat();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(engine::types::ability::TargetRef::Object(swamp)),
        })
        .unwrap();
    runner.advance_until_stack_empty();
    let object = &runner.state().objects[&swamp];
    for kind in [CoreType::Land, CoreType::Creature, CoreType::Artifact] {
        assert!(object.card_types.core_types.contains(&kind));
    }
    assert_eq!(object.counters.get(&CounterType::Plus1Plus1), Some(&1));
    assert!(object.keywords.contains(&Keyword::Menace));
    assert!(runner.state().objects[&kitten].counters.is_empty());
    assert!(!runner.state().objects[&other]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. } | WaitingFor::DeclareAttackers { .. }
    ));
    assert!(runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty());
}

#[test]
fn rowan_paid_color_scoped_grant_snapshots_life_lost_at_resolution() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = export_route_repair_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        spell_pool(2, &[ManaType::Red, ManaType::Red, ManaType::Black]),
    );
    let rowan = scenario.add_real_card(P0, "Rowan, Scion of War", Zone::Battlefield, &db);
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(printed_cost("{R}"))
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, "Shock deals 2 damage to any target.")
        .with_mana_cost(printed_cost("{R}"))
        .id();
    let probe = scenario
        .add_spell_to_hand_from_oracle(P0, "Synthetic black cost probe", true, "You gain 1 life.")
        .with_color(vec![ManaColor::Black])
        .with_mana_cost(printed_cost("{4}{B}"))
        .id();
    let mut runner = scenario.build();
    runner.cast(first).target_player(P0).resolve();
    assert_eq!(runner.state().players[0].life, 18);
    runner.activate(rowan, 0).resolve();
    assert!(runner.state().objects[&rowan].tapped);
    runner.cast(second).target_player(P0).resolve();
    assert_eq!(runner.state().players[0].life, 16);
    runner.cast(probe).resolve();
    // The later two life lost cannot increase the locked reduction from two
    // to four, and the black shard is still paid.
    assert_eq!(runner.state().players[0].life, 17);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(runner.state());
}

#[test]
fn wrenn_paid_loyalty_emblem_grants_retrace_after_its_source_leaves() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::game_state::CastingVariant;
    let db = export_route_repair_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, spell_pool(0, &[ManaType::Red]));
    let wrenn = scenario.add_real_card(P0, "Wrenn and Six", Zone::Battlefield, &db);
    scenario.with_counter(wrenn, engine::types::counter::CounterType::Loyalty, 4);
    let shock = scenario
        .add_spell_to_graveyard(P0, "Shock", true)
        .from_oracle_text("Shock deals 2 damage to any target.")
        .with_mana_cost(printed_cost("{R}"))
        .id();
    let land = scenario.add_land_to_hand(P0, "Retrace land payment").id();
    let mut runner = scenario.build();
    // Fixture counters and the planeswalker characteristic must describe the
    // same seven-loyalty starting permanent before public activation.
    runner.state_mut().objects.get_mut(&wrenn).unwrap().loyalty = Some(7);
    assert_eq!(
        runner.state().objects[&wrenn]
            .counters
            .get(&engine::types::counter::CounterType::Loyalty),
        Some(&7)
    );
    runner.activate(wrenn, 2).resolve();
    assert_eq!(runner.state().objects[&wrenn].zone, Zone::Graveyard);
    let emblems: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_emblem)
        .collect();
    assert_eq!(emblems.len(), 1);
    assert_eq!(emblems[0].controller, P0);
    assert_eq!(emblems[0].zone, Zone::Command);
    runner
        .cast(shock)
        .casting_variant(CastingVariant::Retrace)
        .pay_cost_with(&[land])
        .target_player(P1)
        .resolve();
    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&shock].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[1].life, 18);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(runner.state());
}

#[test]
fn dragonstorm_globe_additional_counter_includes_noncreature_kindred_dragons() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::counter::CounterType;
    let db = export_route_repair_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, spell_pool(2, &[]));
    scenario.add_real_card(P0, "Dragonstorm Globe", Zone::Battlefield, &db);
    let kindred = scenario
        .add_artifact_to_hand_from_oracle(P0, "Noncreature Kindred Dragon", "")
        .with_subtypes(vec!["Dragon"])
        .with_mana_cost(printed_cost("{1}"))
        .id();
    let creature = scenario
        .add_creature_to_hand(P0, "Creature Dragon", 2, 2)
        .with_subtypes(vec!["Dragon"])
        .with_mana_cost(printed_cost("{1}"))
        .id();
    let decoy = scenario.add_creature(P0, "Non-Dragon decoy", 2, 2).id();
    let mut runner = scenario.build();
    let object = runner.state_mut().objects.get_mut(&kindred).unwrap();
    object.card_types.core_types.push(CoreType::Kindred);
    object.base_card_types = object.card_types.clone();
    runner.cast(kindred).resolve();
    runner.cast(creature).resolve();
    for id in [kindred, creature] {
        assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1),
            Some(&1)
        );
    }
    assert!(!runner.state().objects[&kindred]
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    assert!(runner.state().objects[&decoy].counters.is_empty());
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(runner.state());
}

#[test]
fn per_player_graveyard_choices_reanimate_the_complete_selected_set() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = export_route_repair_db();
    for (name, generic, black) in [("Breach the Multiverse", 5, 2), ("Ghouls' Night Out", 3, 2)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, spell_pool(generic, &vec![ManaType::Black; black]));
        for player in [P0, P1] {
            scenario.with_library_top(player, &["Noncreature mill filler"; 11]);
        }
        let own = scenario
            .add_creature_to_graveyard(P0, "Selected own card", 2, 2)
            .id();
        let foreign = scenario
            .add_creature_to_graveyard(P1, "Selected opposing card", 3, 3)
            .id();
        let own_decoy = scenario
            .add_creature_to_graveyard(P0, "Unselected own card", 4, 4)
            .id();
        let foreign_decoy = scenario
            .add_creature_to_graveyard(P1, "Unselected opposing card", 5, 5)
            .id();
        let spell = scenario.add_real_card(P0, name, Zone::Hand, &db);
        let mut runner = scenario.build();
        runner.cast(spell).resolve();
        let mut picks = 0;
        for _ in 0..16 {
            let cards = match &runner.state().waiting_for {
                WaitingFor::ChooseFromZoneOpponentChooser {
                    player, candidates, ..
                } => {
                    assert_eq!(*player, P0);
                    let opponent = [P0, P1][picks];
                    assert!(candidates.contains(&opponent));
                    runner
                        .act(GameAction::ChooseZoneOpponentChooser { opponent })
                        .unwrap();
                    continue;
                }
                WaitingFor::ChooseFromZoneChoice { cards, .. }
                | WaitingFor::EffectZoneChoice { cards, .. } => cards.clone(),
                WaitingFor::Priority { .. }
                    if runner.state().stack.is_empty()
                        && runner.state().resolution_stack.is_empty() =>
                {
                    break
                }
                other => panic!("expected the next real per-player choice on {name}: {other:?}"),
            };
            let selected = [own, foreign]
                .into_iter()
                .find(|id| cards.contains(id))
                .expect("each player's pool must contain its selected card");
            runner
                .act(GameAction::SelectCards {
                    cards: vec![selected],
                })
                .unwrap();
            runner.advance_until_stack_empty();
            picks += 1;
        }
        assert_eq!(picks, 2, "both player choices completed: {name}");
        for id in [own, foreign] {
            assert_eq!(
                runner.state().objects[&id].zone,
                Zone::Battlefield,
                "the mass continuation reads every selected card: {name}"
            );
            assert_eq!(runner.state().objects[&id].controller, P0);
        }
        for id in [own_decoy, foreign_decoy] {
            assert_eq!(runner.state().objects[&id].zone, Zone::Graveyard);
        }
        assert_eq!(runner.state().players[0].mana_pool.total(), 0);
        assert_spell_complete(runner.state());
    }
}

#[test]
fn scour_the_scene_offers_both_hand_and_battlefield_creatures() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = export_route_repair_db();
    for choose_hand in [false, true] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P1, &["Observed card", "Reserve"]);
        let source = scenario.add_real_card(P0, "Scour the Scene", Zone::Battlefield, &db);
        let bauble = scenario.add_real_card(P0, "Mishra's Bauble", Zone::Battlefield, &db);
        let hand = scenario
            .add_creature_to_hand(P0, "Eligible hand card", 2, 2)
            .id();
        let battlefield = scenario
            .add_creature(P0, "Eligible controlled creature", 3, 3)
            .id();
        let opponent = scenario
            .add_creature(P1, "Opponent creature decoy", 4, 4)
            .id();
        let opposing_hand = scenario
            .add_creature_to_hand(P1, "Opponent hand decoy", 5, 5)
            .id();
        let noncreature = scenario
            .add_spell_to_hand(P0, "Noncreature hand decoy", true)
            .id();
        let mut runner = scenario.build();
        runner.activate(bauble, 0).target_player(P1).resolve();
        let WaitingFor::ChooseFromZoneChoice { cards, .. } = &runner.state().waiting_for else {
            panic!("the sacrifice trigger must reach its printed union choice")
        };
        assert!(cards.contains(&hand) && cards.contains(&battlefield));
        for id in [opponent, opposing_hand, noncreature, source] {
            assert!(!cards.contains(&id));
        }
        let selected = if choose_hand { hand } else { battlefield };
        runner
            .act(GameAction::SelectCards {
                cards: vec![selected],
            })
            .unwrap();
        runner.advance_until_stack_empty();
        assert_eq!(runner.state().objects[&selected].perpetual_mods.len(), 1);
        for id in [
            source,
            opponent,
            opposing_hand,
            noncreature,
            if choose_hand { battlefield } else { hand },
        ] {
            assert!(runner.state().objects[&id].perpetual_mods.is_empty());
        }
        assert_eq!(runner.state().objects[&bauble].zone, Zone::Graveyard);
        assert_spell_complete(runner.state());
    }
}

#[test]
fn armored_skyhunter_keeps_the_selected_equipment_and_explicit_creature_host() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = export_route_repair_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let skyhunter = scenario.add_real_card(P0, "Armored Skyhunter", Zone::Battlefield, &db);
    let chosen_host = scenario
        .add_creature(P0, "Chosen attachment host", 2, 2)
        .id();
    let other_host = scenario
        .add_creature(P0, "Unchosen attachment host", 3, 3)
        .id();
    let foreign = scenario.add_creature(P1, "Foreign host decoy", 4, 4).id();
    let equipment = scenario.add_real_card(P0, "Sword of the Meek", Zone::Library, &db);
    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(skyhunter, AttackTarget::Player(P1))])
        .unwrap();
    let mut dig_seen = false;
    let mut host_seen = false;
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if dig_seen && runner.state().stack.is_empty() && runner.state().resolution_stack.is_empty() => break,
            WaitingFor::Priority { .. } => { runner.act(GameAction::PassPriority).unwrap(); }
            WaitingFor::OptionalEffectChoice { .. } => { runner.act(GameAction::DecideOptionalEffect { accept: true }).unwrap(); }
            WaitingFor::DigChoice { selectable_cards, .. } => {
                assert!(selectable_cards.contains(&equipment));
                runner.act(GameAction::SelectCards { cards: vec![equipment] }).unwrap();
                dig_seen = true;
            }
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert!(cards.contains(&chosen_host) && cards.contains(&other_host));
                assert!(!cards.contains(&foreign));
                runner.act(GameAction::SelectCards { cards: vec![chosen_host] }).unwrap();
                host_seen = true;
            }
            other => panic!("the attack trigger must complete its represented Equipment/host choices: {other:?}"),
        }
    }
    assert!(
        dig_seen && host_seen,
        "both printed choices must be reached"
    );
    assert_eq!(runner.state().objects[&equipment].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(chosen_host))
    );
    assert_eq!(runner.state().objects[&chosen_host].power, Some(3));
    assert_eq!(runner.state().objects[&other_host].power, Some(3));
    assert_spell_complete(runner.state());
}

#[test]
fn ghostfire_full_colorless_source_can_damage_a_creature_with_protection_from_red() {
    use super::jev_cost_populations::export_route_repair_db;
    use engine::game::scenario_db::GameScenarioDbExt;
    let db = export_route_repair_db();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, spell_pool(2, &[ManaType::Red]));
    let recipient = scenario
        .add_creature_from_oracle(
            P1,
            "Protection from red recipient",
            2,
            5,
            "Protection from red",
        )
        .id();
    let decoy = scenario
        .add_creature(P1, "Unselected damage decoy", 2, 5)
        .id();
    let spell = scenario.add_real_card(P0, "Ghostfire", Zone::Hand, &db);
    let mut runner = scenario.build();
    runner.cast(spell).target_object(recipient).resolve();
    assert_eq!(
        runner.state().objects[&recipient].damage_marked,
        3,
        "the complete printed colorless source and damage instruction resolve together"
    );
    assert_eq!(runner.state().objects[&decoy].damage_marked, 0);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    assert_spell_complete(runner.state());
}
