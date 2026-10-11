# `DecisionSlot.point` (CR-typed choice point) and `PinnedDecision::Order`'s slot, for a
# persisted game dump.
#
# SINGLE DEFINITION of both derivations. The migration, every one of its controls and the
# readback row's expectation are expressed against THIS file (the row against the engine's
# own authority instead), so no control can certify its own copy of the recipe.
#
# WHY A DERIVATION AND NOT AN OPERATOR ARGUMENT. A slot's serialized shape is the same
# wherever it is stored, so a walk keyed on shape alone cannot say WHICH CHOICE the slot
# names. The naming authority is always the slot's own neighbour — a published point's
# sibling `kind`, a decision's `PinnedDecision` variant tag, or, where a carrier has
# neither, the field's own documented contract. One variant supplied per file would assert
# about every hit in it what is true of at most one: measured on this corpus,
# `combo_infinite_pile_4p_offer`'s single schema point is a CR 702.51a convoke slot while
# the two drain dumps' schema points are CR 601.2c announcements.
#
# EVERY UNRESOLVED HIT ABORTS BY NAME. There is deliberately no fallback point: a wrong
# stamp re-labels which CR choice an answer belongs to, which is the whole identity this
# shape change exists to carry.
#
# THE M1 PASS READS THE UNSTAMPED DOCUMENT THROUGHOUT. `pin_answers_point` is whole-slot
# equality, so a declaration decision's `Targets` slot matches its published point only
# while neither side carries a `point`. `stamp_slot_points` therefore derives EVERY hit's
# point against the document as it was read and only then applies the writes.

# ── the shape walks ───────────────────────────────────────────────────────────────────
# Key-set-exact, so neither can see the other's shape, and neither can see an
# already-stamped object (which is what makes the SKIP path idempotent by construction).
def _slot_paths:
  [ paths(type == "object") as $p
    | select((getpath($p) | keys_unsorted | sort) == ["index", "source"])
    | $p ];

def _order_paths:
  [ paths(type == "object") as $p
    | select((getpath($p) | keys_unsorted | sort) == ["pos", "source"])
    | $p ];

# ── M1: which choice does this slot name? ─────────────────────────────────────────────
# `DecisionPointKind` -> `ChoicePoint`. Total over the kind enum because `ChoicePoint`
# carries a variant for every kind `pin_answers_point` pairs.
def _kind_point($kind):
  if ($kind | type) == "string" then
    if   $kind == "MayChoice"   then "MayGate"
    elif $kind == "UnlessBreak" then "UnlessBreak"
    else error("REFUSE: unmapped DecisionPointKind \($kind)") end
  elif ($kind | type) == "object" and (($kind | keys_unsorted | length) == 1) then
    ($kind | keys_unsorted[0]) as $tag
    | if $tag == "Targets" then
        # The PRODUCER discriminant. `bounded_cycle_pin_slots_for_window` publishes an
        # announcement at `ordered: false`; `pinned_decisions_to_points` sets
        # `ordered: true` unconditionally, and which point such a slot names is a property
        # of the PIN that published it, which the schema does not carry.
        if ($kind[$tag].ordered) == false then "AnnouncedTarget"
        else error("REFUSE: Targets point at ordered:\($kind[$tag].ordered|tojson) — its choice is a property of the pin, not of the schema") end
      elif $tag == "ConvokeTaps" then "ConvokeTaps"
      elif $tag == "ManaColor"   then "ManaColor"
      elif $tag == "Mode"        then "Mode"
      else error("REFUSE: unmapped DecisionPointKind tag \($tag)") end
  else error("REFUSE: unrecognised DecisionPointKind \($kind|tojson)") end;

# `PinnedDecision` variant tag -> `ChoicePoint`. `Targets` delegates to the PUBLISHED
# POINT whose slot it equals: a declaration answers a published point, and
# `pin_answers_point` is whole-slot equality, so exactly one match is the only resolvable
# case.
def _variant_point($doc; $tag; $slot):
  if   $tag == "MayChoice"   then "MayGate"
  elif $tag == "ManaColor"   then "ManaColor"
  elif $tag == "ConvokeTaps" then "ConvokeTaps"
  elif $tag == "Mode"        then "Mode"
  elif $tag == "UnlessBreak" then "UnlessBreak"
  elif $tag == "Targets" then
    ([ $doc
       | paths(type == "object") as $q
       | getpath($q)
       | select(has("slot") and has("kind"))
       | select(.slot == $slot)
       | .kind ]) as $pts
    | if ($pts | length) == 1 then _kind_point($pts[0])
      else error("REFUSE: a Targets pin's slot answers \($pts|length) published points — its choice is not derivable") end
  else error("REFUSE: unmapped PinnedDecision variant \($tag)") end;

