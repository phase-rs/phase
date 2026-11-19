import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CardReportContext } from "../../../hooks/useCardReport.ts";
import { trackEvent } from "../../../services/telemetry.ts";
import { ReportCardButton } from "../ReportCardButton.tsx";

// Telemetry is the boundary: each case decides whether the event was queued.
vi.mock("../../../services/telemetry.ts", () => ({
  trackEvent: vi.fn(),
}));

/**
 * A report context for one card. Every case reports its own oracle id: the
 * reported-cards store lives for the session and is never reset, so a shared
 * card would carry one case's "Reported" into the next.
 */
function reportContext(oracleId: string): CardReportContext {
  return {
    oracleId,
    faceName: "Starship Enterprise",
    name: "Starship Enterprise",
    zone: "Battlefield",
    supported: 3,
    total: 4,
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

// Regression tests for #11001: the button claimed "Reported" before telemetry
// had queued anything, so a dropped event still read as sent.
describe("ReportCardButton", () => {
  it("reads Reported once telemetry queued the event", () => {
    vi.mocked(trackEvent).mockReturnValue(true);
    render(<ReportCardButton className="text-[11px]" {...reportContext("oracle-queued")} />);

    fireEvent.click(screen.getByRole("button", { name: "Report a problem" }));

    expect(trackEvent).toHaveBeenCalledWith(
      "card_report",
      expect.objectContaining({ oracle_id: "oracle-queued", supported: 3, total: 4 }),
    );
    expect(screen.getByRole("button", { name: "Reported ✓" })).toBeDisabled();
  });

  it("never claims Reported for a report telemetry dropped", () => {
    vi.mocked(trackEvent).mockReturnValue(false);
    render(<ReportCardButton className="text-[11px]" {...reportContext("oracle-dropped")} />);

    fireEvent.click(screen.getByRole("button", { name: "Report a problem" }));

    expect(trackEvent).toHaveBeenCalledTimes(1);
    expect(screen.queryByText(/Reported/)).toBeNull();
    expect(screen.getByRole("button", { name: "Not sent" })).toBeEnabled();
  });

  it("sends a dropped report on a later tap that telemetry queues", () => {
    vi.mocked(trackEvent).mockReturnValueOnce(false).mockReturnValueOnce(true);
    render(<ReportCardButton className="text-[11px]" {...reportContext("oracle-retried")} />);

    fireEvent.click(screen.getByRole("button", { name: "Report a problem" }));
    fireEvent.click(screen.getByRole("button", { name: "Not sent" }));

    expect(trackEvent).toHaveBeenCalledTimes(2);
    expect(screen.getByRole("button", { name: "Reported ✓" })).toBeDisabled();
  });

  it("leaves a dropped card reportable on every other surface showing it", () => {
    vi.mocked(trackEvent).mockReturnValue(false);
    render(
      <>
        <ReportCardButton className="text-[11px]" {...reportContext("oracle-shared")} />
        <ReportCardButton className="text-[11px]" {...reportContext("oracle-shared")} />
      </>,
    );

    const [tapped, other] = screen.getAllByRole("button", { name: "Report a problem" });
    fireEvent.click(tapped);

    expect(tapped).toHaveTextContent("Not sent");
    expect(other).toHaveTextContent("Report a problem");
    expect(other).toBeEnabled();
  });
});
