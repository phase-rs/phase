import { expect, it, vi } from "vitest";
import { GzipEnvelopeSocket } from "../gzipEnvelopeSocket";
import { observeSocketDiagnostics } from "../socketDiagnostics";
import { diagnosticIdFor, getDiagnosticHistory } from "../troubleshooting";

it("measures outbound silence from the actual async envelope write, not its nomination", async () => {
  vi.useFakeTimers();
  let writtenAt = 0;
  const raw = Object.assign(new EventTarget(), {
    readyState: 1, onopen: null, onmessage: null, onerror: null, onclose: null,
    send: vi.fn(() => { writtenAt = Date.now(); }), close: vi.fn(),
  });
  const observer = observeSocketDiagnostics(raw as never, 0, () => "not-attempted", () => undefined);
  const socket = new GzipEnvelopeSocket(raw as never, observer.didSend);
  try {
    socket.send(JSON.stringify({ type: "Ping", data: { timestamp: 7 } }));
    expect(raw.send).not.toHaveBeenCalled();
    vi.setSystemTime(Date.now() + 1000);
    await vi.waitFor(() => expect(raw.send).toHaveBeenCalledOnce());
    vi.setSystemTime(writtenAt + 2000);
    raw.dispatchEvent(new CloseEvent("close", { code: 1006 }));
    expect(getDiagnosticHistory().find((entry) => entry.diagnosticId === diagnosticIdFor(raw))).toMatchObject({
      kind: "websocket-loss", sendAgeMs: 2000, receiveAgeMs: null,
    });
  } finally {
    observer.detach();
    vi.useRealTimers();
  }
});

it("reports an ordered write receipt for setup and rejects when the queued write fails", async () => {
  const writes: string[] = [];
  const raw = Object.assign(new EventTarget(), {
    readyState: 1, onopen: null, onmessage: null, onerror: null, onclose: null,
    send: vi.fn((bytes: Uint8Array) => { writes.push(new TextDecoder().decode(bytes)); }),
    close: vi.fn(),
  });
  const socket = new GzipEnvelopeSocket(raw as never);
  const first = socket.sendReceipt(JSON.stringify({ type: "A" }));
  const second = socket.sendReceipt(JSON.stringify({ type: "B" }));
  await expect(Promise.all([first, second])).resolves.toEqual([undefined, undefined]);
  // Both receipts settle only after their own raw write, in FIFO order.
  expect(writes).toHaveLength(2);
  expect(writes[0]).toContain('"A"');
  expect(writes[1]).toContain('"B"');

  raw.send.mockImplementationOnce(() => { throw new Error("write failed"); });
  await expect(socket.sendReceipt(JSON.stringify({ type: "C" }))).rejects.toThrow("write failed");
  // The transport's own failure path still closes on the same seam.
  expect(raw.close).toHaveBeenCalled();
});

it("rejects a queued write when the raw socket starts closing before the close event", async () => {
  const raw = Object.assign(new EventTarget(), {
    readyState: 1, onopen: null, onmessage: null, onerror: null, onclose: null,
    send: vi.fn(), close: vi.fn(),
  });
  const socket = new GzipEnvelopeSocket(raw as never);
  const queued = socket.sendReceipt(JSON.stringify({ type: "Closing" }));
  // The browser has begun closing but has not delivered the close event yet.
  raw.readyState = 2;
  await expect(queued).rejects.toThrow("not open");
  expect(raw.send).not.toHaveBeenCalled();
});

it("records a loss outside a secure context, where crypto.randomUUID is unavailable", () => {
  const original = globalThis.crypto.randomUUID;
  Object.defineProperty(globalThis.crypto, "randomUUID", { value: undefined, configurable: true });
  const raw = Object.assign(new EventTarget(), {
    readyState: 1, onopen: null, onmessage: null, onerror: null, onclose: null, send: vi.fn(), close: vi.fn(),
  });
  try {
    const observer = observeSocketDiagnostics(raw as never, 0, () => "not-attempted", () => undefined);
    raw.dispatchEvent(new CloseEvent("close", { code: 1006 }));
    const id = diagnosticIdFor(raw);
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(getDiagnosticHistory().find((entry) => entry.diagnosticId === id)).toMatchObject({ kind: "websocket-loss", code: 1006 });
    observer.detach();
  } finally {
    Object.defineProperty(globalThis.crypto, "randomUUID", { value: original, configurable: true });
  }
});
