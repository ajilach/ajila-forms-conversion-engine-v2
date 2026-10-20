// What a script can tell of UBS-NOT-1..4: a standalone paragraph opening with a notice label is a
// notice, a notice shows once, and a notice never prints. Red and screen-only source text, and
// working instructions (UBS-IBU-5), need the source and are the author's to judge.
const LABEL = /^(Hinweis|Achtung|Wichtig|Bitte beachten|Note|Please note|Important|Attention|Nota bene|Nota|Avvertenza|Attenzione|Importante|Remarque|Aviso|Atenci[oó]n)\s*:/i;
const PRINT_FLAGS = ["always_in_pdf", "show_if_hidden"];

function masterLanguage(output) {
  const languages = output.languages || [];
  return languages.indexOf("en") >= 0 ? "en" : languages[0];
}

function plain(html) {
  return String(html).replace(/<[^>]*>/g, " ").replace(/&nbsp;|&#160;/g, " ").replace(/\s+/g, " ").trim();
}

// One paragraph: at most one block element, and no list.
function oneParagraph(html) {
  return (String(html).match(/<(p|li|div|h[1-6])[\s>]/gi) || []).length <= 1 && !/<(ul|ol)[\s>]/i.test(html);
}

// A TextDraw that is a notice by its wording: one paragraph opening with a notice label, in some
// language. A draw kept for the printed document (a header line, a PDF-only block) is not one.
function labelledNotice(node, pdfOnly) {
  if (node.type !== "TextDraw" || pdfOnly || node.always_in_pdf || node.dor_header_slot) return false;
  if (typeof node.css === "string" && /subtitle-after-form-title/.test(node.css)) return false;
  const content = node.content || {};
  return Object.keys(content).some(function (lang) {
    return oneParagraph(content[lang]) && LABEL.test(plain(content[lang]));
  });
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);
  let letter = false;
  const targeted = {};
  (function scan(node) {
    if (node.type === "Letterhead") letter = true;
    (node.conditions || []).forEach(function (c) { targeted[c.target_panel_name] = true; });
    (node.children || []).forEach(scan);
  })(output.form);

  // Every notice, with the conditional panels around it: two notices can show together unless
  // each sits in a conditional panel the other is not in.
  const notices = [];
  function visit(node, pointer, gates, pdfOnly) {
    const pdf = pdfOnly || node.always_in_pdf === true;
    if (node.type === "MessageBox") {
      notices.push({ node: node, pointer: pointer, gates: gates });
      PRINT_FLAGS.forEach(function (flag) {
        if (node[flag] === true) {
          violations.push({
            pointer: pointer + "/" + flag,
            message: "`" + node.name + "` is a notice, which never prints, and the writer ignores " + flag +
              ": drop it (text the printed document needs is a TextDraw, not a notice)",
          });
        }
      });
    } else if (!letter && labelledNotice(node, pdf)) {
      notices.push({ node: node, pointer: pointer, gates: gates, label: true });
    }
    const conditional = node.type === "Panel" && (node.is_conditional === true || targeted[node.name] === true);
    const inner = conditional ? gates.concat([node.name]) : gates;
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i, inner, pdf);
  }
  visit(output.form, "/form", [], false);

  function text(n) {
    const content = n.node.content || {};
    const value = Object.prototype.hasOwnProperty.call(content, master) ? content[master] : content[Object.keys(content).sort()[0]];
    return plain(value === undefined ? "" : value).toLowerCase();
  }
  function within(a, b) {
    return a.gates.every(function (g) { return b.gates.indexOf(g) >= 0; });
  }
  const repeated = {};
  for (let i = 0; i < notices.length; i++) {
    for (let j = i + 1; j < notices.length; j++) {
      const a = notices[i];
      const b = notices[j];
      if (repeated[a.pointer] || repeated[b.pointer] || text(a) === "" || text(a) !== text(b)) continue;
      // Keep the one that shows whenever the other does.
      if (within(a, b)) repeated[b.pointer] = a;
      else if (within(b, a)) repeated[a.pointer] = b;
    }
  }
  notices.forEach(function (n) {
    if (repeated[n.pointer]) {
      violations.push({
        pointer: n.pointer,
        message: "`" + n.node.name + "` repeats the notice `" + repeated[n.pointer].node.name +
          "`, which shows wherever this one would: a notice appears once, remove this one",
      });
    } else if (n.label) {
      violations.push({
        pointer: n.pointer + "/type",
        message: "`" + n.node.name + "` is one paragraph opening with a notice label (\"Hinweis:\", \"Nota:\", " +
          "\"Note:\"): it is a notice, so make it a MessageBox and keep the label in its text",
      });
    }
  });
  return { pass: violations.length === 0, violations: violations };
}
