// ─── Surface texture ───
// A card surface redrawn on a 2D canvas from its layout, so a GL copy of a
// board card matches the DOM card it replaces: the surface's frame colour and
// corner radius, and its face image placed and fitted as the DOM places it.
// Text and badges drawn over the face are not reproduced.

import { faceImages } from "./cardAnchors.ts";

/** A surface measured from the DOM, in untransformed layout px. */
export interface SurfaceLayout {
  w: number;
  h: number;
  radius: number;
  /** The outermost opaque background under the face, or `null` when none. */
  frame: string | null;
  /** The face image's box relative to the surface. */
  face: { x: number; y: number; w: number; h: number; fit: string };
}

const isOpaque = (color: string) => {
  const alpha = /rgba?\(([^)]+)\)/.exec(color)?.[1].split(/[\s,/]+/).filter(Boolean)[3];
  return color !== "transparent" && (alpha === undefined || Number(alpha) === 1);
};

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

/** Measures `el`'s surface for `drawSurface`, or `null` when it shows no face. */
export function measureSurfaceLayout(el: HTMLElement): SurfaceLayout | null {
  const [img] = faceImages(el);
  if (!img) return null;
  const offset = layoutOffset(img, el) ?? { x: 0, y: 0 };
  let frame: string | null = null;
  let radius = parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0;
  // The outermost opaque background between the face and the surface is its frame.
  for (let node = img.parentElement; node && node !== el; node = node.parentElement) {
    const style = getComputedStyle(node);
    if (!isOpaque(style.backgroundColor)) continue;
    frame = style.backgroundColor;
    radius = parseFloat(style.borderTopLeftRadius) || radius;
  }
  return {
    w: el.offsetWidth,
    h: el.offsetHeight,
    radius,
    frame,
    face: { ...offset, w: img.offsetWidth, h: img.offsetHeight, fit: getComputedStyle(img).objectFit },
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

/** Draws `layout` with `image` as its face at `pixelRatio`. */
export function drawSurface(layout: SurfaceLayout, image: HTMLImageElement, pixelRatio: number): HTMLCanvasElement {
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(layout.w * pixelRatio));
  canvas.height = Math.max(1, Math.round(layout.h * pixelRatio));
  const context = canvas.getContext("2d");
  if (!context) return canvas;
  context.scale(pixelRatio, pixelRatio);
  if (layout.frame) {
    context.fillStyle = layout.frame;
    context.beginPath();
    context.roundRect(0, 0, layout.w, layout.h, layout.radius);
    context.fill();
  }
  const { x, y, w, h, fit } = layout.face;
  const { sx, sy, sw, sh } = sourceRect(image, fit, w, h);
  context.drawImage(image, sx, sy, sw, sh, x, y, w, h);
  return canvas;
}
