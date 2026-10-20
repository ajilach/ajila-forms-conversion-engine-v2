// The Italian branch's legal registration text is the Italy infobox fragment (its only content).
const INFOBOX = /affrg_italy_infobox$/;
const INFOBOX_PATH = "/content/dam/formsanddocuments/afforms_italy_fragmentlib/affrg_italy_infobox";
// The entities whose registration text the profile defines (UBS-PAGE-6).
const REGISTRATION = ["033"];

// The issuer as Germany ("UBS Europe SE"), Italy ("Per UBS Europe SE (Succursale Italia)", with a
// comma, a dash or a full stop) and Switzerland write it: the feedback repo's ENTITY_LINE
// (`find_subtitle_detector.py`), tested on the first non-empty line as it does, so the unit or the
// address under it does not hide it, while a sentence that only mentions the bank is no issuer.
const ENTITY_LINE = /^\s*(?:per\s+)?ubs\s+(?:europe\s+se|switzerland\s+ag)\b[\s,()\-–]*(?:succursale\s+italia)?[\s,()\-–.]*$/i;
// The validity line under it ("Gültig ab 02.01.2018", "Valid from 09.10.2025"), alone.
const VALIDITY_LINE = /^\s*(?:gültig|gueltig|valid|valido|valida|valable)\b.{0,24}?\d{1,2}\.\d{1,2}\.\d{2,4}\s*\.?\s*$/i;
const BODY = ["TextDraw", "TitleDraw", "MessageBox"];

function plain(text) {
  return String(text).replace(/<[^>]+>/g, " ").replace(/&nbsp;/g, " ").replace(/\s+/g, " ").trim();
}

function lines(html) {
  return String(html).split(/<\/p>|<br\s*\/?>|\n/i).map(plain).filter(function (l) { return l !== ""; });
}

function isSignature(node) {
  return node.type === "Fragment" && typeof node.frag_ref === "string" && /affrg_SignatureGeneric1$/.test(node.frag_ref);
}

// The bank's signature: the fragment itself, or a Repeatable or Panel holding it near the top.
function holdsSignature(node, depth) {
  if (isSignature(node)) return true;
  if (depth >= 3 || (node.type !== "Repeatable" && node.type !== "Panel")) return false;
  return (node.children || []).some(function (c) { return holdsSignature(c, depth + 1); });
}

// A draw is the heading above the bank's signature (UBS-SIG-6) when only static draws stand
// between it and the signature among its siblings: the letter's closing and its unit line too.
function headsSignature(siblings, index) {
  for (let i = index + 1; i < siblings.length; i++) {
    const next = siblings[i];
    if (next.type === "TextDraw" || next.type === "TitleDraw") continue;
    return holdsSignature(next, 0);
  }
  return false;
}

