// A signature block is a Repeatable wrapping a signature fragment (`affrg_SignatureGeneric1`, or a
// market or Swiss fragment whose file name says "signature"). The internal-bank-use family is not a
// signature block, though its global fragment's name ends in `_Signature`.
function isInternalBankUse(fragRef) {
  const lower = String(fragRef).toLowerCase();
  return lower.indexOf("internalbankuse") >= 0 || lower.indexOf("internal_bank_use") >= 0 ||
    lower.indexOf("spazio_riservato") >= 0;
}

function isSignatureFragment(node) {
  if (node.type !== "Fragment" || typeof node.frag_ref !== "string") return false;
  const file = node.frag_ref.split("/").pop().toLowerCase();
  return file.indexOf("signature") >= 0 && !isInternalBankUse(node.frag_ref);
}

function isSignatureBlock(node) {
  return node.type === "Repeatable" && (node.children || []).some(isSignatureFragment);
}

// The internal-bank-use block reaches the reader through the PDF alone (summary_exclude with
// always_in_pdf), and a bank signature inside it shares that: its summary exclusion is no break.
function isInternalBankUseBlock(node) {
  if (node.type === "Fragment") return typeof node.frag_ref === "string" && isInternalBankUse(node.frag_ref);
  return node.type === "Panel" && typeof node.name === "string" && node.name.indexOf("PN_InternalBankUse") === 0;
}

// The first word of a title may not be the act of signing or a name caption (UBS-WRD-2). German
// "Firma" is a company, which can sign, so it is only a caption word in Italian and Spanish.
const CAPTION_WORDS = {
  de: ["unterschrift", "unterschriften", "unterzeichnung", "unterzeichnet", "signatur", "name", "namen",
    "vorname", "nachname"],
  en: ["signature", "signatures", "signed", "sign", "name", "names", "surname"],
  it: ["firma", "firme", "nome", "cognome", "sottoscrizione", "sottoscritto", "sigla"],
  es: ["firma", "firmas", "firmado", "nombre", "apellido", "apellidos"],
  fr: ["signature", "signatures", "signé", "nom", "prénom"],
};
const ARTICLES = {
  de: ["der", "die", "das", "des", "dem", "den", "ein", "eine", "einer", "eines", "einem", "einen"],
  en: ["the", "a", "an"],
  it: ["il", "lo", "la", "i", "gli", "le", "un", "uno", "una", "del", "dello", "della", "dei", "degli", "delle"],
  es: ["el", "la", "los", "las", "un", "una", "unos", "unas", "del", "al", "lo"],
  fr: ["le", "la", "les", "un", "une", "des", "du", "de"],
};
// An elided article: Italian "l'", "un'", "dell'", French "l'", "d'".
const ELIDED = /^(l|un|dell|all|nell|d)['’]/i;

function union(table) {
  const out = [];
  Object.keys(table).forEach(function (k) { table[k].forEach(function (w) { if (out.indexOf(w) < 0) out.push(w); }); });
  return out;
}

function wordsFor(table, lang) {
  const base = String(lang).toLowerCase().split(/[-_]/)[0];
  if (base === "sp") return table.es;
  return table[base] || union(table);
}

function plain(text) {
  return String(text).replace(/<[^>]*>/g, " ").replace(/&nbsp;/g, " ").replace(/&amp;/g, "&")
    .replace(/\s+/g, " ").trim();
}

// The title without a leading list number ("1.", "a)").
function body(text) {
  return plain(text).replace(/^[0-9a-z]{1,2}[.)]\s+/i, "");
}

