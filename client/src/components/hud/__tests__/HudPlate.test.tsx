import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { HudPlate } from "../HudPlate.tsx";

describe("HudPlate", () => {
  afterEach(cleanup);

  it("exposes avatar art separately from the desktop portrait", () => {
    const { container } = render(
      <HudPlate
        label="Player"
        avatarIdentity={{ kind: "external", url: "/avatar.jpg" }}
      >
        <span>40</span>
      </HudPlate>,
    );

    const art = container.querySelector<HTMLElement>("[data-hud-plate-art]");
    expect(art?.querySelector("img")).toHaveAttribute("src", "/avatar.jpg");
    expect(art?.querySelector("img")).toHaveClass("h-full", "w-full", "object-cover");
    expect(container.querySelector("[data-hud-plate-avatar]")).toBeInTheDocument();
  });

  it("renders a full-plate profile fallback when avatar art is unavailable", () => {
    const { container } = render(
      <HudPlate label="Player" seatColor="#22d3ee">
        <span>40</span>
      </HudPlate>,
    );

    const art = container.querySelector<HTMLElement>("[data-hud-plate-art]");
    expect(art).toHaveAttribute("data-avatar-fallback", "true");
    expect(art).toHaveStyle({ color: "#22d3ee" });
    expect(art?.querySelector("[data-hud-plate-profile-fallback]")).toBeInTheDocument();
  });

  it("hides the visual label while preserving a target button's accessible name", () => {
    const { container } = render(
      <HudPlate label="Player" hideLabel onClick={() => undefined}>
        <span>40</span>
      </HudPlate>,
    );

    expect(container.querySelector("[data-hud-plate-label]")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Player" })).toHaveAttribute(
      "data-hud-plate-label-hidden",
      "true",
    );
  });
});
