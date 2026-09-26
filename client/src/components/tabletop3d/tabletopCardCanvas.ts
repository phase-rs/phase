import type { TabletopCardPresentation } from "./tabletopCardPresentation.ts";
import type { PTColor } from "../../viewmodel/cardProps.ts";

export const TABLETOP_CARD_WIDTH = 1005;
export const TABLETOP_CARD_HEIGHT = 1407;

const CORNER_RADIUS = 44;
const PHASE_STAT_BADGE_URL =
  "/tabletop3d/frames/card-conjurer/8th-pt-a.png";
const TITLE_FONT_SIZES = [62, 58, 54, 50, 46] as const;
const RULES_SIZES = [50, 47, 44, 41, 38, 35, 32, 29];
const GEOMETRY = {
  art: { x: 0.078, y: 0.114, width: 0.844, height: 0.437 },
  titleY: 0.076,
  typeY: 0.594,
  textLeft: 0.088,
  textRight: 0.915,
  rulesTop: 0.632,
  rulesBottom: 0.875,
  stats: { x: 0.762, y: 0.888, width: 0.198, height: 0.072 },
  // The badge PNG has a deeper lower rim, drop shadow, and heavier left bevel,
  // so its visible recessed field is optically above and right of canvas center.
  statsTextCenter: { x: 0.51, y: 0.44 },
} as const;
const STAT_VALUE_FONT_HEIGHT_RATIO = 0.52;
const STAT_VALUE_MAX_WIDTH_RATIO = 0.64;
const STAT_POWER_TOUGHNESS_SEPARATOR = "\u200a/\u200a";

export interface TabletopStatColors {
  power: PTColor;
  toughness: PTColor;
}

export interface TabletopStatSegment {
  text: string;
  ink: string;
}

export const TABLETOP_STAT_INK: Record<PTColor, string> = {
  white: "#050505",
  blue: "#176fc1",
  red: "#c33035",
};

/** Shared crop boundaries used by the Three.js battlefield presentation. */
export const TABLETOP_CARD_ART_BOTTOM_RATIO =
  GEOMETRY.art.y + GEOMETRY.art.height;
export const TABLETOP_CARD_STAT_RECT = GEOMETRY.stats;

type FrameLetter = "W" | "U" | "B" | "R" | "G" | "M" | "A" | "V" | "L";

interface RichLine {
  tokens: RichToken[];
  width: number;
  gapBefore: number;
}

export interface TabletopCardTitleLayout {
  fontSize: number;
  text: string;
}

type RichToken =
  | { kind: "text"; text: string; width: number; attachPrevious: boolean }
  | { kind: "pip"; symbol: string; width: number; attachPrevious: boolean };

const imageCache = new Map<string, Promise<HTMLImageElement>>();
let fontsReady: Promise<void> | null = null;

export function tabletopFrameLetter(
  presentation: Pick<TabletopCardPresentation, "colors" | "typeLine">,
): FrameLetter {
  if (presentation.typeLine.includes("Land")) return "L";
  if (presentation.colors.length === 0) {
    return presentation.typeLine.includes("Artifact") ? "A" : "V";
  }
  if (presentation.colors.length > 1) return "M";
  return COLOR_FRAME[presentation.colors[0]] ?? "V";
}

export async function ensureTabletopCardFonts(): Promise<void> {
  if (fontsReady) return fontsReady;
  fontsReady = (async () => {
    if (typeof FontFace === "undefined" || !document.fonts) return;
    const beleren = new FontFace(
      "Tabletop Beleren",
      'url("/tabletop3d/fonts/beleren-b.ttf")',
    );
    const mplantin = new FontFace(
      "Tabletop MPlantin",
      'url("/tabletop3d/fonts/mplantin.ttf")',
    );
    const loaded = await Promise.all([beleren.load(), mplantin.load()]);
    loaded.forEach((font) => document.fonts.add(font));
  })();
  return fontsReady;
}

