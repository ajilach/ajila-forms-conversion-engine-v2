const PARTNER_GENERICS = ["affrg_ContractualPartnerGeneric", "affrg_PartnertoPartnerGeneric",
  "affrg_BeneficialOwnerGeneric", "affrg_PowerofAttorneyGeneric"];

function fileOf(fragRef) {
  return String(fragRef).split("/").pop();
}

function isPartnerGeneric(node) {
  return node.type === "Fragment" && typeof node.frag_ref === "string" &&
    PARTNER_GENERICS.some(function (stem) { return fileOf(node.frag_ref).indexOf(stem) === 0; });
}

// An address fragment (`affrg_AddressGeneric1`, the market `affrg_Address1`, the address blocks),
// not the form-of-address one.
function isAddressFragment(node) {
  if (node.type !== "Fragment" || typeof node.frag_ref !== "string") return false;
  const file = fileOf(node.frag_ref).toLowerCase();
  return file.indexOf("address") >= 0 && file.indexOf("formofaddress") < 0 && file.indexOf("form_address") < 0;
}

function plain(text) {
  return String(text).replace(/<[^>]*>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim();
}

// Captions of the street and of the city in the languages of the sources.
const STREET = /(^|[^a-zà-ÿ])(street|strasse|straße|str\.|calle|rue|via)([^a-zà-ÿ]|$)/i;
const CITY = /(^|[^a-zà-ÿ])(city|town|ort|wohnort|stadt|città|citta|località|localita|comune|ciudad|localidad|población|ville|localité)([^a-zà-ÿ]|$)/i;

function labelMatches(node, re) {
  const label = node.label || {};
  return Object.keys(label).some(function (lang) { return re.test(plain(label[lang])); });
}

// An input someone fills in: a read-only or computed one shows data the form supplies (a branch's
// address, ubs-aem-addresses' contact block).
function isEnteredText(node) {
  return node.type === "TextField" && node.read_only !== true && typeof node.calculate !== "string" &&
    typeof node.default_value !== "string";
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const children = node.children || [];
    // Street and city typed into fields of their own: the address fragment hand-built.
    let street = -1;
    let city = -1;
    for (let i = 0; i < children.length; i++) {
      if (!isEnteredText(children[i])) continue;
      if (street < 0 && labelMatches(children[i], STREET)) street = i;
      else if (city < 0 && labelMatches(children[i], CITY)) city = i;
    }
    if (street >= 0 && city >= 0) {
      violations.push({
        pointer: pointer + "/children/" + street,
        message: "`" + children[street].name + "` and `" + children[city].name + "` hand-build an address: an address " +
          "is the `affrg_AddressGeneric1` fragment (or the party's own `PN_Address`), never loose street and city fields",
      });
    }
    // A party row holds the party's own address in its generic, never a second address fragment.
    if (node.type === "Repeatable" && children.some(isPartnerGeneric)) {
      const row = node;
      (function inRow(parent, at) {
        const kids = parent.children || [];
        for (let i = 0; i < kids.length; i++) {
          const kid = kids[i];
          const kidAt = at + "/children/" + i;
          if (isAddressFragment(kid)) {
            violations.push({
              pointer: kidAt,
              message: "`" + kid.name + "` is an address fragment inside the party row `" + row.name + "`: an address " +
                "that repeats with the person is the partner generic's `PN_Address` (take it out of `init_hide`), and " +
                "one address for all the persons is this fragment right after the row",
            });
          } else if (kid.type !== "Repeatable") {
            inRow(kid, kidAt);
          }
        }
      })(node, pointer);
    }
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
