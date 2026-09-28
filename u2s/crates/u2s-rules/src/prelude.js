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

// `ctx.facts`: exactly the facts the rule declared in `requires` and the
// host resolved for the input being checked. Reading any other name
// throws, so a rule can never depend on a fact it did not declare (and so
// the host's record of which facts a rule reads cannot be incomplete).
// Symbol keys and `toJSON` pass through so `JSON.stringify(ctx.facts)` and
// string coercion keep working.
function __u2s_factsProxy(facts) {
  return new Proxy(facts, {
    get: function (target, key) {
      if (typeof key !== "string" || key === "toJSON") {
        return target[key];
      }
      if (!Object.prototype.hasOwnProperty.call(target, key)) {
        throw new Error(
          "undeclared fact \"" + key + "\": declare it in `const requires = [...]`"
        );
      }
      return target[key];
    },
  });
}
