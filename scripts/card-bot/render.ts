// Renders a card's engine parse into a Discord embed that echoes the frontend's
// Alt-hover "ENGINE PARSE" overlay (client/src/components/card/CardPreview.tsx).
//
// Markdown layout (not a code block): the card's oracle text shows once as a `>`
// blockquote, then a compact one-line-per-item tree. Color comes from emoji —
// category squares (overlay palette), mana pips, the coverage meter — because
// Discord has no theme-adaptive text color (only ANSI, which is theme-fragile).

import type { Build } from "./config";
import type { BuildMeta, CoverageEntry, ParsedItem } from "./coverageData";
import manaEmojiRaw from "./manaEmoji.json";
import type { ScryfallCard } from "./scryfall";

// symbol ("{R}") → custom application-emoji markup ("<:manar:id>"). Populated by
// upload-emoji.ts; empty until then, in which case mana falls back to unicode.
const MANA_EMOJI = manaEmojiRaw as Record<string, string>;

// Category → abbreviation + a colored square matching the overlay palette
// (violet/sky/amber/teal/orange/rose). Unicode emoji render in color in any
// theme — unlike ANSI or custom <:emoji:> (which don't render in markdown spots).
const CATEGORY: Record<ParsedItem["category"], { abbr: string; square: string }> = {
  keyword: { abbr: "KW", square: "🟪" },
  ability: { abbr: "EFF", square: "🟦" },
  trigger: { abbr: "TRG", square: "🟨" },
  static: { abbr: "STC", square: "🟩" },
  replacement: { abbr: "RPL", square: "🟧" },
  cost: { abbr: "CST", square: "🟥" },
};

// Emerald / amber, matching the overlay's support colors.
const COLOR_OK = 0x10b981;
const COLOR_GAPS = 0xf59e0b;

const DESCRIPTION_MAX = 4096;
const DETAIL_VALUE_MAX = 120;
const SOURCE_TEXT_MAX = 160;
const ORACLE_MAX = 600;
const EM = "\u2003"; // em space (regular spaces collapse in markdown)

const truncate = (s: string, max: number) =>
  s.length > max ? `${s.slice(0, max - 1)}…` : s;

// MTG mana → emoji pips. `replace` (not match+join) preserves separators like the
// " // " between split/adventure faces. Generic costs stay plain digits; hybrid /
// Phyrexian / X fall back to {…} when not uploaded.
const COLOR_PIP: Record<string, string> = {
  W: "⚪",
  U: "🔵",
  B: "⚫",
  R: "🔴",
  G: "🟢",
  C: "◇",
  S: "❄️",
};

// Replaces every `{…}` symbol token in arbitrary text with its emoji — works on
// mana costs, oracle text ("{T}: Add {G}"), and quoted fragments alike. Safe to
// run anywhere markdown emoji render (description, `>` blockquotes), but NOT
// inside inline-code/```pre``` spans, where Discord shows the literal markup.
function symbolize(text: string): string {
  return text.replace(/\{[^}]+\}/g, (t) => {
    if (MANA_EMOJI[t]) return MANA_EMOJI[t]; // true pip (incl. tap/hybrid/Phyrexian)
    const sym = t.slice(1, -1);
    if (COLOR_PIP[sym]) return COLOR_PIP[sym]; // unicode fallback
    if (/^\d+$/.test(sym)) return sym;
    return t;
  });
}

const formatMana = (cost: string | null): string | null => (cost ? symbolize(cost) : null);

// A 10-segment coverage bar echoing the Alt overlay's emerald/amber meter.
function supportMeter(supported: number, total: number): string {
  const segs = 10;
  const filled =
    total === 0 ? segs : Math.max(0, Math.min(segs, Math.round((supported / total) * segs)));
  return "🟩".repeat(filled) + "🟧".repeat(segs - filled);
}

interface Counts {
  supported: number;
  total: number;
}

