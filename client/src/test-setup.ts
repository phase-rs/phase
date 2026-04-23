import "@testing-library/jest-dom/vitest";

class TestStorage implements Storage {
  private readonly items = new Map<string, string>();

  get length(): number {
    return this.items.size;
  }

  clear(): void {
    this.items.clear();
  }

  getItem(key: string): string | null {
    return this.items.get(key) ?? null;
  }

  key(index: number): string | null {
    return Array.from(this.items.keys())[index] ?? null;
  }

  removeItem(key: string): void {
    this.items.delete(key);
  }

  setItem(key: string, value: string): void {
    this.items.set(key, String(value));
  }
}

function installStorage(name: "localStorage" | "sessionStorage"): void {
  const storage = new TestStorage();
  Object.defineProperty(globalThis, name, {
    configurable: true,
    value: storage,
    writable: true,
  });

  if (typeof window !== "undefined") {
    Object.defineProperty(window, name, {
      configurable: true,
      value: storage,
      writable: true,
    });
  }
}

installStorage("localStorage");
installStorage("sessionStorage");
