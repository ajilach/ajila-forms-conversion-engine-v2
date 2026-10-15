// The host API on `ctx`. Ordinary JavaScript, not native Rust functions —
// there is no Rust call surface for a script to reach into, and the tree
// walk is bounded by the exact same loop/recursion budget as the script
// that calls it (crates/u2s-rules/src/budget.rs). Loaded before the
// script, so `walk` and `pointerEscape` are in scope when `check` runs.

// RFC 6901 §3: "~" must be encoded first, then "/" — reversing the order
// would turn a literal "~1" in a key into an escaped "/".
function __u2s_pointerEscape(token) {
  return String(token).replace(/~/g, "~0").replace(/\//g, "~1");
}

// Depth-first pre-order walk: `callback(node, pointer)` fires for every
// node, including the root (pointer ""), before its children.
function walk(node, callback) {
  function visit(node, pointer) {
    callback(node, pointer);
    if (Array.isArray(node)) {
      for (let i = 0; i < node.length; i++) {
        visit(node[i], pointer + "/" + i);
      }
    } else if (node !== null && typeof node === "object") {
      for (const key of Object.keys(node)) {
        visit(node[key], pointer + "/" + __u2s_pointerEscape(key));
      }
    }
  }
  visit(node, "");
}
