//! The rules the UBS writer generates, as typed rules (`u2s_mapper_aem::script`)
//! rather than hand-escaped JSON in a template.
//!
//! Each builder returns the rules of one attribute; [`attribute_value`]
//! spells them as the attribute's value (a JCR multi-value of JSON objects),
//! and the XML writer escapes that once. A text that reaches a rule's
//! JavaScript as a string (a repeatable's subject, an option value) is
//! written as a JavaScript string literal by [`js_string`]; every layer
//! above it is the codec's.
//!
//! The JavaScript is the archetypes' own, character for character: the
//! deployed forms carry it, the feedback repository's fixers recognise it by
//! its text, and the decoder (`parser.rs`, `unexpand.rs`) folds it back.

use u2s_mapper_aem::script::{BodyOrder, EventScript, ScriptEvent, encode_script_list};

/// The marker opening every rule a generator owns.
fn generated(archetype: &str) -> String {
    format!(
        "// [{archetype}] Generated automatically. Do not edit: will be overwritten. Create your own different script."
    )
}

/// `rules` as the value of the attribute that holds them.
pub fn attribute_value(rules: &[EventScript]) -> String {
    encode_script_list(rules.iter().map(EventScript::to_json))
}

/// `text` as a JavaScript string literal, quotes included. JSON's string
/// escaping is valid JavaScript.
pub fn js_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string serialises")
}

const REPEATING_PANEL: &str = "repeating-panel";

/// A rule of the repeating-panel archetype: content first, no model.
fn repeating(field: &str, event: ScriptEvent, body: &str) -> EventScript {
    EventScript {
        field: field.into(),
        event,
        content: format!("{}\n{body}", generated(REPEATING_PANEL)),
        order: BodyOrder::ContentFirst,
        model: None,
        archetype: Some(REPEATING_PANEL.into()),
    }
}

/// The visibility rule of the repeating-panel archetype: the code editor's
/// order, with its model.
fn repeating_visible(field: &str, body: &str) -> EventScript {
    EventScript {
        archetype: Some(REPEATING_PANEL.into()),
        ..EventScript::event(
            field,
            ScriptEvent::Visibility,
            format!("{}\n{body}", generated(REPEATING_PANEL)),
        )
    }
}

/// What a repeatable's buttons drive.
pub struct Repeating<'a> {
    /// The repeating panel the Add button adds to.
    pub panel: &'a str,
    /// The signature panel the buttons also drive, if any.
    pub twin: Option<&'a str>,
    /// What one row is, announced by the accessibility helpers.
    pub subject: &'a str,
}

impl Repeating<'_> {
    /// The Remove button's click, visibility and initialize rules.
    pub fn remove(&self) -> [EventScript; 3] {
        let subject = js_string(self.subject);
        let mut click = String::from(
            "var repeatingPanel = this.parent;\nvar addButton = this.parent.parent.BT_Add;\nwindow.forms.ubs.removeInstance(repeatingPanel);",
        );
        if let Some(twin) = self.twin {
            click.push_str(&format!("\nwindow.forms.ubs.removeInstance({twin});"));
        }
        click.push_str(&format!(
            "\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabels(repeatingPanel, {subject}, addButton);\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons(repeatingPanel, {subject}, addButton, this);"
        ));
        if let Some(twin) = self.twin {
            click.push_str(&format!(
                "\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons({twin}, {subject}, \"\", \"\");"
            ));
        }
        let shown = "this.parent.instanceIndex === this.parent.instanceManager.instances.length - 1 && this.parent.instanceManager.instances.length > this.parent.instanceManager.minOccur";
        [
            repeating("BT_Remove", ScriptEvent::Click, &click),
            repeating_visible("BT_Remove", &format!("{shown};")),
            repeating("BT_Remove", ScriptEvent::Initialize, &format!("this.visible = ({shown});")),
        ]
    }

    /// The Add button's click, visibility and initialize rules.
    pub fn add(&self) -> [EventScript; 3] {
        let (panel, subject) = (self.panel, js_string(self.subject));
        let mut click = format!("window.forms.ubs.addInstance(this.parent.{panel});");
        if let Some(twin) = self.twin {
            click.push_str(&format!("\nwindow.forms.ubs.addInstance({twin});"));
        }
        click.push_str(&format!(
            "\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabels(this.parent.{panel}, {subject}, this);\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons(this.parent.{panel}, {subject}, this, this.parent.{panel}.instanceManager.instances[this.parent.{panel}.instanceManager.instances.length - 1].BT_Remove);"
        ));
        if let Some(twin) = self.twin {
            click.push_str(&format!(
                "\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons({twin}, {subject}, \"\", \"\");"
            ));
        }
        let shown = format!(
            "this.parent.{panel}.instanceManager.instances.length < this.parent.{panel}.instanceManager.maxOccur"
        );
        [
            repeating("BT_Add", ScriptEvent::Click, &click),
            repeating_visible("BT_Add", &format!("{shown};")),
            repeating("BT_Add", ScriptEvent::Initialize, &format!("this.visible = ({shown});")),
        ]
    }
}

