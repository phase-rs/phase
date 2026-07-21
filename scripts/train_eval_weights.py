#!/usr/bin/env python3
"""Train turn-phase-aware AI evaluation weights from 17Lands Premier Draft replay data.

Extracts per-turn board state features from 17Lands replay CSVs, splits into
three game phases (early T1-3, mid T4-7, late T8+), trains a separate logistic
regression for each phase, and outputs phase-bucketed EvalWeights as JSON.

This maximizes leverage of the temporal signal in 17Lands data: what predicts
winning changes dramatically across game phases.

Usage:
    python3 scripts/train_eval_weights.py --data-dir ~/Downloads --output data/learned-weights.json
"""

import argparse
import glob
import json
import os
import sys
from pathlib import Path

import numpy as np
import pandas as pd
from sklearn.linear_model import LogisticRegression
from sklearn.model_selection import train_test_split


# Features extracted from each turn snapshot. Dropped total_permanent_diff
# (linear combination of creature_count + land + non_creature — redundant in
# a linear model and causes collinearity).
FEATURE_NAMES = [
    "life_diff",
    "creature_count_diff",
    "creature_mv_diff",
    "hand_diff",
    "land_diff",
    "non_creature_diff",
    "mana_spent_diff",
]

# Mapping from regression feature names to EvalWeights struct fields.
FEATURE_TO_WEIGHT = {
    "life_diff": "life",
    "creature_count_diff": "board_presence",
    "creature_mv_diff": "board_power",
    "hand_diff": "hand_size",
    "non_creature_diff": "card_advantage",
}

# Hand-tuned defaults for weights 17Lands cannot measure.
HAND_TUNED = {
    "board_toughness": 1.0,
    "aggression": 0.5,
    "zone_quality": 0.3,
    "synergy": 0.5,
}

# Target maximum absolute weight value after scaling.
MAX_WEIGHT_SCALE = 2.5

# Self-play harvest features. Names are exactly the EvalWeights struct fields, so
# each fitted coefficient maps DIRECTLY onto its weight (no proxy map). All 9 are
# fitted; `energy_offset` is a fixed-coefficient control fed to the regression and
# then discarded (matching the engine's post-weighting energy offset).
SELFPLAY_FEATURE_NAMES = [
    "life",
    "board_presence",
    "board_power",
    "board_toughness",
    "hand_size",
    "aggression",
    "card_advantage",
    "zone_quality",
    "synergy",
]
SELFPLAY_CONTROL = "energy_offset"

# Smoke thresholds under --allow-tiny-corpus (vs the production 1000 / 100).
TINY_GLOBAL_MIN = 10
TINY_PHASE_MIN = 2

# Turn boundaries for game phases.
EARLY_MAX = 3   # turns 1-3
MID_MAX = 7     # turns 4-7
# turns 8+ = late

# Column suffixes needed per turn-side prefix.
_TURN_SUFFIXES = [
    "_eot_user_life", "_eot_oppo_life",
    "_eot_user_creatures_in_play", "_eot_oppo_creatures_in_play",
    "_eot_user_cards_in_hand", "_eot_oppo_cards_in_hand",
    "_eot_user_lands_in_play", "_eot_oppo_lands_in_play",
    "_eot_user_non_creatures_in_play", "_eot_oppo_non_creatures_in_play",
    "_user_mana_spent", "_oppo_mana_spent",
]


def turn_phase(turn: int) -> str:
    """Classify a turn number into a game phase."""
    if turn <= EARLY_MAX:
        return "early"
    elif turn <= MID_MAX:
        return "mid"
    else:
        return "late"


def needed_columns() -> list[str]:
    """Pre-compute all column names we need from the CSV."""
    cols = ["won", "user_game_win_rate_bucket", "user_n_games_bucket"]
    for turn in range(1, 31):
        for side in ["user", "oppo"]:
            prefix = f"{side}_turn_{turn}"
            for suffix in _TURN_SUFFIXES:
                cols.append(f"{prefix}{suffix}")
    return cols


