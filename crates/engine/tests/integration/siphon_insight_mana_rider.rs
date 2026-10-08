//! Regression: the any-color / any-type mana rider that follows a cast grant —
//! "You may play the exiled card for as long as it remains exiled, and you may
//! spend mana as though it were mana of any color to cast that spell" (Siphon
//! Insight), "If you cast a spell this way, mana of any type can be spent to
//! cast it" (Bloodsoaked Insight) — is a payment concession that applies only
//! to mana spent casting through the granted permission (CR 609.4b; CR 118.14
//! says so for "mana of any type", the any-color rider names "that spell"),
//! not an effect of its own.
//!
//! Bug: the rider chunk reached the catch-all `SpendManaAsAnyColor` branch of
//! `lower_imperative_clause` and became a sibling `GenericEffect` with a bare
//! board-wide static. Two consequences, both measured before the fix:
//!
//!   * its "you may" was promoted to `AbilityDefinition.optional`, so the
//!     engine paused resolution with a `WaitingFor::OptionalEffectChoice` after
//!     the dig — a prompt for a choice the card never offers;
//!   * the granted permission was recorded with `mana_spend_permission: None`
//!     and no cast-time payment check consults the transient static, so the
//!     exiled card could not be paid for with off-color mana at all
//!     (`CastSpell` → `ActionNotAllowed("Cannot pay mana cost")`).
//!
//! Fix: `try_parse_mana_spend_rider` recognizes the rider and, when the clause
//! it follows grants a cast without a concession, emits it as
//! `PriorModifier::ManaSpendPermission` — folded onto that grant's
//! `mana_spend_permission` by `attach_mana_spend_permission_to_prior_cast_grant`.
//! Where the conjunct stays inside the grant's own sentence, the inline
//! recognizers gained the objects they were missing ("… to cast it", the
//! comma before "and mana of any …") — Court of Locthwain, #8481, whose whole
//! sentence used to be swallowed by the catch-all with the grant.
//! The tests here drive the real resolution (`GameScenario` / `GameRunner` /
//! `GameAction`) end to end: no optional prompt, the recorded permission
//! carries the concession, and the granted card is actually cast with
//! off-color lands (or, for the monarch, for free).
//!
//! Payment: "any type" (CR 118.14) also covers colorless mana — a colored
//! mana may pay a `{C}` requirement through an any-type grant, never through
//! an any-color one (CR 106.1a vs CR 106.1b). `ManaSpendPermission::
//! allows_payment_as` is the one projection every payment path reads; before
//! it, both permissions collapsed to "any color" and `{C}` stayed unpayable.

