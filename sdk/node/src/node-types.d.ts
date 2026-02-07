/**
 * Minimal Node.js type declarations for the RustQueue SDK.
 *
 * These cover only the APIs used by the SDK (net.Socket, setTimeout, etc.).
 * When @types/node is installed (e.g. in a consumer project), these are
 * superseded by the full declarations.
 */

// ── Global: fetch API (Node.js 18+) ─────────────────────────────────────────

declare function fetch(
  input: string | URL,
  init?: RequestInit
): Promise<Response>;

interface RequestInit {
  method?: string;
  headers?: Record<string, string>;
  body?: string;
  signal?: AbortSignal;
}

interface Response {
  ok: boolean;
  status: number;
  statusText: string;
  json(): Promise<unknown>;
  text(): Promise<string>;
}

// ── Global: Timers ───────────────────────────────────────────────────────────

declare function setTimeout(
  callback: (...args: unknown[]) => void,
  ms: number,
  ...args: unknown[]
): NodeJS.Timeout;

declare function clearTimeout(id: NodeJS.Timeout | undefined): void;

declare namespace NodeJS {
  interface Timeout {
    ref(): this;
    unref(): this;
    hasRef(): boolean;
    [Symbol.dispose](): void;
  }
}

// ── Module: node:net ─────────────────────────────────────────────────────────

declare module "node:net" {
  import { EventEmitter } from "node:events";

  class Socket extends EventEmitter {
    constructor(options?: { fd?: number; allowHalfOpen?: boolean; readable?: boolean; writable?: boolean });
    connect(port: number, host: string, connectionListener?: () => void): this;
    write(data: string, encoding?: string, callback?: (err?: Error) => void): boolean;
    setEncoding(encoding: string): this;
    destroy(error?: Error): this;
    end(callback?: () => void): this;
    on(event: "data", listener: (data: string) => void): this;
    on(event: "error", listener: (err: Error) => void): this;
    on(event: "close", listener: (hadError: boolean) => void): this;
    on(event: "connect", listener: () => void): this;
    on(event: string, listener: (...args: unknown[]) => void): this;
    once(event: "error", listener: (err: Error) => void): this;
    once(event: string, listener: (...args: unknown[]) => void): this;
    removeListener(event: string, listener: (...args: unknown[]) => void): this;
    removeAllListeners(event?: string): this;
  }
}

// ── Module: node:events ──────────────────────────────────────────────────────

declare module "node:events" {
  class EventEmitter {
    on(event: string, listener: (...args: unknown[]) => void): this;
    once(event: string, listener: (...args: unknown[]) => void): this;
    off(event: string, listener: (...args: unknown[]) => void): this;
    emit(event: string, ...args: unknown[]): boolean;
    removeListener(event: string, listener: (...args: unknown[]) => void): this;
    removeAllListeners(event?: string): this;
    listeners(event: string): ((...args: unknown[]) => void)[];
  }
}
