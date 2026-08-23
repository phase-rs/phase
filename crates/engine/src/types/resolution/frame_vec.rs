//! The frame vector, and the only positions that can address it.
//!
//! [`ResolutionStack`] permits three ways to reach a frame: the top, a frame
//! adjacent to one you already hold, and the frame a
//! [`PostReplacementFrameId`] names. Everything else — scanning for the first
//! frame of some kind, remembering an index across a mutation, removing from
//! the middle — is a positional GUESS about a structural relationship the stack
//! does not guarantee.
//!
//! That rule used to be enforced by `scripts/check-resolution-frame-boundaries.sh`
//! grepping for `frames.iter().position(..)`, because `frames` was private to a
//! 7,000-line module and Rust privacy is module-scoped: "private" bought
//! nothing against the code sitting next to it. This module is the type-level
//! version of the same rule.
//!
//! [`FrameSlot`] is an opaque position with a private field, so it can be minted
//! by exactly five methods: [`FrameVec::top`], [`FrameVec::below`],
//! [`FrameVec::above`] and [`FrameVec::by_id`], which are the three sanctioned
//! access modes above, plus [`FrameVec::slot_at_captured_depth`].
//!
//! [`ChildStackDepth`] is the module's second opaque value, minted by exactly
//! one method: [`FrameVec::capture_depth`], which takes no argument and reads
//! the stack's own length.
//!
//! Reading or mutating a frame requires a slot. The one depth-addressed door,
//! [`FrameVec::slot_at_captured_depth`], requires a [`ChildStackDepth`], and so
//! does [`FrameVec::insert_at_child_boundary`]. A `usize` obtained by scanning
//! — `iter().position(..)`, arithmetic on `len()`, a literal — still compiles,
//! and it can no longer be spent on a mutation: it is neither a slot nor a
//! depth, and neither can be built from one. The one method that still accepts
//! one hands back a frame to read.
//!
//! [`FrameVec::frame_at_offset`] is the only method here that still takes a
//! bare `usize`. It returns a frame and never a slot, so it cannot widen
//! addressing, and `scripts/check-resolution-frame-boundaries.sh` fails if a
//! second bare-`usize` parameter appears in this module, wherever in the
//! parameter list it sits — behind a closure parameter or a generic list
//! included. That check is a text scan, so it does not see, for example, a
//! `usize` renamed by a type alias or a method a macro generated.
//!
//! Two operations are absent rather than restricted. `remove`, `swap_remove`,
//! `retain`, `drain`, `truncate` and `clear` have no wrapper here because the
//! stack has no legitimate use for them: frames leave through [`FrameVec::pop`]
//! at the top, or not at all. Absence costs nothing while the use count is zero,
//! and a future caller must add the method — and justify it — rather than
//! quietly reach for one that already exists.
//!
//! [`ResolutionStack`]: super::ResolutionStack

use serde::{Deserialize, Serialize};

use crate::types::game_state::PostReplacementFrameId;

use super::ResolutionFrame;

/// A position in a [`FrameVec`].
///
/// The field is private to this module, so a `FrameSlot` can only come from one
/// of the five minting methods listed in the module docs. That is the whole
/// mechanism: it makes "which frames may I address?" a question the compiler
/// answers.
///
/// A slot is a position, not a handle. Pushing, popping or inserting can move
/// the frame a slot refers to, so a slot held across a mutation may name a
/// different frame or none at all. [`FrameVec::get`] and [`FrameVec::get_mut`]
/// therefore return [`Option`] rather than asserting — the type stops you
/// addressing a frame you never located, and does not pretend to stop you
/// addressing one that has since moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameSlot(usize);

/// A resolution-stack DEPTH recorded before a child producer ran.
///
/// The field is private to this module, so a `ChildStackDepth` can only come
/// from [`FrameVec::capture_depth`] — a real read of the current frame count.
/// A `usize` from `iter().position(..)`, from arithmetic on `len()`, or from a
/// literal cannot become one, which is what closes the last positional door
/// into frame addressing: [`FrameVec::slot_at_captured_depth`] and
/// [`FrameVec::insert_at_child_boundary`] are its only consumers, and neither
/// accepts a bare `usize` any more.
///
/// Ordering compares depths, so a capture taken now can be compared against one
/// taken earlier to ask how far the stack has grown — the only arithmetic any
/// caller performs on it.
///
/// A depth is a recorded length, not a handle. Frames can retire below it while
/// the child producer runs, so the consumers return [`Option`] and `bool`
/// rather than asserting. The type proves the number came from a real stack
/// read; it does not prove the boundary is still live, and — since two nested
/// producers each hold one — it does not prove you are spending the right one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChildStackDepth(usize);

impl std::fmt::Display for ChildStackDepth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The backing storage for [`ResolutionStack`]'s frames.
///
/// Serialized transparently, so the wire format is exactly the `Vec` this
/// wraps and no save migration is involved.
///
/// [`ResolutionStack`]: super::ResolutionStack
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct FrameVec {
    frames: Vec<ResolutionFrame>,
}

impl FrameVec {
    pub(super) fn len(&self) -> usize {
        self.frames.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Read-only iteration, for validation, comparison and projection.
    ///
    /// This does expose `position`/`find` through [`Iterator`] — but their
    /// `usize` cannot be turned into a [`FrameSlot`], so a scan can be written
    /// and its result cannot be used. That is the intended shape: iteration
    /// over frames is legitimate, addressing a frame you found by scanning is
    /// not.
    pub(super) fn iter(&self) -> std::slice::Iter<'_, ResolutionFrame> {
        self.frames.iter()
    }

    pub(super) fn last(&self) -> Option<&ResolutionFrame> {
        self.frames.last()
    }

