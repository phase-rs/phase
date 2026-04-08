Send your plan to an agent for architectural review. The reviewer MUST probe each of the following dimensions and reject the plan if any are violated:

## 1. Class vs Card
- How many cards or patterns does this plan cover? If it solves for a single card rather than a category, **reject**.
- What is the general pattern? Could a future card with similar Oracle text reuse this implementation unchanged?

## 2. Building Block Reuse
- Which modules from the building block table (see CLAUDE.md) were consulted?
- Does the plan duplicate logic that already exists in `parser/oracle_nom/`, `parser/oracle_util.rs`, `game/filter.rs`, `game/quantity.rs`, `game/ability_utils.rs`, `game/keywords.rs`, or any other existing module?
- If the plan introduces a new helper, is there truly no existing helper that covers the same need?

## 3. Trace Verification
- What analogous existing feature was traced end-to-end before designing this plan?
- The plan must name the traced feature and list the file path it followed. If no trace was performed, **reject**.

## 4. Abstraction Layer Correctness
- Is every piece of logic in the right module? Parser logic in `parser/`, game logic in `game/`, effect handlers in `game/effects/`, types in `types/`.
- Does any game logic leak into the frontend or WASM bridge?
- Does any display/formatting logic leak into the engine?

## 5. Idiomatic Rust
- Are there any `bool` fields that should be typed enums (`ControllerRef`, `Comparator`, `Option<T>`)?
- Are there any wildcard `_` match arms that should be exhaustive?
- Does the plan use `strip_prefix` chaining over `format!()` + matching?

## 5a. Nom Combinator Compliance (HARD GATE for parser files)
If the plan modifies ANY file under `crates/engine/src/parser/`, this check is mandatory. **Reject** if any of the following are true:
- The plan uses `contains()`, `starts_with()`, `ends_with()`, or `find()` for detection, dispatch, or classification of Oracle text lines or phrases.
- The plan describes a "heuristic" to detect whether a line is "probably" a certain type (e.g., `text.contains("gets ")` to detect statics). The correct approach is to try the actual parser: `parse_static_line(text).is_some()`.
- The plan introduces string-matching where a nom combinator (`tag()`, `alt()`, `preceded()`, `value()`) or existing building block would serve the same purpose.
- The plan does not specify which nom combinators or existing parser functions will be used for each detection/dispatch step.

The parser IS the detector. Never duplicate parser logic as a string heuristic.

## 6. CR Verification
- Does every CR annotation reference a verified rule number? (Grepped against `docs/MagicCompRules.txt`?)
- If the plan references CR numbers, are they correct? Verify at least one by grepping.

## 7. Skill Checklist Adherence
- Which skill(s) apply to this task? (`/add-engine-effect`, `/oracle-parser`, `/add-keyword`, `/add-trigger`, `/add-static-ability`, `/add-replacement-effect`, `/add-interactive-effect`, `/casting-stack-conditions`)
- Are all steps from the relevant skill checklist(s) accounted for in the plan? If steps are missing, **reject** with the specific missing steps listed.

## Review Process
Address all feedback from the reviewer and send the revised plan back. Repeat until the reviewer identifies no remaining gaps (max 3 rounds).
