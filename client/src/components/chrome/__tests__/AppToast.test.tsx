import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { AppToast } from "../AppToast.tsx";
import { useAppNotificationStore } from "../../../stores/appToastStore.ts";

describe("AppToast", () => {
  beforeEach(() => {
    useAppNotificationStore.setState({ notification: null, expiresAt: 0 });
  });

  afterEach(() => {
    cleanup();
  });

  it("positions a contextual notification at the supplied rendered-object anchor", () => {
    useAppNotificationStore.setState({
      notification: {
        title: "Action failed",
        description: "Engine error: ObjectId(200) must be blocked by 2 or more creatures",
        anchor: { x: 240, y: 320, placement: "above" },
      },
      expiresAt: Date.now() + 5_000,
    });

    render(<AppToast />);

    const toast = screen.getByRole("status");
    expect(toast).toHaveClass("z-[200]");
    expect(toast).toHaveStyle({ left: "240px", top: "320px" });
    expect(screen.getByText("Engine error: ObjectId(200) must be blocked by 2 or more creatures")).toBeInTheDocument();
  });
});
