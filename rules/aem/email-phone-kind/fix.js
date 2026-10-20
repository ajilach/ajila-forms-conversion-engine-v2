// Makes every input that asks for an email address or a telephone number the email or telephone
// component (a NumberField becomes a TextField; the name takes `EML_` or `TEL_`, and the
// `copy_from`, `calculate` and letter subject references follow it), then wraps each run of contact
// inputs that sits beside a panel in a leaf panel of its own, the shape of the issued corpus's
// `PN_TelephoneAndEmail` (`set_contact_leaf_panel.py` of the feedback repo): a run narrower than
// a row gets the width it occupied and its fields fill it. The same classification as check.js.
const NOT_WORD_BEFORE = "(?<![\\p{L}\\p{N}_])";
const NOT_WORD_AFTER = "(?![\\p{L}\\p{N}_])";
const EMAIL_RE = new RegExp(
  NOT_WORD_BEFORE + "(e[-\\s.]?mail|courriel|posta\\s+elettronica|pec)" + NOT_WORD_AFTER, "iu");
const PHONE_RE = new RegExp(
  NOT_WORD_BEFORE +
    "(tel|telefon|telefono|telefonico|telefonica|telephone|t[ée]l[ée]phone|" +
    "tel[eé]fono|phone|mobile|mobil|handy|natel|cellulare|m[oó]vil|" +
    "fax|telefax|facsimile)" + NOT_WORD_AFTER, "iu");
const DENY_LABEL = ["telefonische bestellung", "telefonico canale", "titolare / numero di telefono"];
const DENY_NAME = ["TXT_MailingAddressConfirmation", "TXT_Amountimobili", "TXT_TotaleImmobili"];
const CAMEL_RE = /(?<=[a-zà-ÿ0-9])(?=[A-ZÀ-Þ])|(?<=[A-ZÀ-Þ])(?=[A-ZÀ-Þ][a-zà-ÿ])/g;
const PANELS = ["Panel", "Repeatable", "Fragment"];
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];
const WRAPPER = "PN_TelephoneAndEmail";

function norm(text) {
  return String(text || "").replace(/\s+/g, " ").trim();
}

function masterLanguage(output) {
  const languages = output.languages || [];
  return languages.indexOf("en") >= 0 ? "en" : languages[0];
}

function masterText(texts, master) {
  const map = texts || {};
  if (Object.prototype.hasOwnProperty.call(map, master)) return String(map[master]);
  const keys = Object.keys(map).sort();
  return keys.length > 0 ? String(map[keys[0]]) : "";
}

function classify(label, name) {
  for (const denied of DENY_NAME) {
    if (String(name || "").startsWith(denied)) return null;
  }
  const text = norm(label);
  const lower = text.toLowerCase();
  for (const denied of DENY_LABEL) {
    if (lower.indexOf(denied) >= 0) return null;
  }
  const probe = text !== "" ? text : norm(String(name || "").replace(/_/g, " ").replace(CAMEL_RE, " "));
  if (EMAIL_RE.test(probe)) return "Email";
  if (PHONE_RE.test(probe)) return "Telephone";
  return null;
}

function isContact(node) {
  return node.type === "TextField" && (node.kind === "Email" || node.kind === "Telephone");
}

function excluded(node) {
  return (node.summary_exclude === true || node.dor_exclude === true) && node.always_in_pdf !== true;
}

function emitted(node) {
  return !excluded(node) && (node.visible !== false || node.show_if_hidden === true || node.always_in_pdf === true);
}

