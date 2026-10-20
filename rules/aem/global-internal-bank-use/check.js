const GLOBAL = "afforms_global_fragmentlib/affrg_global_InternalBankUse_Text_OURef_Signature";
// The block's heading, as the sources word it.
const HEADING = /bankintern|internal bank|bank internal|for internal use|uso interno|riservat[oa] alla banca|usage interne|interne de la banque|uso exclusivo/i;

function isInternalBankUse(fragRef) {
  const lower = String(fragRef).toLowerCase();
  return lower.indexOf("internalbankuse") >= 0 || lower.indexOf("internal_bank_use") >= 0;
}

function isContentBlock(node) {
  return node.type === "Panel" && typeof node.name === "string" && node.name.indexOf("PN_InternalBankUse") === 0;
}

function isBlock(node) {
  return (node.type === "Fragment" && typeof node.frag_ref === "string" && isInternalBankUse(node.frag_ref)) ||
    isContentBlock(node);
}

function plain(text) {
  return String(text).replace(/<[^>]*>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim().toLowerCase();
}

function texts(node) {
  const map = node.content || {};
  return Object.keys(map).map(function (lang) { return plain(map[lang]); }).filter(function (t) { return t !== ""; });
}

function isDraw(node) {
  return node && (node.type === "TextDraw" || node.type === "TitleDraw");
}

function check(output, ctx) {
  const violations = [];
  const blocks = [];
  function visit(node, pointer, chain) {
    if (typeof node.frag_ref === "string" && isInternalBankUse(node.frag_ref) && node.frag_ref.indexOf(GLOBAL) < 0) {
      violations.push({
        pointer: pointer + "/frag_ref",
        message: "`" + node.name + "` references " + node.frag_ref + "; the standard internal-bank-use block is " +
          "/content/dam/formsanddocuments/" + GLOBAL + ", and any other content is a content panel `PN_InternalBankUseOnly`",
      });
    }
    const here = chain.concat([{ node: node, pointer: pointer }]);
    if (isBlock(node)) {
      blocks.push({ node: node, pointer: pointer, chain: here });
      return;
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, here);
  }
  visit(output.form, "/form", []);

  // A form has as many blocks as its source, each where the source shows it (UBS-IBU-1), so neither
  // their number nor their place is checked here.
  blocks.forEach(function (block) {
    const node = block.node;
    // A content panel reaches the PDF alone, as the writer makes the fragment do.
    if (isContentBlock(node)) {
      const missing = [];
      if (node.visible !== false) missing.push("visible: false");
      if (node.summary_exclude !== true) missing.push("summary_exclude");
      if (node.always_in_pdf !== true) missing.push("always_in_pdf");
      if (missing.length > 0) {
        violations.push({
          pointer: block.pointer,
          message: "`" + node.name + "` is the internal-bank-use block and reaches the PDF alone: set " + missing.join(", "),
        });
      }
      if (node.dor_exclude === true) {
        violations.push({
          pointer: block.pointer + "/dor_exclude",
          message: "`" + node.name + "` carries dor_exclude, which drops the internal-bank-use block from the PDF: delete it",
        });
      }
    }
    // The heading once: as the block's own, never repeated next to it or inside it.
    const own = [];
    if (isContentBlock(node)) {
      const first = (node.children || [])[0];
      if (isDraw(first)) texts(first).forEach(function (t) { own.push(t); });
      (node.children || []).slice(1).forEach(function (c, i) {
        if (isDraw(c) && texts(c).some(function (t) { return own.indexOf(t) >= 0; })) {
          violations.push({
            pointer: block.pointer + "/children/" + (i + 1),
            message: "`" + c.name + "` repeats the heading of the internal-bank-use block: delete it",
          });
        }
      });
    }
    const parentEntry = block.chain[block.chain.length - 2];
    const siblings = parentEntry.node.children || [];
    const at = siblings.indexOf(node);
    [at - 1, at + 1].forEach(function (j) {
      const sib = siblings[j];
      if (!isDraw(sib)) return;
      if (texts(sib).some(function (t) { return HEADING.test(t) || own.indexOf(t) >= 0; })) {
        violations.push({
          pointer: parentEntry.pointer + "/children/" + j,
          message: "`" + sib.name + "` repeats the heading of the internal-bank-use block `" + node.name + "`, which " +
            (isContentBlock(node) ? "opens with its own heading" : "the fragment prints itself") + ": delete it",
        });
      }
    });
  });
  return { pass: violations.length === 0, violations: violations };
}