function check(output, ctx) {
  const violations = [];
  const top = output.form.children || [];
  let lastPage = -1;
  for (let i = 0; i < top.length; i++) {
    if (top[i].type === "Panel" && top[i].is_page === true) lastPage = i;
  }
  const lastPointer = lastPage < 0 ? null : "/form/children/" + lastPage;
  const entity = String((output.variables || {}).formrange_entity || "");

  // The issuer's own lines, as the slot2 draw prints them, per language.
  const issuer = {};
  walk(output.form, function (node) {
    if (node && node.type === "TextDraw" && node.dor_header_slot === "slot2") {
      const content = node.content || {};
      for (const lang of Object.keys(content)) {
        issuer[lang] = (issuer[lang] || []).concat(lines(content[lang]).map(function (l) { return l.toLowerCase(); }));
      }
    }
  });

  const shown = [];
  function visit(node, pointer, page, siblings, index) {
    if (BODY.indexOf(node.type) >= 0 && typeof node.dor_header_slot !== "string" && !headsSignature(siblings, index)) {
      const content = node.content || {};
      for (const lang of Object.keys(content)) {
        const all = lines(content[lang]);
        const head = all.length > 0 ? all[0] : "";
        if (head === "") continue;
        if (ENTITY_LINE.test(head) || (issuer[lang] || []).indexOf(head.toLowerCase()) >= 0) {
          violations.push({
            pointer: pointer + "/content/" + lang,
            message: "`" + node.name + "` opens with the issuer \"" + head + "\", which is only the page header's " +
              "`slot2` line (UBS-PAGE-2): delete the draw. The one exception is the heading right above the " +
              "bank's signature",
          });
        } else if (all.length === 1 && VALIDITY_LINE.test(head)) {
          violations.push({
            pointer: pointer + "/content/" + lang,
            message: "`" + node.name + "` is the validity line \"" + head + "\", which is only the page header's " +
              "`slot3` line (UBS-PAGE-2): delete the draw",
          });
        }
      }
    }
    if (node.type === "Fragment" && typeof node.frag_ref === "string" && INFOBOX.test(node.frag_ref)) {
      if (node.visible === false) {
        // The hidden copy that prints the text at the end of the document.
        const missing = [];
        if (node.always_in_pdf !== true) missing.push("always_in_pdf: true");
        if (node.summary_exclude !== true) missing.push("summary_exclude: true");
        if (missing.length > 0) {
          violations.push({
            pointer: pointer,
            message: "the hidden infobox copy `" + node.name + "` needs " + missing.join(" and ") +
              ", or it prints nowhere in the finished document",
          });
        }
        if (node.dor_exclude === true) {
          violations.push({
            pointer: pointer + "/dor_exclude",
            message: "the hidden infobox copy `" + node.name + "` must not set dor_exclude: it would drop the copy from the PDF again",
          });
        }
        if (page !== lastPage) {
          violations.push({
            pointer: pointer,
            message: "the hidden infobox copy `" + node.name + "` belongs inside the last page of the form, where the finished document prints it",
          });
        }
      } else {
        shown.push({ node: node, pointer: pointer });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      visit(children[i], pointer + "/children/" + i, node === output.form ? i : page, children, i);
    }
  }
  visit(output.form, "/form", -2, [], 0);

  // UBS-PAGE-6: the registration text, once, at the end of the last page.
  const required = REGISTRATION.indexOf(entity) >= 0;
  if (!required) {
    for (const s of shown) {
      violations.push({
        pointer: s.pointer,
        message: "`" + s.node.name + "` is the Italian branch's registration text, and this form's entity is " +
          (entity || "unknown") + ", not 033: delete it",
      });
    }
  } else if (shown.length === 0) {
    violations.push({
      pointer: lastPointer === null ? "/form" : lastPointer + "/children",
      message: "a form of entity " + entity + " carries its issuer's legal registration text even where the source " +
        "does not print it (UBS-PAGE-6): add a `Fragment` with frag_ref `" + INFOBOX_PATH + "` as the last " +
        "component of the last page",
    });
  } else {
    for (let k = 1; k < shown.length; k++) {
      violations.push({
        pointer: shown[k].pointer,
        message: "`" + shown[k].node.name + "` is a second copy of the registration text on screen: the form " +
          "carries it once, at the end of the last page, so delete this one",
      });
    }
    const one = shown[0];
    const page = lastPage < 0 ? [] : (top[lastPage].children || []);
    const at = page.indexOf(one.node);
    // After it only what does not show on screen: hidden nodes, the internal bank use, footnotes.
    const later = at < 0 ? [] : page.slice(at + 1).filter(function (c) {
      return c.visible !== false && c.type !== "FootnotePlaceholder" &&
        !(typeof c.frag_ref === "string" && c.frag_ref.indexOf("InternalBankUse") >= 0);
    });
    if (at < 0 || later.length > 0) {
      violations.push({
        pointer: one.pointer,
        message: "the registration text `" + one.node.name + "` goes at the end of the last page" +
          (lastPage < 0 ? "" : " (`" + top[lastPage].name + "`)") + ", as its last component on screen: move it there",
      });
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