// A uuid derived from a seed (FNV-1a, four lanes), so a fix is the same each time it runs.
function uuidFrom(seed) {
  let hex = "";
  for (let lane = 0; lane < 4; lane++) {
    let h = (0x811c9dc5 ^ (lane * 0x9e3779b1)) >>> 0;
    const text = lane + ":" + seed;
    for (let i = 0; i < text.length; i++) {
      h ^= text.charCodeAt(i);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    hex += ("00000000" + h.toString(16)).slice(-8);
  }
  return hex.slice(0, 8) + "-" + hex.slice(8, 12) + "-4" + hex.slice(13, 16) + "-8" + hex.slice(17, 20) + "-" + hex.slice(20, 32);
}

function renameIn(template, renames) {
  return String(template).replace(/\{\{|\}\}|\{([A-Za-z_][A-Za-z0-9_]*)\}/g, function (match, name) {
    return name !== undefined && Object.prototype.hasOwnProperty.call(renames, name) ? "{" + renames[name] + "}" : match;
  });
}

function fix(output, ctx) {
  const master = masterLanguage(output);
  const form = JSON.parse(JSON.stringify(output.form));
  const names = {};
  (function collect(node) {
    if (typeof node.name === "string") names[node.name] = true;
    for (const child of node.children || []) collect(child);
  })(form);
  function unique(base) {
    let name = base;
    for (let n = 2; Object.prototype.hasOwnProperty.call(names, name); n++) name = base + "_" + n;
    names[name] = true;
    return name;
  }

  // 1. The kind, and the name that goes with it.
  const renames = {};
  function retype(node) {
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      const plainText = child.type === "TextField" && (!child.kind || child.kind === "Plain");
      if ((plainText || child.type === "NumberField") && child.read_only !== true) {
        const kind = classify(masterText(child.label, master), child.name);
        if (kind) {
          const prefix = kind === "Email" ? "EML_" : "TEL_";
          const stem = String(child.name).replace(/^(TXT|NB)_/, "");
          const name = unique(prefix + stem);
          renames[child.name] = name;
          const field = JSON.parse(JSON.stringify(child));
          field.type = "TextField";
          field.kind = kind;
          field.name = name;
          if (!Object.prototype.hasOwnProperty.call(field, "max_chars")) field.max_chars = null;
          delete field.lead_digits;
          delete field.frac_digits;
          children[i] = field;
        }
      }
      retype(child);
    }
  }
  retype(form);

  // 2. The references to a renamed input.
  (function references(node) {
    if (INPUTS.indexOf(node.type) >= 0) {
      if (typeof node.copy_from === "string" && Object.prototype.hasOwnProperty.call(renames, node.copy_from)) {
        node.copy_from = renames[node.copy_from];
      }
      if (typeof node.calculate === "string") node.calculate = renameIn(node.calculate, renames);
    }
    if (node.type === "Letterhead" && node.subject) {
      for (const lang of Object.keys(node.subject)) node.subject[lang] = renameIn(node.subject[lang], renames);
    }
    for (const child of node.children || []) references(child);
  })(form);

  // 3. A leaf panel around each run of contact inputs beside a panel.
  (function wrap(node, hidden) {
    const out = hidden || excluded(node);
    const children = node.children || [];
    if (!out && children.some(function (c) { return PANELS.indexOf(c.type) >= 0 && !excluded(c); })) {
      const next = [];
      for (let i = 0; i < children.length;) {
        if (!isContact(children[i])) { next.push(children[i]); i++; continue; }
        let end = i;
        while (end + 1 < children.length && isContact(children[end + 1])) end++;
        const run = children.slice(i, end + 1);
        if (!run.some(emitted)) { Array.prototype.push.apply(next, run); i = end + 1; continue; }
        const width = run.reduce(function (sum, f) { return sum + (typeof f.colspan === "number" ? f.colspan : 12); }, 0);
        const fields = width >= 12 ? run : run.map(function (f) {
          return Object.assign({}, f, { colspan: Math.max(1, Math.min(12, Math.round((typeof f.colspan === "number" ? f.colspan : 12) * 12 / width))) });
        });
        next.push({
          type: "Panel", uuid: uuidFrom(run[0].uuid + ":" + WRAPPER), name: unique(WRAPPER), title: {},
          children: fields, is_page: false, visible: true, is_conditional: false, dor_num_cols: 2,
          colspan: Math.min(12, width), dor_colspan: null, bind_ref: null, frag_ref: null,
        });
        i = end + 1;
      }
      node.children = next;
    }
    for (const child of node.children || []) wrap(child, out);
  })(form, false);

  // One replace per page that changed (the form's own list where a page was added).
  const before = output.form.children || [];
  const after = form.children || [];
  if (before.length !== after.length) return [{ op: "replace", path: "/form/children", value: after }];
  const ops = [];
  for (let i = 0; i < after.length; i++) {
    if (JSON.stringify(before[i]) !== JSON.stringify(after[i])) ops.push({ op: "replace", path: "/form/children/" + i, value: after[i] });
  }
  return ops;
}