    pub(super) fn last_mut(&mut self) -> Option<&mut ResolutionFrame> {
        self.frames.last_mut()
    }

    pub(super) fn push(&mut self, frame: ResolutionFrame) {
        self.frames.push(frame);
    }

    pub(super) fn pop(&mut self) -> Option<ResolutionFrame> {
        self.frames.pop()
    }

    /// The top of the stack, if any.
    pub(super) fn top(&self) -> Option<FrameSlot> {
        self.frames.len().checked_sub(1).map(FrameSlot)
    }

    /// The frame immediately beneath `slot` — the adjacent-pair boundary.
    pub(super) fn below(&self, slot: FrameSlot) -> Option<FrameSlot> {
        slot.0.checked_sub(1).map(FrameSlot)
    }

    /// The frame immediately above `slot`, if the stack reaches that far.
    pub(super) fn above(&self, slot: FrameSlot) -> Option<FrameSlot> {
        let above = slot.0.checked_add(1)?;
        (above < self.frames.len()).then_some(FrameSlot(above))
    }

    /// The frame `id` names — the single identity-addressed lookup.
    ///
    /// Sound because ids come from a monotonic allocator that never rewinds
    /// within an action, so a stale id matches nothing rather than aliasing a
    /// later frame, and unstamped frames carry `None` and can never match.
    /// Those two properties are pinned by
    /// `v2_reader_recovers_discard_allocator_and_rejects_duplicate_frame_ids`
    /// and `h6a_legacy_id_less_post_replacement_frames_restore_unstamped`.
    pub(super) fn by_id(&self, id: PostReplacementFrameId) -> Option<FrameSlot> {
        self.frames
            .iter()
            .position(|frame| {
                matches!(frame, ResolutionFrame::PostReplacement(drains) if drains.frame_id() == Some(id))
            })
            .map(FrameSlot)
    }

    /// Record the current stack depth, before running a child producer.
    ///
    /// The sole constructor of [`ChildStackDepth`]. It takes no argument by
    /// design: the value is the stack's own length, so there is no number a
    /// caller could supply.
    pub(super) fn capture_depth(&self) -> ChildStackDepth {
        ChildStackDepth(self.frames.len())
    }

    /// The slot at a stack DEPTH captured before a child producer ran.
    ///
    /// This is the only method that turns a captured depth into an addressable
    /// position, and it exists because the depth originates far outside this
    /// module: an effect calls `ResolutionStack::capture_child_boundary`, runs a
    /// child producer, and hands the recorded depth back so the owner can be
    /// parked beneath the child stack that producer raised. Fifteen origins
    /// capture it that way — five files under `game/effects/`, plus
    /// `game/engine_debug.rs`.
    ///
    /// The argument is a [`ChildStackDepth`], which only
    /// [`FrameVec::capture_depth`] produces, so a scan result cannot be passed
    /// here at all: the door that used to be open at the call site is now closed
    /// at the type. Each origin holds its depth from capture to spend.
    pub(super) fn slot_at_captured_depth(&self, depth: ChildStackDepth) -> Option<FrameSlot> {
        (depth.0 < self.frames.len()).then_some(FrameSlot(depth.0))
    }

    /// Read the frame at a raw offset during a full-stack walk.
    ///
    /// `validate` traverses every frame and asks questions about a frame's
    /// neighbours by offset; it must, since checking a whole-stack invariant is
    /// precisely a whole-stack operation. This returns a frame and never a
    /// [`FrameSlot`], so an offset — including one produced by a scan — cannot
    /// be laundered into something addressable. That is the line this module
    /// draws: reading is not the hazard, addressing for mutation is.
    pub(super) fn frame_at_offset(&self, offset: usize) -> Option<&ResolutionFrame> {
        self.frames.get(offset)
    }

    pub(super) fn get(&self, slot: FrameSlot) -> Option<&ResolutionFrame> {
        self.frames.get(slot.0)
    }

    /// Overwrite the frame at `slot`, leaving stack length unchanged.
    pub(super) fn replace(&mut self, slot: FrameSlot, frame: ResolutionFrame) {
        self.frames[slot.0] = frame;
    }

    pub(super) fn get_mut(&mut self, slot: FrameSlot) -> Option<&mut ResolutionFrame> {
        self.frames.get_mut(slot.0)
    }

    /// Exchange two located frames, preserving stack length.
    pub(super) fn swap(&mut self, a: FrameSlot, b: FrameSlot) {
        self.frames.swap(a.0, b.0);
    }

    /// Insert `frame` so that it sits immediately beneath the frame currently
    /// at `slot`, lifting `slot` and everything above it by one.
    pub(super) fn insert_below(&mut self, slot: FrameSlot, frame: ResolutionFrame) {
        self.frames.insert(slot.0, frame);
    }

    /// Insert `frame` at a stack DEPTH captured before a child producer ran.
    ///
    /// This is the one depth-addressed entry point, and it is deliberately not
    /// a slot operation: `depth` is not a position someone located in the
    /// current stack, it is a length recorded earlier and handed back, so the
    /// frames above it are exactly the child stack the producer created. It
    /// returns nothing addressable, so a depth cannot be laundered into a
    /// [`FrameSlot`] by inserting with it.
    /// The depth itself is unforgeable now, so both directions are closed: a
    /// scanned `usize` cannot become a depth, and a depth cannot become a slot.
    ///
    /// Returns `false` when `depth` does not name a boundary with at least one
    /// child frame above it; the caller reports that as a typed error.
    pub(super) fn insert_at_child_boundary(
        &mut self,
        depth: ChildStackDepth,
        frame: ResolutionFrame,
    ) -> bool {
        if depth.0 >= self.frames.len() {
            return false;
        }
        self.frames.insert(depth.0, frame);
        true
    }
}