use engine::ai_support::legal_actions;
use engine::game::derived_views::derive_views;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{CastingPermission, ManaSpendPermission, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const SIPHON_INSIGHT: &str = "Look at the top two cards of target opponent's library. Exile one of \
them face down and put the other on the bottom of that library. You may play the exiled card for as \
long as it remains exiled, and you may spend mana as though it were mana of any color to cast that \
spell.";

const COURT_OF_LOCTHWAIN: &str = "When this enchantment enters, you become the monarch.\n\
At the beginning of your upkeep, exile the top card of target opponent's library. You may play that \
card for as long as it remains exiled, and mana of any type can be spent to cast it. If you're the \
monarch, until end of turn, you may cast a spell from among cards exiled with this enchantment \
without paying its mana cost.";

const EVELYN_THE_COVETOUS: &str = "Flash\n\
Whenever Evelyn or another Vampire you control enters, exile the top card of each player's library \
with a collection counter on it.\n\
Once each turn, you may play a card from exile with a collection counter on it if it was exiled by an \
ability you controlled, and you may spend mana as though it were mana of any color to cast it.";

/// Vizier of the Menagerie's concession line — "mana of any type", so a
/// colored mana may pay a creature spell's `{C}` (CR 118.14).
const VIZIER_CONCESSION: &str = "You can spend mana of any type to cast creature spells.";

const BLOODSOAKED_INSIGHT: &str = "Target opponent exiles the top three cards of their library. Until \
the end of your next turn, you may play those cards. If you cast a spell this way, mana of any type \
can be spent to cast it.";

/// What the drive saw: every prompt kind it answered, in order, plus the cards
/// the dig offered. The prompt list is the reach guard — the dig step proves
/// the grant resolved — and the discriminator: pre-fix an
/// `OptionalEffectChoice` sat between the dig and the return to priority.
struct Drive {
    prompts: Vec<&'static str>,
    dug: Vec<ObjectId>,
}

/// Cast `spell` (already free) and resolve it, answering only the prompts the
/// card's own text calls for. Any other prompt fails the test by name.
fn cast_and_resolve(runner: &mut GameRunner, spell: ObjectId) -> Drive {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("CastSpell accepted");
    settle(runner)
}

/// Answer the prompts a resolving grant calls for until the stack is empty.
fn settle(runner: &mut GameRunner) -> Drive {
    let mut drive = Drive {
        prompts: Vec::new(),
        dug: Vec::new(),
    };
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                drive.prompts.push("OrderTriggers");
                let order = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("OrderTriggers accepted");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                drive.prompts.push("TargetSelection");
                let choice = target_slots[selection.current_slot]
                    .legal_targets
                    .iter()
                    .find(|t| **t == TargetRef::Player(P1))
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("ChooseTarget accepted");
            }
            WaitingFor::DigChoice { cards, .. } => {
                drive.prompts.push("DigChoice");
                drive.dug = cards.clone();
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![cards[0]],
                    })
                    .expect("SelectCards accepted");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return drive;
                }
                drive.prompts.push("Priority");
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while resolving the grant: {other:?}"),
        }
    }
    panic!("the grant never resolved back to an empty stack");
}

/// The concession every permission recorded on `card` carries. Exact: a
/// permission without one is reported as `None`, so a half-stamped card fails.
fn recorded_concessions(runner: &GameRunner, card: ObjectId) -> Vec<Option<ManaSpendPermission>> {
    runner.state().objects[&card]
        .casting_permissions
        .iter()
        .map(|permission| match permission {
            CastingPermission::ExileWithAltCost {
                mana_spend_permission,
                ..
            }
            | CastingPermission::PlayFromExile {
                mana_spend_permission,
                ..
            } => *mana_spend_permission,
            other => panic!("unexpected permission recorded on the granted card: {other:?}"),
        })
        .collect()
}

/// Cast the granted `card` from exile paying with the caster's lands and let
/// it resolve. Returns how many of `lands` ended tapped.
fn cast_granted_card(runner: &mut GameRunner, card: ObjectId, lands: &[ObjectId]) -> usize {
    assert!(
        legal_actions(runner.state()).iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == card)
        ),
        "the granted card must be offered as a legal cast with off-color lands"
    );
    let card_id = runner.state().objects[&card].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the granted card is cast with off-color lands");
    for _ in 0..10 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority accepted");
            }
            other => panic!("unexpected prompt while the granted card resolves: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Graveyard,
        "the granted sorcery resolved and went to its owner's graveyard"
    );
    lands
        .iter()
        .filter(|land| runner.state().objects[land].tapped)
        .count()
}

