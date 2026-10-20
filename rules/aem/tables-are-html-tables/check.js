// The scripted part of the merged legacy-table-panels rule: a `TBL_` panel of static draws is the
// pre-HTML-component workaround. Added: a table's rows and cells are the same in every language.
const TR = /<tr\b[^>]*>([\s\S]*?)(?=<tr\b|<\/table>|$)/gi;
const CELL = /<t([hd])\b([^>]*)>/gi;

// The shape of a table's markup: a header or not, then each row's cells (counting a cell's
// colspan), e.g. "head|h3|d3|d3".
function shape(html) {
  const rows = [/<thead[\s>]/i.test(html) ? "head" : "nohead"];
  const tr = new RegExp(TR.source, TR.flags);
  let m;
  while ((m = tr.exec(html)) !== null) {
    let cells = 0;
    let kind = "d";
    const cell = new RegExp(CELL.source, CELL.flags);
    let c;
    while ((c = cell.exec(m[1])) !== null) {
      const span = /\bcolspan\s*=\s*["']?([0-9]+)/i.exec(c[2]);
      cells += span ? Number(span[1]) : 1;
      if (c[1].toLowerCase() === "h") kind = "h";
    }
    rows.push(kind + cells);
  }
  return rows;
}

function check(output, ctx) {
  const violations = [];
  function visit(node, pointer) {
    const children = node.children || [];
    const name = typeof node.name === "string" ? node.name : "";
    if (node.type === "Panel" && name.startsWith("TBL_") && children.length > 0 &&
        children.every(function (c) { return c.type === "TextDraw" || c.type === "TitleDraw"; })) {
      violations.push({
        pointer: pointer,
        message: "panel `" + name + "` lays a table out as static text; make it an HtmlDisplayer",
      });
    }
    if (node.type === "HtmlDisplayer" && name.startsWith("TBL_") && node.content && typeof node.content === "object") {
      let first = null;
      for (const lang of Object.keys(node.content)) {
        const html = String(node.content[lang]);
        if (!/<table[\s>]/i.test(html)) continue;
        const rows = shape(html);
        if (first === null) {
          first = { lang: lang, rows: rows };
          continue;
        }
        if (rows.join("|") === first.rows.join("|")) continue;
        let what;
        if (rows[0] !== first.rows[0]) what = (rows[0] === "head" ? "a" : "no") + " `<thead>`";
        else if (rows.length !== first.rows.length) what = (rows.length - 1) + " rows";
        else {
          let k = 1;
          while (rows[k] === first.rows[k]) k++;
          what = "row " + k + " of " + rows[k].slice(1) + " cells" + (rows[k][0] === "h" ? " (header)" : "");
        }
        violations.push({
          pointer: pointer + "/content/" + lang,
          message: "`" + name + "` has " + what + " in " + lang + ", unlike " + first.lang +
            ": the table has the source's rows and columns in every language",
        });
      }
    }
    for (let i = 0; i < children.length; i++) visit(children[i], pointer + "/children/" + i);
  }
  visit(output.form, "/form");
  return { pass: violations.length === 0, violations: violations };
}
