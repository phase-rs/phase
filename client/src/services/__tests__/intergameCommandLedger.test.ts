import { describe, expect, it } from "vitest";

import {
  commandAcknowledgement,
  consumeIntergamePermit,
  draftIntergameDigest,
  IntergameCommandController,
} from "../intergameCommandLedger";

function heldCommand() {
  const controller = new IntergameCommandController();
  const command = controller.hold({
    commandId: "command-1",
    matchId: "traditional-match",
    gameNumber: 2,
    seat: 1,
    payload: { type: "ChoosePlayDraw", playFirst: false },
    launchPayload: { match: "traditional-match", seat: 1 },
    launchDigest: draftIntergameDigest({ match: "traditional-match", seat: 1 }),
  });
  return { controller, command, acknowledgement: commandAcknowledgement(command) };
}

describe("IntergameCommandController", () => {
  it("releases a Traditional between-games command exactly once", () => {
    const { controller, command, acknowledgement } = heldCommand();
    expect(controller.authorize(command.commandId, acknowledgement)?.status).toBe("Authorized");
    const permit = controller.begin(command.commandId, acknowledgement);
    expect(permit).not.toBeNull();
    expect(consumeIntergamePermit(permit!, acknowledgement)).toBe(true);
    expect(controller.receipt(command.commandId, acknowledgement, "receipt-1")?.status).toBe("Receipted");
    expect(controller.begin(command.commandId, acknowledgement)).toBeNull();
  });

  it("rejects forged acknowledgements and a stale launch digest", () => {
    const { controller, command, acknowledgement } = heldCommand();
    expect(controller.authorize(command.commandId, { ...acknowledgement, payloadDigest: "forged" })).toBeNull();
    expect(controller.authorize(command.commandId, { ...acknowledgement, launchDigest: "stale" })).toBeNull();
    expect(controller.snapshot()[0].status).toBe("Pending");
  });

  it("does not replay a command interrupted after the pre-execution transition", () => {
    const { controller, command, acknowledgement } = heldCommand();
    controller.authorize(command.commandId, acknowledgement);
    expect(controller.begin(command.commandId, acknowledgement)).not.toBeNull();
    const recovered = new IntergameCommandController(controller.snapshot());
    recovered.recover();
    expect(recovered.snapshot()[0]).toMatchObject({ status: "Receipted", receiptId: "recovered" });
    expect(recovered.begin(command.commandId, acknowledgement)).toBeNull();
  });
});
