import { afterEach, describe, expect, it } from "vitest";

import { measureSurfaceLayout } from "../surfaceTexture.ts";

function box(el: HTMLElement, left: number, top: number, width: number, height: number, parent: HTMLElement | null) {
  Object.defineProperty(el, "offsetLeft", { configurable: true, value: left });
  Object.defineProperty(el, "offsetTop", { configurable: true, value: top });
  Object.defineProperty(el, "offsetWidth", { configurable: true, value: width });
  Object.defineProperty(el, "offsetHeight", { configurable: true, value: height });
  Object.defineProperty(el, "offsetParent", { configurable: true, value: parent });
}

afterEach(() => {
  document.body.replaceChildren();
});

describe("surface layout", () => {
  it("V8-8: an art-crop tile keeps its frame colour and radius, and the face's inset box and fit", () => {
    const surface = document.createElement("div");
    const frame = document.createElement("div");
    frame.style.backgroundColor = "rgb(21, 21, 21)";
    frame.style.borderTopLeftRadius = "6px";
    const art = document.createElement("div");
    art.style.backgroundColor = "rgba(0, 0, 0, 0.5)";
    const img = document.createElement("img");
    img.style.objectFit = "cover";
    art.appendChild(img);
    frame.appendChild(art);
    surface.appendChild(frame);
    document.body.appendChild(surface);
    box(surface, 0, 0, 90, 66, null);
    box(img, 5, 18, 80, 44, surface);

    expect(measureSurfaceLayout(surface)).toEqual({
      w: 90,
      h: 66,
      radius: 6,
      frame: "rgb(21, 21, 21)",
      face: { x: 5, y: 18, w: 80, h: 44, fit: "cover" },
    });
  });

  it("V8-8: a full card with no opaque frame, and a surface with no face", () => {
    const surface = document.createElement("div");
    const img = document.createElement("img");
    surface.appendChild(img);
    document.body.appendChild(surface);
    box(surface, 0, 0, 63, 88, null);
    box(img, 0, 0, 63, 88, surface);
    expect(measureSurfaceLayout(surface)).toMatchObject({ frame: null, face: { x: 0, y: 0, w: 63, h: 88 } });

    box(img, 0, 0, 4, 4, surface);
    expect(measureSurfaceLayout(surface)).toBeNull();
  });
});
