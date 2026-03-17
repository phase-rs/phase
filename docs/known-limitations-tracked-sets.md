# Known Limitations: Tracked Set Recording for Delayed Triggers

## ReplacementChoice during multi-target exile

When `change_zone::resolve()` processes multiple targets and a replacement effect
triggers a `ReplacementResult::NeedsChoice` (e.g., a commander choosing between exile
and command zone), the function returns early after setting `WaitingFor::ChooseReplacement`.
Remaining targets in `ability.targets` are dropped — they are never processed.

**Impact**: The tracked set captures only the objects that were successfully moved before
the interruption. When the delayed trigger fires, only those objects are returned.

**Root cause**: `resolve()` iterates targets inline and has no mechanism to resume
iteration after a player choice. The `ChooseReplacement` handler in `engine.rs`
(lines ~326-380) processes the single replacement choice and re-queues the replacement,
but does not re-enter the multi-target loop.

**Fix approach**: Add the effect to the deferral list in `resolve_ability_chain()` and
implement `pending_continuation` resume logic in the `ChooseReplacement` arm of
`engine.rs`. This would allow the remaining targets to be processed after the player
responds to the replacement choice. This is a broader fix that affects the core
resolution loop and should be scoped as its own task.

**Cards affected**: Any card that exiles multiple targets with a delayed return AND
where one of those targets has a replacement effect on zone changes (e.g., commanders
with Yorion, or creatures with replacement-based exile protection).
