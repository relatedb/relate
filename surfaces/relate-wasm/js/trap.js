// Shipped with the WASM bundle (scripts/build-wasm.sh copies it next to
// relate.js) so every host classifies engine failures the same way.

/**
 * Whether `err`, thrown by a call into the bundle, means the WebAssembly
 * instance trapped (a Rust panic, stack exhaustion, an out-of-bounds access)
 * and cannot be used again. A trapped instance must be replaced, or the page
 * reloaded; any other error leaves it usable.
 *
 * Traps always cross the boundary as Error values — a WebAssembly.RuntimeError
 * from the trap itself, or a plain Error with the trap's message (a
 * RuntimeError cannot be structured-cloned out of a worker, so a host on the
 * main thread re-creates it). Anything that is not an Error never came from
 * the bundle and is not a trap.
 *
 * Stack exhaustion crosses as each engine words it: Chromium and WebKit
 * "Maximum call stack size exceeded", Firefox "too much recursion" (JS) or
 * "call stack exhausted" (wasm). An
 * overflowed instance can also throw "recursive use of an object detected…"
 * on the NEXT call (its borrow was never released) — that instance is just
 * as dead.
 *
 * @param {unknown} err
 * @returns {boolean}
 */
export function isWasmTrap(err) {
  if (err instanceof WebAssembly.RuntimeError) return true;
  if (!(err instanceof Error)) return false;
  return (
    // Native wasm trap words.
    /unreachable|memory access out of bounds/i.test(err.message) ||
    // Stack exhaustion, as each engine words it.
    /call stack exhausted|maximum call stack size|too much recursion/i.test(err.message) ||
    // An overflowed instance's borrow guard, thrown on the next call.
    /recursive use of an object/i.test(err.message)
  );
}