/// Siphon Insight: ", and you may spend mana as though it were mana of any
/// color to cast that spell" — the conjunct form, on a `CastFromZone` grant.
#[test]
fn siphon_insights_any_color_rider_is_scoped_to_the_exiled_card_and_asks_nothing() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // P1's library, top first: a {G} sorcery over a filler card.
    let filler = scenario.add_card_to_library_top(P1, "Filler");
    let green = {
        let mut b = scenario.add_spell_to_library_top(P1, "Green Sorcery", false);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        });
        b.id()
    };
    // P0 has only Swamps to pay with.
    let swamps = [
        scenario.add_basic_land(P0, ManaColor::Black),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let siphon = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Siphon Insight", false, SIPHON_INSIGHT);
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();

    let drive = cast_and_resolve(&mut runner, siphon);

    // DISCRIMINATOR 1: the dig ran (reach guard) and NO optional-effect prompt
    // followed it. Pre-fix: ["Priority", "Priority", "DigChoice",
    // "OptionalEffectChoice"] — the last one panicked the drive by name.
    assert_eq!(
        drive.prompts,
        vec!["Priority", "Priority", "DigChoice"],
        "resolving Siphon Insight asks for the dig choice and nothing else"
    );
    assert_eq!(
        drive.dug,
        vec![green, filler],
        "the dig offered P1's top two cards"
    );
    assert_eq!(runner.state().objects[&green].zone, Zone::Exile);
    assert!(runner.state().objects[&green].face_down, "exiled face down");

    // DISCRIMINATOR 2: every permission recorded on the exiled card carries the
    // printed concession. Pre-fix both read `None`.
    assert_eq!(
        recorded_concessions(&runner, green),
        vec![
            Some(ManaSpendPermission::AnyColor),
            Some(ManaSpendPermission::AnyColor)
        ],
        "\"any color\" rides onto the granted permission as AnyColor"
    );

    // DISCRIMINATOR 3: the {G} card is cast with a Swamp. Pre-fix `CastSpell`
    // was refused with "Cannot pay mana cost".
    let tapped = cast_granted_card(&mut runner, green, &swamps);
    assert_eq!(tapped, 1, "exactly one Swamp paid for {{G}}");
}

/// Bloodsoaked Insight: "If you cast a spell this way, mana of any type can be
/// spent to cast it." — the separate-sentence form with the conditional
/// prefix, on a `GrantCastingPermission { PlayFromExile }` grant.
#[test]
fn bloodsoaked_insights_any_type_rider_is_scoped_to_the_exiled_cards() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let deep = scenario.add_card_to_library_top(P1, "Deep");
    let third = scenario.add_card_to_library_top(P1, "Third");
    let second = scenario.add_card_to_library_top(P1, "Second");
    let green = {
        let mut b = scenario.add_spell_to_library_top(P1, "Green Sorcery", false);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        });
        b.id()
    };
    let swamps = [
        scenario.add_basic_land(P0, ManaColor::Black),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let bloodsoaked = {
        let mut b = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Bloodsoaked Insight",
            false,
            BLOODSOAKED_INSIGHT,
        );
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();

    let drive = cast_and_resolve(&mut runner, bloodsoaked);
    assert_eq!(
        drive.prompts,
        vec!["Priority", "Priority"],
        "resolving Bloodsoaked Insight asks nothing"
    );
    for card in [green, second, third] {
        assert_eq!(
            runner.state().objects[&card].zone,
            Zone::Exile,
            "top three exiled"
        );
    }
    assert_eq!(
        runner.state().objects[&deep].zone,
        Zone::Library,
        "the fourth card stays"
    );

    assert_eq!(
        recorded_concessions(&runner, green),
        vec![Some(ManaSpendPermission::AnyTypeOrColor)],
        "\"any type\" rides onto the granted permission as AnyTypeOrColor"
    );

    let tapped = cast_granted_card(&mut runner, green, &swamps);
    assert_eq!(tapped, 1, "exactly one Swamp paid for {{G}}");
}

