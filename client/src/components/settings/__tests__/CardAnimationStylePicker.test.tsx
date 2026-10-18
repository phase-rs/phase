import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { CardAnimationStyle } from "../../../animation/types.ts";
import { CARD_ANIMATION_PREVIEW_MOMENTS, momentIndexAt } from "../cardAnimationPreview.ts";
import { CardAnimationStylePicker } from "../CardAnimationStylePicker.tsx";

let reducedMotion = false;
vi.mock("framer-motion", async (importOriginal) => ({
  ...(await importOriginal<typeof import("framer-motion")>()),
  useReducedMotion: () => reducedMotion,
}));

function renderPicker(value: CardAnimationStyle = "webgl") {
  const onChange = vi.fn();
  const view = render(<CardAnimationStylePicker value={value} onChange={onChange} />);
  const group = screen.getByRole("radiogroup", { name: "Card Animations" });
  const videos = Array.from(view.container.querySelectorAll("video"));
  return { onChange, group, videos, rerender: view.rerender };
}

describe("CardAnimationStylePicker", () => {
  let play: ReturnType<typeof vi.spyOn>;
  let pause: ReturnType<typeof vi.spyOn>;
  let focusVisible = true;

  beforeEach(() => {
    reducedMotion = false;
    focusVisible = true;
    const matches = Element.prototype.matches;
    vi.spyOn(Element.prototype, "matches").mockImplementation(function (this: Element, selector: string) {
      return selector === ":focus-visible" ? focusVisible : matches.call(this, selector);
    });
    play = vi.spyOn(HTMLMediaElement.prototype, "play").mockResolvedValue(undefined);
    pause = vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(() => {});
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("exposes the two styles as a radio group and selects on click", () => {
    const { onChange, group } = renderPicker("webgl");

    expect(screen.getByRole("radio", { name: "New" })).toBeChecked();
    expect(screen.getByRole("radio", { name: "Classic" })).not.toBeChecked();

    fireEvent.click(screen.getByRole("radio", { name: "Classic" }));

    expect(onChange).toHaveBeenCalledWith("classic");
    expect(group).toBeInTheDocument();
  });

  it("does not play anything on click alone", () => {
    renderPicker();

    fireEvent.click(screen.getByRole("radio", { name: "Classic" }));

    expect(play).not.toHaveBeenCalled();
  });

  it("plays both clips in lockstep while hovered, then pauses on the moment's peak frame", () => {
    const { group, videos } = renderPicker();
    expect(videos).toHaveLength(2);
    expect(videos.every((video) => video.loop && video.muted)).toBe(true);

    fireEvent.pointerEnter(group, { pointerType: "mouse" });

    expect(play).toHaveBeenCalledTimes(2);
    expect(videos.map((video) => video.currentTime)).toEqual([0, 0]);

    fireEvent.pointerLeave(group, { pointerType: "mouse" });

    expect(pause).toHaveBeenCalledTimes(2);
    expect(videos.map((video) => video.currentTime)).toEqual([
      CARD_ANIMATION_PREVIEW_MOMENTS[0].peak,
      CARD_ANIMATION_PREVIEW_MOMENTS[0].peak,
    ]);
  });

  it("keeps playing, without restarting, while the pointer stays", () => {
    const { group, onChange } = renderPicker();

    fireEvent.pointerEnter(group, { pointerType: "mouse" });
    fireEvent.focus(screen.getByRole("radio", { name: "New" }));
    fireEvent.click(screen.getByRole("radio", { name: "Classic" }));

    expect(play).toHaveBeenCalledTimes(2);
    expect(onChange).toHaveBeenCalledWith("classic");
  });

  it("labels the spell on screen as the loop moves between moments", () => {
    const { group, videos } = renderPicker();
    const [first, second] = CARD_ANIMATION_PREVIEW_MOMENTS;
    expect(screen.getByText(first.spell)).toBeInTheDocument();

    fireEvent.pointerEnter(group, { pointerType: "mouse" });
    act(() => {
      videos[0].currentTime = second.start + 0.5;
      fireEvent.timeUpdate(videos[0]);
    });

    expect(screen.getByText(second.spell)).toBeInTheDocument();
  });

  it("re-syncs the Classic clip when it drifts from the New clip", () => {
    const { group, videos } = renderPicker();

    fireEvent.pointerEnter(group, { pointerType: "mouse" });
    videos[0].currentTime = 3;
    videos[1].currentTime = 2;
    fireEvent.timeUpdate(videos[0]);

    expect(videos[1].currentTime).toBe(3);
  });

  it("previews on keyboard focus and stops when focus leaves the group", () => {
    const { videos } = renderPicker();
    const tile = screen.getByRole("radio", { name: "New" });

    fireEvent.focus(tile);
    expect(play).toHaveBeenCalledTimes(2);

    fireEvent.blur(tile, { relatedTarget: document.body });
    expect(pause).toHaveBeenCalledTimes(2);
    expect(videos[0].paused).toBe(true);
  });

  it("toggles both previews when a touch tap changes the selected tile", () => {
    const { onChange, videos, rerender } = renderPicker();
    const newTile = screen.getByRole("radio", { name: "New" });
    const classicTile = screen.getByRole("radio", { name: "Classic" });
    focusVisible = false;

    fireEvent.pointerEnter(newTile, { pointerType: "touch" });
    expect(play).not.toHaveBeenCalled();

    fireEvent.pointerDown(newTile, { pointerType: "touch" });
    fireEvent.focus(newTile);
    fireEvent.click(newTile);
    expect(play).toHaveBeenCalledTimes(2);
    expect(onChange).toHaveBeenLastCalledWith("webgl");

    fireEvent.pointerDown(classicTile, { pointerType: "touch" });
    fireEvent.blur(newTile, { relatedTarget: classicTile });
    fireEvent.focus(classicTile);
    fireEvent.click(classicTile);
    expect(pause).toHaveBeenCalledTimes(2);
    expect(play).toHaveBeenCalledTimes(2);
    expect(videos.map((video) => video.currentTime)).toEqual([
      CARD_ANIMATION_PREVIEW_MOMENTS[0].peak,
      CARD_ANIMATION_PREVIEW_MOMENTS[0].peak,
    ]);
    expect(onChange).toHaveBeenCalledTimes(2);
    expect(onChange).toHaveBeenLastCalledWith("classic");

    rerender(<CardAnimationStylePicker value="classic" onChange={onChange} />);
    expect(classicTile).toBeChecked();
    expect(classicTile).toHaveAttribute("tabindex", "0");
    expect(newTile).not.toBeChecked();

    fireEvent.pointerDown(classicTile, { pointerType: "touch" });
    fireEvent.click(classicTile);
    expect(play).toHaveBeenCalledTimes(4);

    fireEvent.pointerDown(classicTile, { pointerType: "touch" });
    fireEvent.click(classicTile);
    expect(pause).toHaveBeenCalledTimes(4);
    expect(play).toHaveBeenCalledTimes(4);
    expect(videos.map((video) => video.currentTime)).toEqual([
      CARD_ANIMATION_PREVIEW_MOMENTS[0].peak,
      CARD_ANIMATION_PREVIEW_MOMENTS[0].peak,
    ]);
  });

  it("resumes keyboard preview after touch leaves a tile focused and stopped", () => {
    const { onChange, videos, rerender } = renderPicker();
    const newTile = screen.getByRole("radio", { name: "New" });
    const classicTile = screen.getByRole("radio", { name: "Classic" });
    focusVisible = false;

    fireEvent.pointerDown(newTile, { pointerType: "touch" });
    fireEvent.focus(newTile);
    fireEvent.pointerDown(newTile, { pointerType: "touch" });
    expect(pause).toHaveBeenCalledTimes(2);

    focusVisible = true;
    fireEvent.keyDown(newTile, { key: "ArrowRight" });
    expect(onChange).toHaveBeenLastCalledWith("classic");
    expect(play).toHaveBeenCalledTimes(4);
    rerender(<CardAnimationStylePicker value="classic" onChange={onChange} />);
    expect(classicTile).toBeChecked();

    fireEvent.blur(classicTile, { relatedTarget: document.body });
    expect(pause).toHaveBeenCalledTimes(4);
    expect(videos.map((video) => video.currentTime)).toEqual([
      CARD_ANIMATION_PREVIEW_MOMENTS[0].peak,
      CARD_ANIMATION_PREVIEW_MOMENTS[0].peak,
    ]);
  });

  it("moves the selection with the arrow keys", () => {
    const { onChange } = renderPicker("webgl");

    fireEvent.keyDown(screen.getByRole("radio", { name: "New" }), { key: "ArrowRight" });
    expect(onChange).toHaveBeenLastCalledWith("classic");

    fireEvent.keyDown(screen.getByRole("radio", { name: "New" }), { key: "ArrowLeft" });
    expect(onChange).toHaveBeenLastCalledWith("classic");
  });

  it("plays nothing on hover or focus when reduced motion is requested, but still selects", () => {
    reducedMotion = true;
    const { onChange, group, videos } = renderPicker();

    fireEvent.pointerEnter(group, { pointerType: "mouse" });
    fireEvent.focus(screen.getByRole("radio", { name: "New" }));
    fireEvent.pointerDown(group, { pointerType: "touch" });
    fireEvent.keyDown(screen.getByRole("radio", { name: "New" }), { key: "ArrowRight" });

    expect(play).not.toHaveBeenCalled();
    expect(videos.every((video) => video.getAttribute("poster"))).toBe(true);

    fireEvent.click(screen.getByRole("radio", { name: "Classic" }));
    expect(onChange).toHaveBeenCalledWith("classic");
  });

  it("pauses an active preview when reduced motion turns on", () => {
    const view = render(<CardAnimationStylePicker value="webgl" onChange={vi.fn()} />);
    fireEvent.pointerEnter(screen.getByRole("radiogroup"), { pointerType: "mouse" });
    expect(play).toHaveBeenCalledTimes(2);

    reducedMotion = true;
    view.rerender(<CardAnimationStylePicker value="webgl" onChange={vi.fn()} />);

    expect(pause).toHaveBeenCalledTimes(2);
  });

  it("keeps only the selected tile in the tab order", () => {
    renderPicker("classic");

    expect(screen.getByRole("radio", { name: "Classic" })).toHaveAttribute("tabindex", "0");
    expect(screen.getByRole("radio", { name: "New" })).toHaveAttribute("tabindex", "-1");
  });
});

describe("momentIndexAt", () => {
  it("returns the last moment that has started", () => {
    const [, second, third] = CARD_ANIMATION_PREVIEW_MOMENTS;

    expect(momentIndexAt(0)).toBe(0);
    expect(momentIndexAt(second.start - 0.01)).toBe(0);
    expect(momentIndexAt(second.start)).toBe(1);
    expect(momentIndexAt(third.start + 1)).toBe(2);
  });
});