export async function renderTabletopCardCanvas(
  presentation: TabletopCardPresentation,
  artSource: string,
): Promise<HTMLCanvasElement> {
  const frameLetter = tabletopFrameLetter(presentation);
  const [frameImage, artImage, statBadgeImage] = await Promise.all([
    loadImage(frameUrl(frameLetter)),
    loadImage(artSource),
    loadImage(PHASE_STAT_BADGE_URL),
    ensureTabletopCardFonts(),
  ]);
  const canvas = document.createElement("canvas");
  canvas.width = TABLETOP_CARD_WIDTH;
  canvas.height = TABLETOP_CARD_HEIGHT;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("2D canvas unavailable");

  context.beginPath();
  context.roundRect(
    0,
    0,
    TABLETOP_CARD_WIDTH,
    TABLETOP_CARD_HEIGHT,
    CORNER_RADIUS,
  );
  context.clip();
  context.fillStyle = "#0b0b0b";
  context.fillRect(0, 0, TABLETOP_CARD_WIDTH, TABLETOP_CARD_HEIGHT);

  const art = GEOMETRY.art;
  drawCover(
    context,
    artImage,
    art.x * TABLETOP_CARD_WIDTH,
    art.y * TABLETOP_CARD_HEIGHT,
    art.width * TABLETOP_CARD_WIDTH,
    art.height * TABLETOP_CARD_HEIGHT,
  );
  context.drawImage(frameImage, 0, 0, TABLETOP_CARD_WIDTH, TABLETOP_CARD_HEIGHT);

  const left = GEOMETRY.textLeft * TABLETOP_CARD_WIDTH;
  const right = GEOMETRY.textRight * TABLETOP_CARD_WIDTH;
  const ink = "#171310";
  const symbols = presentation.manaSymbols;
  const pipSize = 50;
  const pipGap = 6;
  const pipRowWidth =
    symbols.length > 0 ? symbols.length * (pipSize + pipGap) : 0;

  context.fillStyle = ink;
  context.font = '54px "Tabletop Beleren", "Times New Roman", serif';
  drawVerticallyCenteredText(
    context,
    presentation.name,
    left,
    GEOMETRY.titleY * TABLETOP_CARD_HEIGHT,
    right - left - (pipRowWidth > 0 ? pipRowWidth + 16 : 0),
  );

  if (symbols.length > 0) {
    const images = await Promise.all(
      symbols.map((symbol) => loadImage(pipUrl(symbol))),
    );
    let x = right - pipRowWidth + pipGap;
    const y = GEOMETRY.titleY * TABLETOP_CARD_HEIGHT - pipSize / 2;
    context.save();
    context.shadowColor = "rgba(0, 0, 0, 0.5)";
    context.shadowBlur = 6;
    context.shadowOffsetY = 4;
    images.forEach((image) => {
      if (presentation.manaCostReduced) {
        context.beginPath();
        context.arc(
          x + pipSize / 2,
          y + pipSize / 2,
          pipSize / 2 + 4,
          0,
          Math.PI * 2,
        );
        context.strokeStyle = "#53ef86";
        context.lineWidth = 6;
        context.shadowColor = "rgba(55, 255, 126, 0.8)";
        context.shadowBlur = 10;
        context.stroke();
        context.shadowColor = "rgba(0, 0, 0, 0.5)";
        context.shadowBlur = 6;
      }
      context.drawImage(image, x, y, pipSize, pipSize);
      x += pipSize + pipGap;
    });
    context.restore();
  }

  context.font = '46px "Tabletop Beleren", "Times New Roman", serif';
  drawVerticallyCenteredText(
    context,
    presentation.typeLine,
    left,
    GEOMETRY.typeY * TABLETOP_CARD_HEIGHT,
    right - left,
  );

  if (presentation.rulesText) {
    await drawRulesText(
      context,
      presentation.rulesText,
      left,
      right,
      ink,
    );
  }

  const statText = tabletopCardStatText(presentation);
  if (statText) {
    drawTabletopStatText(
      context,
      statText,
      statBadgeImage,
      tabletopCardStatColors(presentation),
    );
  }

  return canvas;
}

export function tabletopCardStatText(
  presentation: Pick<
    TabletopCardPresentation,
    "damageMarked" | "loyalty" | "power" | "toughness"
  >,
): string | null {
  const effectiveToughness =
    presentation.toughness == null
      ? null
      : presentation.toughness - presentation.damageMarked;
  return presentation.power != null && effectiveToughness != null
    ? `${presentation.power}${STAT_POWER_TOUGHNESS_SEPARATOR}${effectiveToughness}`
    : presentation.loyalty == null
      ? null
      : String(presentation.loyalty);
}