def count_ids_vec(series: pd.Series) -> np.ndarray:
    """Vectorized: count pipe-separated IDs. Returns 0 for NaN/empty."""
    str_series = series.astype(str)
    mask = series.notna() & (str_series != "") & (str_series != "nan")
    result = np.zeros(len(series), dtype=np.float64)
    if mask.any():
        result[mask.values] = str_series[mask].str.count(r"\|").values + 1
    return result


def _sum_mv_one(val: object, card_mv: dict) -> float:
    """Sum mana values for a single pipe-separated Arena ID string."""
    if pd.isna(val):
        return 0.0
    s = str(val).strip()
    if s == "" or s == "nan":
        return 0.0
    total = 0.0
    for part in s.split("|"):
        try:
            total += card_mv.get(int(float(part)), 0.0)
        except (ValueError, OverflowError):
            continue
    return total


def sum_mv_vec(series: pd.Series, card_mv: dict) -> np.ndarray:
    """Sum mana values for pipe-separated Arena IDs across a Series.

    Uses apply() with a tight loop — faster than explode+groupby for short
    pipe-lists (typically 3-7 creature IDs per cell).
    """
    return series.apply(_sum_mv_one, args=(card_mv,)).values.astype(np.float64)


def load_card_data(data_dir: str) -> dict:
    """Load cards.csv and return Arena ID -> mana_value mapping."""
    cards_path = os.path.join(data_dir, "cards.csv")
    if not os.path.exists(cards_path):
        print(f"ERROR: cards.csv not found at {cards_path}", file=sys.stderr)
        sys.exit(1)

    cards = pd.read_csv(cards_path)
    card_mv = {}
    for _, row in cards.iterrows():
        try:
            arena_id = int(row["id"])
            mv = float(row["mana_value"]) if pd.notna(row["mana_value"]) else 0.0
            card_mv[arena_id] = mv
        except (ValueError, KeyError):
            continue

    print(f"Loaded {len(card_mv)} card entries from cards.csv", file=sys.stderr)
    return card_mv


def discover_replay_files(data_dir: str) -> list[str]:
    """Find all 17Lands Premier Draft replay CSVs."""
    pattern = os.path.join(data_dir, "replay_data_public.*.PremierDraft.csv")
    files = sorted(glob.glob(pattern))
    if not files:
        print(f"ERROR: No replay CSVs found matching {pattern}", file=sys.stderr)
        sys.exit(1)

    print(f"\nDiscovered {len(files)} replay file(s):", file=sys.stderr)
    for f in files:
        size_mb = os.path.getsize(f) / (1024 * 1024)
        set_code = Path(f).name.split(".")[1]
        print(f"  {set_code}: {Path(f).name} ({size_mb:.0f} MB)", file=sys.stderr)

    if len(files) < 3:
        print(
            f"\nWARNING: Only {len(files)} set(s) found. "
            "Recommend 3-5 sets for robust weights (per D-03).",
            file=sys.stderr,
        )

    return files


