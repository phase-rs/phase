type RandomUuid = `${string}-${string}-${string}-${string}-${string}`;
type FillRandomValues = (bytes: Uint8Array) => Uint8Array;

export function createRandomUUID(
  getRandomValues: FillRandomValues = (bytes) => globalThis.crypto.getRandomValues(bytes),
): RandomUuid {
  const bytes = new Uint8Array(16);
  getRandomValues(bytes);

  bytes[6] = (bytes[6] & 0x0f) | 0x40;
  bytes[8] = (bytes[8] & 0x3f) | 0x80;

  const hex = [...bytes].map((byte) => byte.toString(16).padStart(2, "0"));
  return `${hex[0]}${hex[1]}${hex[2]}${hex[3]}-${hex[4]}${hex[5]}-${hex[6]}${hex[7]}-${hex[8]}${hex[9]}-${hex[10]}${hex[11]}${hex[12]}${hex[13]}${hex[14]}${hex[15]}`;
}

export function installCryptoRandomUUID(): void {
  if (!globalThis.crypto || typeof globalThis.crypto.randomUUID === "function") {
    return;
  }

  Object.defineProperty(globalThis.crypto, "randomUUID", {
    value: () => createRandomUUID(),
    configurable: true,
  });
}

installCryptoRandomUUID();