/** Creates a dedicated, padded badge texture so mipmaps cannot sample card art. */
export async function renderTabletopStatBadgeCanvas(
  statText: string,
  colors?: TabletopStatColors,
): Promise<HTMLCanvasElement> {
  const [background] = await Promise.all([
    loadImage(PHASE_STAT_BADGE_URL),
    ensureTabletopCardFonts(),
  ]);
  const canvas = document.createElement("canvas");
  canvas.width = background.naturalWidth;
  canvas.height = background.naturalHeight;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("2D canvas unavailable");

  context.drawImage(background, 0, 0, canvas.width, canvas.height);
  drawTabletopStatValue(
    context,
    statText,
    canvas.width * GEOMETRY.statsTextCenter.x,
    canvas.height * GEOMETRY.statsTextCenter.y,
    canvas.width,
    canvas.height,
    colors,
  );
  return canvas;
}

/**
 * Redraws the printed P/T or loyalty value as a high-contrast UI readout.
 * The Three.js battlefield lifts this exact texture region onto a larger mesh,
 * so a solid light field and large serif glyphs survive mipmaps and camera
 * tilt while retaining the lighter printed character of a physical card.
 */
export function drawTabletopStatText(
  context: CanvasRenderingContext2D,
  statText: string,
  background?: CanvasImageSource,
  colors?: TabletopStatColors,
): void {
  const stats = GEOMETRY.stats;
  const x = stats.x * TABLETOP_CARD_WIDTH;
  const y = stats.y * TABLETOP_CARD_HEIGHT;
  const width = stats.width * TABLETOP_CARD_WIDTH;
  const height = stats.height * TABLETOP_CARD_HEIGHT;
  const outerInset = 2;
  const insetX = 12;
  const insetY = 10;

  context.save();
  if (background) {
    context.save();
    traceRoundedRect(context, x, y, width, height, height * 0.28);
    context.clip();
    context.drawImage(
      background,
      x - width * 0.03,
      y - height * 0.05,
      width * 1.06,
      height * 1.1,
    );
    context.restore();
  } else {
    traceRoundedRect(
      context,
      x + outerInset,
      y + outerInset,
      width - outerInset * 2,
      height - outerInset * 2,
      height * 0.28,
    );
    context.fillStyle = "#747579";
    context.fill();
    context.strokeStyle = "#38393c";
    context.lineWidth = 4;
    context.stroke();

    const fieldX = x + insetX;
    const fieldY = y + insetY;
    const fieldWidth = width - insetX * 2;
    const fieldHeight = height - insetY * 2;
    traceRoundedRect(
      context,
      fieldX,
      fieldY,
      fieldWidth,
      fieldHeight,
      fieldHeight * 0.24,
    );
    context.fillStyle = "rgba(247, 244, 234, 0.98)";
    context.fill();
    context.strokeStyle = "#a7a7a3";
    context.lineWidth = 3;
    context.stroke();
  }

  drawTabletopStatValue(
    context,
    statText,
    (stats.x + stats.width * GEOMETRY.statsTextCenter.x)
      * TABLETOP_CARD_WIDTH,
    (stats.y + stats.height * GEOMETRY.statsTextCenter.y)
      * TABLETOP_CARD_HEIGHT,
    width,
    height,
    colors,
  );
  context.restore();
}

function drawTabletopStatValue(
  context: CanvasRenderingContext2D,
  statText: string,
  centerX: number,
  centerY: number,
  badgeWidth: number,
  badgeHeight: number,
  colors?: TabletopStatColors,
): void {
  const fontSize = fitTabletopStatFontSize(
    statText,
    badgeWidth,
    badgeHeight,
    (value, candidateFontSize) => {
      context.font = tabletopStatFont(candidateFontSize);
      return context.measureText(value).width;
    },
  );
  context.font = tabletopStatFont(fontSize);
  const metrics = context.measureText(statText);
  const ascent = metrics.actualBoundingBoxAscent ?? 0;
  const descent = metrics.actualBoundingBoxDescent ?? 0;
  const inkLeft = metrics.actualBoundingBoxLeft ?? 0;
  const inkRight = metrics.actualBoundingBoxRight ?? metrics.width;
  context.textAlign = "left";
  context.textBaseline = "alphabetic";
  const startX = centerX + (inkLeft - inkRight) / 2;
  const baselineY = centerY + (ascent - descent) / 2;
  let cursorX = startX;
  tabletopStatSegments(statText, colors).forEach((segment) => {
    context.fillStyle = segment.ink;
    context.fillText(segment.text, cursorX, baselineY);
    cursorX += context.measureText(segment.text).width;
  });
}

