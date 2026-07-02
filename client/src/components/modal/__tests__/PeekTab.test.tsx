import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { PeekTab } from "../DialogShell.tsx";
import { PeekRestoreTab } from "../DialogHost.tsx";

describe("PeekTab mobile (direction=bottom)", () => {
  afterEach(() => {
    cleanup();
  });

  it("anchors the collapse cue to the dialog top-right", () => {
    render(
      <div className="relative">
        <PeekTab direction="bottom" onClick={() => {}} />
      </div>,
    );
    const button = screen.getByLabelText("Move dialog out of the way");
    expect(button.className).toMatch(/right-3/);
    expect(button.className).toMatch(/top-1/);
    expect(button.className).not.toMatch(/left-1\/2/);
    expect(button.className).not.toMatch(/bottom-0/);
  });
});

describe("PeekRestoreTab mobile (direction=bottom)", () => {
  afterEach(() => {
    cleanup();
  });

  it("anchors the restore cue to the left edge below mid-screen", () => {
    render(<PeekRestoreTab direction="bottom" onClick={() => {}} />);
    const button = screen.getByLabelText("Restore dialog");
    expect(button.className).toMatch(/left-3/);
    expect(button.className).toMatch(/top-\[63%\]/);
    expect(button.className).toMatch(/h-9/);
    expect(button.className).toMatch(/w-9/);
    expect(button.className).not.toMatch(/right-3/);
    expect(button.className).not.toMatch(/left-1\/2/);
    expect(button.className).not.toMatch(/bottom-3/);
  });
});

describe("PeekTab desktop (direction=right)", () => {
  afterEach(() => {
    cleanup();
  });

  it("keeps the collapse cue on the dialog right edge", () => {
    render(
      <div className="relative">
        <PeekTab direction="right" onClick={() => {}} />
      </div>,
    );
    const button = screen.getByLabelText("Move dialog out of the way");
    expect(button.className).toMatch(/right-0/);
    expect(button.className).toMatch(/top-1\/2/);
  });
});
