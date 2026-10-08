// A script that reads the form's language (directly or through a `metadata` variable); only such
// scripts are judged here.
function readsLanguage(text) {
  return text.indexOf("getFormMetadata(") >= 0 && /\.language\b/.test(text);
}
const CHOICES = ["Dropdown", "Checkbox", "RadioButton"];

// Every `fd:scripts` start tag in a serialized passthrough child.
function scriptsTags(xml) {
  const tags = [];
  let at = xml.indexOf("<fd:scripts");
  while (at >= 0) {
    const end = xml.indexOf(">", at);
    tags.push(end < 0 ? xml.slice(at) : xml.slice(at, end));
    at = xml.indexOf("<fd:scripts", at + 1);
  }
  return tags;
}

// The `fd:` attributes of a start tag, name to raw (still escaped) value.
function attributes(tag) {
  const found = {};
  const re = /\s(fd:[A-Za-z]+)="([^"]*)"/g;
  let m;
  while ((m = re.exec(tag)) !== null) found[m[1]] = m[2];
  return found;
}

// An attribute value as AEM reads it: XML entities first, then JCR's backslash escapes.
function decode(raw) {
  const xml = raw
    .replace(/&quot;/g, "\"")
    .replace(/&apos;/g, "'")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&amp;/g, "&");
  return xml.replace(/\\(.)/g, "$1");
}

// The language codes a script tests with `indexOf("xx")`.
function testedCodes(content) {
  const codes = [];
  const re = /indexOf\(\s*["']([A-Za-z-]+)["']\s*\)/g;
  let m;
  while ((m = re.exec(content)) !== null) codes.push(m[1].toLowerCase());
  return codes;
}

function check(output, ctx) {
  const violations = [];
  const languages = (output.languages || []).map(function (l) { return String(l).toLowerCase(); });
  const spanish = languages.indexOf("es") >= 0 || languages.indexOf("sp") >= 0;

  // The panels some choice shows: the writer gives them their own visibility scripts.
  const targeted = {};
  function collect(node) {
    const conditions = node.conditions || [];
    for (let i = 0; i < conditions.length; i++) targeted[conditions[i].target_panel_name] = true;
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) collect(children[i]);
  }
  collect(output.form);

  function visit(node, pointer) {
    const raw = (node.passthrough && node.passthrough.raw_children) || [];
    let gated = false;
    let scriptsElements = 0;
    for (let i = 0; i < raw.length; i++) {
      const at = pointer + "/passthrough/raw_children/" + i;
      const tags = scriptsTags(String(raw[i]));
      scriptsElements += tags.length;
      for (const tag of tags) {
        const attrs = attributes(tag);
        for (const name in attrs) {
          const text = decode(attrs[name]);
          if (!readsLanguage(text)) continue;
          gated = true;
          const say = function (message) {
            violations.push({ pointer: at, message: "`" + node.name + "`'s language script " + message });
          };
          if (name !== "fd:init") {
            say("is on " + name + ": it belongs on fd:init, the Initialize event, which runs when the form loads");
            continue;
          }
          let models;
          try {
            models = JSON.parse(text);
          } catch (e) {
            say("is not a SCRIPTMODEL array once unescaped (" + e + "): write it in the shape the rule gives");
            continue;
          }
          const script = Array.isArray(models) && models.length === 1 && models[0] && models[0].script;
          if (!script || typeof script.content !== "string") {
            say("is not one SCRIPTMODEL with a script content: write it in the shape the rule gives");
            continue;
          }
          if (script.event !== "Initialize") {
            say("has the event \"" + script.event + "\": it must be \"Initialize\"");
          }
          if (script.field !== node.name) {
            say("names the field \"" + script.field + "\": it must name the node it is on, `" + node.name + "`");
          }
          const content = script.content;
          if (content.indexOf("toLowerCase()") < 0) {
            say("does not lower-case the language, so a form opened with `DE` would not match `de`");
          }
          if (/language\s*!?==?\s*["']/.test(content) || /["']\s*!?==?\s*language\b/.test(content)) {
            say("compares the language with ==: test each code with language.indexOf(\"xx\") !== -1");
          }
          const codes = testedCodes(content);
          if (codes.length === 0) {
            say("tests no language code with indexOf");
          }
          for (const code of codes) {
            const known = languages.indexOf(code) >= 0 || ((code === "es" || code === "sp") && spanish);
            if (!known) {
              say("tests \"" + code + "\", which is not a language of the form (" + languages.join(", ") + ")");
            }
          }
          const es = codes.indexOf("es") >= 0;
          const sp = codes.indexOf("sp") >= 0;
          if (es !== sp) {
            say("tests Spanish as \"" + (es ? "es" : "sp") + "\" only: the platform files it under either code, test both");
          }
          if (content.indexOf("showAFShowDor(this)") < 0 || content.indexOf("hideAFHideDor(this)") < 0) {
            say("does not call both window.forms.ubs.showAFShowDor(this) and hideAFHideDor(this), so the Document of Record does not follow the visibility");
          }
        }
      }
    }
    if (gated) {
      if (scriptsElements > 1) {
        violations.push({
          pointer: pointer + "/passthrough/raw_children",
          message: "`" + node.name + "` carries " + scriptsElements + " fd:scripts elements: AEM takes one, so put every event in it",
        });
      }
      if (node.visible !== false) {
        violations.push({
          pointer: pointer + "/visible",
          message: "`" + node.name + "` is gated by a language script but authored visible: author it visible: false, so it stays hidden until the script runs",
        });
      }
      const conditional = node.is_conditional === true || targeted[node.name] === true ||
        (CHOICES.indexOf(node.type) >= 0 && (node.conditions || []).length > 0);
      if (conditional) {
        violations.push({
          pointer: pointer,
          message: "`" + node.name + "` already gets visibility scripts from the writer, so the language script would be a second fd:scripts: wrap the node in a Panel and gate the Panel",
        });
      }
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