/** Splits only P/T readouts so each numeral can retain its own modifier ink. */
export function tabletopStatSegments(
  statText: string,
  colors?: TabletopStatColors,
): TabletopStatSegment[] {
  const separatorIndex = colors ? statText.indexOf("/") : -1;
  if (separatorIndex < 0 || !colors) {
    return [{ text: statText, ink: TABLETOP_STAT_INK.white }];
  }
  return [
    { text: statText.slice(0, separatorIndex), ink: TABLETOP_STAT_INK[colors.power] },
    { text: "/", ink: TABLETOP_STAT_INK.white },
    {
      text: statText.slice(separatorIndex + 1),
      ink: TABLETOP_STAT_INK[colors.toughness],
    },
  ];
}

export function tabletopCardStatColors(
  presentation: Pick<TabletopCardPresentation, "powerColor" | "toughnessColor">,
): TabletopStatColors {
  return {
    power: presentation.powerColor,
    toughness: presentation.toughnessColor,
  };
}

/** Fits every P/T or loyalty value inside the asset's recessed inner field. */
export function fitTabletopStatFontSize(
  statText: string,
  badgeWidth: number,
  badgeHeight: number,
  measure: (value: string, fontSize: number) => number,
): number {
  const preferredSize = badgeHeight * STAT_VALUE_FONT_HEIGHT_RATIO;
  const measuredWidth = measure(statText, preferredSize);
  const maxWidth = badgeWidth * STAT_VALUE_MAX_WIDTH_RATIO;
  return measuredWidth > maxWidth && measuredWidth > 0
    ? (preferredSize * maxWidth) / measuredWidth
    : preferredSize;
}

function tabletopStatFont(fontSize: number): string {
  return `600 ${fontSize}px "Tabletop MPlantin", "Times New Roman", serif`;
}

function traceRoundedRect(
  context: CanvasRenderingContext2D,
  x: number,
  y: number,
  width: number,
  height: number,
  radius: number,
): void {
  context.beginPath();
  context.roundRect(
    x,
    y,
    width,
    height,
    Math.min(radius, height / 2, width / 2),
  );
}

/**
 * Fits a card name without CanvasRenderingContext2D's horizontal max-width
 * compression, which makes long names unnaturally thin at battlefield scale.
 */
export function fitTabletopCardTitle(
  text: string,
  maxWidth: number,
  measure: (candidate: string, fontSize: number) => number,
): TabletopCardTitleLayout {
  for (const fontSize of TITLE_FONT_SIZES) {
    if (measure(text, fontSize) <= maxWidth) {
      return { fontSize, text };
    }
  }

  const fontSize = TITLE_FONT_SIZES[TITLE_FONT_SIZES.length - 1];
  const ellipsis = "…";
  const characters = Array.from(text);
  while (characters.length > 0) {
    const candidate = `${characters.join("").trimEnd()}${ellipsis}`;
    if (measure(candidate, fontSize) <= maxWidth) {
      return { fontSize, text: candidate };
    }
    characters.pop();
  }
  return { fontSize, text: ellipsis };
}

/**
 * Strengthens only the world-space battlefield crown. Inspection and hand
 * cards retain the full printed treatment, while small permanents receive a
 * quiet parchment backing and a larger, unsquashed name.
 */