def process_replay_files(
    files: list[str],
    card_mv: dict,
    min_win_rate: float,
    min_games: int,
) -> tuple[dict[str, np.ndarray], dict[str, np.ndarray], list[str]]:
    """Stream replay CSVs and extract training features bucketed by game phase.

    Uses vectorized pandas operations instead of row iteration for performance.
    Only reads the ~720 columns needed (out of ~2800) via usecols filtering.

    Returns (phase_features, phase_labels, set_codes) where phase_features
    values are (N, 7) numpy arrays and phase_labels values are (N,) arrays.
    """
    phase_feat_chunks: dict[str, list[np.ndarray]] = {"early": [], "mid": [], "late": []}
    phase_label_chunks: dict[str, list[np.ndarray]] = {"early": [], "mid": [], "late": []}
    set_codes = []
    total_games = 0
    total_filtered_games = 0
    total_samples = 0

    # Pre-compute column names we need.
    want_cols = set(needed_columns())

    for filepath in files:
        set_code = Path(filepath).name.split(".")[1]
        set_codes.append(set_code)
        file_samples = 0
        file_games = 0
        file_filtered = 0

        print(f"\nProcessing {set_code}...", file=sys.stderr)

        # Read header to find which of our wanted columns actually exist.
        header = pd.read_csv(filepath, nrows=0).columns.tolist()
        use_cols = [c for c in header if c in want_cols]
        print(f"  Reading {len(use_cols)}/{len(header)} columns", file=sys.stderr)

        for chunk in pd.read_csv(
            filepath, chunksize=50000, usecols=use_cols, low_memory=False
        ):
            file_games += len(chunk)

            # Skill and experience filters
            if "user_game_win_rate_bucket" in chunk.columns:
                chunk = chunk[chunk["user_game_win_rate_bucket"] >= min_win_rate]
            if "user_n_games_bucket" in chunk.columns:
                chunk = chunk[chunk["user_n_games_bucket"] >= min_games]

            file_filtered += len(chunk)
            if chunk.empty:
                continue

            # Filter out rows with unknown game outcomes (NaN → True if not caught)
            chunk = chunk[chunk["won"].notna()]
            if chunk.empty:
                continue

            won = chunk["won"].values.astype(bool)

            # Process each turn vectorized across all rows in the chunk.
            for turn in range(1, 31):
                phase = turn_phase(turn)

                for side in ["user", "oppo"]:
                    prefix = f"{side}_turn_{turn}"
                    life_col = f"{prefix}_eot_user_life"

                    oppo_life_col = f"{prefix}_eot_oppo_life"
                    if life_col not in chunk.columns or oppo_life_col not in chunk.columns:
                        continue

                    # Rows where this turn exists (life column is not NaN)
                    valid = chunk[life_col].notna()
                    if not valid.any():
                        continue

                    sub = chunk[valid]
                    sub_won = won[valid.values]

                    # All features computed vectorized across the sub-DataFrame
                    life_diff = (
                        sub[f"{prefix}_eot_user_life"].values
                        - sub[f"{prefix}_eot_oppo_life"].values
                    )

                    creature_count_diff = (
                        count_ids_vec(sub[f"{prefix}_eot_user_creatures_in_play"])
                        - count_ids_vec(sub[f"{prefix}_eot_oppo_creatures_in_play"])
                    )

                    creature_mv_diff = (
                        sum_mv_vec(sub[f"{prefix}_eot_user_creatures_in_play"], card_mv)
                        - sum_mv_vec(sub[f"{prefix}_eot_oppo_creatures_in_play"], card_mv)
                    )

                    user_hand_count = count_ids_vec(
                        sub[f"{prefix}_eot_user_cards_in_hand"]
                    )
                    oppo_hand = (
                        sub[f"{prefix}_eot_oppo_cards_in_hand"]
                        .fillna(0)
                        .values.astype(np.float64)
                    )
                    hand_diff = user_hand_count - oppo_hand

                    land_diff = (
                        count_ids_vec(sub[f"{prefix}_eot_user_lands_in_play"])
                        - count_ids_vec(sub[f"{prefix}_eot_oppo_lands_in_play"])
                    )

                    nc_diff = (
                        count_ids_vec(sub[f"{prefix}_eot_user_non_creatures_in_play"])
                        - count_ids_vec(sub[f"{prefix}_eot_oppo_non_creatures_in_play"])
                    )

                    mana_user_col = f"{prefix}_user_mana_spent"
                    mana_oppo_col = f"{prefix}_oppo_mana_spent"
                    user_mana = (
                        sub[mana_user_col].fillna(0).values.astype(np.float64)
                        if mana_user_col in sub.columns
                        else np.zeros(len(sub))
                    )
                    oppo_mana = (
                        sub[mana_oppo_col].fillna(0).values.astype(np.float64)
                        if mana_oppo_col in sub.columns
                        else np.zeros(len(sub))
                    )
                    mana_diff = user_mana - oppo_mana

                    # Stack into (N, 7) feature matrix
                    features = np.column_stack([
                        life_diff, creature_count_diff, creature_mv_diff,
                        hand_diff, land_diff, nc_diff, mana_diff,
                    ])

                    phase_feat_chunks[phase].append(features)
                    phase_label_chunks[phase].append(sub_won.astype(np.int64))
                    file_samples += len(features)

        total_games += file_games
        total_filtered_games += file_filtered
        total_samples += file_samples
        print(
            f"  {set_code}: {file_games} games, {file_filtered} after filter, "
            f"{file_samples} training samples",
            file=sys.stderr,
        )

    print(
        f"\nTotal: {total_games} games, {total_filtered_games} after filter, "
        f"{total_samples} training samples",
        file=sys.stderr,
    )

    # Concatenate all arrays per phase
    phase_features = {}
    phase_labels = {}
    for phase in ["early", "mid", "late"]:
        if phase_feat_chunks[phase]:
            phase_features[phase] = np.vstack(phase_feat_chunks[phase])
            phase_labels[phase] = np.concatenate(phase_label_chunks[phase])
        else:
            phase_features[phase] = np.empty((0, 7))
            phase_labels[phase] = np.empty(0)
        print(
            f"  {phase}: {len(phase_features[phase])} samples",
            file=sys.stderr,
        )

    return phase_features, phase_labels, set_codes