/// Court of Locthwain (#8481): "You may play that card for as long as it
/// remains exiled, and mana of any type can be spent to cast it." — the inline
/// conjunct with "… to cast it". Pre-fix the whole sentence was captured by the
/// catch-all static and no permission existed at all: the exiled card was never
/// offered ("won't even give me the option to pay to cast"). Driven through P0's
/// own upkeep so the trigger, its player target, the exile, the recorded
/// permission and the cast are all real. `monarch` exercises the card's other
/// half — "If you're the monarch, until end of turn, you may cast a spell from
/// among cards exiled with this enchantment without paying its mana cost" —
/// which the report also names ("as the monarch it should be free to cast").
fn court_of_locthwain(monarch: bool) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let deeper = scenario.add_card_to_library_top(P1, "Deeper");
    let green = {
        let mut b = scenario.add_spell_to_library_top(P1, "Green Sorcery", false);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        });
        b.id()
    };
    // P1 draws once while their turn is crossed: a filler on top keeps the
    // {G} card as P1's top card at P0's upkeep. P0 draws from filler too.
    let p1_draw = scenario.add_card_to_library_top(P1, "P1 Draw");
    for _ in 0..4 {
        scenario.add_card_to_library_top(P0, "P0 Filler");
    }
    let swamps = [
        scenario.add_basic_land(P0, ManaColor::Black),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let court = scenario
        .add_enchantment_from_oracle(P0, "Court of Locthwain", COURT_OF_LOCTHWAIN)
        .id();
    let mut runner = scenario.build();
    if monarch {
        // Court's own ETB made its controller the monarch when it entered;
        // the enchantment starts on the battlefield here, so set the crown.
        runner.state_mut().monarch = Some(P0);
    }

    // Cross P1's turn into P0's next upkeep; the auto-advance stops at the
    // trigger's target prompt.
    runner.advance_to_phase(Phase::Upkeep);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(runner.state().active_player, P0, "back in P0's turn");
    assert_eq!(runner.state().phase, Phase::Upkeep);
    // Reach guard: the upkeep trigger is on the stack (its single legal player
    // target, P1, was announced on the way).
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Court's upkeep trigger is waiting to resolve"
    );
    assert_eq!(runner.state().stack[0].source_id, court);

    let drive = settle(&mut runner);
    // DISCRIMINATOR: no prompt beyond passing priority. Pre-fix the swallowed
    // sentence surfaced an `OptionalEffectChoice` here.
    assert_eq!(
        drive.prompts,
        vec!["Priority", "Priority"],
        "resolving the trigger asks nothing"
    );
    assert_eq!(
        runner.state().objects[&p1_draw].zone,
        Zone::Hand,
        "P1 drew the filler"
    );
    assert_eq!(
        runner.state().objects[&green].zone,
        Zone::Exile,
        "top card exiled"
    );
    assert_eq!(runner.state().objects[&deeper].zone, Zone::Library);

    // The paid permission carries the concession in both cases; the monarch's
    // free cast is a source-linked window (`ExiledBySource`, until end of
    // turn) that leaves no permission on the card — it shows in the cast
    // below, where no land is tapped.
    assert_eq!(
        recorded_concessions(&runner, green),
        vec![Some(ManaSpendPermission::AnyTypeOrColor)],
        "\"any type\" rides onto the granted permission"
    );
    // Move to P0's main phase and cast the {G} card.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(runner.state().active_player, P0);
    let untapped_before = swamps
        .iter()
        .filter(|land| !runner.state().objects[land].tapped)
        .count();
    assert_eq!(
        untapped_before, 2,
        "both Swamps untapped after P0's untap step"
    );
    let tapped = cast_granted_card(&mut runner, green, &swamps);
    if monarch {
        assert_eq!(tapped, 0, "the monarch casts for free — no Swamp tapped");
    } else {
        assert_eq!(tapped, 1, "exactly one Swamp paid for {{G}}");
    }
}

#[test]
fn court_of_locthwains_exiled_card_is_castable_with_any_mana() {
    court_of_locthwain(false);
}

#[test]
fn court_of_locthwains_exiled_card_is_free_for_the_monarch() {
    court_of_locthwain(true);
}

