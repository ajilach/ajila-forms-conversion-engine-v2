const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];

// Where the source gives the input no caption, the label still comes from the source
// (CNV-INP-1; precedence over ubs-aem-verbatim-source-text).
const NO_CAPTION = "; where the source gives it no caption, use the nearest source text that names it " +
  "(then its group's caption, its option's text, or a readable field name), verbatim and without " +
  "surrounding brackets";

// The text a label shows: without the hidden footnote descriptions, the footnote references and
// the rest of its markup. A label may carry all of these (ubs-aem-footnotes).
function shown(text) {
  return String(text)
    .replace(/<p\b[^>]*class="[^"]*footnoteDescription[^"]*"[^>]*>[\s\S]*?<\/p>/g, "")
    .replace(/<span\b[^>]*data-af-footnote-id[^>]*>[\s\S]*?<\/span>/g, "")
    .replace(/<[^<>]*>/g, "")
    .replace(/&nbsp;/g, " ")
    .trim();
}

function problem(text) {
  const t = shown(text);
  if (t === "") return "is empty";
  if (t.startsWith("(") && t.endsWith(")")) return "is only a parenthetical hint";
  if (/^[\s._…\-]+$/.test(t) && /[._…\-]{3}/.test(t.replace(/\s+/g, ""))) return "is only a placeholder of filler characters";
  if (/^[A-Za-z]+_[A-Za-z0-9_]*$/.test(t)) return "is a technical field name, not words";
  return null;
}

function optionsCarryText(node, lang) {
  return (node.options || []).some(function (o) {
    const label = o.label || {};
    return Object.prototype.hasOwnProperty.call(label, lang) && String(label[lang]).trim() !== "";
  });
}

function check(output, ctx) {
  const violations = [];
  const languages = output.languages || [];
  function visit(node, pointer) {
    if (INPUTS.indexOf(node.type) >= 0) {
      const label = node.label || {};
      for (const lang of languages) {
        const exempt = node.type === "Checkbox" && optionsCarryText(node, lang);
        const text = Object.prototype.hasOwnProperty.call(label, lang) ? label[lang] : "";
        const why = problem(text);
        if (why && !(why === "is empty" && exempt)) {
          const at = Object.prototype.hasOwnProperty.call(label, lang) ? pointer + "/label/" + lang : pointer + "/label";
          violations.push({
            pointer: at,
            message: node.type + " `" + node.name + "`'s " + lang + " label " + why +
              (why === "is empty" ? NO_CAPTION : ""),
          });
        }
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
