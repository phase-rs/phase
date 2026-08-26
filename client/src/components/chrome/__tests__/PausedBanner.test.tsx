import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { PausedBanner } from "../PausedBanner";

describe("PausedBanner", () => {
  afterEach(cleanup);

  it("shows an accessible Resume control only when the host supplies a handler", async () => {
    const onResume = vi.fn();
    const user = userEvent.setup();
    render(<PausedBanner isVisible reason="Paused by host" onResume={onResume} />);

    await user.click(screen.getByRole("button", { name: "Resume" }));

    expect(onResume).toHaveBeenCalledOnce();
  });

  it("does not expose a resume control to guests", () => {
    render(<PausedBanner isVisible reason="Paused by host" />);

    expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
  });
});