/// Exile a `{C}` sorcery from P1's library with `grant` and try to cast it from
/// two Swamps (plus a Wastes played from hand when `wastes`). `orrery` adds
/// Chromatic Orrery — a board-wide ANY-COLOR static that must not mask the
/// any-type concession elected with the grant. `vizier` makes the exiled card a
/// creature and adds Vizier of the Menagerie's static any-type concession for
/// creature spells, which must combine with the grant's own. Returns whether
/// it was cast.
fn colorless_requirement_case(
    grant: &str,
    wastes: bool,
    orrery: bool,
    vizier: bool,
    mode: CastPaymentMode,
) -> bool {
    let (grant_text, expected) = match grant {
        "Siphon Insight" => (SIPHON_INSIGHT, ManaSpendPermission::AnyColor),
        "Bloodsoaked Insight" => (BLOODSOAKED_INSIGHT, ManaSpendPermission::AnyTypeOrColor),
        "Evelyn, the Covetous" => (EVELYN_THE_COVETOUS, ManaSpendPermission::AnyColor),
        other => unreachable!("no such grant in this file: {other}"),
    };
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Deep", "Third", "Second"] {
        scenario.add_card_to_library_top(P1, name);
    }
    // Evelyn exiles the top card of EACH library.
    scenario.add_card_to_library_top(P0, "Own Top");
    let colorless = {
        let mut b = scenario.add_spell_to_library_top(P1, "Colorless Sorcery", false);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Colorless],
            generic: 0,
        });
        if vizier {
            b.as_creature();
        }
        b.id()
    };
    if vizier {
        scenario.add_creature_from_oracle(P0, "Vizier of the Menagerie", 3, 4, VIZIER_CONCESSION);
    }
    let mut lands = vec![
        scenario.add_basic_land(P0, ManaColor::Black),
        scenario.add_basic_land(P0, ManaColor::Black),
    ];
    let wastes = wastes.then(|| {
        scenario
            .add_land_to_hand(P0, "Wastes")
            .from_oracle_text("{T}: Add {C}.")
            .id()
    });
    if orrery {
        scenario.add_artifact_from_oracle(
            P0,
            "Chromatic Orrery",
            "You may spend mana as though it were mana of any color.",
        );
    }
    let spell = {
        let mut b = if grant == "Evelyn, the Covetous" {
            scenario.add_creature_to_hand_from_oracle(P0, grant, 2, 5, grant_text)
        } else {
            scenario.add_spell_to_hand_from_oracle(P0, grant, false, grant_text)
        };
        b.with_mana_cost(ManaCost::default());
        b.id()
    };
    let mut runner = scenario.build();
    cast_and_resolve(&mut runner, spell);
    // Reach guard: the {C} card is exiled and carries the printed concession.
    assert_eq!(runner.state().objects[&colorless].zone, Zone::Exile);
    assert_eq!(recorded_concessions(&runner, colorless)[0], Some(expected));

    if let Some(wastes) = wastes {
        let card_id = runner.state().objects[&wastes].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: wastes,
                card_id,
            })
            .expect("Wastes is played");
        lands.push(wastes);
    }
    let offered = legal_actions(runner.state()).iter().any(
        |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == colorless),
    );
    let card_id = runner.state().objects[&colorless].card_id;
    let cast = runner.act(GameAction::CastSpell {
        object_id: colorless,
        card_id,
        targets: vec![],
        payment_mode: mode,
    });
    if mode == CastPaymentMode::Manual && cast.is_ok() {
        // Pin one black mana to the sole {C} pip: the pin gate and the
        // client's remaining-cost view read the same typed permission.
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ));
        runner
            .act(GameAction::ActivateAbility {
                source_id: lands[0],
                ability_index: 0,
            })
            .expect("a Swamp taps for {B}");
        let pip_id = runner.state().players[0].mana_pool.mana[0].pip_id;
        runner
            .act(GameAction::SpendPoolMana { pip_id })
            .expect("black mana may be pinned to {C} under any type");
        assert_eq!(
            derive_views(runner.state(), Some(P0)).pending_payment_remaining,
            Some(ManaCost::NoCost),
            "the pinned black mana covers the {{C}} pip"
        );
        runner
            .act(GameAction::PassPriority)
            .expect("the pinned payment finishes the cast");
    }
    let on_stack = runner.state().objects[&colorless].zone == Zone::Stack;
    assert_eq!(
        offered, on_stack,
        "{grant}: the legal-action offer and the real cast agree"
    );
    if !on_stack {
        assert!(cast.is_err(), "{grant}: the refused cast reports an error");
        assert_eq!(runner.state().objects[&colorless].zone, Zone::Exile);
        return false;
    }
    let object = &runner.state().objects[&colorless];
    assert_eq!(object.mana_spent_to_cast_amount, 1, "one mana paid {{C}}");
    // CR 609.4b: the concession never changes what mana was actually spent.
    let black = u32::from(wastes.is_none());
    assert_eq!(object.colors_spent_to_cast.black, black);
    let tapped: Vec<_> = lands
        .iter()
        .filter(|land| runner.state().objects[land].tapped)
        .copied()
        .collect();
    assert_eq!(tapped.len(), 1, "exactly one land paid {{C}}");
    if let Some(wastes) = wastes {
        assert_eq!(tapped, vec![wastes], "the real colorless mana paid {{C}}");
    }
    true
}

