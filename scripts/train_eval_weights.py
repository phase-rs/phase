#!/usr/bin/env python3
"""Train AI evaluation weights from 17Lands Premier Draft replay data.

Extracts per-turn board state features from 17Lands replay CSVs, trains a
logistic regression model to predict game outcomes, and outputs the learned
coefficients as scaled EvalWeights for the phase-ai Rust crate.

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


FEATURE_NAMES = [
    "life_diff",
    "creature_count_diff",
    "creature_mv_diff",
    "hand_diff",
    "land_diff",
    "non_creature_diff",
    "mana_spent_diff",
    "total_permanent_diff",
]

# Mapping from regression feature names to EvalWeights struct fields.
# Features not in this map contribute to the model but don't directly map
# to a single EvalWeight field.
FEATURE_TO_WEIGHT = {
    "life_diff": "life",
    "creature_count_diff": "board_presence",
    "creature_mv_diff": "board_power",
    "hand_diff": "hand_size",
}

# Hand-tuned defaults for weights 17Lands cannot measure.
HAND_TUNED = {
    "board_toughness": 1.0,
    "aggression": 0.5,
}

# Target maximum absolute weight value after scaling.
MAX_WEIGHT_SCALE = 2.5


def count_ids(value) -> int:
    """Count pipe-separated IDs. Returns 0 for NaN/empty."""
    if pd.isna(value):
        return 0
    s = str(value).strip()
    if s == "" or s == "nan":
        return 0
    return len(s.split("|"))


def sum_mv(value, card_mv: dict) -> float:
    """Sum mana values for pipe-separated Arena IDs."""
    if pd.isna(value):
        return 0.0
    s = str(value).strip()
    if s == "" or s == "nan":
        return 0.0
    total = 0.0
    for part in s.split("|"):
        try:
            arena_id = int(float(part))
            total += card_mv.get(arena_id, 0.0)
        except (ValueError, OverflowError):
            continue
    return total


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


def extract_turn_features(
    row: pd.Series,
    prefix: str,
    card_mv: dict,
    is_user_turn: bool,
) -> list[float] | None:
    """Extract differential features for one turn.

    Returns feature vector or None if turn data is missing.
    """
    life_col = f"{prefix}_eot_user_life"
    if life_col not in row.index or pd.isna(row.get(life_col)):
        return None

    user_life = float(row[f"{prefix}_eot_user_life"])
    oppo_life = float(row[f"{prefix}_eot_oppo_life"])

    user_creatures = row.get(f"{prefix}_eot_user_creatures_in_play")
    oppo_creatures = row.get(f"{prefix}_eot_oppo_creatures_in_play")
    user_hand = row.get(f"{prefix}_eot_user_cards_in_hand")
    oppo_hand = row.get(f"{prefix}_eot_oppo_cards_in_hand")
    user_lands = row.get(f"{prefix}_eot_user_lands_in_play")
    oppo_lands = row.get(f"{prefix}_eot_oppo_lands_in_play")
    user_nc = row.get(f"{prefix}_eot_user_non_creatures_in_play")
    oppo_nc = row.get(f"{prefix}_eot_oppo_non_creatures_in_play")

    # Mana spent columns use a different naming pattern (no _eot_ prefix)
    user_mana = row.get(f"{prefix}_user_mana_spent", 0.0)
    oppo_mana = row.get(f"{prefix}_oppo_mana_spent", 0.0)
    user_mana = float(user_mana) if pd.notna(user_mana) else 0.0
    oppo_mana = float(oppo_mana) if pd.notna(oppo_mana) else 0.0

    life_diff = user_life - oppo_life
    creature_count_diff = count_ids(user_creatures) - count_ids(oppo_creatures)
    creature_mv_diff = sum_mv(user_creatures, card_mv) - sum_mv(oppo_creatures, card_mv)

    # Hand: user hand is pipe-separated IDs, opponent hand is a count
    user_hand_count = count_ids(user_hand)
    oppo_hand_count = float(oppo_hand) if pd.notna(oppo_hand) else 0.0
    hand_diff = user_hand_count - oppo_hand_count

    land_diff = count_ids(user_lands) - count_ids(oppo_lands)
    non_creature_diff = count_ids(user_nc) - count_ids(oppo_nc)
    mana_spent_diff = user_mana - oppo_mana
    total_permanent_diff = creature_count_diff + land_diff + non_creature_diff

    # For opponent turns, flip perspective: the "user" in the CSV is still
    # the player whose game outcome we know, so differentials stay the same
    # direction. The label gets flipped in the caller for oppo turns.

    return [
        life_diff,
        creature_count_diff,
        creature_mv_diff,
        hand_diff,
        land_diff,
        non_creature_diff,
        mana_spent_diff,
        total_permanent_diff,
    ]


def process_replay_files(
    files: list[str],
    card_mv: dict,
    min_win_rate: float,
    min_games: int,
) -> tuple[np.ndarray, np.ndarray, list[str]]:
    """Stream replay CSVs and extract training features.

    Returns (X, y, set_codes).
    """
    all_features = []
    all_labels = []
    set_codes = []
    total_games = 0
    total_filtered_games = 0

    for filepath in files:
        set_code = Path(filepath).name.split(".")[1]
        set_codes.append(set_code)
        file_samples = 0
        file_games = 0
        file_filtered = 0

        print(f"\nProcessing {set_code}...", file=sys.stderr)

        for chunk in pd.read_csv(filepath, chunksize=10000, low_memory=False):
            file_games += len(chunk)

            # Skill and experience filters
            if "user_game_win_rate_bucket" in chunk.columns:
                chunk = chunk[chunk["user_game_win_rate_bucket"] >= min_win_rate]
            if "user_n_games_bucket" in chunk.columns:
                chunk = chunk[chunk["user_n_games_bucket"] >= min_games]

            file_filtered += len(chunk)

            for _, row in chunk.iterrows():
                won = bool(row.get("won", False))

                for turn in range(1, 31):
                    # User's turn
                    user_prefix = f"user_turn_{turn}"
                    feats = extract_turn_features(row, user_prefix, card_mv, True)
                    if feats is not None:
                        all_features.append(feats)
                        all_labels.append(1 if won else 0)
                        file_samples += 1

                    # Opponent's turn
                    oppo_prefix = f"oppo_turn_{turn}"
                    feats = extract_turn_features(row, oppo_prefix, card_mv, False)
                    if feats is not None:
                        all_features.append(feats)
                        # For opponent turns, the board state is from the user's
                        # perspective (user_ columns are still the same player),
                        # so the label stays the same.
                        all_labels.append(1 if won else 0)
                        file_samples += 1

        total_games += file_games
        total_filtered_games += file_filtered
        print(
            f"  {set_code}: {file_games} games, {file_filtered} after filter, "
            f"{file_samples} training samples",
            file=sys.stderr,
        )

    print(
        f"\nTotal: {total_games} games, {total_filtered_games} after filter, "
        f"{len(all_features)} training samples",
        file=sys.stderr,
    )

    X = np.array(all_features, dtype=np.float64)
    y = np.array(all_labels, dtype=np.float64)

    return X, y, set_codes


def train_model(
    X: np.ndarray, y: np.ndarray
) -> tuple[LogisticRegression, float, float]:
    """Train logistic regression and return model + accuracy metrics."""
    X_train, X_test, y_train, y_test = train_test_split(
        X, y, test_size=0.2, random_state=42, stratify=y
    )

    model = LogisticRegression(penalty="l2", C=1.0, max_iter=1000, random_state=42)
    model.fit(X_train, y_train)

    train_accuracy = model.score(X_train, y_train)
    test_accuracy = model.score(X_test, y_test)

    print(f"\nModel accuracy:", file=sys.stderr)
    print(f"  Train: {train_accuracy:.4f}", file=sys.stderr)
    print(f"  Test:  {test_accuracy:.4f}", file=sys.stderr)

    return model, train_accuracy, test_accuracy


def extract_and_scale_weights(
    model: LogisticRegression,
) -> tuple[dict, dict]:
    """Extract coefficients and scale to EvalWeights range.

    Returns (raw_coefficients, scaled_weights).
    """
    raw_coefs = {}
    for name, coef in zip(FEATURE_NAMES, model.coef_[0]):
        raw_coefs[name] = round(float(coef), 6)

    print(f"\nRaw coefficients:", file=sys.stderr)
    for name, coef in raw_coefs.items():
        sign = "+" if coef >= 0 else ""
        print(f"  {name}: {sign}{coef}", file=sys.stderr)

    # Sanity checks
    if raw_coefs["life_diff"] <= 0:
        print(
            "WARNING: life_diff coefficient is non-positive! "
            "This is unexpected -- higher life should predict winning.",
            file=sys.stderr,
        )
    if raw_coefs["creature_count_diff"] <= 0:
        print(
            "WARNING: creature_count_diff coefficient is non-positive! "
            "This is unexpected -- more creatures should predict winning.",
            file=sys.stderr,
        )

    # Scale: map coefficients that correspond to EvalWeights fields
    # so the maximum absolute value becomes MAX_WEIGHT_SCALE.
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

    print(f"\nScaled EvalWeights (max={MAX_WEIGHT_SCALE}):", file=sys.stderr)
    for name, val in weights.items():
        source = "17Lands" if name not in HAND_TUNED else "hand-tuned"
        print(f"  {name}: {val} ({source})", file=sys.stderr)

    return raw_coefs, weights


def main():
    parser = argparse.ArgumentParser(
        description="Train AI evaluation weights from 17Lands replay data."
    )
    parser.add_argument(
        "--data-dir",
        default=os.path.expanduser("~/Downloads"),
        help="Directory containing replay CSVs and cards.csv (default: ~/Downloads)",
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
    args = parser.parse_args()

    print("=== 17Lands EvalWeights Training ===\n", file=sys.stderr)

    # Load card metadata
    card_mv = load_card_data(args.data_dir)

    # Discover replay files
    files = discover_replay_files(args.data_dir)

    # Extract features
    X, y, set_codes = process_replay_files(
        files, card_mv, args.min_win_rate, args.min_games
    )

    if len(X) < 1000:
        print(
            f"ERROR: Only {len(X)} training samples extracted. "
            "Need at least 1000 for meaningful training.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Train model
    model, train_accuracy, test_accuracy = train_model(X, y)

    # Extract and scale weights
    raw_coefs, weights = extract_and_scale_weights(model)

    # Build output JSON
    output = {
        "source": "17lands_PremierDraft",
        "sets": set_codes,
        "filter": f"win_rate >= {args.min_win_rate}, games >= {args.min_games}",
        "sample_count": len(X),
        "train_accuracy": round(train_accuracy, 4),
        "test_accuracy": round(test_accuracy, 4),
        "feature_names": FEATURE_NAMES,
        "raw_coefficients": raw_coefs,
        "weights": weights,
    }

    # Ensure output directory exists
    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)

    with open(args.output, "w") as f:
        json.dump(output, f, indent=2)
        f.write("\n")

    print(f"\nWeights written to {args.output}", file=sys.stderr)
    print(f"Sample count: {len(X)}", file=sys.stderr)
    print("Done.", file=sys.stderr)


if __name__ == "__main__":
    main()