def train_model(
    X: np.ndarray, y: np.ndarray, phase_name: str
) -> tuple[LogisticRegression, float, float]:
    """Train logistic regression for a single phase and return model + accuracy."""
    # Clean out rows with inf or NaN (corrupt data from extreme game states)
    finite_mask = np.isfinite(X).all(axis=1)
    if not finite_mask.all():
        n_bad = (~finite_mask).sum()
        print(f"  {phase_name}: dropping {n_bad} rows with inf/NaN values", file=sys.stderr)
        X = X[finite_mask]
        y = y[finite_mask]

    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=0.2, random_state=42, stratify=y
    )

    model = LogisticRegression(penalty="l2", C=1.0, max_iter=1000, random_state=42)
    model.fit(X_train, y_train)

    train_accuracy = model.score(X_train, y_train)
    test_accuracy = model.score(X_test, y_test)

    print(f"\n  {phase_name} accuracy: train={train_accuracy:.4f} test={test_accuracy:.4f}", file=sys.stderr)

    return model, train_accuracy, test_accuracy


def extract_and_scale_weights(
    model: LogisticRegression,
    phase_name: str,
) -> tuple[dict, dict]:
    """Extract coefficients and scale to EvalWeights range.

    Returns (raw_coefficients, scaled_weights).
    """
    raw_coefs = {}
    for name, coef in zip(FEATURE_NAMES, model.coef_[0]):
        raw_coefs[name] = round(float(coef), 6)

    print(f"  {phase_name} raw coefficients:", file=sys.stderr)
    for name, coef in raw_coefs.items():
        sign = "+" if coef >= 0 else ""
        print(f"    {name}: {sign}{coef}", file=sys.stderr)

    # Sanity checks
    if raw_coefs["life_diff"] <= 0:
        print(
            f"  WARNING: {phase_name} life_diff coefficient is non-positive!",
            file=sys.stderr,
        )
    if raw_coefs["creature_count_diff"] <= 0:
        print(
            f"  WARNING: {phase_name} creature_count_diff coefficient is non-positive!",
            file=sys.stderr,
        )

    # Scale mapped coefficients so max absolute value = MAX_WEIGHT_SCALE.
    mapped_coefs = {
        feat: raw_coefs[feat]
        for feat in FEATURE_TO_WEIGHT
        if feat in raw_coefs
    }

    max_abs = max(abs(v) for v in mapped_coefs.values()) if mapped_coefs else 1.0
    scale_factor = MAX_WEIGHT_SCALE / max_abs if max_abs > 0 else 1.0

    weights = {}
    for feat_name, weight_name in FEATURE_TO_WEIGHT.items():
        scaled = abs(raw_coefs[feat_name]) * scale_factor
        weights[weight_name] = round(scaled, 4)

    # Add hand-tuned defaults for unmeasurable weights
    weights.update(HAND_TUNED)

    print(f"  {phase_name} scaled weights:", file=sys.stderr)
    for name, val in weights.items():
        source = "17Lands" if name not in HAND_TUNED else "hand-tuned"
        print(f"    {name}: {val} ({source})", file=sys.stderr)

    return raw_coefs, weights