/// CR 118.14 + CR 106.1b: "mana of any type can be spent" pays `{C}` with a
/// Swamp — auto-pay and a manual pin alike, with or without a board-wide
/// any-color static beside it. Pre-fix both permissions projected to "any
/// color", which never pays `{C}`, so the cast was refused.
#[test]
fn any_type_rider_pays_a_colorless_requirement_with_colored_mana() {
    for mode in [CastPaymentMode::Auto, CastPaymentMode::Manual] {
        for orrery in [false, true] {
            assert!(
                colorless_requirement_case("Bloodsoaked Insight", false, orrery, false, mode),
                "{mode:?}, Orrery={orrery}: the {{C}} card is cast with a Swamp"
            );
        }
    }
}

/// CR 609.4b + CR 106.1a: "as though it were mana of any color" does not reach
/// colorless — the `{C}` card is refused with only Swamps (Orrery, also any
/// color, changes nothing) and cast once a Wastes supplies real `{C}`. The
/// half that keeps the any-type test above from passing on an engine that
/// lets any mana pay anything. Evelyn, the Covetous prints "any color" too;
/// her collection-counter grant used to be recorded as any type.
#[test]
fn any_color_rider_still_needs_real_colorless_mana() {
    for grant in ["Siphon Insight", "Evelyn, the Covetous"] {
        for orrery in [false, true] {
            assert!(
                !colorless_requirement_case(grant, false, orrery, false, CastPaymentMode::Auto),
                "{grant}, Orrery={orrery}: Swamps alone must not pay {{C}} under any color"
            );
        }
        assert!(
            colorless_requirement_case(grant, true, false, false, CastPaymentMode::Auto),
            "{grant}: with a Wastes, the {{C}} card is cast"
        );
    }
}

/// CR 118.14 + CR 609.4b: Vizier of the Menagerie's STATIC "mana of any type"
/// concession pays a creature spell's `{C}` with a Swamp — and nothing else:
/// a `{C}` sorcery stays unpayable (the concession names creature spells), and
/// Chromatic Orrery's any-COLOR static never pays `{C}`. Pre-fix every static
/// concession projected to "any color", so the creature was refused too.
#[test]
fn vizier_static_any_type_pays_a_colorless_creature_spell() {
    for (vizier, orrery, creature, castable) in [
        (true, false, true, true),
        (true, false, false, false),
        (false, true, true, false),
    ] {
        let mut scenario = GameScenario::new_n_player(2, 42);
        scenario.at_phase(Phase::PreCombatMain);
        let swamp = scenario.add_basic_land(P0, ManaColor::Black);
        if vizier {
            scenario.add_creature_from_oracle(
                P0,
                "Vizier of the Menagerie",
                3,
                4,
                VIZIER_CONCESSION,
            );
        }
        if orrery {
            scenario.add_artifact_from_oracle(
                P0,
                "Chromatic Orrery",
                "You may spend mana as though it were mana of any color.",
            );
        }
        let spell = {
            let mut b = scenario.add_spell_to_hand(P0, "Colorless Card", false);
            b.with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Colorless],
                generic: 0,
            });
            if creature {
                b.as_creature();
            }
            b.id()
        };
        let mut runner = scenario.build();
        let label = format!("Vizier={vizier}, Orrery={orrery}, creature={creature}");
        let offered = legal_actions(runner.state()).iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell),
        );
        assert_eq!(offered, castable, "{label}: legal-action offer");
        let card_id = runner.state().objects[&spell].card_id;
        let cast = runner.act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        });
        assert_eq!(cast.is_ok(), castable, "{label}: the real cast");
        if castable {
            assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
            assert!(
                runner.state().objects[&swamp].tapped,
                "{label}: the Swamp paid {{C}}"
            );
            assert_eq!(
                runner.state().objects[&spell].colors_spent_to_cast.black,
                1,
                "CR 609.4b: the concession does not recolor the mana spent"
            );
        } else {
            assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
        }
    }
}