/** Recursively counts supported vs total nodes (the overlay's "3/5" fraction). */
function countItems(items: ParsedItem[], acc: Counts = { supported: 0, total: 0 }): Counts {
  for (const item of items) {
    acc.total += 1;
    if (item.supported) acc.supported += 1;
    if (item.children?.length) countItems(item.children, acc);
  }
  return acc;
}

/** The card's oracle text as a `>` blockquote, with `{…}` symbols as emoji. */
function oracleQuote(text: string): string {
  return symbolize(truncate(text.trim(), ORACLE_MAX))
    .split("\n")
    .map((l) => `> ${l}`)
    .join("\n");
}

/** One compact markdown line per parse item, em-space indented by depth. */
function treeLines(items: ParsedItem[], depth = 0, out: string[] = []): string[] {
  for (const item of items) {
    const indent = EM.repeat(depth);
    const cat = CATEGORY[item.category];
    if (item.supported) {
      let line = `${indent}${cat.square} **${cat.abbr}** ${symbolize(item.label)}`;
      if (item.details?.length) {
        // Detail values stay in inline-code chips — emoji can't render there, so
        // these keep the raw `{…}` text (intentional: code is the "verbatim" lane).
        const detail = item.details
          .map(([k, v]) => `${k} \`${truncate(v, DETAIL_VALUE_MAX)}\``)
          .join(" · ");
        line += ` · ${detail}`;
      }
      out.push(line);
    } else {
      // Unsupported lines read clearly as "engine couldn't handle this": a
      // leading ❌ (distinct from the category squares) and a struck-through
      // label, with the unparsed Oracle fragment quoted (symbols → emoji).
      let line = `${indent}❌ **${cat.abbr}** ~~${symbolize(item.label)}~~`;
      if (item.source_text)
        line += ` — “${symbolize(truncate(item.source_text, SOURCE_TEXT_MAX))}”`;
      out.push(line);
    }
    if (item.children?.length) treeLines(item.children, depth + 1, out);
  }
  return out;
}

/** Joins tree lines within the description budget, noting any dropped tail. */
function fitLines(lines: string[], budget: number): string {
  const kept: string[] = [];
  let used = 0;
  let dropped = 0;
  for (const line of lines) {
    if (used + line.length + 1 > budget) {
      dropped = lines.length - kept.length;
      break;
    }
    kept.push(line);
    used += line.length + 1;
  }
  let body = kept.join("\n");
  if (dropped > 0) body += `\n-# … ${dropped} more line(s) — see in-app`;
  return body;
}

/**
 * The header block, top of the description:
 *   line 1 — **Card Name** (linked to Scryfall) · mana cost
 *   line 2 — **Type line**
 *   then   — oracle text as a `>` blockquote
 *
 * The name lives here (not the embed `title`) so the mana emoji can sit beside
 * it — custom emoji don't render in an embed title, only in description text.
 */
function cardHeader(entry: CoverageEntry, scry: ScryfallCard | null): string {
  const link = scry?.scryfallUri;
  const name = `**${entry.card_name}**`;
  const mana = formatMana(scry?.manaCost ?? null);

  const lines: string[] = [];
  lines.push(mana ? `${link ? `[${name}](${link})` : name}  ${mana}` : link ? `[${name}](${link})` : name);
  if (scry?.typeLine) lines.push(`**${scry.typeLine}**`);
  if (entry.oracle_text) lines.push(oracleQuote(entry.oracle_text));
  return lines.join("\n");
}

function footerText(build: Build, meta: BuildMeta | null): string {
  const bits = [build.toUpperCase()];
  if (meta?.commit_short) bits.push(meta.commit_short);
  if (meta?.mtgjson_date) bits.push(`MTGJSON ${meta.mtgjson_date}`);
  return bits.join("  ·  ");
}

/** A Discord embed object (the subset we populate). */
export interface Embed {
  author?: { name: string };
  title?: string;
  url?: string;
  description?: string;
  color?: number;
  thumbnail?: { url: string };
  footer?: { text: string };
}

/** Per-face rendering overrides for multi-face (DFC) cards. */
export interface FaceOptions {
  /** This face's own image (replaces the whole-card front image). */
  faceImage?: string | null;
  /** Description budget for this embed. Two faces must share Discord's 6000-char
   *  per-message total, so the caller passes a smaller budget than the default. */
  descriptionBudget?: number;
}

