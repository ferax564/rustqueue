/**
 * RustQueue Node.js SDK — Error types.
 * @module
 */

import type { RustQueueErrorInfo } from "./types.js";

/**
 * Error thrown by the RustQueue SDK when the server returns an error response
 * or when a network/protocol error occurs.
 *
 * @example
 * ```ts
 * try {
 *   await client.push("emails", "send", { to: "alice@example.com" });
 * } catch (err) {
 *   if (err instanceof RustQueueError) {
 *     console.error(`[${err.code}] ${err.message}`);
 *   }
 * }
 * ```
 */
export class RustQueueError extends Error {
  /** Machine-readable error code (e.g. "NOT_FOUND", "VALIDATION_ERROR"). */
  public readonly code: string;

  /** HTTP status code, if the error originated from an HTTP response. */
  public readonly statusCode?: number;

  constructor(code: string, message: string, statusCode?: number) {
    super(message);
    this.name = "RustQueueError";
    this.code = code;
    this.statusCode = statusCode;

    // Maintain proper prototype chain for instanceof checks.
    Object.setPrototypeOf(this, RustQueueError.prototype);
  }

  /**
   * Create a RustQueueError from a structured server error response.
   */
  static fromServerError(
    error: RustQueueErrorInfo,
    statusCode?: number
  ): RustQueueError {
    return new RustQueueError(error.code, error.message, statusCode);
  }
}
