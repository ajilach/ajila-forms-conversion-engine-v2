// Signature fragments the feedback repo's catalogue knows a signer-name field for, by file name.
const SIGNATURE_FRAGMENTS = [
  "affrg_arsignature1", "affrg_authorizedsignature_frag", "affrg_bosignature1",
  "affrg_clientsignature1", "affrg_depositorsignature1", "affrg_legalguardiansignature1",
  "affrg_signature_account_holder", "affrg_ubseuropesignature1", "affrg_germany_client_signature",
  "affrg_global_accountholdersignature_signature_place_date_name",
  "affrg_global_authorizedrepresentativesignature_signature_place_date_name",
  "affrg_global_beneficialownersignature_signature_place_date_name",
  "affrg_global_clientsignature_signature_place_date_name",
  "eforms-19415-authorized-representative-signature-special-use-case--single-",
  "affrg_legalrepresentativesignature1", "affrg_accountholdersignature",
  "affrg_italy_client_signature", "affrg_signaturelegalrepresen", "affrg_signaturegeneric1",
];
const SIGNATURE_HINT = /sign|signat|firma|unterschrift/i;
// A trailing instance number of one or two digits, with or without a separator. A longer run is a
// hash (`...510d0d43` is not a second instance of anything).
const TRAILING_NUMBER = /(?<![0-9])(?:[ _-])?([0-9]{1,2})$/;

function splitNumber(name) {
  const text = String(name || "").trim();
  const m = TRAILING_NUMBER.exec(text);
  if (!m) return null;
  const stem = text.slice(0, m.index).replace(/^[ _-]+|[ _-]+$/g, "");
  return stem === "" ? null : { stem: stem, number: m[1] };
}

function isSignatureBlock(node) {
  if (typeof node.frag_ref === "string" && node.frag_ref !== "") {
    const file = node.frag_ref.split("/").pop().toLowerCase();
    return SIGNATURE_FRAGMENTS.indexOf(file) >= 0;
  }
  if (typeof node.name !== "string" || !SIGNATURE_HINT.test(node.name)) return false;
  let found = false;
  function look(parent) {
    const children = parent.children || [];
    for (let i = 0; i < children.length && !found; i++) {
      if (typeof children[i].name === "string" && SIGNATURE_HINT.test(children[i].name)) found = true;
      else look(children[i]);
    }
  }
  look(node);
  return found;
}

// Whether the members are numbered exactly 1 to N, N at least 2.
function numberedFromOne(members) {
  const numbers = Array.from(new Set(members.map(function (m) { return m.number; }))).sort();
  const expected = members.map(function (_, i) { return String(i + 1); });
  return members.length >= 2 && numbers.length === members.length && numbers.join(",") === expected.join(",");
}

// What a block is apart from its numbers: types, texts, options and required states, digits dropped.
function shape(node) {
  const undigit = function (v) { return JSON.stringify(v === undefined ? null : v).replace(/[0-9]/g, ""); };
  return [
    node.type, undigit(node.name), undigit(node.title), undigit(node.label), undigit(node.content),
    undigit((node.options || []).map(function (o) { return o.label; })), node.mandatory === true, node.frag_ref || "",
    (node.children || []).map(shape).join(";"),
  ].join("|");
}

// Whether a node is or holds a signature block, which the signature grouping reports.
function holdsSignature(node) {
  return isSignatureBlock(node) || (node.children || []).some(holdsSignature);
}

function report(violations, members, message) {
  const names = members.map(function (m) { return "`" + m.name + "`"; }).join(", ");
  members.forEach(function (m) {
    violations.push({ pointer: m.pointer + "/name", message: names + " " + message(members.length) });
  });
}

function check(output, ctx) {
  const violations = [];
  const groups = {};
  const order = [];
  function visit(node, pointer) {
    if (node.type !== "Root" && isSignatureBlock(node)) {
      const split = splitNumber(node.name);
      if (split) {
        if (!groups[split.stem]) { groups[split.stem] = []; order.push(split.stem); }
        groups[split.stem].push({ name: node.name, number: split.number, pointer: pointer });
      }
    }
    // Sibling panels numbered 1 to N that are the same block apart from the number (UBS-REP-3).
    const siblings = {};
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      if (child.type !== "Panel" || holdsSignature(child)) continue;
      const split = splitNumber(child.name);
      if (!split) continue;
      (siblings[split.stem] = siblings[split.stem] || []).push({
        name: child.name, number: split.number, pointer: pointer + "/children/" + i, shape: shape(child),
      });
    }
    Object.keys(siblings).forEach(function (stem) {
      const members = siblings[stem];
      if (!numberedFromOne(members) || members.some(function (m) { return m.shape !== members[0].shape; })) return;
      report(violations, members, function (n) {
        return "are numbered copies of one block (UBS-REP-3): use one Repeatable with min_occur = max_occur = " + n +
          ", titled without the number";
      });
    });
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  order.forEach(function (stem) {
    const members = groups[stem];
    if (!numberedFromOne(members)) return;
    report(violations, members, function (n) {
      return "are numbered copies of one signature block: use one Repeatable wrapping affrg_SignatureGeneric1 with " +
        "min_occur = max_occur = " + n + ", titled without the number";
    });
  });
  return { pass: violations.length === 0, violations: violations };
}
