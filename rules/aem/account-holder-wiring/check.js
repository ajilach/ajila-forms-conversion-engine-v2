const PARTNER_GENERICS = ["affrg_ContractualPartnerGeneric", "affrg_PartnertoPartnerGeneric",
  "affrg_BeneficialOwnerGeneric", "affrg_PowerofAttorneyGeneric"];

// The sub-panels each partner generic of `afforms_ubs_fragmentlib` holds, in order, and whether it
// ships them shown (`true`) or hidden (`false`): the fragment library the profile embeds, read the
// way the writer reads it (the panels directly under the fragment's root panel).
const FULL = [["PN_EntityBasic", false], ["PN_FormAddress", false], ["PN_IndividualBasic", true],
  ["PN_Address", true], ["PN_DOBNationality", false], ["PN_DateIncorporation", false]];
const BASIC = [["PN_EntityBasic", false], ["PN_FormAddress", false], ["PN_IndividualBasic", true],
  ["PN_DOBNationality", false], ["PN_DateIncorporation", false]];
const BASIC_ENTITY = [["PN_EntityBasic", true], ["PN_FormAddress", false], ["PN_IndividualBasic", true],
  ["PN_DOBNationality", false], ["PN_DateIncorporation", false]];
const SUB_PANELS = {
  affrg_ContractualPartnerGeneric1: FULL,
  affrg_ContractualPartnerGenericBasic1: BASIC,
  affrg_PartnertoPartnerGeneric1: FULL,
  affrg_PartnertoPartnerGenericBasic1: BASIC_ENTITY,
  affrg_BeneficialOwnerGeneric1: FULL,
  affrg_BeneficialOwnerGenericBasic1: BASIC_ENTITY,
  affrg_PowerofAttorneyGeneric1: FULL,
  affrg_PowerofAttorneyGenericBasic1: BASIC_ENTITY,
};

function fileOf(fragRef) {
  return String(fragRef).split("/").pop();
}

function isPartnerGeneric(fragRef) {
  const fragment = fileOf(fragRef);
  return PARTNER_GENERICS.some(function (stem) { return fragment.indexOf(stem) === 0; });
}

function list(value) {
  return Array.isArray(value) ? value.map(String) : [];
}

// The sub-panels a copy shows once its Initialize rule ran, or null for a fragment not in the table.
function shown(node) {
  const panels = SUB_PANELS[fileOf(node.frag_ref)];
  if (!panels) return null;
  const hide = list(node.init_hide);
  const show = list(node.init_show);
  return panels.filter(function (p) {
    return show.indexOf(p[0]) >= 0 || (p[1] && hide.indexOf(p[0]) < 0);
  }).map(function (p) { return p[0]; });
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer, parent) {
    const isFragment = node.type === "Fragment" && typeof node.frag_ref === "string";
    if (isFragment && isPartnerGeneric(node.frag_ref) && (!parent || parent.type !== "Repeatable")) {
      violations.push({
        pointer: pointer,
        message: "`" + node.name + "` is a partner generic (" + fileOf(node.frag_ref) +
          ") outside a Repeatable: a party is a Repeatable named `RCP_<stem>` wrapping its fragment, so the " +
          "Add and Remove buttons and the signature pairing exist",
      });
    }
    // A partner generic's sub-panels, named directly (a dotted path reaches a component inside one,
    // which the build checks against the fragment library, as it does for every other fragment).
    const hide = list(node.init_hide).filter(function (p) { return p.indexOf(".") < 0; });
    const show = list(node.init_show).filter(function (p) { return p.indexOf(".") < 0; });
    const panels = isFragment && isPartnerGeneric(node.frag_ref) ? SUB_PANELS[fileOf(node.frag_ref)] : null;
    if (panels && (hide.length > 0 || show.length > 0)) {
      const names = panels.map(function (p) { return p[0]; });
      hide.concat(show).forEach(function (sub) {
        if (names.indexOf(sub) < 0) {
          violations.push({
            pointer: pointer + (hide.indexOf(sub) >= 0 ? "/init_hide" : "/init_show"),
            message: "`" + node.name + "` names `" + sub + "`, which " + fileOf(node.frag_ref) +
              " does not have; its sub-panels are " + names.join(", "),
          });
        }
      });
      show.forEach(function (sub) {
        const panel = panels.filter(function (p) { return p[0] === sub; })[0];
        if (panel && panel[1]) {
          violations.push({
            pointer: pointer + "/init_show",
            message: "`" + node.name + "` shows `" + sub + "`, which it already shows: `init_show` takes only " +
              "the sub-panels it ships hidden",
          });
        } else if (panel && hide.indexOf(sub) >= 0) {
          violations.push({
            pointer: pointer + "/init_show",
            message: "`" + node.name + "` both hides and shows `" + sub + "`",
          });
        }
      });
    }
    // A party row holds one party: several partner generics in it are copies of one fragment
    // (a party split around fields of its own, UBS-PTY-5), each showing its own sub-panels.
    if (node.type === "Repeatable") {
      const copies = [];
      (node.children || []).forEach(function (c, i) {
        if (c.type === "Fragment" && typeof c.frag_ref === "string" && isPartnerGeneric(c.frag_ref)) {
          copies.push({ node: c, pointer: pointer + "/children/" + i });
        }
      });
      const owner = {};
      for (let i = 0; i < copies.length; i++) {
        const copy = copies[i];
        if (copy.node.frag_ref !== copies[0].node.frag_ref) {
          violations.push({
            pointer: copy.pointer + "/frag_ref",
            message: "`" + node.name + "` holds partner generics of two roles (" + fileOf(copies[0].node.frag_ref) +
              " and " + fileOf(copy.node.frag_ref) + "): a row is one party, so give each role its own Repeatable",
          });
          continue;
        }
        if (copies.length < 2) continue;
        (shown(copy.node) || []).forEach(function (panel) {
          if (owner[panel] !== undefined) {
            violations.push({
              pointer: copy.pointer + (Array.isArray(copy.node.init_hide) ? "/init_hide" : ""),
              message: "`" + owner[panel] + "` and `" + copy.node.name + "` in `" + node.name + "` both show `" + panel +
                "`: each copy of a split party shows its own sub-panels, so hide `" + panel + "` in one of them",
            });
          } else {
            owner[panel] = copy.node.name;
          }
        });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, node);
  }
  visit(output.form, "/form", null);
  return { pass: violations.length === 0, violations: violations };
}