export function drawTabletopBattlefieldTitle(
  context: CanvasRenderingContext2D,
  name: string,
  manaSymbolCount: number,
): void {
  const left = GEOMETRY.textLeft * TABLETOP_CARD_WIDTH;
  const right = GEOMETRY.textRight * TABLETOP_CARD_WIDTH;
  const pipSize = 50;
  const pipGap = 6;
  const pipRowWidth =
    manaSymbolCount > 0 ? manaSymbolCount * (pipSize + pipGap) : 0;
  const maxWidth = Math.max(
    120,
    right - left - (pipRowWidth > 0 ? pipRowWidth + 16 : 0),
  );
  const centerY = GEOMETRY.titleY * TABLETOP_CARD_HEIGHT;
  const layout = fitTabletopCardTitle(
    name,
    maxWidth,
    (candidate, fontSize) => {
      context.font = `${fontSize}px "Tabletop Beleren", "Times New Roman", serif`;
      return context.measureText(candidate).width;
    },
  );

  context.save();
  context.beginPath();
  context.roundRect(left - 12, centerY - 38, maxWidth + 24, 76, 14);
  context.fillStyle = "rgba(247, 239, 216, 0.84)";
  context.fill();
  context.strokeStyle = "rgba(19, 16, 13, 0.42)";
  context.lineWidth = 2;
  context.stroke();

  context.font = `${layout.fontSize}px "Tabletop Beleren", "Times New Roman", serif`;
  context.fillStyle = "#15110e";
  context.strokeStyle = "rgba(255, 250, 235, 0.7)";
  context.lineWidth = 2.5;
  context.lineJoin = "round";
  drawVerticallyCenteredText(
    context,
    layout.text,
    left,
    centerY,
    undefined,
    "Ag",
    true,
  );
  context.restore();
}

async function drawRulesText(
  context: CanvasRenderingContext2D,
  rulesText: string,
  left: number,
  right: number,
  ink: string,
): Promise<void> {
  const boxTop = GEOMETRY.rulesTop * TABLETOP_CARD_HEIGHT;
  const boxHeight =
    (GEOMETRY.rulesBottom - GEOMETRY.rulesTop) * TABLETOP_CARD_HEIGHT;
  const maxWidth = right - left;
  const paragraphs = rulesText.split("\n");
  let fit: {
    fontSize: number;
    lineHeight: number;
    pipSize: number;
    spaceWidth: number;
    lines: RichLine[];
    totalHeight: number;
  } | null = null;

  for (const fontSize of RULES_SIZES) {
    context.font = `${fontSize}px "Tabletop MPlantin", Georgia, serif`;
    const lineHeight = Math.round(fontSize * 1.12);
    const inlinePipSize = Math.round(fontSize * 0.98);
    const spaceWidth = context.measureText(" ").width;
    const paragraphGap = Math.round(fontSize * 0.35);
    const lines = paragraphs.flatMap((paragraph, index) =>
      layoutParagraph(
        tokenizeTabletopRulesParagraph(context, paragraph, inlinePipSize),
        maxWidth,
        spaceWidth,
        index > 0 ? paragraphGap : 0,
      ),
    );
    const totalHeight = lines.reduce(
      (height, line) => height + lineHeight + line.gapBefore,
      0,
    );
    fit = {
      fontSize,
      lineHeight,
      pipSize: inlinePipSize,
      spaceWidth,
      lines,
      totalHeight,
    };
    if (totalHeight <= boxHeight) break;
  }
  if (!fit) return;
  while (fit.totalHeight > boxHeight && fit.lines.length > 1) {
    const dropped = fit.lines.pop();
    if (dropped) {
      fit.totalHeight -= fit.lineHeight + dropped.gapBefore;
    }
  }

  const symbols = [
    ...new Set(
      fit.lines.flatMap((line) =>
        line.tokens.flatMap((token) =>
          token.kind === "pip" ? [token.symbol] : [],
        ),
      ),
    ),
  ];
  const pipImages = new Map(
    await Promise.all(
      symbols.map(async (symbol) => [symbol, await loadImage(pipUrl(symbol))] as const),
    ),
  );

  context.fillStyle = ink;
  context.font = `${fit.fontSize}px "Tabletop MPlantin", Georgia, serif`;
  context.textBaseline = "alphabetic";
  let y = boxTop + Math.max(0, (boxHeight - fit.totalHeight) / 2);
  for (const line of fit.lines) {
    y += line.gapBefore;
    const baseline = y + fit.fontSize * 0.82;
    let x = left;
    line.tokens.forEach((token, index) => {
      if (token.kind === "text") {
        context.fillText(token.text, x, baseline);
      } else {
        const image = pipImages.get(token.symbol);
        if (image) {
          context.drawImage(
            image,
            x,
            baseline - fit.pipSize * 0.9,
            fit.pipSize,
            fit.pipSize,
          );
        }
      }
      const next = line.tokens[index + 1];
      x += token.width + (next && !next.attachPrevious ? fit.spaceWidth : 0);
    });
    y += fit.lineHeight;
  }
}