function firstWord(text) {
  const m = /^[A-Za-zÀ-ÖØ-öø-ÿ]+/.exec(body(text));
  return m ? m[0].toLowerCase() : "";
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

// The issuing entity as a heading line: "UBS Europe SE", "UBS Europe SE (Succursale Italia)",
// "UBS Switzerland AG", "UBS AG".
const ENTITY_LINE = /^(?:per\s+|für\s+|for\s+)?ubs\s+(?:europe\s+se|switzerland\s+ag|ag|sa)\b/i;

function isEntityHeading(node) {
  if (!node || (node.type !== "TitleDraw" && node.type !== "TextDraw")) return false;
  const content = node.content || {};
  return Object.keys(content).some(function (lang) { return ENTITY_LINE.test(plain(content[lang])); });
}

function check(output, ctx) {
  const violations = [];
  const master = masterLanguage(output);
  const languages = output.languages || [];
  let isLetter = false;
  walk(output.form, function (node) {
    if (node !== null && typeof node === "object" && node.type === "Letterhead") isLetter = true;
  });
  const reported = {};

  function report(pointer, message) {
    if (reported[pointer]) return;
    reported[pointer] = true;
    violations.push({ pointer: pointer, message: message });
  }

  // In a letter the bank's officers follow ubs-aem-letter-bank-signature: their title is the
  // officers' own caption, under the entity line.
  function isLetterOfficers(node) {
    return isLetter && (node.name === "RCP_SGN_UBS" || (node.children || []).some(function (c) {
      return isSignatureFragment(c) && c.name === "PN_SGN_UBS";
    }));
  }

  function titleChecks(node, pointer, previous) {
    const title = node.title || {};
    const own = masterText(title, master);
    if (plain(own) === "" || own === node.name) {
      report(pointer + "/title", "the signature `" + node.name + "` has no title: title it with the signer as a " +
        "noun (\"Client\", \"Kontoinhaber\", \"Cliente\"), or with the heading the source gives this block, in every language");
      return;
    }
    Object.keys(title).forEach(function (lang) {
      const text = String(title[lang]);
      if (plain(text) === "") return;
      const word = firstWord(text);
      const at = pointer + "/title/" + lang;
      if (wordsFor(CAPTION_WORDS, lang).indexOf(word) >= 0) {
        report(at, "the " + lang + " title of the signature `" + node.name + "` is \"" + plain(text) + "\", a caption: " +
          "the title is the signer alone, as a noun in the nominative (\"Unterschrift des Kontoinhabers\" gives " +
          "\"Kontoinhaber\", \"Firma del cliente\" gives \"Cliente\")");
      } else if (wordsFor(ARTICLES, lang).indexOf(word) >= 0 || ELIDED.test(body(text))) {
        report(at, "the " + lang + " title of the signature `" + node.name + "` starts with an article (\"" + plain(text) +
          "\"): drop the article and write the signer in the nominative");
      } else if (!isLetter && /\bUBS\b/.test(plain(text))) {
        report(at, "the " + lang + " title of the signature `" + node.name + "` names the bank (\"" + plain(text) + "\"): " +
          "the bank's signature is titled \"Responsible person\", and the entity name is a TitleDraw right before it");
      }
    });
    const bank = !isLetter && (node.name === "RCP_SGN_UBS" || isEntityHeading(previous));
    if (bank) {
      if (!isEntityHeading(previous)) {
        report(pointer, "`" + node.name + "` is the bank's signature: the entity name (\"UBS Europe SE\") is a heading " +
          "TitleDraw right before it, in every language");
      }
      if (languages.indexOf("en") >= 0 && plain(title.en === undefined ? "" : title.en).toLowerCase() !== "responsible person") {
        report(pointer + "/title" + (title.en === undefined ? "" : "/en"), "`" + node.name + "` signs for the bank under the " +
          "entity heading: its English title is \"Responsible person\", and each other language says the same");
      }
    }
  }

  function visit(node, pointer, chain) {
    if (node === null || typeof node !== "object") return;
    const here = chain.concat([{ node: node, pointer: pointer }]);
    if (isSignatureFragment(node)) {
      const parent = chain.length > 0 ? chain[chain.length - 1].node : null;
      if (!parent || parent.type !== "Repeatable") {
        report(pointer, "the signature fragment `" + node.name + "` stands outside a Repeatable, so nothing titles it: " +
          "wrap it in a Repeatable `RCP_SGN_<stem>` (bounds from the source, 1 and 1 for a single signer) titled with " +
          "the signer, and name the fragment `PN_SGN_<stem>`");
      }
      for (let i = here.length - 1; i >= 0; i--) {
        const n = here[i].node;
        const p = here[i].pointer;
        if (n.dor_exclude === true) {
          report(p + "/dor_exclude", "`" + n.name + "` is excluded from the Document of Record and holds the signature `" +
            node.name + "`: a signature and every panel holding it print, so delete dor_exclude (and summary_exclude); " +
            "only its visibility decides whether it shows");
        }
        if (n.summary_exclude === true && !isInternalBankUseBlock(n)) {
          report(p + "/summary_exclude", "`" + n.name + "` is excluded from the summary and holds the signature `" +
            node.name + "`: a signature and every panel holding it reach the summary, so delete summary_exclude");
        }
      }
    }
    const children = node.children || [];
    const signatures = [];
    for (let i = 0; i < children.length; i++) {
      const child = children[i];
      const at = pointer + "/children/" + i;
      if (isSignatureBlock(child) && !isLetterOfficers(child)) {
        titleChecks(child, at, i > 0 ? children[i - 1] : null);
        signatures.push({ node: child, pointer: at });
      }
      visit(child, at, here);
    }
    // Two sibling signatures never share a title (UBS-SIG-1).
    for (let a = 0; a < signatures.length; a++) {
      for (let b = 0; b < a; b++) {
        const ta = signatures[a].node.title || {};
        const tb = signatures[b].node.title || {};
        const shared = Object.keys(ta).filter(function (lang) {
          const text = plain(ta[lang]).toLowerCase();
          return text !== "" && tb[lang] !== undefined && plain(tb[lang]).toLowerCase() === text;
        });
        if (shared.length > 0) {
          report(signatures[a].pointer + "/title/" + shared[0], "the signatures `" + signatures[b].node.name + "` and `" +
            signatures[a].node.name + "` are both titled \"" + plain(ta[shared[0]]) + "\": two signatures side by side " +
            "name different signers, so title each with its own (or with the heading the source gives it)");
          break;
        }
      }
    }
  }
  visit(output.form, "/form", []);

  // A twin (`twin_of`) names a party Repeatable, or the fragment one wraps, and repeats with its
  // bounds: the party's Add and Remove are the only ones it has.
  const parties = {};
  const twins = [];
  function collect(node, pointer) {
    if (node.type === "Repeatable") {
      parties[node.name] = node;
      (node.children || []).forEach(function (c) {
        if (c.type === "Fragment") parties[c.name] = node;
      });
      if (typeof node.twin_of === "string") twins.push({ node: node, pointer: pointer });
    }
    (node.children || []).forEach(function (c, i) { collect(c, pointer + "/children/" + i); });
  }
  collect(output.form, "/form");
  twins.forEach(function (twin) {
    const party = parties[twin.node.twin_of];
    if (!party || party === twin.node) {
      report(twin.pointer + "/twin_of", "`" + twin.node.name + "` repeats with `" + twin.node.twin_of + "`, which is " +
        "no party Repeatable of the form nor a fragment one wraps: name the party's Repeatable or its fragment, " +
        "or remove twin_of where the source's signature has an Add of its own");
    } else if (party.min_occur !== twin.node.min_occur || party.max_occur !== twin.node.max_occur) {
      report(twin.pointer + "/min_occur", "`" + twin.node.name + "` repeats with `" + party.name + "` but has bounds " +
        twin.node.min_occur + ".." + twin.node.max_occur + " where the party has " + party.min_occur + ".." +
        party.max_occur + ": a twin has as many rows as its party, so take the party's bounds");
    }
  });
  return { pass: violations.length === 0, violations: violations };
}
