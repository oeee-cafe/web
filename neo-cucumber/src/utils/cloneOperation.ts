import type { PainterOperation } from "../operations";

/**
 * A deep copy of one operation, without `structuredClone`.
 *
 * `structuredClone` is Firefox 94, and this package runs as far back as
 * Firefox 56 for Waterfox Classic's sake. A JSON round-trip is not a
 * substitute: `fill-region` carries its flood coverage as a `Uint8Array`,
 * which `JSON.stringify` turns into an object keyed by index, and the replay
 * that reads it back would fill a different set of pixels than the canvas it
 * was recorded on.
 *
 * The vocabulary in `operations.ts` is plain data, typed arrays and nothing
 * else -- no cycles, no `Map`, no `Date` -- so this covers it exactly. Adding
 * a richer payload there means extending this.
 */
export function cloneOperation(operation: PainterOperation): PainterOperation {
  return cloneValue(operation) as PainterOperation;
}

function cloneValue(value: unknown): unknown {
  if (value === null || typeof value !== "object") return value;
  if (value instanceof Uint8Array) return value.slice();
  if (Array.isArray(value)) return value.map(cloneValue);

  const copy: Record<string, unknown> = {};
  for (const [key, member] of Object.entries(value)) {
    copy[key] = cloneValue(member);
  }
  return copy;
}