/** Builds the parse-breakdown embed for a found card (one face for a DFC). */
export function renderCardEmbed(
  entry: CoverageEntry,
  scry: ScryfallCard | null,
  build: Build,
  meta: BuildMeta | null,
  opts: FaceOptions = {},
): Embed {
  const counts = countItems(entry.parse_details);
  const ok = entry.supported;
  const budgetMax = opts.descriptionBudget ?? DESCRIPTION_MAX;

  const meter = supportMeter(counts.supported, counts.total);
  const summary = ok
    ? `${meter}  ${counts.supported}/${counts.total} · fully supported`
    : `${meter}  ${counts.supported}/${counts.total} · ${entry.gap_count} gap${entry.gap_count === 1 ? "" : "s"}`;

  // Header (name · mana / type / oracle) sits up top; the coverage meter moves
  // down to introduce the parse tree it summarizes.
  const head = cardHeader(entry, scry);

  let description: string;
  if (entry.parse_details.length === 0) {
    description = `${head}\n\n${summary}\n\n*Vanilla — no parsed abilities*`;
  } else {
    const budget = budgetMax - head.length - summary.length - 4; // "\n\n" + "\n\n"
    description = `${head}\n\n${summary}\n\n${fitLines(treeLines(entry.parse_details), budget)}`;
  }

  // A DFC face carries its own image; otherwise use the whole-card front image.
  const image = opts.faceImage !== undefined ? opts.faceImage : scry?.image ?? null;

  return {
    author: { name: "ENGINE PARSE" },
    description,
    color: ok ? COLOR_OK : COLOR_GAPS,
    // Top-right thumbnail: compact beside the parse, click/tap to expand to the
    // full card. Discord auto-reflows it on mobile.
    thumbnail: image ? { url: image } : undefined,
    footer: { text: footerText(build, meta) },
  };
}

/**
 * Builds an embed for a token. Tokens have no engine parse tree (they are not
 * playable card entries), so this shows the Scryfall characteristics: type line,
 * P/T, and rules text. `oracleText` is null until the R2 data carries it, in
 * which case the rules line is simply omitted.
 */
export function renderTokenEmbed(token: ScryfallCard, build: Build, meta: BuildMeta | null): Embed {
  const name = `**${token.name}**`;
  const lines: string[] = [token.scryfallUri ? `[${name}](${token.scryfallUri})` : name];
  if (token.typeLine) lines.push(`**${token.typeLine}**`);
  if (token.power !== null && token.toughness !== null) {
    lines.push(`\`${token.power}/${token.toughness}\``);
  }
  if (token.oracleText) lines.push(oracleQuote(token.oracleText));
  return {
    author: { name: "TOKEN" },
    description: lines.join("\n"),
    color: COLOR_OK,
    thumbnail: token.image ? { url: token.image } : undefined,
    footer: { text: footerText(build, meta) },
  };
}

/**
 * Fallback embed for a DFC face that has Scryfall data but no coverage entry
 * (rare — both faces are normally in coverage-data). Shows the face image + name
 * without a parse tree.
 */
export function renderFaceFallback(
  faceName: string,
  faceImage: string | null,
  build: Build,
  meta: BuildMeta | null,
): Embed {
  return {
    author: { name: "ENGINE PARSE" },
    description: `**${faceName}**\n\n-# No parse data for this face in the \`${build}\` build.`,
    color: COLOR_GAPS,
    thumbnail: faceImage ? { url: faceImage } : undefined,
    footer: { text: footerText(build, meta) },
  };
}

/** Builds the "card not found" embed. */
export function renderNotFound(name: string, build: Build): Embed {
  return {
    author: { name: "ENGINE PARSE" },
    title: name,
    description: `No card named **${name}** in the \`${build}\` build's parse data. Check the spelling, or pick a suggestion from autocomplete.`,
    color: COLOR_GAPS,
  };
}
