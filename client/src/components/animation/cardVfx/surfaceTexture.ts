// ─── Surface texture ───
// A card surface redrawn on a 2D canvas from its layout, so a GL copy of a
// board card matches the DOM card it replaces: each painted layer between the
// surface and its face (an art-crop tile's border, colour frame and art well),
// then the face image placed and fitted as the DOM places it. Text and badges
// drawn over the face are not reproduced.

import { faceImages } from "./cardAnchors.ts";

/** A box in the surface's untransformed layout px. */
interface SurfaceBox {
  x: number;
  y: number;
  w: number;
  h: number;
  radius: number;
}

/** A painted layer: one colour, or left-to-right gradient stops. */
export interface SurfaceFill extends SurfaceBox {
  paint: string | readonly string[];
}

/** A surface measured from the DOM, in untransformed layout px. */
export interface SurfaceLayout {
  w: number;
  h: number;
  /** The outline's corner radius. */
  radius: number;
  /** Painted layers from the outermost in. */
  fills: SurfaceFill[];
  face: SurfaceBox & { fit: string };
}

const COLOR = /rgba?\([^)]*\)|#[0-9a-f]{3,8}/gi;
const HORIZONTAL_GRADIENT = /^linear-gradient\((?:to right|90deg),\s*(.+)\)$/;

const isTransparent = (color: string) => color === "transparent" || /^rgba\(.*,\s*0\)$/.test(color);

/** The paint of an element's background, or `null` when it paints nothing. */
function paintOf(style: CSSStyleDeclaration): SurfaceFill["paint"] | null {
  const stops = HORIZONTAL_GRADIENT.exec(style.backgroundImage)?.[1].match(COLOR);
  if (stops?.length) return stops;
  return isTransparent(style.backgroundColor) ? null : style.backgroundColor;
}

/** `child`'s layout offset inside `ancestor`, or `null` when `ancestor` is not
 *  on its offset-parent chain. Offsets ignore transforms, as the pose does. */
function layoutOffset(child: HTMLElement, ancestor: HTMLElement): { x: number; y: number } | null {
  let x = 0;
  let y = 0;
  let node: Element | null = child;
  while (node instanceof HTMLElement && node !== ancestor) {
    x += node.offsetLeft;
    y += node.offsetTop;
    node = node.offsetParent;
  }
  return node === ancestor ? { x, y } : null;
}

function boxOf(node: HTMLElement, surface: HTMLElement, style: CSSStyleDeclaration): SurfaceBox | null {
  const offset = layoutOffset(node, surface);
  return offset && { ...offset, w: node.offsetWidth, h: node.offsetHeight, radius: parseFloat(style.borderTopLeftRadius) || 0 };
}

/** Measures `el`'s surface for `drawSurface`, or `null` when it shows no face. */
export function measureSurfaceLayout(el: HTMLElement): SurfaceLayout | null {
  const [img] = faceImages(el);
  if (!img) return null;
  const imgStyle = getComputedStyle(img);
  const face = boxOf(img, el, imgStyle) ?? { x: 0, y: 0, w: el.offsetWidth, h: el.offsetHeight, radius: 0 };
  const fills: SurfaceFill[] = [];
  for (let node = img.parentElement; node && node !== el; node = node.parentElement) {
    const style = getComputedStyle(node);
    const paint = paintOf(style);
    const box = paint && boxOf(node, el, style);
    if (paint && box) fills.unshift({ ...box, paint });
  }
  return {
    w: el.offsetWidth,
    h: el.offsetHeight,
    radius: fills[0]?.radius ?? (parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0),
    fills,
    face: { ...face, fit: imgStyle.objectFit },
  };
}

/** The source rectangle of `image` that `fit` shows in a `w`×`h` box. */
function sourceRect(image: HTMLImageElement, fit: string, w: number, h: number) {
  const iw = image.naturalWidth;
  const ih = image.naturalHeight;
  if (fit !== "cover") return { sx: 0, sy: 0, sw: iw, sh: ih };
  const scale = Math.max(w / iw, h / ih);
  const sw = w / scale;
  const sh = h / scale;
  return { sx: (iw - sw) / 2, sy: (ih - sh) / 2, sw, sh };
}

function fillStyle(context: CanvasRenderingContext2D, { x, w, paint }: SurfaceFill): string | CanvasGradient {
  if (typeof paint === "string") return paint;
  const gradient = context.createLinearGradient(x, 0, x + w, 0);
  paint.forEach((stop, i) => gradient.addColorStop(paint.length > 1 ? i / (paint.length - 1) : 0, stop));
  return gradient;
}

/** Draws `layout` with `image` as its face at `pixelRatio`. */
export function drawSurface(layout: SurfaceLayout, image: HTMLImageElement, pixelRatio: number): HTMLCanvasElement {
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(layout.w * pixelRatio));
  canvas.height = Math.max(1, Math.round(layout.h * pixelRatio));
  const context = canvas.getContext("2d");
  if (!context) return canvas;
  context.scale(pixelRatio, pixelRatio);
  for (const fill of layout.fills) {
    context.fillStyle = fillStyle(context, fill);
    context.beginPath();
    context.roundRect(fill.x, fill.y, fill.w, fill.h, fill.radius);
    context.fill();
  }
  const { x, y, w, h, radius, fit } = layout.face;
  const { sx, sy, sw, sh } = sourceRect(image, fit, w, h);
  context.save();
  context.beginPath();
  context.roundRect(x, y, w, h, radius);
  context.clip();
  context.drawImage(image, sx, sy, sw, sh, x, y, w, h);
  context.restore();
  return canvas;
}