def load_selfplay_corpus(
    glob_pattern: str,
) -> tuple[dict[str, np.ndarray], dict[str, np.ndarray], dict, list[str], set]:
    """Read JSONL harvest shards, skip meta lines, bucket rows by turn phase.

    Returns (phase_features, phase_labels, meta, files, seeds) where
    phase_features values are (N, 10) arrays (9 fitted features + the
    `energy_offset` control) and phase_labels values are (N,) arrays of 0/1.
    """
    files = sorted(glob.glob(glob_pattern))
    if not files:
        print(
            f"ERROR: No self-play JSONL shards matched {glob_pattern}",
            file=sys.stderr,
        )
        sys.exit(1)

    phase_feat: dict[str, list[list[float]]] = {"early": [], "mid": [], "late": []}
    phase_lab: dict[str, list[int]] = {"early": [], "mid": [], "late": []}
    meta: dict = {}
    seeds: set = set()
    total = 0
    columns = SELFPLAY_FEATURE_NAMES + [SELFPLAY_CONTROL]

    for path in files:
        with open(path) as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                obj = json.loads(line)
                if "meta" in obj:
                    # First meta line wins for provenance; shards share a run.
                    if not meta:
                        meta = obj["meta"]
                    continue
                feats = obj["features"]
                phase = turn_phase(int(obj["turn"]))
                phase_feat[phase].append([float(feats[name]) for name in columns])
                phase_lab[phase].append(1 if obj["won"] else 0)
                seeds.add(int(obj["seed"]))
                total += 1

    phase_features = {}
    phase_labels = {}
    for phase in ["early", "mid", "late"]:
        if phase_feat[phase]:
            phase_features[phase] = np.asarray(phase_feat[phase], dtype=np.float64)
            phase_labels[phase] = np.asarray(phase_lab[phase], dtype=np.int64)
        else:
            phase_features[phase] = np.empty((0, len(columns)))
            phase_labels[phase] = np.empty(0)
        print(
            f"  {phase}: {len(phase_features[phase])} samples",
            file=sys.stderr,
        )

    print(f"\nTotal self-play samples: {total}", file=sys.stderr)
    return phase_features, phase_labels, meta, files, seeds