/// A conditional panel's rules: shown while any `(field, value)` trigger
/// holds. The visibility expression only runs when a trigger changes, so the
/// initialize rule repeats it, as an assignment, for a freshly opened form.
pub fn show(panel: &str, triggers: &[(String, String)]) -> (EventScript, EventScript) {
    let condition = triggers
        .iter()
        .map(|(field, value)| format!("{field}.value == {}", js_string(value)))
        .collect::<Vec<_>>()
        .join(" || ");
    let visible = EventScript {
        model: Some("SHOW_EXPRESSION".into()),
        ..EventScript::event(
            panel,
            ScriptEvent::Visibility,
            format!(
                "if ({condition}) {{\n  window.forms.ubs.showAFShowDor(this);\n  true;\n}} else {{\n  window.forms.ubs.hideAFHideDor(this);\n  false;\n}}\n"
            ),
        )
    };
    let init = EventScript::event(
        panel,
        ScriptEvent::Initialize,
        format!(
            "if ({condition}) {{\n  window.forms.ubs.showAFShowDor(this);\n  this.visible = true;\n}} else {{\n  window.forms.ubs.hideAFHideDor(this);\n  this.visible = false;\n}}\n"
        ),
    );
    (visible, init)
}

/// A panel a configurator choice decides, and the repeatables in it.
pub struct ResetTarget {
    pub panel: String,
    pub repeats: Vec<String>,
}

const CONFIGURATOR_RESET: &str = "configurator-reset-on-change";

/// A configurator choice's value-commit rule: every panel it decides is
/// emptied, its repeatables first, so no option shows another's data.
pub fn configurator_reset(choice: &str, targets: &[ResetTarget]) -> EventScript {
    let mut content = generated(CONFIGURATOR_RESET);
    for target in targets {
        for repeat in &target.repeats {
            content.push_str(&format!("\nwindow.forms.ubs.resetAllPanelInstances({repeat});"));
        }
        content.push_str(&format!("\n{}.resetData();", target.panel));
    }
    EventScript {
        archetype: Some(CONFIGURATOR_RESET.into()),
        ..EventScript::event(choice, ScriptEvent::ValueCommit, content)
    }
}

const LOCALIZED_VALUE: &str = "localized-value";
/// How [`localized_value`] opens the texts it picks from, on a line of its own.
const TEXTS: &str = "  var texts = ";