# The per-carrier derivation, and it is total over the carriers the walk returns: a
# carrier this does not resolve stops the whole run.
def _point_at($doc; $p):
  ($doc | getpath($p)) as $slot
  | ($doc | getpath($p[0:-1])) as $parent
  | if $p[-1] == "slot" and ($parent | type) == "object" and ($parent | has("kind")) then
      # a published `DecisionPoint`: its sibling `kind` names the choice.
      _kind_point($parent.kind)
    elif $p[-1] == "slot" and ($p[-2] | type) == "string" then
      # a `PinnedDecision`, externally tagged: the variant tag names the choice.
      _variant_point($doc; $p[-2]; $slot)
    elif ($p | length) >= 3 and $p[-3] == "victim_slot" then
      # CR 601.2c. `PeriodicDelta.victim_slot`'s own contract says these are ANNOUNCED
      # rather than published, and that a slot here with no matching schema point is
      # CORRECT — so slot equality is the wrong instrument at this carrier and the field's
      # documented contract is the right one.
      "AnnouncedTarget"
    else
      error("REFUSE: no governing neighbour for the slot at \($p|map(tostring)|join(".")) — the carrier set is closed by this walk")
    end;

def stamp_slot_points:
  . as $doc
  | [ _slot_paths[] as $p | { p: $p, point: _point_at($doc; $p) } ] as $derived
  | reduce $derived[] as $d (.;
      getpath($d.p) as $s
      | setpath($d.p; { source: $s.source, point: $d.point, index: $s.index }));

# ── M2: `PinnedDecision::Order` gains the slot every other variant carries ─────────────
# The tag names the point outright — `Order`'s slot is minted at `TriggerOrder` and
# nowhere else, and no published point carries `TriggerOrder`. The ordinal is 0 because
# the record's ordinal axis is instances of one (source, point) class, and the drive's
# cursor — not the slot — is what separates two triggers of one object.
def _order_slot($doc; $p):
  ($doc | getpath($p)) as $o
  | if $p[-1] != "Order" then
      error("REFUSE: a {source,pos} object outside a PinnedDecision::Order tag at \($p|map(tostring)|join(".")) — the carrier set is closed by this walk")
    else
      ([ $doc | getpath($p[0:-2])[] | select(type == "object" and has("Order")) | .Order.source ]) as $srcs
      | if ($srcs | length) != ($srcs | unique | length) then
          error("REFUSE: two Order decisions of one template name the SAME source at \($p|map(tostring)|join(".")) — which instance is which is not derivable from the serialized form")
        else
          { slot: { source: $o.source, point: "TriggerOrder", index: 0 }, pos: $o.pos }
        end
    end;

def stamp_order_slots:
  . as $doc
  | [ _order_paths[] as $p | { p: $p, v: _order_slot($doc; $p) } ] as $derived
  | reduce $derived[] as $d (.; setpath($d.p; $d.v));

def stamp_decision_slot_points: stamp_slot_points | stamp_order_slots;

# ── counts: what a dump NEEDS, and what a stamped dump GOT ────────────────────────────
def m1_need: _slot_paths | length;
def m2_need: _order_paths | length;
def m1_got:
  [ paths(type == "object") as $p
    | select((getpath($p) | keys_unsorted | sort) == ["index", "point", "source"]) ] | length;
def m2_got:
  [ paths(type == "object") as $p
    | select($p[-1] == "Order" and ((getpath($p) | keys_unsorted | sort) == ["pos", "slot"])) ] | length;

# ── arm 1's key-BLIND unstamp ─────────────────────────────────────────────────────────
# Not a fixed top-level `del()` list: these keys are added at every object OF A SHAPE, so
# the deletion has to be keyed on the shape too. Deepest paths first, so rebuilding a
# `{slot, pos}` carrier never discards an already-unstamped child.
def unstamp_decision_slot_points:
  reduce ([ paths(type == "object") as $p
            | select((getpath($p) | keys_unsorted | sort) == ["index", "point", "source"])
            | $p ] | sort_by(length) | reverse)[] as $p
    (.; getpath($p) as $s | setpath($p; { source: $s.source, index: $s.index }))
  | reduce ([ paths(type == "object") as $p
              | select($p[-1] == "Order" and ((getpath($p) | keys_unsorted | sort) == ["pos", "slot"]))
              | $p ] | sort_by(length) | reverse)[] as $p
      (.; getpath($p) as $o | setpath($p; { source: $o.slot.source, pos: $o.pos }));