def train_selfplay_model(
    X: np.ndarray, y: np.ndarray, phase_name: str
):
    """Train logistic regression for one self-play phase.

    Skips single-class phases with a warning instead of crashing inside
    `train_test_split(stratify=y)` (which requires ≥2 classes). Returns
    `(model, train_acc, test_acc)` or `None` when skipped.
    """
    finite_mask = np.isfinite(X).all(axis=1)
    if not finite_mask.all():
        n_bad = int((~finite_mask).sum())
        print(f"  {phase_name}: dropping {n_bad} rows with inf/NaN values", file=sys.stderr)
        X = X[finite_mask]
        y = y[finite_mask]

    # train_test_split(stratify=y) requires at least two classes AND at least
    # two members per class; a single-class phase (all wins or all losses) or a
    # class with one member (reachable under --allow-tiny-corpus, whose
    # per-phase floor is 2) is unsplittable — skip it rather than crash.
    classes, class_counts = np.unique(y, return_counts=True)
    if len(classes) < 2 or class_counts.min() < 2:
        print(
            f"  WARNING: {phase_name} lacks two outcome classes with >=2 "
            "members each — skipping (cannot stratify).",
            file=sys.stderr,
        )
        return None

    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=0.2, random_state=42, stratify=y
    )
    model = LogisticRegression(penalty="l2", C=1.0, max_iter=1000, random_state=42)
    model.fit(X_train, y_train)
    train_accuracy = model.score(X_train, y_train)
    test_accuracy = model.score(X_test, y_test)
    print(
        f"\n  {phase_name} accuracy: train={train_accuracy:.4f} test={test_accuracy:.4f}",
        file=sys.stderr,
    )
    return model, train_accuracy, test_accuracy


def extract_selfplay_weights(model: LogisticRegression, phase_name: str) -> tuple[dict, dict]:
    """Map self-play coefficients DIRECTLY onto weight names (no proxy map).

    The `energy_offset` control coefficient is recorded but discarded — it is a
    fixed serve-time offset, not a fitted weight. Remaining coefficients are
    `abs()`-ed and scaled so the max maps to MAX_WEIGHT_SCALE. HAND_TUNED is NOT
    applied: self-play measures every weight.
    """
    columns = SELFPLAY_FEATURE_NAMES + [SELFPLAY_CONTROL]
    raw_coefs = {name: round(float(coef), 6) for name, coef in zip(columns, model.coef_[0])}

    print(f"  {phase_name} raw coefficients:", file=sys.stderr)
    for name, coef in raw_coefs.items():
        sign = "+" if coef >= 0 else ""
        tag = " (control, discarded)" if name == SELFPLAY_CONTROL else ""
        print(f"    {name}: {sign}{coef}{tag}", file=sys.stderr)

    fitted = {name: raw_coefs[name] for name in SELFPLAY_FEATURE_NAMES}
    max_abs = max(abs(v) for v in fitted.values()) if fitted else 1.0
    scale_factor = MAX_WEIGHT_SCALE / max_abs if max_abs > 0 else 1.0
    weights = {name: round(abs(coef) * scale_factor, 4) for name, coef in fitted.items()}

    print(f"  {phase_name} scaled weights (all self-play fitted):", file=sys.stderr)
    for name, val in weights.items():
        print(f"    {name}: {val}", file=sys.stderr)

    return raw_coefs, weights


