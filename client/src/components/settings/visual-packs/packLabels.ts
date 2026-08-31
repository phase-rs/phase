import type { TFunction } from "i18next";

import type { CatalogRoot, PackId } from "../../../services/visualPacks/types.ts";

const PRINTING = /^printing:([a-z0-9]{3,6})$/;
const LOCALE = /^locale:(de|es|fr|it|pt):([a-z0-9]{3,6})$/;

/**
 * How much of a 64-hex digest is shown.
 *
 * Enough to tell two installed packs apart at a glance and to quote in a bug
 * report; not enough to be mistaken for something the user is meant to read.
 * Twelve hex characters is 48 bits. The panel DOES compare digests, in roughly
 * fifteen places — estimate binding, summary acceptance, operation identity,
 * the curated drift verdict — and every one of those compares the full value.
 * Only display truncates, so a shortened digest is never an operand.
 */
const DIGEST_PREFIX = 12;

/**
 * The name a user sees for an installed or planned pack.
 *
 * `PackId` is a wire identity with a grammar (`printing:fin`,
 * `locale:de:fin`), and the panel rendered it verbatim: a user managing their
 * own downloads was reading the selector vocabulary of the backend. The set
 * code stays, uppercased, because it IS what the user typed to install the
 * pack and is the only part that distinguishes one printing pack from another.
 *
 * Unrecognised ids fall through to the raw value rather than to a placeholder.
 * `PackId`'s pattern admits exactly the five shapes below, so this is
 * unreachable through the brand — but the row it labels is an INSTALLED pack,
 * read back from a database written by an older version of this app, and a row
 * the panel cannot name must still be nameable to whoever is asked to remove
 * it.
 */
export function packLabel(id: PackId, t: TFunction<"settings">): string {
  if (id === "core") return t("visualPacks.packs.core");
  if (id === "complete") return t("visualPacks.packs.complete");
  if (id === "curated") return t("visualPacks.packs.curated");
  if (id === "deck_library") return t("visualPacks.packs.deckLibrary");
  const printing = PRINTING.exec(id);
  if (printing) return t("visualPacks.packs.printing", { set: printing[1].toUpperCase() });
  const locale = LOCALE.exec(id);
  if (locale) return t("visualPacks.packs.locale", { set: locale[2].toUpperCase(), language: locale[1].toUpperCase() });
  return id;
}

/** A digest as a label — see `DIGEST_PREFIX`. */
export function shortDigest(digest: CatalogRoot): string {
  return `${digest.slice(0, DIGEST_PREFIX)}…`;
}