/// A field's Initialize rule giving it the text of the reader's language:
/// `texts` by every code the form's metadata may report the language as, and
/// `fallback` for any other. It fills only an empty field, so a value entered
/// or restored from a draft stays.
///
/// A field's value is not translated by AEM's dictionary (only a static
/// text's is), so this is how a prefilled value reads in the form's language.
pub fn localized_value(
    field: &str,
    texts: &std::collections::BTreeMap<String, String>,
    fallback: &str,
) -> EventScript {
    let texts = serde_json::to_string(texts).expect("a map of strings serialises");
    let content = format!(
        "{}\nif (!this.value) {{\n{TEXTS}{texts};\n  this.value = texts[window.forms.ubs.getFormMetadata().language] || {};\n}}",
        generated(LOCALIZED_VALUE),
        js_string(fallback)
    );
    EventScript {
        archetype: Some(LOCALIZED_VALUE.into()),
        ..EventScript::event(field, ScriptEvent::Initialize, content)
    }
}

/// The texts a [`localized_value`] rule picks from, by language code; `None`
/// for any other rule.
pub fn localized_values(content: &str) -> Option<std::collections::BTreeMap<String, String>> {
    if !content.starts_with(&generated(LOCALIZED_VALUE)) {
        return None;
    }
    let line = content.lines().find_map(|l| l.strip_prefix(TEXTS))?;
    serde_json::from_str(line.strip_suffix(';')?).ok()
}

pub const LETTER_SUBJECT: &str = "letter-subject";
/// The field a letter's subject is printed from.
pub const SUBJECT_FIELD: &str = "AF_Subject";

/// The rule that writes a letter's subject: the reader's language's text
/// (`texts` by every code the form's metadata may report the language as,
/// `fallback` for any other) with each `{NAME}` replaced by the current value
/// of the field `NAME` (nothing while it is empty), and `{{` / `}}` written as
/// `{` / `}`. The same rule runs on the subject's Initialize and on the value
/// commit (and Initialize) of every field it names, so the subject follows
/// them. `references` are the names the texts use.
pub fn letter_subject(
    texts: &std::collections::BTreeMap<String, String>,
    fallback: &str,
    references: &[String],
) -> String {
    let texts = serde_json::to_string(texts).expect("a map of strings serialises");
    format!(
        "{}\n{TEXTS_TOP}{texts};\n{}\n\
         var text = texts[window.forms.ubs.getFormMetadata().language] || {};\n\
         {SUBJECT_FIELD}.value = text.replace({FILL_PATTERN}, {FILL_FUNCTION});",
        generated(LETTER_SUBJECT),
        values_line(references),
        js_string(fallback)
    )
}

/// What a `{NAME}` template's rule ([`letter_subject`], [`calculated_value`])
/// matches: `{{`, `}}` and each `{NAME}`.
const FILL_PATTERN: &str = r"/\{\{|\}\}|\{([A-Za-z_][A-Za-z0-9_]*)\}/g";
/// What it puts in their place: a brace for `{{` and `}}`, and the current
/// value of the field `NAME` (from `values`, see [`values_line`]), nothing
/// while it is empty.
const FILL_FUNCTION: &str = "function (all, name) {\n  if (!name) { return all.charAt(0); }\n  var value = values[name];\n  return value === null || value === undefined ? \"\" : String(value);\n}";

