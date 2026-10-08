function check(output, ctx) {
  const violations = [];
  const languages = Object.keys(output.sources || {});
  const assets = output.assets || [];
  for (let i = 0; i < assets.length; i++) {
    const asset = assets[i];
    if (asset.kind !== "text") continue;
    const content = asset.content || {};
    const at = "/assets/" + i + "/content";
    for (const lang of languages) {
      if (!Object.prototype.hasOwnProperty.call(content, lang)) {
        violations.push({
          pointer: at,
          message: "asset `" + asset.key + "` has no `" + lang + "` entry: add the source's " + lang + " wording",
        });
      } else if (typeof content[lang] !== "string" || content[lang].trim() === "") {
        violations.push({
          pointer: at + "/" + lang,
          message: "asset `" + asset.key + "` is blank in `" + lang + "`: write the source's " + lang + " wording",
        });
      }
    }
    for (const lang of Object.keys(content)) {
      if (languages.indexOf(lang) < 0) {
        violations.push({
          pointer: at + "/" + lang,
          message: "asset `" + asset.key + "` is written in `" + lang + "`, which `sources` does not declare " +
            "(" + languages.join(", ") + "): delete it, or add the language to `sources`",
        });
      }
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
