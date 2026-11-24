// Classifying a thrown error with isWasmTrap: which errors mean the
// WebAssembly instance trapped (and must be replaced) and which leave it
// usable. Assumes trap.js is next to this file (as scripts/build-wasm.sh
// emits it into surfaces/relate-wasm/pkg/; see docs/api/wasm/spec.md).
import { isWasmTrap } from "./trap.js";

function expectUnusable(err, label) {
  if (!isWasmTrap(err)) {
    throw new Error(`isWasmTrap(${label}) returned false, expected true (instance dead)`);
  }
}

function expectUsable(err, label) {
  if (isWasmTrap(err)) {
    throw new Error(`isWasmTrap(${label}) returned true, expected false (instance usable)`);
  }
}

// A Rust panic on wasm32 crosses as WebAssembly.RuntimeError — classified
// by type, whatever the message says.
expectUnusable(new WebAssembly.RuntimeError("remainder by zero"), "RuntimeError");

// Native wasm trap words, as a plain Error (the shape a host on the main
// thread sees: a RuntimeError cannot be structured-cloned out of a worker).
expectUnusable(new Error("unreachable executed"), "unreachable");
expectUnusable(new Error("memory access out of bounds"), "out-of-bounds access");

// Stack exhaustion, as each engine words it.
expectUnusable(new RangeError("Maximum call stack size exceeded"), "Chromium overflow");
expectUnusable(new RangeError("Maximum call stack size exceeded."), "WebKit overflow");
expectUnusable(new Error("call stack exhausted"), "Firefox wasm overflow");
// Firefox throws this as InternalError (an Error subclass); the
// classifier reads only the message, so a plain Error is the same path.
expectUnusable(new Error("too much recursion"), "Firefox overflow");

// An overflowed instance throws this on the next call (borrow never
// released) — that instance is just as dead.
expectUnusable(
  new Error(
    "recursive use of an object detected which would lead to unsafe aliasing in rust",
  ),
  "dead-instance follow-on",
);

// Ordinary errors leave the instance usable.
expectUsable(new Error("UnknownFunction: unknown function"), "engine error");
expectUsable(new RangeError("Invalid array length"), "unrelated RangeError");

// Anything that is not an Error never came from the bundle: not a trap,
// whatever its string form happens to match.
expectUsable("too much recursion", "trap-worded string");
expectUsable({ message: "too much recursion" }, "trap-worded object");
