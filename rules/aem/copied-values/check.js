// CNV-VAL-1..3, CNV-INP-7: where an input's value comes from. Mirrors `check_inputs` of the UBS
// layer (`document.rs`), which refuses to build a form whose `calculate` or `copy_from` names no
// input of the form or the input itself, or an input with both, and `subject_references`
// (`letterhead.rs`), which reads a `calculate` template: `{NAME}` names an input, `{{` and `}}`
// write a brace, and any other brace is refused.
const INPUTS = ["TextField", "NumberField", "DatePicker", "Dropdown", "Checkbox", "RadioButton"];
const NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

// The names a template reads, or the reason it cannot be read.
function references(template) {
  const names = [];
  let rest = String(template);
  for (;;) {
    const at = rest.search(/[{}]/);
    if (at < 0) return { names: names };
    const brace = rest[at];
    const after = rest.slice(at + 1);
    if (after[0] === brace) { rest = after.slice(1); continue; }
    if (brace === "}") return { error: "a lone `}` (write `}}` for a brace)" };
    const end = after.indexOf("}");
    if (end < 0) return { error: "an unclosed `{` (write `{{` for a brace)" };
    const name = after.slice(0, end);
    if (!NAME.test(name)) return { error: "`{" + name + "}`, which names no input (a reference is `{NAME}`)" };
    if (names.indexOf(name) < 0) names.push(name);
    rest = after.slice(end + 1);
  }
}

function given(value) {
  return value !== undefined && value !== null && value !== "";
}

function check(output, ctx) {
  const violations = [];
  const inputs = [];
  const names = {};
  function collect(node, pointer) {
    if (INPUTS.indexOf(node.type) >= 0) {
      inputs.push({ node: node, pointer: pointer });
      if (typeof node.name === "string") names[node.name] = true;
    }
    const children = node.children || [];
    for (let i = 0; i < children.length; i++) collect(children[i], pointer + "/children/" + i);
  }
  collect(output.form, "/form");

  function source(node, at, name) {
    if (name === node.name) {
      violations.push({ pointer: at, message: "`" + node.name + "` takes its value from itself: name the input it copies" });
    } else if (!Object.prototype.hasOwnProperty.call(names, name)) {
      violations.push({
        pointer: at,
        message: "`" + node.name + "` takes its value from `" + name + "`, which is no input of the form (a " +
          "fragment's fields cannot be named): name the input the source copies, by its `name`",
      });
    }
  }

  for (const entry of inputs) {
    const node = entry.node;
    const p = entry.pointer;
    const readOnly = node.read_only === true;
    if (given(node.copy_from)) {
      source(node, p + "/copy_from", String(node.copy_from));
      if (given(node.calculate)) {
        violations.push({
          pointer: p + "/copy_from",
          message: "`" + node.name + "` both calculates its value and copies `" + node.copy_from + "`: keep one, " +
            "`calculate` where nobody can change it, `copy_from` where it stays editable",
        });
      } else if (readOnly) {
        violations.push({
          pointer: p + "/copy_from",
          message: "`" + node.name + "` is read-only and copies `" + node.copy_from + "`: a value copied into an input " +
            "nobody can change is a calculation, `calculate: \"{" + node.copy_from + "}\"` instead of `copy_from`",
        });
      }
    }
    if (given(node.calculate)) {
      const read = references(node.calculate);
      if (read.error) {
        violations.push({ pointer: p + "/calculate", message: "`" + node.name + "` calculates its value from " + read.error });
      } else {
        for (const name of read.names) source(node, p + "/calculate", name);
      }
      if (!readOnly) {
        violations.push({
          pointer: Object.prototype.hasOwnProperty.call(node, "read_only") ? p + "/read_only" : p + "/calculate",
          message: "`" + node.name + "` is calculated but editable, so what is typed is overwritten: set `read_only: " +
            "true`, or, where the source lets it be changed, use `copy_from` instead",
        });
      }
    }
    if (readOnly && node.mandatory === true) {
      violations.push({
        pointer: p + "/mandatory",
        message: "`" + node.name + "` is read-only and mandatory: nobody filling in the form can satisfy it, and a " +
          "fixed value is not required, set `mandatory: false`",
      });
    }
    if (readOnly && !given(node.default_value) && !given(node.calculate) && !given(node.copy_from)) {
      violations.push({
        pointer: p + "/read_only",
        message: "`" + node.name + "` is read-only and gets no value (no `default_value`, `calculate` or `copy_from`), " +
          "so it stays empty: give it the value the source shows, or make it editable",
      });
    }
  }
  return { pass: violations.length === 0, violations: violations };
}
