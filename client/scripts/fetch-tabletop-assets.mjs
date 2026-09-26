/**
 * Download the runtime-only frame, font, and mana-symbol assets used by the
 * tabletop 3D client. Third-party frame/font files stay gitignored.
 */
import { access, copyFile, mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const cardConjurerRevision = "0eb50b03ccbfb65c20acc9a940f8aa699ce16523";
const cardConjurerPreviewRevision =
  "2fcddba8966156d484cedf54d8214996748dd5e1";
const cardConjurerRaw =
  `https://raw.githubusercontent.com/fiahdrgn473/CardConjurer/${cardConjurerRevision}`;
const cardConjurerPreviewStatBadge =
  `https://raw.githubusercontent.com/Investigamer/cardconjurer/${cardConjurerPreviewRevision}/img/frames/8th/pt/a.png`;
const clientRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const outputRoot = join(clientRoot, "public", "tabletop3d");
const manaFontSource = join(
  clientRoot,
  "node_modules",
  "mana-font",
  "fonts",
  "mana.woff2",
);

const frameColors = ["W", "U", "B", "R", "G", "M", "A", "V", "L"];
const statColors = ["W", "U", "B", "R", "G", "M", "A", "V", "C"];
const frames = [
  ...frameColors.map((color) => `img/frames/m15/regular/m15Frame${color}.png`),
  ...statColors.map((color) => `img/frames/m15/regular/m15PT${color}.png`),
];
const fonts = ["fonts/beleren-b.ttf", "fonts/mplantin.ttf"];
const pips = [
  ...Array.from({ length: 21 }, (_, index) => String(index)),
  "X",
  "C",
  "S",
  "P",
  "T",
  "Q",
  "E",
  "W",
  "U",
  "B",
  "R",
  "G",
  "WU",
  "WB",
  "UB",
  "UR",
  "BR",
  "BG",
  "RG",
  "RW",
  "GW",
  "GU",
  "2W",
  "2U",
  "2B",
  "2R",
  "2G",
  "WP",
  "UP",
  "BP",
  "RP",
  "GP",
];

async function fetchTo(url, destination) {
  try {
    await access(destination);
    return;
  } catch {
    const response = await fetch(url, {
      headers: { "User-Agent": "phase.rs-development/1.0" },
    });
    if (!response.ok) throw new Error(`${response.status} ${url}`);
    const bytes = Buffer.from(await response.arrayBuffer());
    await mkdir(dirname(destination), { recursive: true });
    await writeFile(destination, bytes);
  }
}

for (const relativePath of frames) {
  await fetchTo(
    `${cardConjurerRaw}/${relativePath}`,
    join(outputRoot, "frames", "m15", relativePath.split("/").at(-1)),
  );
}

for (const relativePath of fonts) {
  await fetchTo(
    `${cardConjurerRaw}/${relativePath}`,
    join(outputRoot, "fonts", relativePath.split("/").at(-1)),
  );
}

await mkdir(join(outputRoot, "fonts"), { recursive: true });
await copyFile(manaFontSource, join(outputRoot, "fonts", "mana.woff2"));

const { default: sharp } = await import("sharp");
// Keep this third-party artwork out of git; fresh builds fetch the exact pinned
// Card Conjurer source asset at setup time.
await fetchTo(
  cardConjurerPreviewStatBadge,
  join(outputRoot, "frames", "card-conjurer", "8th-pt-a.png"),
);

for (const symbol of pips) {
  const destination = join(outputRoot, "pips", `${symbol}.png`);
  try {
    await access(destination);
    continue;
  } catch {
    const response = await fetch(
      `https://svgs.scryfall.io/card-symbols/${symbol}.svg`,
      { headers: { "User-Agent": "phase.rs-development/1.0" } },
    );
    if (!response.ok) throw new Error(`${response.status} pip ${symbol}`);
    let svg = await response.text();
    if (!/<svg[^>]*\swidth=/.test(svg)) {
      svg = svg.replace("<svg ", "<svg width='100' height='100' ");
    }
    await mkdir(dirname(destination), { recursive: true });
    await sharp(Buffer.from(svg)).resize(232, 232).png().toFile(destination);
  }
}

console.log("Tabletop 3D frame, font, and mana assets are ready.");