/// The line reading the current value of each field a template names.
fn values_line(references: &[String]) -> String {
    let values = references
        .iter()
        .map(|name| format!("{}: {name}.value", js_string(name)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("var values = {{{values}}};")
}

/// How [`letter_subject`] opens its texts, on a line of its own.
const TEXTS_TOP: &str = "var texts = ";

/// The texts a [`letter_subject`] rule writes from, by language code; `None`
/// for any other rule.
pub fn letter_subject_values(content: &str) -> Option<std::collections::BTreeMap<String, String>> {
    if !content.starts_with(&generated(LETTER_SUBJECT)) {
        return None;
    }
    let line = content.lines().find_map(|l| l.strip_prefix(TEXTS_TOP))?;
    serde_json::from_str(line.strip_suffix(';')?).ok()
}

/// A German form's banking relationship (feedback #104): `0319` and
/// disabled. Italy and every other entity share the same fragment but keep
/// what they have, so the writer gates this on the entity, not the fragment.
pub const BANKING_DEFAULT_DE_INIT: &str =
    "TXT_BankingRelationship1.value = '0319';\nTXT_BankingRelationship1.enabled = false;";

/// The initialize rule a fragment carries: its `statements`
/// ([`super::fragment_init::statements`]), after the German banking
/// relationship's default where it is one; `None` when it has nothing to do.
/// AEM's Expression Editor hands back exactly ONE SCRIPTMODEL per field and
/// event: two sharing `this` and `Initialize` are joined by a bare comma into
/// invalid JSON, and the editor then fails the moment it loads the field,
/// taking the page's editor data with it. So this is one rule, however many
/// statements it carries. `guide_path` is the fragment's own; the banking
/// default names its field as written.
pub fn fragment_init(
    frag_ref: &str,
    guide_path: &str,
    german_entity: bool,
    statements: &[String],
) -> Option<EventScript> {
    let banking = frag_ref.ends_with("affrg_BankingRelationship1") && german_entity;
    if banking {
        let mut rule = banking_default_de();
        for statement in statements {
            rule.content.push('\n');
            rule.content.push_str(statement);
        }
        Some(rule)
    } else if statements.is_empty() {
        None
    } else {
        Some(EventScript::event(guide_path, ScriptEvent::Initialize, statements.join("\n")))
    }
}

const COPIED_VALUE: &str = "copied-value";

/// An input's value-commit rule copying its value into each of `targets`, the
/// inputs that take their value from it (`InputProps::copy_from`). A target
/// stays editable; a target that already holds the value is not written
/// again, so two inputs copying each other settle on the one value.
pub fn copied_value(field: &str, targets: &[String]) -> EventScript {
    let mut content = generated(COPIED_VALUE);
    for target in targets {
        content.push_str(&format!(
            "\nif ({target}.value !== this.value) {{\n  {target}.value = this.value;\n}}"
        ));
    }
    EventScript {
        archetype: Some(COPIED_VALUE.into()),
        ..EventScript::event(field, ScriptEvent::ValueCommit, content)
    }
}

const CONDITIONAL_ACCESS: &str = "conditional-access";

/// One component a choice locks or unlocks: its name, the choice's values
/// that decide it, and whether those values enable it (`true`) or disable it.
pub struct AccessTarget {
    pub target: String,
    pub values: Vec<String>,
    pub enable: bool,
}

/// A choice's rules locking and unlocking `targets` (`InputProps::enable_conditions`),
/// the shape the issued forms write by hand: on Value Commit each target's
/// `enabled` follows the choice, and a target it disables is emptied
/// (`resetData`); on Initialize the targets take their opening state without
/// being emptied, so a restored draft keeps its values. A checkbox list holds
/// its ticked values comma-separated, so each value is looked for in that list
/// (`checkbox`); a radio button group or a drop-down holds one value.
pub fn conditional_access(field: &str, targets: &[AccessTarget], checkbox: bool) -> [EventScript; 2] {
    let test = |value: &str| {
        if checkbox {
            format!("String(this.value).split(\",\").indexOf({}) !== -1", js_string(value))
        } else {
            format!("this.value == {}", js_string(value))
        }
    };
    let body = |reset: bool| {
        let mut content = generated(CONDITIONAL_ACCESS);
        for target in targets {
            let matched = target.values.iter().map(|v| test(v)).collect::<Vec<_>>().join(" || ");
            let state = if target.enable {
                format!("this.value !== null && ({matched})")
            } else {
                format!("!(this.value !== null && ({matched}))")
            };
            let name = &target.target;
            content.push_str(&format!("\n{name}.enabled = {state};"));
            if reset {
                content.push_str(&format!("\nif (!{name}.enabled) {{\n  {name}.resetData();\n}}"));
            }
        }
        content
    };
    [ScriptEvent::ValueCommit, ScriptEvent::Initialize].map(|event| EventScript {
        archetype: Some(CONDITIONAL_ACCESS.into()),
        ..EventScript::event(field, event, body(event == ScriptEvent::ValueCommit))
    })
}

const CALCULATED_VALUE: &str = "calculated-value";

/// An input's Calculate rule (`InputProps::calculate`): it always holds
/// `template` with each `{NAME}` replaced by the current value of the input
/// `NAME` (nothing while it is empty) and `{{` / `}}` written as `{` / `}`,
/// the same filling a letter's subject does. `references` are the names the
/// template uses.
pub fn calculated_value(field: &str, template: &str, references: &[String]) -> EventScript {
    let content = format!(
        "{}\n{}\nthis.value = {}.replace({FILL_PATTERN}, {FILL_FUNCTION});",
        generated(CALCULATED_VALUE),
        values_line(references),
        js_string(template)
    );
    EventScript {
        archetype: Some(CALCULATED_VALUE.into()),
        ..EventScript::event(field, ScriptEvent::Calculate, content)
    }
}

const LANGUAGE_GATE: &str = "language-gate";

/// A node's Initialize rule showing it, on screen and in the DoR, only when
/// the form is read in one of `languages` (`AemAttrs::only_languages`), and
/// hiding it otherwise: the shape UBS writes for content only some language
/// versions carry. The language is lower-cased and each code tested with
/// `indexOf`, so a regional code (`de-ch`) matches its language; Spanish is
/// tested as both `es` and the platform's `sp`.
pub fn language_gate(field: &str, languages: &[String]) -> EventScript {
    let mut codes: Vec<String> = Vec::new();
    for language in languages {
        let code = language.to_lowercase();
        let spanish = code == "es" || code == "sp";
        for code in if spanish { vec!["es".to_owned(), "sp".to_owned()] } else { vec![code] } {
            if !codes.contains(&code) {
                codes.push(code);
            }
        }
    }
    let test = codes
        .iter()
        .map(|code| format!("language.indexOf({}) !== -1", js_string(code)))
        .collect::<Vec<_>>()
        .join(" || ");
    let content = format!(
        "{}\nvar language = window.forms.ubs.getFormMetadata().language.toLowerCase();\n\
         if ({test}) {{\n  window.forms.ubs.showAFShowDor(this);\n}} else {{\n  window.forms.ubs.hideAFHideDor(this);\n}}",
        generated(LANGUAGE_GATE)
    );
    EventScript {
        archetype: Some(LANGUAGE_GATE.into()),
        ..EventScript::event(field, ScriptEvent::Initialize, content)
    }
}

const SIGNATURE_NAME_FILL: &str = "signature-name-fill";

/// Where a signature's name is filled from: the rows of a party and of its
/// signature, each as a script reaches its instances.
pub struct NameFill {
    /// The party's instance-managed panel (`RCP_CPGRP_repeat`).
    pub data_repeat: String,
    /// The path from one of its instances to the party's name fields
    /// (`RCP_CPGRP_inner.PN_CPGRP.PN_IndividualBasic.PN_Name_Individual`).
    pub data_path: String,
    /// The signature's instance-managed panel (`RCP_SGN_CPGRP_repeat`).
    pub sign_repeat: String,
    /// The path from one of its instances to the signer's name field
    /// (`RCP_SGN_CPGRP_inner.PN_SGN_CPGRP.PN_GenericSignature.TXT_Name_Generic`).
    pub sign_path: String,
}

/// The rules that fill each signature's name from its party's first and last
/// name (UBS-SIG-4), on the hidden field `anchor`: UBS's own loop (BAVP_033,
/// feedback PROBLEM-signature-name-fill) on Calculate, which follows a name as
/// it changes, and on Initialize, for a form opened with data already in it
/// (a draft, a prefill), where it fills only an empty name. The name stays
/// editable.
pub fn signature_name_fill(anchor: &str, fill: &NameFill) -> [EventScript; 2] {
    let body = |only_empty: bool| {
        // Both write the field as `<panel>.instanceManager.instances[i].<path>.value = ...`, the
        // shape the feedback repository's guard reads a fill by, on either event.
        let write = if only_empty {
            "  if (!{SIGN}.value) {\n    {SIGN}.value = name;\n  }"
        } else {
            "  {SIGN}.value = name;"
        };
        format!(
            "{}\nvar len = {data}.instanceManager.instances.length;\n\nfor (var i = 0; i < len; i++) {{\n\
             \x20 var pnIndividual = {data}.instanceManager.instances[i].{data_path};\n\
             \x20 var lastName = pnIndividual.TXT_LastName.value;\n\
             \x20 var firstName = pnIndividual.TXT_FirstName.value;\n\
             \x20 var name = ((firstName ? firstName : \"\") + \" \" + (lastName ? lastName : \"\")).trim();\n\
             {write}\n}}",
            generated(SIGNATURE_NAME_FILL),
            data = fill.data_repeat,
            data_path = fill.data_path,
            write = write.replace(
                "{SIGN}",
                &format!("{}.instanceManager.instances[i].{}", fill.sign_repeat, fill.sign_path)
            ),
        )
    };
    // Spelled as the feedback repository's fixer writes it, without an
    // `_archetype`: its marker opens the content.
    let rule = |event, only_empty| EventScript::event(anchor, event, body(only_empty));
    [rule(ScriptEvent::Calculate, false), rule(ScriptEvent::Initialize, true)]
}

/// The German banking relationship's initialize rule, on whichever node
/// holds the fragment.
pub fn banking_default_de() -> EventScript {
    EventScript::event("this", ScriptEvent::Initialize, BANKING_DEFAULT_DE_INIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Remove button's click rule, as the deployed forms carry it: one
    /// list item, content first, the subject a JavaScript string.
    #[test]
    fn a_remove_click_rule_is_the_archetypes_own() {
        let [click, ..] = Repeating { panel: "RCP_X_repeat", twin: None, subject: "Client" }.remove();
        assert_eq!(
            attribute_value(&[click]),
            r#"[{"script":{"content":"// [repeating-panel] Generated automatically. Do not edit: will be overwritten. Create your own different script.\\nvar repeatingPanel = this.parent;\\nvar addButton = this.parent.parent.BT_Add;\\nwindow.forms.ubs.removeInstance(repeatingPanel);\\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabels(repeatingPanel\, \\"Client\\"\, addButton);\\nwindow.forms.ubs.accessibility.setRepeatPanelAccessibilityLabelsForButtons(repeatingPanel\, \\"Client\\"\, addButton\, this);"\,"event":"Click"\,"field":"BT_Remove"}\,"nodeName":"SCRIPTMODEL"\,"version":1\,"enabled":true\,"_archetype":"repeating-panel"}]"#
        );
    }

    /// The texts a localized value picks from come back out of the rule, and
    /// a text holding a newline or a quote stays on its one line.
    #[test]
    fn a_localized_value_reads_back() {
        let texts: std::collections::BTreeMap<String, String> = [
            ("de".to_owned(), "Betreff: \"A\"\nB".to_owned()),
            ("it".to_owned(), "Oggetto".to_owned()),
        ]
        .into_iter()
        .collect();
        let rule = localized_value("AF_Subject", &texts, "Oggetto");
        assert_eq!(localized_values(&rule.content), Some(texts));
        assert!(rule.content.contains("window.forms.ubs.getFormMetadata().language"));
        assert_eq!(localized_values("this.value = 1;"), None);
    }

    /// The subject a letter-subject rule writes, run as the form runs it, with
    /// `values` as the named fields' current values.
    fn run_subject(language: &str, values: &[(&str, Option<&str>)]) -> String {
        let texts = [
            ("it".to_owned(), "Notifica {{art}}\nPolizza n.: {TXT_PolizzaN}\nContraente: {TXT_Contraente}".to_owned()),
            ("en".to_owned(), "Notice\nPolicy no.: {TXT_PolizzaN}".to_owned()),
        ]
        .into_iter()
        .collect();
        let names: Vec<String> = values.iter().map(|(n, _)| (*n).to_owned()).collect();
        let rule = letter_subject(&texts, "Notifica", &names);
        let mut source = format!(
            "var window = {{forms: {{ubs: {{getFormMetadata: function () {{ return {{language: {}}}; }}}}}}}};\n\
             var {SUBJECT_FIELD} = {{value: null}};\n",
            js_string(language)
        );
        for (name, value) in values {
            let value = value.map_or("null".to_owned(), js_string);
            source.push_str(&format!("var {name} = {{value: {value}}};\n"));
        }
        source.push_str(&format!("(function () {{\n{rule}\n}})();\n{SUBJECT_FIELD}.value;"));
        let mut context = boa_engine::Context::default();
        let result = context
            .eval(boa_engine::Source::from_bytes(source.as_bytes()))
            .unwrap_or_else(|e| panic!("the rule runs: {e}\n{source}"));
        result.to_string(&mut context).unwrap().to_std_string_escaped()
    }

    /// The subject shows the named fields' values, nothing for an empty one,
    /// a brace for a doubled one, and reads back to the texts it was given.
    #[test]
    fn a_letter_subject_shows_the_fields_it_names() {
        let values = [("TXT_PolizzaN", Some("P-123")), ("TXT_Contraente", None)];
        assert_eq!(run_subject("it", &values), "Notifica {art}\nPolizza n.: P-123\nContraente: ");
        assert_eq!(run_subject("en", &values), "Notice\nPolicy no.: P-123");
        assert_eq!(run_subject("fr", &values), "Notifica");
        let texts = [("it".to_owned(), "A {X}".to_owned())].into_iter().collect();
        assert_eq!(letter_subject_values(&letter_subject(&texts, "A", &["X".into()])), Some(texts));
    }

    /// What a localized value leaves in its field, run as the form runs it.
    fn run_localized(language: &str, value: &str) -> String {
        let texts = [("de".to_owned(), "Betreff: \"A\"\nB".to_owned()), ("it".to_owned(), "Oggetto".to_owned())]
            .into_iter()
            .collect();
        let rule = localized_value("AF_Subject", &texts, "Oggetto");
        let source = format!(
            "var window = {{forms: {{ubs: {{getFormMetadata: function () {{ return {{language: {}}}; }}}}}}}};\n\
             var field = {{value: {}}};\n(function () {{\n{}\n}}).call(field);\nfield.value;",
            js_string(language),
            js_string(value),
            rule.content
        );
        let mut context = boa_engine::Context::default();
        let result = context
            .eval(boa_engine::Source::from_bytes(source.as_bytes()))
            .unwrap_or_else(|e| panic!("the rule runs: {e}\n{source}"));
        result.to_string(&mut context).unwrap().to_std_string_escaped()
    }

    /// The rule fills an empty field with its language's text, or the
    /// fallback's for a language it has none for, and leaves a filled field.
    #[test]
    fn a_localized_value_picks_the_readers_language() {
        assert_eq!(run_localized("de", ""), "Betreff: \"A\"\nB");
        assert_eq!(run_localized("fr", ""), "Oggetto");
        assert_eq!(run_localized("de", "Prot. 42"), "Prot. 42");
    }

    #[test]
    fn a_show_rule_compares_with_the_value_as_a_string() {
        let (visible, _) = show("PN_A", &[("RB_X".into(), "a\"b".into()), ("CB_Y".into(), "true".into())]);
        assert!(visible.content.starts_with(r#"if (RB_X.value == "a\"b" || CB_Y.value == "true") {"#));
        assert_eq!(visible.model.as_deref(), Some("SHOW_EXPRESSION"));
    }
}
