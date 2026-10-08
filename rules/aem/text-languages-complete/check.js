// The translatable texts of the tree that reach the reader (`for_each_text` of `translated.rs`
// lists more): the title of a page, which the writer renders as its heading, the labels of the
// inputs, the content of the draws and the HtmlDisplayer, and each option's label. The title of
// anything below a page does not render, a Fragment's is never written and a Root's is the form
// code, so none of those is held to a language; a Repeatable's is judged against the source
// (ubs-aem-repeatable-add-label).
const LABEL_TYPES = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];
const CONTENT_TYPES = ["TextDraw", "MessageBox", "TitleDraw", "HtmlDisplayer"];

function isText(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function check(output, ctx) {
  const violations = [];
  const languages = output.languages || [];

  function text(map, pointer, what) {
    if (!isText(map)) return;
    const keys = Object.keys(map);
    for (const key of keys) {
      if (languages.indexOf(key) < 0) {
        violations.push({
          pointer: pointer + "/" + key,
          message: what + " is written in `" + key + "`, which the form does not ship (languages: " +
            languages.join(", ") + "): delete it, or add the language to `languages` if the source has it",
        });
      }
    }
    // A text empty in every language is a deliberately empty one.
    const written = keys.some(function (key) { return typeof map[key] === "string" && map[key].trim() !== ""; });
    if (!written) return;
    for (const lang of languages) {
      if (!Object.prototype.hasOwnProperty.call(map, lang)) {
        violations.push({
          pointer: pointer,
          message: what + " has no `" + lang + "` entry: add the source's " + lang + " wording",
        });
      } else if (typeof map[lang] !== "string" || map[lang].trim() === "") {
        violations.push({
          pointer: pointer + "/" + lang,
          message: what + " is empty in `" + lang + "`: write the source's " + lang + " wording",
        });
      }
    }
  }

  function visit(node, pointer) {
    const who = "`" + (node.name || node.type) + "`";
    if (node.type === "Panel" && node.is_page === true) text(node.title, pointer + "/title", who + " page title");
    if (LABEL_TYPES.indexOf(node.type) >= 0) text(node.label, pointer + "/label", who + " label");
    if (CONTENT_TYPES.indexOf(node.type) >= 0) text(node.content, pointer + "/content", who + " content");
    const options = node.options || [];
    for (let i = 0; i < options.length; i++) {
      text(options[i].label, pointer + "/options/" + i + "/label", who + " option " + i + " label");
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
