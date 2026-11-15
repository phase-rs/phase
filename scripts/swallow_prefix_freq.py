#!/usr/bin/env python3
"""Aggregate dropped Oracle clauses by SHARED PREFIX, not full snippet.

Each engine swallow warning in the target class contributes AT MOST one row: its
own rejected gap phrase when the record carries one, otherwise a regex-located
tail running from the trigger phrase ("if", "unless", "you may", etc.) onward. A
warning contributes none when it repeats its class on one card, when no regex
locates a tail, or when the normalized tail is shorter than `prefix_words` — so
the row count below is a LOWER BOUND on the class's warning count, not equal to
it. Group by that row's first N words to find shared lead-ins that span distinct
tails.

Goal: surface shared lead-ins — "you control a [...]" from a gap phrase,
"if you control a [...]" from a regex tail — that several cards reach via
different tails, even though no full-tail normalization matches.
"""
import argparse
import json
import re
from collections import Counter, defaultdict
from pathlib import Path

PARENS = re.compile(r"\([^)]*\)")
MANA   = re.compile(r"(?:\{[^}]+\})+")
NUM    = re.compile(r"\b\d+\b")
PUNCT  = re.compile(r"[\"'`,;:!?]")
WS     = re.compile(r"\s+")

def normalize(text: str, card_name: str) -> str:
    t = text.lower()
    n = card_name.lower()
    t = t.replace(n, "~")
    short = n.split(",")[0]
    if short and short != n:
        t = t.replace(short, "~")
    t = PARENS.sub("", t)
    t = MANA.sub("{COST}", t)
    t = NUM.sub("N", t)
    t = PUNCT.sub("", t)
    t = WS.sub(" ", t).strip()
    return t

TRIGGERS = {
    "Condition_If":           re.compile(r"(?<![a-z])if [a-z]"),
    "Condition_Unless":       re.compile(r"\bunless\b"),
    "Condition_AsLongAs":     re.compile(r"\bas long as\b"),
    "Optional_YouMay":        re.compile(r"\byou may\b"),
    "Duration_ThisTurn":      re.compile(r"\bthis turn\b"),
    "Duration_UntilEndOfTurn":re.compile(r"\buntil end of turn\b"),
    "Replacement_Instead":    re.compile(r"\binstead\b"),
    "DynamicQty":             re.compile(r"\b(?:equal to|for each|\btwice\b|where x is|the number of|half (?:your|their|its|the) (?:life|library))\b"),
}

def sentence_around(text: str, start: int, end: int) -> str:
    s = max(0, text.rfind(".", 0, start) + 1)
    e = text.find(".", end)
    if e == -1:
        e = len(text)
    return text[s:e].strip()

def gap_phrase(w):
    """The engine's own rejected phrase, or None.

    Read with `.get`, never `w["gap"]`: the key is omitted whenever the axis named no
    phrase — the same `skip_serializing_if` treatment `items` already gets, which is
    why some records serialize with no `items` key either.

    The value is joined across every field except the tag, so this needs no copy of
    the ClauseGap field-name table.
    """
    g = w.get("gap")
    if not g:
        return None
    return " ".join(str(v) for k, v in g.items() if k != "kind")


ap = argparse.ArgumentParser(description=__doc__,
                             formatter_class=argparse.RawDescriptionHelpFormatter)
ap.add_argument("--card-data", type=Path, default=Path("client/public/card-data.json"),
                help="path to card-data.json")
ap.add_argument("target_class", nargs="?", default="Condition_If")
ap.add_argument("prefix_words", nargs="?", type=int, default=4)
ap.add_argument("top_n", nargs="?", type=int, default=30)
args = ap.parse_args()

CARDS        = json.load(open(args.card_data))
target_class = args.target_class
prefix_words = args.prefix_words
top_n        = args.top_n

freq    = Counter()
samples = defaultdict(list)
n_gap   = 0
n_total = 0

for cname, card in CARDS.items():
    warnings = card.get("parse_warnings") or []
    if not warnings:
        continue
    raw = card.get("oracle_text") or ""
    cleaned = PARENS.sub("", raw).lower()
    seen = set()
    for w in warnings:
        # The detector is a real field now; TargetFallback / IgnoredRemainder carry none.
        if not isinstance(w, dict) or w.get("type") != "SwallowedClause":
            continue
        cls = w["detector"]
        if cls != target_class or cls in seen:
            continue
        seen.add(cls)
        # Prefer the engine's own typed phrase; fall back to the regex locator so this
        # still runs against an export written before the field existed.
        #
        # NOTE the SHAPE DIFFERENCE, which is real and visible in the output: a gap
        # phrase EXCLUDES the trigger word ("you control a Dragon") while the regex tail
        # INCLUDES it ("if you control a Dragon"). The header line reports how many rows
        # came from each source so a reader is never guessing which shape a row is.
        phrase = gap_phrase(w)
        if phrase is not None:
            tail = phrase
            from_gap = True
        else:
            regex = TRIGGERS.get(cls)
            if not regex:
                continue
            rm = regex.search(cleaned)
            if not rm:
                continue
            # Capture text starting at the trigger phrase and take next N words
            tail = cleaned[rm.start():]
            from_gap = False
        norm = normalize(tail, card["name"])
        words = norm.split()
        if len(words) < prefix_words:
            continue
        # Counted only AFTER the short-tail drop above, so the header's denominator is the
        # population the histogram actually holds: n_total == sum(freq.values()). Counting
        # before the drop reported more rows than the table below contains.
        n_total += 1
        if from_gap:
            n_gap += 1
        prefix = " ".join(words[:prefix_words])
        freq[prefix] += 1
        if len(samples[prefix]) < 4:
            samples[prefix].append(card["name"])

print(f"\n## {target_class} prefixes (first {prefix_words} words)  —  {sum(freq.values())} cards, {len(freq)} distinct prefixes")
print(f"({n_gap} of {n_total} rows from engine gap phrases; gap phrases exclude the trigger word, regex tails include it)\n")
for prefix, count in freq.most_common(top_n):
    ex = ", ".join(samples[prefix][:4])
    print(f"  [{count:>4}]  {prefix}")
    print(f"          ex: {ex}")
