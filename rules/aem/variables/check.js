const CODE = /^[A-Z0-9]{3,6}$/;
const ENTITY = /^[0-9]{3}$/;

function check(output, ctx) {
  const violations = [];
  const variables = output.variables || {};
  function need(key, pattern, what) {
    const value = variables[key];
    const at = Object.prototype.hasOwnProperty.call(variables, key) ? "/variables/" + key : "/variables";
    if (typeof value !== "string" || !pattern.test(value)) {
      violations.push({
        pointer: at,
        message:
          key + " is " + (typeof value === "string" ? "\"" + value + "\"" : "missing") + "; it must be " +
          what + ", as in the source form's variables",
      });
    }
  }
  need("formrange_code", CODE, "the form code, 3 to 6 upper-case letters and digits (for example AAEV)");
  need("formrange_entity", ENTITY, "a three-digit entity code (019, 033, 001 or another entity's)");
  return { pass: violations.length === 0, violations: violations };
}