export function tokenizeTabletopRulesParagraph(
  context: Pick<CanvasRenderingContext2D, "measureText">,
  paragraph: string,
  pipSize: number,
): RichToken[] {
  const tokens: RichToken[] = [];
  const pipPattern = /\{([^}]+)\}/g;
  paragraph
    .split(/\s+/)
    .filter(Boolean)
    .forEach((word) => {
      let cursor = 0;
      let hasWordToken = false;
      for (const match of word.matchAll(pipPattern)) {
        const matchIndex = match.index;
        if (matchIndex > cursor) {
          const text = word.slice(cursor, matchIndex);
          tokens.push({
            kind: "text",
            text,
            width: context.measureText(text).width,
            attachPrevious: hasWordToken,
          });
          hasWordToken = true;
        }
        tokens.push({
          kind: "pip",
          symbol: match[1],
          width: pipSize,
          attachPrevious: hasWordToken,
        });
        hasWordToken = true;
        cursor = matchIndex + match[0].length;
      }
      if (cursor < word.length) {
        const text = word.slice(cursor);
        tokens.push({
          kind: "text",
          text,
          width: context.measureText(text).width,
          attachPrevious: hasWordToken,
        });
      }
    });
  return tokens;
}

function layoutParagraph(
  tokens: RichToken[],
  maxWidth: number,
  spaceWidth: number,
  gapBefore: number,
): RichLine[] {
  const lines: RichLine[] = [];
  let line: RichLine = { tokens: [], width: 0, gapBefore: 0 };
  tokens.forEach((token) => {
    const separator =
      line.tokens.length > 0 && !token.attachPrevious ? spaceWidth : 0;
    const nextWidth = line.width + separator + token.width;
    if (line.tokens.length > 0 && nextWidth > maxWidth) {
      lines.push(line);
      line = { tokens: [token], width: token.width, gapBefore: 0 };
    } else {
      line.tokens.push(token);
      line.width = nextWidth;
    }
  });
  if (line.tokens.length > 0) lines.push(line);
  if (lines[0]) lines[0].gapBefore = gapBefore;
  return lines;
}

function drawCover(
  context: CanvasRenderingContext2D,
  image: HTMLImageElement,
  x: number,
  y: number,
  width: number,
  height: number,
): void {
  const scale = Math.max(width / image.width, height / image.height);
  const sourceWidth = width / scale;
  const sourceHeight = height / scale;
  const sourceX = (image.width - sourceWidth) / 2;
  const sourceY = (image.height - sourceHeight) / 2;
  context.drawImage(
    image,
    sourceX,
    sourceY,
    sourceWidth,
    sourceHeight,
    x,
    y,
    width,
    height,
  );
}

function drawVerticallyCenteredText(
  context: CanvasRenderingContext2D,
  text: string,
  x: number,
  centerY: number,
  maxWidth?: number,
  reference = "Ag",
  stroke = false,
): void {
  const metrics = context.measureText(reference);
  const ascent = metrics.actualBoundingBoxAscent ?? 0;
  const descent = metrics.actualBoundingBoxDescent ?? 0;
  context.textBaseline = "alphabetic";
  const baseline = centerY + (ascent - descent) / 2;
  if (stroke) context.strokeText(text, x, baseline, maxWidth);
  context.fillText(text, x, baseline, maxWidth);
}

function loadImage(source: string): Promise<HTMLImageElement> {
  const cached = imageCache.get(source);
  if (cached) return cached;
  const promise = new Promise<HTMLImageElement>((resolve, reject) => {
    const image = new Image();
    image.crossOrigin = "anonymous";
    image.onload = () => resolve(image);
    image.onerror = () => {
      imageCache.delete(source);
      reject(new Error(`Unable to load Tabletop card asset: ${source}`));
    };
    image.src = source;
  });
  imageCache.set(source, promise);
  return promise;
}

function frameUrl(letter: FrameLetter): string {
  return `/tabletop3d/frames/m15/m15Frame${letter}.png`;
}

export function tabletopPipUrl(symbol: string): string {
  return pipUrl(symbol);
}

function pipUrl(symbol: string): string {
  return `/tabletop3d/pips/${symbol.replace(/\//g, "")}.png`;
}

const COLOR_FRAME: Partial<
  Record<TabletopCardPresentation["colors"][number], FrameLetter>
> = {
  White: "W",
  Blue: "U",
  Black: "B",
  Red: "R",
  Green: "G",
};
