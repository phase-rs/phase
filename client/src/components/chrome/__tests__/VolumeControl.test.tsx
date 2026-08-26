import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../../../audio/AudioManager.ts", () => ({
  audioManager: { dispose: vi.fn(), restart: vi.fn(), ensurePlayback: vi.fn() },
}));

import { useAudioHealthStore } from "../../../stores/audioHealthStore.ts";
import { VolumeControl } from "../VolumeControl";

afterEach(() => {
  cleanup();
  useAudioHealthStore.setState({ deviceBlocked: false });
});

describe.each([["game"], ["chrome"]] as const)("VolumeControl (%s variant)", (variant) => {
  it("exposes the blocked-device status as a touch-reachable description, not just the hover title", () => {
    useAudioHealthStore.setState({ deviceBlocked: true });
    render(<VolumeControl variant={variant} />);

    const button = screen.getByRole("button", { name: "Mute" });
    const describedById = button.getAttribute("aria-describedby");
    expect(describedById).toBeTruthy();

    const status = document.getElementById(describedById!);
    expect(status).toHaveTextContent("Audio unavailable — system audio server is not responding. Restart the app to retry.");
    expect(status).toHaveClass("sr-only");
    // Action name stays the action, not the status (round-2 review requirement).
    expect(button).toHaveAccessibleName("Mute");
  });

  it("omits the description entirely when the device isn't blocked", () => {
    render(<VolumeControl variant={variant} />);

    const button = screen.getByRole("button", { name: "Mute" });
    expect(button).not.toHaveAttribute("aria-describedby");
  });
});
