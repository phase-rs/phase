use rand::Rng;

use crate::game::players;
use crate::types::ability::{
    ChoiceType, ChoiceValue, ChosenAttribute, Effect, EffectError, EffectKind, ResolvedAbility,
    SeatDirection, TargetSelectionMode,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;

/// Choose: present the player with a named set of options (creature type, color, etc.).
/// CR 700.2: Modal and choice-based spells/abilities require the controller to choose
/// from available options as part of casting or resolution.
/// Sets WaitingFor::NamedChoice so the player can select one.
/// The engine processes the ChooseOption response in engine.rs,
/// storing the result in GameState::last_named_choice for continuations.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // NOTE: a random `Effect::Choose` (`selection: Random`) is resolved upstream
    // in `resolve_ability_chain` via `resolve_random_in_chain` and never reaches
    // this interactive resolver, so `selection` is intentionally ignored here.
    let (choice_type, persist) = match &ability.effect {
        Effect::Choose {
            choice_type,
            persist,
            ..
        } => (choice_type.clone(), *persist),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected Choose effect".to_string(),
            ))
        }
    };

    let options = compute_options(
        state,
        &choice_type,
        ability.controller,
        ability.source_id,
        &ability.chosen_players,
    );

    // CR 609.3: If an effect attempts to do something impossible, it does only
    // as much as possible. When the engine enumerates the legal options for a
    // choice and the list is empty (e.g. "choose a player" once every eligible
    // player has already been chosen earlier in this resolution, or a "choose
    // an ability the target has" with no abilities to remove), there is nothing
    // to choose. The choice does nothing; the chain driver then skips any
    // continuation that depends on the missing chosen value while allowing
    // independent siblings to proceed. Emitting a `WaitingFor::NamedChoice`
    // with no options would wedge the game (issue #3040): the legal-action
    // enumerator yields no `ChooseOption`, so no player can advance the
    // decision. `CardName` / `Word` / `Artist` are excluded because their value
    // is player-supplied, so an empty engine list there is expected, not
    // impossible (only `CardName` has a wired free-text supply path today;
    // `Word` / `Artist` are a separate known frontend gap — see
    // `options_supplied_by_player`).
    if options.is_empty() && !choice_type.options_supplied_by_player() {
        state.cost_payment_failed_flag = true;
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    state.waiting_for = WaitingFor::NamedChoice {
        player: ability.controller,
        choice_type,
        options,
        source_id: if persist {
            Some(ability.source_id)
        } else {
            None
        },
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 608.2d (override) + CR 701.9b (analogous) + CR 109.4: Resolve a random
/// `Effect::Choose` in place, mutating `ability` so the chain's downstream
/// sub-ability propagation (`apply_parent_chain_context`) and any
/// `ControllerRef::ChosenPlayer`-scoped sub (Strax's "When you do, ~ fights
/// another target creature that player controls") see the game-selected value —
/// the controller does NOT choose. Mirrors `random_select_targets_for_ability`
/// for targets: the pick happens at the resolution point with a mutable
/// ability, so no interactive `WaitingFor::NamedChoice` is ever raised.
///
/// Returns `true` when the choice was resolved (random + a value was picked, or
/// random + impossible/empty so the effect did nothing per CR 609.3). Returns
/// `false` for a non-random `Effect::Choose`, leaving it to the interactive
/// `resolve` path. Emits the `EffectResolved` event itself when it resolves.
pub(crate) fn resolve_random_in_chain(
    state: &mut GameState,
    ability: &mut ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> bool {
    let (choice_type, persist) = match &ability.effect {
        Effect::Choose {
            choice_type,
            persist,
            selection: TargetSelectionMode::Random,
        } => (choice_type.clone(), *persist),
        _ => return false,
    };

    let options = compute_options(
        state,
        &choice_type,
        ability.controller,
        ability.source_id,
        &ability.chosen_players,
    );

    // CR 609.3: An impossible random choice (no legal option) does nothing; the
    // chain then skips any continuation that depends on the missing value while
    // independent siblings proceed — mirrors the interactive empty-options path.
    if options.is_empty() && !choice_type.options_supplied_by_player() {
        state.cost_payment_failed_flag = true;
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return true;
    }
    if options.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return true;
    }

    // CR 608.2d (override): the game selects uniformly at random.
    let index = state.rng.random_range(0..options.len());
    let chosen = options[index].clone();

    let source_id = if persist {
        Some(ability.source_id)
    } else {
        None
    };
    bind_named_choice(state, &choice_type, &chosen, source_id);

    // CR 608.2c + CR 109.4: A `Choose(Player)`/`Choose(Opponent)` answer binds a
    // resolution-scoped chosen player. Append it to the resolving ability's
    // `chosen_players` so the dependent sub (`ControllerRef::ChosenPlayer`) and
    // any later `Choose(Player)` in this resolution see it; the chain propagates
    // it to the sub via `apply_parent_chain_context`.
    if matches!(
        choice_type,
        ChoiceType::Player | ChoiceType::Opponent { .. }
    ) {
        if let Ok(pid) = chosen.parse::<u8>() {
            let mut updated = ability.chosen_players.clone();
            updated.push(PlayerId(pid));
            ability.set_chosen_players_recursive(&updated);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });
    true
}

/// CR 607.2d + CR 613.1 + CR 109.4: Bind a resolved named choice into game
/// state. Single authority shared by the interactive `ChooseOption` answer
/// handler and the random `Effect::Choose` resolver so the persist-attribute,
/// layer-recompute, and `last_named_choice` paths stay byte-identical.
///
/// Faithfully reproduces the state-side binding the interactive handler
/// performs (`engine_resolution_choices.rs`): when `source_id` is `Some`, a
/// persistable choice is pushed onto the source's `chosen_attributes` and (for
/// the layer-affecting choice kinds) layers are recomputed; `last_named_choice`
/// is always set. The resolution-scoped `chosen_players` append for
/// `Player`/`Opponent` choices is the CALLER's responsibility because its
/// destination differs (the interactive path appends to the stashed
/// continuation chain; the random path mutates the resolving ability directly).
pub(crate) fn bind_named_choice(
    state: &mut GameState,
    choice_type: &ChoiceType,
    choice: &str,
    source_id: Option<ObjectId>,
) {
    if let Some(obj_id) = source_id {
        // CR 608.2d: A multi-keyword choice (`ChoiceType::Keyword { count > 1 }`,
        // e.g. Greymond's "choose two abilities from among ...") arrives as one
        // comma-joined answer ("First Strike, Vigilance"). Split it on ',' and
        // trim each token (tolerating "A, B" / "A,B" / "A ,B" whitespace
        // variants) and persist one `ChosenAttribute::Keyword` per token so each
        // chosen ability is independently readable by the `AddChosenKeyword`
        // plural grant. The single-keyword path (count == 1, and every other
        // choice type) produces a single attribute, byte-identical to before.
        let attrs: Vec<ChosenAttribute> = match choice_type {
            ChoiceType::Keyword { options, count } if *count > 1 => choice
                .split(',')
                .filter_map(|token| {
                    ChosenAttribute::from_choice(
                        ChoiceType::Keyword {
                            options: options.clone(),
                            count: 1,
                        },
                        token.trim(),
                    )
                })
                .collect(),
            // CR 607.2d + CR 508.1c: The directional "choose left or right"
            // prompt (Pramikon, Sky Rampart; Mystic Barrier; Teyo, Geometric
            // Tactician) arrives as a two-option `ChoiceType::Labeled`. Hijack
            // ONLY the exact 2-option {left, right} set (case-insensitive, both
            // present) into a typed `ChosenAttribute::Direction` so the CR
            // 508.1c gate can read the seat direction. A Labeled choice that
            // merely includes "Left" among other options (e.g.
            // ["Left", "Center", "Right"]) is NOT a directional choice — it
            // falls through to the generic `Label` path below.
            ChoiceType::Labeled { options }
                if options.len() == 2
                    && options
                        .iter()
                        .all(|o| SeatDirection::from_choice_label(o).is_some())
                    && options.iter().any(|o| {
                        SeatDirection::from_choice_label(o) == Some(SeatDirection::Left)
                    })
                    && options.iter().any(|o| {
                        SeatDirection::from_choice_label(o) == Some(SeatDirection::Right)
                    }) =>
            {
                match SeatDirection::from_choice_label(choice) {
                    Some(dir) => vec![ChosenAttribute::Direction(dir)],
                    None => ChosenAttribute::from_choice(choice_type.clone(), choice)
                        .into_iter()
                        .collect(),
                }
            }
            _ => ChosenAttribute::from_choice(choice_type.clone(), choice)
                .into_iter()
                .collect(),
        };
        if !attrs.is_empty() {
            if let Some(obj) = state.objects.get_mut(&obj_id) {
                // CR 608.2d: A keyword choice represents the CURRENT answer set,
                // not an accumulation. A source that makes a fresh keyword choice
                // each time its effect resolves (Angelic Skirmisher — "At the
                // beginning of combat on your turn, choose first strike, vigilance,
                // or lifelink. Creatures you control gain that ability until end of
                // turn") must REPLACE its prior keyword answer, otherwise the
                // `AddChosenKeyword` plural read would grant every historical
                // choice. Clear only `ChosenAttribute::Keyword` (Greymond's single
                // as-enters bind clears nothing, so its behavior is unchanged);
                // every other chosen-attribute kind (Color, Subtype, CardName,
                // Label, …) is untouched so RemoveChosenKeyword/Urborg and the
                // anchor-word/Morophon cards keep accumulating per their own rules.
                if matches!(choice_type, ChoiceType::Keyword { .. }) {
                    obj.chosen_attributes
                        .retain(|a| !matches!(a, ChosenAttribute::Keyword(_)));
                }
                // CR 607.2d "the last chosen direction": a re-choice (Mystic
                // Barrier's upkeep re-selection) REPLACES the prior direction.
                // Clear only `ChosenAttribute::Direction`, mirroring the Keyword
                // retain above, so exactly one direction (the last) survives.
                if attrs
                    .iter()
                    .any(|a| matches!(a, ChosenAttribute::Direction(_)))
                {
                    obj.chosen_attributes
                        .retain(|a| !matches!(a, ChosenAttribute::Direction(_)));
                }
                for attr in attrs {
                    obj.chosen_attributes.push(attr);
                }
                // CR 607.2d + CR 613.1: Persisted ETB/modal choices (card name,
                // creature type, card type, color, etc.) can gate
                // source-dependent continuous or rule effects. Layer evaluation
                // may have run before the choice was made — re-run.
                if matches!(
                    choice_type,
                    ChoiceType::CardName
                        | ChoiceType::CreatureType { .. }
                        | ChoiceType::CardType { .. }
                        | ChoiceType::BasicLandType
                        | ChoiceType::Color { .. }
                        | ChoiceType::Keyword { .. }
                        | ChoiceType::Player
                        | ChoiceType::Opponent { .. }
                        // CR 613.1: A persisted `Label` gates `ChosenLabelIs`
                        // continuous statics — anchor-word modal permanents
                        // (Khans Sieges) and the modal as-enters P/T class
                        // (Primal Plasma/Clay, Corrupted Shapeshifter, Aquamorph
                        // Entity), whose Layer-7b SetPower/SetToughness apply only
                        // while the chosen label is active. Without re-running
                        // layers here the pre-choice printed P/T survives (a modal
                        // creature that entered printed 0/0 would then die to SBAs
                        // before its gated static could set its real P/T).
                        | ChoiceType::Labeled { .. }
                ) {
                    crate::game::layers::mark_layers_full(state);
                }
            }
        }
    }

    state.last_named_choice = ChoiceValue::from_choice(choice_type, choice);
}

const FALLBACK_CREATURE_TYPES: &[&str] = &[
    "Human",
    "Elf",
    "Goblin",
    "Merfolk",
    "Zombie",
    "Soldier",
    "Wizard",
    "Dragon",
    "Angel",
    "Demon",
    "Beast",
    "Bird",
    "Cat",
    "Elemental",
    "Faerie",
    "Giant",
    "Knight",
    "Rogue",
    "Spirit",
    "Vampire",
    "Warrior",
];

const ODD_OR_EVEN: &[&str] = &["Odd", "Even"];

const BASIC_LAND_TYPES: &[&str] = &["Plains", "Island", "Swamp", "Mountain", "Forest"];

const CARD_TYPES: &[&str] = &[
    "Artifact",
    "Creature",
    "Enchantment",
    "Instant",
    "Land",
    "Planeswalker",
    "Sorcery",
];

/// CR 205.3i: All land subtypes. Derived from `is_land_subtype()` in `types/card_type.rs`.
const LAND_TYPES: &[&str] = &[
    "Cave",
    "Desert",
    "Forest",
    "Gate",
    "Island",
    "Lair",
    "Locus",
    "Mine",
    "Mountain",
    "Plains",
    "Planet",
    "Power-Plant",
    "Sphere",
    "Swamp",
    "Tower",
    "Town",
    "Urza's",
];

/// Compute the valid options for a given choice type.
/// CR 700.2: The controller of a modal spell or ability chooses options as part of
/// casting or resolution. If an option would be illegal, it can't be chosen.
///
/// `already_chosen` is the resolution-scoped list of players picked by earlier
/// `Choose(Player)` instructions in this chain. CR 608.2c + the Gluntch card
/// ruling ("three distinct players") require each successive "choose a player"
/// to exclude players already chosen — `ChoiceType::Player` and
/// `ChoiceType::Opponent` filter them out. When fewer eligible players remain
/// than the card asks for, the options list is empty and the choice (and its
/// dependent effect) does nothing — the standard empty-options path.
fn compute_options(
    state: &GameState,
    choice_type: &ChoiceType,
    controller: PlayerId,
    source_id: crate::types::identifiers::ObjectId,
    already_chosen: &[PlayerId],
) -> Vec<String> {
    match choice_type {
        // CR 205.3m: Creature types are shared between creature and kindred cards.
        // A non-empty `options` restricts the offered set to an explicit
        // Oracle-listed candidate list (A Killer Among Us' "secretly choose
        // Human, Merfolk, or Goblin"), preserving source order; empty ⇒ all
        // creature types (Morophon / Changeling).
        ChoiceType::CreatureType { options } => {
            if !options.is_empty() {
                options.clone()
            } else if state.all_creature_types.is_empty() {
                to_strings(FALLBACK_CREATURE_TYPES)
            } else {
                let mut types = state.all_creature_types.clone();
                types.sort();
                types.dedup();
                types
            }
        }
        // CR 105.1 + CR 105.4: A color choice is one of white, blue, black, red, or green.
        ChoiceType::Color { excluded } => ManaColor::ALL
            .iter()
            .filter(|color| !excluded.contains(color))
            .map(|color| color_name(*color).to_string())
            .collect(),
        ChoiceType::OddOrEven => to_strings(ODD_OR_EVEN),
        // CR 305.6: The basic land types are Plains, Island, Swamp, Mountain, and Forest.
        ChoiceType::BasicLandType => to_strings(BASIC_LAND_TYPES),
        // CR 205.2a: The card types are artifact, battle, conspiracy, creature,
        // dungeon, enchantment, instant, land, phenomenon, plane, planeswalker,
        // scheme, sorcery, kindred, and vanguard. `excluded` narrows the offered
        // set (e.g. Archon of Valor's Reach restricts to artifact, enchantment,
        // instant, sorcery, planeswalker by excluding creature and land).
        ChoiceType::CardType { excluded } => CARD_TYPES
            .iter()
            .filter(|name| {
                name.parse::<CoreType>()
                    .is_ok_and(|core_type| !excluded.contains(&core_type))
            })
            .map(|name| name.to_string())
            .collect(),
        // CardName options are provided by the frontend from its local card database.
        // The engine sends an empty list to avoid serializing 30k+ names every state update.
        ChoiceType::CardName => Vec::new(),
        ChoiceType::NumberRange { min, max } => (*min..=*max).map(|n| n.to_string()).collect(),
        ChoiceType::Labeled { options } => options.clone(),
        // CR 205.3i: Land types include the basic land types plus Cave, Desert, Gate, etc.
        ChoiceType::LandType => to_strings(LAND_TYPES),
        // CR 102.3: An opponent is any player not on the choosing player's team
        // (in a free-for-all game, every other player). `players::opponents`
        // already drops eliminated players (CR 104.3a — a player who loses
        // leaves the game and is no longer an opponent).
        // CR 608.2c: Exclude players already chosen earlier in this resolution.
        // CR 102.3 + CR 608.2d: When a `restriction` is present ("with the most
        // life among your opponents"), narrow the eligible set to opponents
        // satisfying that `PlayerFilter` — the controller then picks ONE of the
        // qualifying opponents (CR 608.2d handles ties), keeping it a single
        // pick rather than fanning the effect out to every tied opponent.
        ChoiceType::Opponent { restriction } => players::opponents(state, controller)
            .iter()
            .filter(|id| !already_chosen.contains(id))
            .filter(|id| {
                restriction.as_ref().is_none_or(|filter| {
                    super::matches_player_scope(state, **id, filter, controller, source_id)
                })
            })
            .map(|id| id.0.to_string())
            .collect(),
        // CR 102.1: A player is one of the people in the game.
        // CR 608.2c: Exclude players already chosen earlier in this resolution.
        ChoiceType::Player => state
            .seat_order
            .iter()
            .filter(|id| !already_chosen.contains(id))
            .map(|id| id.0.to_string())
            .collect(),
        ChoiceType::TwoColors => two_color_options(),
        ChoiceType::Word | ChoiceType::Artist => Vec::new(),
        // CR 608.2d: "Choose an ability the target has, then remove it" —
        // option labels come from the typed `Keyword` list emitted by the
        // converter. Empty option lists are legal (the choice resolves with
        // no options, and the dependent effect is a no-op). For `count > 1`
        // (Greymond's "choose two abilities from among ...") each option is a
        // comma-joined unordered count-combination of the keyword names, so the
        // player makes a single selection of the whole group.
        ChoiceType::Keyword { options, count } => {
            if *count > 1 {
                keyword_choice_options(options, *count)
            } else {
                options.iter().map(|kw| kw.to_string()).collect()
            }
        }
    }
}

fn to_strings(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|&s| s.to_string()).collect()
}

fn color_name(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "White",
        ManaColor::Blue => "Blue",
        ManaColor::Black => "Black",
        ManaColor::Red => "Red",
        ManaColor::Green => "Green",
    }
}