def run_selfplay(args) -> None:
    """Fit all 9 EvalWeights per phase from a self-play harvest corpus."""
    print("=== Self-Play Phase-Aware EvalWeights Training ===\n", file=sys.stderr)

    phase_features, phase_labels, meta, files, seeds = load_selfplay_corpus(args.selfplay_glob)

    total_samples = sum(len(v) for v in phase_features.values())
    global_min = TINY_GLOBAL_MIN if args.allow_tiny_corpus else 1000
    if total_samples < global_min:
        print(
            f"ERROR: Only {total_samples} self-play samples "
            f"(need >= {global_min}; pass --allow-tiny-corpus for smoke runs).",
            file=sys.stderr,
        )
        sys.exit(1)

    phase_min = TINY_PHASE_MIN if args.allow_tiny_corpus else 100
    print("\n--- Training phase-specific self-play models ---", file=sys.stderr)
    phase_results = {}
    for phase in ["early", "mid", "late"]:
        X = phase_features[phase].astype(np.float64)
        y = phase_labels[phase].astype(np.int64)
        if len(X) < phase_min:
            print(
                f"  WARNING: {phase} has only {len(X)} samples "
                f"(< {phase_min}), skipping",
                file=sys.stderr,
            )
            continue
        trained = train_selfplay_model(X, y, phase)
        if trained is None:
            continue
        model, train_acc, test_acc = trained
        raw_coefs, weights = extract_selfplay_weights(model, phase)
        phase_results[phase] = {
            "sample_count": int(len(X)),
            "train_accuracy": round(train_acc, 4),
            "test_accuracy": round(test_acc, 4),
            "raw_coefficients": raw_coefs,
            "weights": weights,
        }

    if not phase_results:
        # Every phase was empty, sub-threshold, or single-class — no fittable
        # signal. A fully single-class corpus lands here.
        print(
            "ERROR: no phase produced fittable weights (all phases were empty, "
            "below threshold, or single-class). Nothing to write.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Identity/provenance: the artifact is the identity-bearing object.
    output = {
        "kind": "selfplay_phase_weights",
        "source": "phase_ai_selfplay_harvest",
        "git_sha": meta.get("git_sha"),
        "card_data_hash": meta.get("card_data_hash"),
        "difficulty": meta.get("difficulty"),
        "corpus_files": [os.path.basename(f) for f in files],
        "base_seeds": sorted(seeds),
        "total_sample_count": total_samples,
        "feature_names": SELFPLAY_FEATURE_NAMES,
        "control_feature": SELFPLAY_CONTROL,
        "turn_boundaries": {
            "early": f"turns 1-{EARLY_MAX}",
            "mid": f"turns {EARLY_MAX + 1}-{MID_MAX}",
            "late": f"turns {MID_MAX + 1}+",
        },
        "phases": phase_results,
    }

    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)
    with open(args.output, "w") as f:
        json.dump(output, f, indent=2)
        f.write("\n")

    print(f"\nSelf-play weights written to {args.output}", file=sys.stderr)

    # Comparison table: self-play fit vs the shipped 17Lands baseline (read from
    # data/learned-weights.json when present). LR is convex, so 17Lands weights are
    # the comparison baseline / shipped fallback, not a numerical initialization.
    baseline = {}
    baseline_path = os.path.join("data", "learned-weights.json")
    if os.path.exists(baseline_path):
        try:
            with open(baseline_path) as bf:
                baseline = json.load(bf).get("phases", {})
        except (OSError, json.JSONDecodeError):
            baseline = {}

    print("\n=== Self-play vs 17Lands weight comparison ===", file=sys.stderr)
    for phase in ["early", "mid", "late"]:
        sp = phase_results.get(phase, {}).get("weights")
        if not sp:
            continue
        bl = baseline.get(phase, {}).get("weights", {})
        print(f"\n[{phase}]  {'weight':<16}{'selfplay':>10}{'17lands':>10}", file=sys.stderr)
        for name in SELFPLAY_FEATURE_NAMES:
            sp_val = sp.get(name)
            bl_val = bl.get(name, "-")
            bl_str = f"{bl_val:>10.4f}" if isinstance(bl_val, (int, float)) else f"{bl_val:>10}"
            print(f"  {'':<2}{name:<16}{sp_val:>10.4f}{bl_str}", file=sys.stderr)

    print("\nDone.", file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(
        description="Train turn-phase-aware AI evaluation weights from 17Lands replay data."
    )
    parser.add_argument(
        "--data-dir",
        default="data/17lands",
        help="Directory containing replay CSVs and cards.csv (default: data/17lands)",
    )
    parser.add_argument(
        "--output",
        default="data/learned-weights.json",
        help="Output JSON path (default: data/learned-weights.json)",
    )
    parser.add_argument(
        "--min-win-rate",
        type=float,
        default=0.55,
        help="Minimum user_game_win_rate_bucket filter (default: 0.55)",
    )
    parser.add_argument(
        "--min-games",
        type=int,
        default=50,
        help="Minimum user_n_games_bucket filter (default: 50)",
    )
    parser.add_argument(
        "--source",
        choices=["17lands", "selfplay"],
        default="17lands",
        help="Training data source (default: 17lands; the existing path is untouched)",
    )
    parser.add_argument(
        "--selfplay-glob",
        default="data/selfplay/*.jsonl",
        help="Glob for self-play harvest JSONL shards (--source selfplay)",
    )
    parser.add_argument(
        "--allow-tiny-corpus",
        action="store_true",
        help="Lower sample-count guards to smoke thresholds "
        f"({TINY_GLOBAL_MIN} global / {TINY_PHASE_MIN} per phase) for pipeline validation",
    )
    args = parser.parse_args()

    if args.source == "selfplay":
        run_selfplay(args)
        return

    print("=== 17Lands Phase-Aware EvalWeights Training ===\n", file=sys.stderr)

    # Load card metadata
    card_mv = load_card_data(args.data_dir)

    # Discover replay files
    files = discover_replay_files(args.data_dir)

    # Extract features bucketed by game phase
    phase_features, phase_labels, set_codes = process_replay_files(
        files, card_mv, args.min_win_rate, args.min_games
    )

    total_samples = sum(len(v) for v in phase_features.values())
    if total_samples < 1000:
        print(
            f"ERROR: Only {total_samples} training samples extracted. "
            "Need at least 1000 for meaningful training.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Train one model per game phase
    print("\n--- Training phase-specific models ---", file=sys.stderr)
    phase_results = {}

    for phase in ["early", "mid", "late"]:
        X = phase_features[phase].astype(np.float64)
        y = phase_labels[phase].astype(np.float64)

        if len(X) < 100:
            print(f"  WARNING: {phase} has only {len(X)} samples, skipping", file=sys.stderr)
            continue

        model, train_acc, test_acc = train_model(X, y, phase)
        raw_coefs, weights = extract_and_scale_weights(model, phase)

        phase_results[phase] = {
            "sample_count": int(len(X)),
            "train_accuracy": round(train_acc, 4),
            "test_accuracy": round(test_acc, 4),
            "raw_coefficients": raw_coefs,
            "weights": weights,
        }

    # Build output JSON
    output = {
        "kind": "17lands_phase_weights",
        "source": "17lands_PremierDraft_phase_aware",
        "sets": set_codes,
        "filter": f"win_rate >= {args.min_win_rate}, games >= {args.min_games}",
        "total_sample_count": total_samples,
        "feature_names": FEATURE_NAMES,
        "turn_boundaries": {
            "early": f"turns 1-{EARLY_MAX}",
            "mid": f"turns {EARLY_MAX + 1}-{MID_MAX}",
            "late": f"turns {MID_MAX + 1}+",
        },
        "phases": phase_results,
    }

    # Ensure output directory exists
    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)

    with open(args.output, "w") as f:
        json.dump(output, f, indent=2)
        f.write("\n")

    print(f"\nPhase-aware weights written to {args.output}", file=sys.stderr)
    print(f"Total samples: {total_samples}", file=sys.stderr)

    # Summary table
    print("\n=== Phase Weight Comparison ===", file=sys.stderr)
    header = f"{'weight':<18}"
    for phase in ["early", "mid", "late"]:
        header += f"  {phase:>8}"
    print(header, file=sys.stderr)
    print("-" * len(header), file=sys.stderr)

    if phase_results:
        all_weight_names = list(phase_results[next(iter(phase_results))]["weights"].keys())
        for wname in all_weight_names:
            row = f"{wname:<18}"
            for phase in ["early", "mid", "late"]:
                val = phase_results.get(phase, {}).get("weights", {}).get(wname, "-")
                if isinstance(val, float):
                    row += f"  {val:>8.4f}"
                else:
                    row += f"  {str(val):>8}"
            print(row, file=sys.stderr)

    print("\nDone.", file=sys.stderr)


if __name__ == "__main__":
    main()
