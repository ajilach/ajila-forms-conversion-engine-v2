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
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  order.forEach(function (stem) {
    const members = groups[stem];
    const numbers = Array.from(new Set(members.map(function (m) { return m.number; }))).sort();
    const expected = members.map(function (_, i) { return String(i + 1); });
    if (members.length < 2 || numbers.length !== members.length ||
        numbers.join(",") !== expected.join(",")) return;
    const names = members.map(function (m) { return "`" + m.name + "`"; }).join(", ");
    members.forEach(function (m) {
      violations.push({
        pointer: m.pointer + "/name",
        message:
          "signature blocks " + names + " are numbered copies of one block: use one Repeatable " +
          "wrapping affrg_SignatureGeneric1 with min_occur = max_occur = " + members.length +
          ", titled without the number",
      });
    });
  });
  return { pass: violations.length === 0, violations: violations };
}