/// Generate all 10 two-color combinations from the 5 mana colors.
/// Order within a pair doesn't matter, so we use ordered pairs (i < j).
fn two_color_options() -> Vec<String> {
    let mut options = Vec::with_capacity(10);
    let colors: Vec<_> = ManaColor::ALL
        .iter()
        .map(|color| color_name(*color))
        .collect();
    for (i, &c1) in colors.iter().enumerate() {
        for &c2 in &colors[i + 1..] {
            options.push(format!("{c1}, {c2}"));
        }
    }
    options
}

/// CR 608.2d: Generate every comma-joined unordered `count`-combination of the
/// keyword option list (Greymond's "choose two abilities from among first
/// strike, vigilance, and lifelink" → `["First Strike, Vigilance", "First
/// Strike, Lifelink", "Vigilance, Lifelink"]`). Mirrors `two_color_options`:
/// order within a combination doesn't matter, so combinations use ascending
/// index tuples (no permutations). The resulting comma-joined string is one
/// selectable option; `bind_named_choice` splits it back into individual
/// `ChosenAttribute::Keyword` entries at resolution.
///
/// INVARIANT: this `", "`-join / split round-trip is only safe because every
/// keyword admitted by the parser (`parse_keyword_from_oracle`) has a
/// comma-free `Display` string. A future "choose N abilities" list that admits
/// a Debug-fallback keyword whose `Display` contains a comma would mis-tokenize
/// — keep the parser's keyword allowlist comma-free, or persist structured
/// tokens instead of a joined string.
fn keyword_choice_options(
    options: &[crate::types::keywords::Keyword],
    count: usize,
) -> Vec<String> {
    let names: Vec<String> = options.iter().map(|kw| kw.to_string()).collect();
    let mut result = Vec::new();
    let mut indices: Vec<usize> = (0..count).collect();
    if count == 0 || count > names.len() {
        return result;
    }
    loop {
        result.push(
            indices
                .iter()
                .map(|&i| names[i].clone())
                .collect::<Vec<_>>()
                .join(", "),
        );
        // Advance the combination indices (lexicographic next-combination).
        let mut i = count;
        loop {
            if i == 0 {
                return result;
            }
            i -= 1;
            if indices[i] != i + names.len() - count {
                break;
            }
        }
        indices[i] += 1;
        for j in (i + 1)..count {
            indices[j] = indices[j - 1] + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_choose_ability(choice_type: ChoiceType) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Choose {
                choice_type,
                persist: false,
                selection: crate::types::ability::TargetSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn choose_creature_type_sets_named_choice() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec!["Elf".to_string(), "Goblin".to_string()];

        let ability = make_choose_ability(ChoiceType::creature_type());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice {
                player,
                choice_type,
                options,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*choice_type, ChoiceType::creature_type());
                assert!(options.contains(&"Elf".to_string()));
                assert!(options.contains(&"Goblin".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_color_offers_five_colors() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::color());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 5);
                assert!(options.contains(&"White".to_string()));
                assert!(options.contains(&"Blue".to_string()));
                assert!(options.contains(&"Black".to_string()));
                assert!(options.contains(&"Red".to_string()));
                assert!(options.contains(&"Green".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_color_with_excluded_color_offers_remaining_colors() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::color_excluding(vec![ManaColor::White]));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice {
                choice_type,
                options,
                ..
            } => {
                assert_eq!(
                    *choice_type,
                    ChoiceType::Color {
                        excluded: vec![ManaColor::White],
                    }
                );
                assert_eq!(options, &["Blue", "Black", "Red", "Green"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_odd_or_even_offers_two_options() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::OddOrEven);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, &["Odd", "Even"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_basic_land_type_offers_five_types() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::BasicLandType);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 5);
                assert!(options.contains(&"Forest".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_card_type_offers_seven_types() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::card_type());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 7);
                assert!(options.contains(&"Creature".to_string()));
                assert!(options.contains(&"Instant".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    // CR 205.2a: Archon of Valor's Reach restricts the card-type choice to
    // "artifact, enchantment, instant, sorcery, or planeswalker" by excluding
    // Creature and Land from the offered set.
    #[test]
    fn choose_card_type_excludes_restricted_types() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::card_type_excluding(vec![
            CoreType::Creature,
            CoreType::Land,
        ]));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 5);
                assert!(!options.contains(&"Creature".to_string()));
                assert!(!options.contains(&"Land".to_string()));
                assert!(options.contains(&"Planeswalker".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_creature_type_with_empty_all_types_uses_fallback() {
        let mut state = GameState::new_two_player(42);
        // all_creature_types is empty by default
        let ability = make_choose_ability(ChoiceType::creature_type());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert!(!options.is_empty());
                assert!(options.contains(&"Human".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_card_name_sends_empty_options() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::CardName);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice {
                choice_type,
                options,
                ..
            } => {
                assert_eq!(*choice_type, ChoiceType::CardName);
                assert!(options.is_empty());
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn resolve_emits_effect_resolved_event() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::color());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            GameEvent::EffectResolved { kind, source_id } => {
                assert_eq!(*kind, EffectKind::Choose);
                assert_eq!(*source_id, ObjectId(100));
            }
            other => panic!("Expected EffectResolved, got {:?}", other),
        }
    }

    #[test]
    fn choose_number_range_generates_options() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Choose {
                choice_type: ChoiceType::NumberRange { min: 0, max: 5 },
                persist: false,
                selection: crate::types::ability::TargetSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, &["0", "1", "2", "3", "4", "5"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_labeled_uses_provided_options() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Choose {
                choice_type: ChoiceType::Labeled {
                    options: vec!["Left".to_string(), "Right".to_string()],
                },
                persist: false,
                selection: crate::types::ability::TargetSelectionMode::Chosen,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, &["Left", "Right"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_land_type_offers_all_land_types() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::LandType);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert!(options.contains(&"Plains".to_string()));
                assert!(options.contains(&"Forest".to_string()));
                assert!(options.contains(&"Sphere".to_string()));
                assert!(options.contains(&"Urza's".to_string()));
                assert!(options.len() >= 14);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_opponent_lists_opponents() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::Opponent { restriction: None });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                // Player 0 is controller, so opponent is player 1
                assert_eq!(options, &["1"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_player_lists_all_players() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::Player);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 2);
                assert!(options.contains(&"0".to_string()));
                assert!(options.contains(&"1".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_player_excludes_already_chosen_players() {
        // CR 608.2c + Gluntch ruling: a successive "choose a player" omits
        // players already chosen earlier in the same resolution.
        let mut state = GameState::new_two_player(42);
        let mut ability = make_choose_ability(ChoiceType::Player);
        ability.chosen_players = vec![PlayerId(0)];
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, &["1"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_player_with_all_players_chosen_resolves_as_no_op() {
        // CR 609.3 (issue #3040): when every eligible player is already chosen,
        // the engine-enumerated option set is empty — choosing is impossible, so
        // the choice does nothing and resolution continues. It must NOT raise a
        // `WaitingFor::NamedChoice` with no options, which would wedge the game
        // (the legal-action enumerator yields no `ChooseOption` to advance it).
        let mut state = GameState::new_two_player(42);
        // A non-Priority sentinel so we can prove `resolve` doesn't install the
        // empty `NamedChoice` and doesn't otherwise touch `waiting_for`.
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let mut ability = make_choose_ability(ChoiceType::Player);
        ability.chosen_players = vec![PlayerId(0), PlayerId(1)];
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::NamedChoice { .. }),
            "an impossible choice must not wedge on an empty NamedChoice"
        );
        // The effect still resolved (CR 609.3 "as much as possible" = nothing).
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn choose_empty_keyword_list_resolves_as_no_op() {
        // CR 609.3 + CR 608.2d (issue #3040): "choose an ability the target has"
        // with no removable abilities enumerates to an empty option set. The
        // choice is impossible, so it resolves as a no-op rather than emitting an
        // unsatisfiable `NamedChoice`.
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let ability = make_choose_ability(ChoiceType::Keyword {
            options: vec![],
            count: 1,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            !matches!(state.waiting_for, WaitingFor::NamedChoice { .. }),
            "an empty keyword choice must not wedge on an empty NamedChoice"
        );
    }

    #[test]
    fn choose_card_name_with_empty_options_still_prompts() {
        // CR 609.3 boundary: `CardName` options are supplied by the frontend's
        // card database at runtime, so an empty engine list is expected, not
        // impossible. The no-op short-circuit must NOT fire here — the prompt
        // still goes up so the player can name a card.
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::CardName);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::NamedChoice { .. }),
            "CardName is player-supplied — empty engine options must still prompt"
        );
    }

    #[test]
    fn choose_two_colors_offers_ten_combinations() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::TwoColors);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                // C(5,2) = 10 unique pairs
                assert_eq!(options.len(), 10);
                assert!(options.contains(&"White, Blue".to_string()));
                assert!(options.contains(&"Red, Green".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    // CR 608.2d: Urborg's "target creature loses first strike or swampwalk"
    // surfaces a two-option `ChoiceType::Keyword` prompt. Each option label
    // comes from `Keyword`'s `Display` impl (typed match — no string concat
    // over Debug names).
    #[test]
    fn choose_keyword_offers_typed_keyword_labels() {
        use crate::types::keywords::Keyword;
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::Keyword {
            options: vec![Keyword::FirstStrike, Keyword::Landwalk("Swamp".to_string())],
            count: 1,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 2);
                assert!(options.contains(&"First Strike".to_string()));
                assert!(options.contains(&"Swampwalk".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    /// CR 608.2d (override) + CR 109.4: a random `Choose(Player)` binds a player
    /// into the ability's `chosen_players` (so a dependent `ChosenPlayer`-scoped
    /// sub sees it) without raising the interactive `NamedChoice` prompt.
    #[test]
    fn resolve_random_in_chain_binds_player_without_prompting() {
        let mut state = GameState::new_two_player(42);
        let mut ability = ResolvedAbility::new(
            Effect::Choose {
                choice_type: ChoiceType::Player,
                persist: false,
                selection: TargetSelectionMode::Random,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let handled = resolve_random_in_chain(&mut state, &mut ability, &mut events);
        assert!(handled, "random Choose must be handled inline");
        assert!(
            !matches!(state.waiting_for, WaitingFor::NamedChoice { .. }),
            "random selection must not raise an interactive prompt"
        );
        assert_eq!(
            ability.chosen_players.len(),
            1,
            "the game-selected player is bound into chosen_players"
        );
        assert!(state.last_named_choice.is_some());
    }

    #[test]
    fn resolve_random_in_chain_ignores_non_random() {
        // Building-block regression: a Chosen Choose is left to the interactive
        // `resolve` path (returns false; raises nothing here).
        let mut state = GameState::new_two_player(42);
        let mut ability = make_choose_ability(ChoiceType::Player);
        let mut events = Vec::new();
        assert!(!resolve_random_in_chain(
            &mut state,
            &mut ability,
            &mut events
        ));
    }

    /// V10 (matthewevans regression for PR #4638) — CR 608.2d + CR 613.1f: A
    /// source that makes a REPEATED single-keyword choice over time (Angelic
    /// Skirmisher: "At the beginning of each combat, choose first strike,
    /// vigilance, or lifelink. Creatures you control gain that ability until end
    /// of turn") must REPLACE its stored keyword answer each time it chooses, not
    /// accumulate. This drives the PRODUCTION `ChooseOption` / `bind_named_choice`
    /// path (no manual `chosen_attributes` seed): the real parsed choose+grant
    /// chain, re-hosted as an activated ability, is fired twice.
    ///
    /// Choose First Strike (grant applies), then on a later activation choose
    /// Lifelink, and assert the granted set is the CURRENT choice ONLY — Lifelink
    /// granted, First Strike NO LONGER granted. Without the keyword-clear in
    /// `bind_named_choice`, both historical `ChosenAttribute::Keyword` entries
    /// survive and the `AddChosenKeyword` plural read grants First Strike AND
    /// Lifelink — exactly the regression this guards (revert the `.retain(..)` in
    /// `bind_named_choice` and the final `!has_kw(FirstStrike)` assert fails).
    #[test]
    fn repeated_keyword_choice_replaces_prior_answer() {
        use crate::game::keywords::has_keyword;
        use crate::game::layers::evaluate_layers;
        use crate::game::scenario::{GameRunner, GameScenario};
        use crate::parser::oracle_trigger::parse_trigger_line;
        use crate::types::ability::AbilityKind;
        use crate::types::actions::GameAction;
        use crate::types::keywords::Keyword;
        use crate::types::phase::Phase;

        const P0: PlayerId = PlayerId(0);

        /// Re-evaluate layers and report whether `id` currently has `keyword`.
        fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
            runner.state_mut().layers_dirty.mark_full();
            evaluate_layers(runner.state_mut());
            has_keyword(&runner.state().objects[&id], keyword)
        }

        /// Activate `source`'s keyword-choice ability through the REAL pipeline and
        /// answer the surfaced `NamedChoice` with `keyword` via the production
        /// `GameAction::ChooseOption` handler (no manual `chosen_attributes` seed).
        fn drive_keyword_choice(runner: &mut GameRunner, source: ObjectId, keyword: &str) {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                })
                .expect("activate the keyword-choice ability");
            runner.advance_until_stack_empty();

            let WaitingFor::NamedChoice { options, .. } = runner.state().waiting_for.clone() else {
                panic!(
                    "must pause on the keyword NamedChoice, got {}",
                    runner.waiting_for_kind()
                );
            };
            let choice = options
                .into_iter()
                .find(|o| o == keyword)
                .unwrap_or_else(|| panic!("expected a {keyword:?} keyword option"));

            runner
                .act(GameAction::ChooseOption { choice })
                .expect("answer the keyword choice");
            runner.advance_until_stack_empty();
        }

        // Parse Angelic Skirmisher's real "choose a keyword; creatures you control
        // gain that ability" chain, then re-host it as an ACTIVATED ability so the
        // test can drive the same choose+grant twice on demand without staging two
        // full combat phases. The choose/bind path exercised is identical.
        let trigger = parse_trigger_line(
            "At the beginning of each combat, choose first strike, vigilance, or \
             lifelink. Creatures you control gain that ability until end of turn.",
            "Angelic Skirmisher",
        );
        let mut activated = *trigger
            .execute
            .expect("Angelic Skirmisher trigger must have an execute chain");
        activated.kind = AbilityKind::Activated;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        let skirmisher = {
            let mut b = scenario.add_creature(P0, "Angelic Skirmisher", 4, 4);
            b.with_subtypes(vec!["Angel"]);
            b.with_ability_definition(activated);
            b.id()
        };
        let ally = scenario.add_creature(P0, "Footsoldier", 2, 2).id();

        let mut runner = scenario.build();

        // --- First activation: choose First Strike ---
        drive_keyword_choice(&mut runner, skirmisher, "First Strike");
        assert!(
            has_kw(&mut runner, ally, &Keyword::FirstStrike),
            "after choosing First Strike, the ally must have it"
        );
        assert!(
            !has_kw(&mut runner, ally, &Keyword::Lifelink),
            "Lifelink was never chosen yet"
        );

        // --- Second activation (a later combat): choose Lifelink ---
        drive_keyword_choice(&mut runner, skirmisher, "Lifelink");

        // The stored answer set must now be the CURRENT choice ONLY.
        let chosen = runner.state().objects[&skirmisher].chosen_keywords();
        assert_eq!(
            chosen,
            vec![&Keyword::Lifelink],
            "the second choice must REPLACE the first — only Lifelink stored, got {chosen:?}"
        );
        assert!(
            has_kw(&mut runner, ally, &Keyword::Lifelink),
            "after choosing Lifelink, the ally must have it"
        );
        assert!(
            !has_kw(&mut runner, ally, &Keyword::FirstStrike),
            "the FIRST choice (First Strike) must NO LONGER be granted — a keyword \
             choice represents the current answer set, not an accumulation"
        );
    }

    /// Create a bare battlefield object to receive a bound choice.
    #[cfg(test)]
    fn seed_source(state: &mut GameState) -> ObjectId {
        use crate::types::identifiers::CardId;
        crate::game::zones::create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            "Pramikon, Sky Rampart".to_string(),
            crate::types::zones::Zone::Battlefield,
        )
    }

    /// CR 607.2d: The {Left,Right} hijack fires ONLY for the exact 2-option
    /// {left,right} set. A Labeled prompt that merely INCLUDES "Left" among other
    /// options (["Left","Center","Right"]) is an ordinary anchor-word Label, not
    /// a direction — it must persist as a `Label` and `chosen_direction()` stays
    /// `None`. Revert the `options.len() == 2` guard → this stores a Direction.
    #[test]
    fn labeled_three_options_including_left_stays_a_label() {
        use crate::types::ability::SeatDirection;

        let mut state = GameState::new_two_player(42);
        let src = seed_source(&mut state);
        let choice_type = ChoiceType::Labeled {
            options: vec!["Left".into(), "Center".into(), "Right".into()],
        };
        bind_named_choice(&mut state, &choice_type, "Left", Some(src));

        let obj = &state.objects[&src];
        assert_eq!(
            obj.chosen_direction(),
            None,
            "a 3-option labeled choice must NOT be hijacked into a Direction"
        );
        assert_eq!(
            obj.chosen_label(),
            Some("Left"),
            "it must persist as an ordinary anchor-word Label"
        );
        // Sanity: SeatDirection typing still recognises the token in isolation.
        assert_eq!(
            SeatDirection::from_choice_label("Left"),
            Some(SeatDirection::Left)
        );
    }

    /// CR 607.2d: The exact 2-option {left,right} set is hijacked into a typed
    /// `Direction` (case-insensitive), and NO `Label` is stored. Revert the
    /// hijack branch → this stores a `Label` and `chosen_direction()` is `None`.
    #[test]
    fn labeled_left_right_binds_direction_not_label() {
        use crate::types::ability::SeatDirection;

        let mut state = GameState::new_two_player(42);
        let src = seed_source(&mut state);
        let choice_type = ChoiceType::Labeled {
            options: vec!["Left".into(), "Right".into()],
        };
        // Lowercase answer proves case-insensitive typing via from_choice_label.
        bind_named_choice(&mut state, &choice_type, "left", Some(src));

        let obj = &state.objects[&src];
        assert_eq!(obj.chosen_direction(), Some(SeatDirection::Left));
        assert_eq!(
            obj.chosen_label(),
            None,
            "the directional choice must NOT also leave a Label"
        );
    }

    /// CR 607.2d "the last chosen direction": re-choosing Right after Left leaves
    /// exactly one `Direction(Right)` — the prior direction is cleared. Revert the
    /// Direction retain-clear → both directions accumulate and the count is 2.
    #[test]
    fn rechoosing_direction_replaces_prior() {
        use crate::types::ability::{ChosenAttribute, SeatDirection};

        let mut state = GameState::new_two_player(42);
        let src = seed_source(&mut state);
        let choice_type = ChoiceType::Labeled {
            options: vec!["Left".into(), "Right".into()],
        };
        bind_named_choice(&mut state, &choice_type, "Left", Some(src));
        bind_named_choice(&mut state, &choice_type, "Right", Some(src));

        let obj = &state.objects[&src];
        let directions: Vec<_> = obj
            .chosen_attributes
            .iter()
            .filter(|a| matches!(a, ChosenAttribute::Direction(_)))
            .collect();
        assert_eq!(
            directions.len(),
            1,
            "exactly one Direction must survive a re-choice, got {directions:?}"
        );
        assert_eq!(obj.chosen_direction(), Some(SeatDirection::Right));
    }
}
