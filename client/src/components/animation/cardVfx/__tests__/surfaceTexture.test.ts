import { afterEach, describe, expect, it } from "vitest";

import { measureSurfaceLayout } from "../surfaceTexture.ts";

function box(el: HTMLElement, left: number, top: number, width: number, height: number, parent: HTMLElement | null) {
  Object.defineProperty(el, "offsetLeft", { configurable: true, value: left });
  Object.defineProperty(el, "offsetTop", { configurable: true, value: top });
  Object.defineProperty(el, "offsetWidth", { configurable: true, value: width });
  Object.defineProperty(el, "offsetHeight", { configurable: true, value: height });
  Object.defineProperty(el, "offsetParent", { configurable: true, value: parent });
}

function child(parent: HTMLElement, style: Partial<CSSStyleDeclaration> = {}) {
  const el = document.createElement("div");
  Object.assign(el.style, style);
  parent.appendChild(el);
  return el;
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("surface layout", () => {
  it("V8-8: an art-crop tile keeps each painted layer at its own box, outermost first, and the face's box and fit", () => {
    const surface = document.createElement("div");
    document.body.appendChild(surface);
    const border = child(surface, { backgroundColor: "rgb(21, 21, 21)", borderTopLeftRadius: "6px" });
    const frame = child(border, {
      backgroundImage: "linear-gradient(to right, rgb(10, 20, 30), rgb(40, 50, 60))",
      borderTopLeftRadius: "3px",
    });
    const well = child(frame, { backgroundColor: "rgb(0, 0, 0)" });
    const column = child(well, { backgroundColor: "rgba(0, 0, 0, 0)" });
    const img = document.createElement("img");
    img.style.objectFit = "cover";
    column.appendChild(img);
    box(surface, 0, 0, 90, 66, null);
    box(border, 0, 0, 90, 66, surface);
    box(frame, 3, 3, 84, 60, border);
    box(well, 2, 15, 80, 43, frame);
    box(img, 0, 0, 80, 43, well);

    expect(measureSurfaceLayout(surface)).toEqual({
      w: 90,
      h: 66,
      radius: 6,
      fills: [
        { x: 0, y: 0, w: 90, h: 66, radius: 6, paint: "rgb(21, 21, 21)" },
        { x: 3, y: 3, w: 84, h: 60, radius: 3, paint: ["rgb(10, 20, 30)", "rgb(40, 50, 60)"] },
        { x: 5, y: 18, w: 80, h: 43, radius: 0, paint: "rgb(0, 0, 0)" },
      ],
      face: { x: 5, y: 18, w: 80, h: 43, radius: 0, fit: "cover" },
    });
  });

  it("V8-8: a full card paints no frame; a surface with no face has no layout", () => {
    const surface = document.createElement("div");
    surface.style.borderTopLeftRadius = "8px";
    const img = document.createElement("img");
    surface.appendChild(img);
    document.body.appendChild(surface);
    box(surface, 0, 0, 63, 88, null);
    box(img, 0, 0, 63, 88, surface);
    expect(measureSurfaceLayout(surface)).toMatchObject({ radius: 8, fills: [], face: { x: 0, y: 0, w: 63, h: 88 } });

    box(img, 0, 0, 4, 4, surface);
    expect(measureSurfaceLayout(surface)).toBeNull();
  });
});