/// CR 609.4b: every concession in force applies to one payment. Siphon
/// Insight's grant is any COLOR, Vizier's static any TYPE for creature spells:
/// together a Swamp pays an exiled creature's `{C}`. (Without Vizier the same
/// cast is refused — `any_color_rider_still_needs_real_colorless_mana`.)
#[test]
fn a_static_any_type_concession_combines_with_an_any_color_grant() {
    assert!(colorless_requirement_case(
        "Siphon Insight",
        false,
        false,
        true,
        CastPaymentMode::Auto
    ));
}

/// North Star's line: a one-spell, mana-cost-only concession.
const NORTH_STAR: &str = "{4}, {T}: For one spell this turn, you may spend mana as though it were \
mana of any type to pay that spell's mana cost. (Additional costs are still paid normally.)";

/// North Star's concession covers ONE spell's mana cost; by CR 118.14 Swamps
/// could pay that spell's `{C}`. The clause is now the standalone concession
/// gap and grants nothing, so even the first `{C}` spell is refused, as are a
/// second one and a `{C}` activation — each checked through the legal-action
/// offer and the real action. Flip the first spell once North Star is
/// supported. This also held before the clause became a gap: the board-wide
/// static it used to lower to never reached a payment. The discriminating pin
/// is `standalone_mana_spend_concession_is_a_gap`.
#[test]
fn north_star_gap_grants_no_payment() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let swamps: Vec<_> = (0..7)
        .map(|_| scenario.add_basic_land(P0, ManaColor::Black))
        .collect();
    let colorless_spells: Vec<_> = ["First Colorless Card", "Second Colorless Card"]
        .into_iter()
        .map(|name| {
            scenario
                .add_spell_to_hand(P0, name, false)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Colorless],
                    generic: 0,
                })
                .id()
        })
        .collect();
    let engine_artifact = scenario
        .add_artifact_from_oracle(P0, "Colorless Engine", "{C}, {T}: You gain 1 life.")
        .id();
    let north_star = scenario
        .add_artifact_from_oracle(P0, "North Star", NORTH_STAR)
        .id();
    let mut runner = scenario.build();
    runner.activate(north_star, 0).resolve();
    // Reach guard: the ability was activated and resolved — North Star and
    // four Swamps are tapped, the stack is empty.
    assert!(runner.state().objects[&north_star].tapped);
    let tapped = swamps
        .iter()
        .filter(|swamp| runner.state().objects[swamp].tapped)
        .count();
    assert_eq!(tapped, 4, "North Star's {{4}} was paid");
    assert!(runner.state().stack.is_empty());

    for &spell in &colorless_spells {
        let offered = legal_actions(runner.state()).iter().any(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == spell),
        );
        let card_id = runner.state().objects[&spell].card_id;
        let cast = runner.act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        });
        assert!(!offered, "a {{C}} spell is not offered");
        assert!(cast.is_err(), "a {{C}} spell is refused");
        assert_eq!(runner.state().objects[&spell].zone, Zone::Hand);
    }
    let offered = legal_actions(runner.state()).iter().any(|action| {
        matches!(action, GameAction::ActivateAbility { source_id, .. } if *source_id == engine_artifact)
    });
    let activation = runner.act(GameAction::ActivateAbility {
        source_id: engine_artifact,
        ability_index: 0,
    });
    assert!(!offered, "a {{C}} activation is not offered");
    assert!(activation.is_err(), "a {{C}} activation is refused");
}
