import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../../../audio/AudioManager.ts", () => ({
  audioManager: { dispose: vi.fn(), restart: vi.fn(), ensurePlayback: vi.fn() },
}));

import {
  type AudioUnavailableReason,
  useAudioHealthStore,
} from "../../../stores/audioHealthStore.ts";
import { VolumeControl } from "../VolumeControl";

afterEach(() => {
  cleanup();
  useAudioHealthStore.setState({ unavailable: null });
});

// Each reason has its own remedy, so the description must name the fault the
// user actually hit — a shared "audio is off" string would send someone with a
// missing GStreamer plugin set off restarting the app forever (issue #6744).
const REASON_MESSAGE = {
  "device-wedged":
    "Audio unavailable — system audio server is not responding. Restart the app to retry.",
  "media-unavailable":
    "Audio unavailable — this system is missing the media components needed to play sound. Install your distribution's GStreamer plugin packages, then restart the app.",
} as const satisfies Record<AudioUnavailableReason, string>;

describe.each([["game"], ["chrome"]] as const)("VolumeControl (%s variant)", (variant) => {
  it.each(Object.entries(REASON_MESSAGE) as [AudioUnavailableReason, string][])(
    "exposes the %s status as a touch-reachable description, not just the hover title",
    (reason, message) => {
      useAudioHealthStore.setState({ unavailable: reason });
      render(<VolumeControl variant={variant} />);

      const button = screen.getByRole("button", { name: "Mute" });
      const describedById = button.getAttribute("aria-describedby");
      expect(describedById).toBeTruthy();

      const status = document.getElementById(describedById!);
      expect(status).toHaveTextContent(message);
      expect(status).toHaveClass("sr-only");
      // Action name stays the action, not the status (round-2 review requirement).
      expect(button).toHaveAccessibleName("Mute");
    },
  );

  it("omits the description entirely when audio is healthy", () => {
    render(<VolumeControl variant={variant} />);

    const button = screen.getByRole("button", { name: "Mute" });
    expect(button).not.toHaveAttribute("aria-describedby");
  });
});
