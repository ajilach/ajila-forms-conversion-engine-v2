// UBS-INP-1: only a radio button group or a checkbox list may hide its label (`hide_title`); AEM
// breaks the form when another input hides its title (backlog "Hidden labels"). UBS-INP-2: a label
// longer than 80 characters is a caption, which goes into a text before the input. A single
// checkbox whose label is its one option shows only that option (the writer hides its title), so
// its length is not held to the limit.
const LABELLED = ["TextField", "NumberField", "DatePicker", "Dropdown", "RadioButton", "Checkbox"];
const MAY_HIDE = ["RadioButton", "Checkbox"];

// A single checkbox whose label is its one option's text: a statement of its own.
function statement(node) {
  const options = node.options || [];
  if (node.type !== "Checkbox" || options.length !== 1) return false;
  const label = node.label || {};
  const option = options[0].label || {};
  return Object.keys(label).every(function (lang) { return plain(label[lang]) === plain(option[lang] || ""); });
}
const MAX_LABEL = 80;

function plain(text) {
  return String(text).replace(/<[^>]*>/g, "").replace(/&nbsp;|\u00a0/g, " ").replace(/&[a-z]+;|&#[0-9]+;/gi, "x")
    .replace(/\s+/g, " ").trim();
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    if (node.hide_title === true && MAY_HIDE.indexOf(node.type) < 0) {
      violations.push({
        pointer: pointer + "/hide_title",
        message: node.type + " `" + node.name + "` hides its label, which only a RadioButton or a Checkbox list may: " +
          "remove `hide_title`",
      });
    }
    if (LABELLED.indexOf(node.type) >= 0 && !statement(node)) {
      const label = node.label || {};
      for (const lang of Object.keys(label)) {
        const length = Array.from(plain(label[lang])).length;
        if (length > MAX_LABEL) {
          violations.push({
            pointer: pointer + "/label/" + lang,
            message: node.type + " `" + node.name + "`'s " + lang + " label has " + length + " characters, more than " +
              MAX_LABEL + ": put the full caption verbatim into a TextDraw right before the input and label the " +
              "input with the short phrase of the caption that names what is asked",
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
