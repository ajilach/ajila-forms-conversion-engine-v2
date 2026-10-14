//! The UBS letterhead: the letter top of a form whose source is a letter
//! (sender, receiver window, date and references, subject).
//!
//! A form holding one is a letter to the UBS DoR as well: the summary script
//! (`summary-backend-creator.js`) builds the receiver window, the date and
//! references block and the bold subject from the letterhead's fields, and puts
//! the sender into the DoR header slot of `AF_Sender_Info_For_Header_Slot`,
//! once the form's metadata component carries `redactoLayout="letterDor"`. The
//! writer derives that flag, and the letter DoR template, from the node's
//! presence alone (`lower.rs`).
//!
//! The component itself is the profile's (`profiles/ubs/aem/letterhead.xml`,
//! AAQR_033's letterhead with every node kept out of the summary and the DoR).
//! What a document decides about it is what this node holds: who the letter is
//! from and to, its subject and its reference lines. [`render`] writes those
//! into the profile's component, [`parse`] reads them back from a package.

use std::collections::BTreeMap;

use u2s_aem::model::RawJcrNode;
use u2s_mapper_aem::jcr::tree::JcrNode;
use u2s_mapper_aem::script::{EventScript, ScriptEvent, decode_script_list};
use uuid::Uuid;

use super::{AemConfig, AemI18nText, AemNode};

/// Who a letter is from or to.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind")]
pub enum LetterParty {
    /// The source prints this address: the bank's own as the sender, or UBS as
    /// the receiver of a letter the client writes. It is prefilled and hidden on
    /// screen, and the DoR prints it.
    Fixed {
        address: Box<LetterAddress>,
        /// The receiver's opening line, per language, plain text: what the
        /// source prints above the receiver's name ("Spettabile", "An"). The
        /// letter prints it as the window's first line. A receiver's only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opening: Option<AemI18nText>,
    },
    /// The person filling in the form enters this address: the fields are shown,
    /// empty.
    Entered {
        /// The receiver's opening line, as for a fixed address.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        opening: Option<AemI18nText>,
    },
}

impl LetterParty {
    /// An entered address with no opening line.
    pub fn entered() -> Self {
        LetterParty::Entered { opening: None }
    }

    /// The opening line printed above the address, if any.
    pub fn opening(&self) -> Option<&AemI18nText> {
        match self {
            LetterParty::Fixed { opening, .. } | LetterParty::Entered { opening } => {
                opening.as_ref()
            }
        }
    }

    fn with_opening(self, text: Option<AemI18nText>) -> Self {
        match self {
            LetterParty::Fixed { address, .. } => LetterParty::Fixed {
                address,
                opening: text,
            },
            LetterParty::Entered { .. } => LetterParty::Entered { opening: text },
        }
    }
}

/// A letter's address, as the letter window prints it. Either a company (with
/// `company`, optionally in bold) or a person (`title`, `first_name`,
/// `last_name`), never both: the DoR prints the one or the other.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct LetterAddress {
    /// The company or entity name. Setting it makes this a company address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    /// A person's title ("Dr."), printed before the name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub street_no: Option<String>,
    /// An additional address line, printed after the street.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postal_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    /// Canton, state or province.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// The country as the form's country list values it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Print the company name in bold. Only for a company address.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub bold_company: bool,
}

/// The letterhead fields an address fills, by the part of their name after
/// `AF_Sender_` / `AF_Receiver_`, in the order the letter window prints them.
const ADDRESS_FIELDS: [&str; 11] = [
    "CompanyName",
    "Title",
    "FirstName",
    "LastName",
    "Street",
    "Street_No",
    "Additional_Details",
    "PostalCode",
    "City",
    "State",
    "Country",
];

impl LetterAddress {
    /// Each field of [`ADDRESS_FIELDS`] with what this address puts in it.
    fn fields(&self) -> [(&'static str, &Option<String>); 11] {
        [
            (ADDRESS_FIELDS[0], &self.company),
            (ADDRESS_FIELDS[1], &self.title),
            (ADDRESS_FIELDS[2], &self.first_name),
            (ADDRESS_FIELDS[3], &self.last_name),
            (ADDRESS_FIELDS[4], &self.street),
            (ADDRESS_FIELDS[5], &self.street_no),
            (ADDRESS_FIELDS[6], &self.additional),
            (ADDRESS_FIELDS[7], &self.postal_code),
            (ADDRESS_FIELDS[8], &self.city),
            (ADDRESS_FIELDS[9], &self.state),
            (ADDRESS_FIELDS[10], &self.country),
        ]
    }

    /// The address with the field named `suffix` set to `value`.
    fn with(mut self, suffix: &str, value: String) -> Self {
        let slot = match suffix {
            "CompanyName" => &mut self.company,
            "Title" => &mut self.title,
            "FirstName" => &mut self.first_name,
            "LastName" => &mut self.last_name,
            "Street" => &mut self.street,
            "Street_No" => &mut self.street_no,
            "Additional_Details" => &mut self.additional,
            "PostalCode" => &mut self.postal_code,
            "City" => &mut self.city,
            "State" => &mut self.state,
            "Country" => &mut self.country,
            other => unreachable!("`{other}` is no address field"),
        };
        *slot = Some(value);
        self
    }

    /// What makes this address one the letter window cannot print as it is.
    fn problems(&self, side: Side) -> Vec<String> {
        let mut problems = Vec::new();
        let who = side.label();
        for (field, value) in self.fields() {
            if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
                problems.push(format!(
                    "the {who} address's `{}` is empty: leave it out instead",
                    json_key(field)
                ));
            }
        }
        if self.fields().iter().all(|(_, v)| v.is_none()) {
            problems.push(format!(
                "the {who} address is `Fixed` and holds nothing: give it its lines, or make it `Entered`"
            ));
        }
        let person = self.title.is_some() || self.first_name.is_some() || self.last_name.is_some();
        if self.company.is_some() && person {
            problems.push(format!(
                "the {who} address names a company and a person, and the letter window prints only one \
                 of them: keep `company`, or `title`/`first_name`/`last_name`"
            ));
        }
        if self.bold_company && self.company.is_none() {
            problems.push(format!(
                "the {who} address sets `bold_company` without a `company`"
            ));
        }
        problems
    }
}

/// The document's key for an address field.
fn json_key(field: &str) -> &'static str {
    match field {
        "CompanyName" => "company",
        "Title" => "title",
        "FirstName" => "first_name",
        "LastName" => "last_name",
        "Street" => "street",
        "Street_No" => "street_no",
        "Additional_Details" => "additional",
        "PostalCode" => "postal_code",
        "City" => "city",
        "State" => "state",
        "Country" => "country",
        other => unreachable!("`{other}` is no address field"),
    }
}

/// The two sides of a letter, as the letterhead names their fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Sender,
    Receiver,
}

impl Side {
    /// The part of every field name that says which side it belongs to.
    fn stem(self) -> &'static str {
        match self {
            Side::Sender => "Sender",
            Side::Receiver => "Receiver",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Side::Sender => "sender",
            Side::Receiver => "receiver",
        }
    }

    /// The side's heading draw.
    fn subtitle(self) -> String {
        format!("ST_Letter_{}_Subtitle", self.stem())
    }

    /// The side's address panel.
    fn panel(self) -> String {
        format!("PN_{}_Address", self.stem())
    }

    fn field(self, suffix: &str) -> String {
        format!("AF_{}_{suffix}", self.stem())
    }

    /// The panel of the person's names.
    fn names(self) -> String {
        format!("PN_{}_Names", self.stem())
    }

    /// The panel of the company name.
    fn company(self) -> String {
        format!("PN_{}_Company", self.stem())
    }

    /// The person/company choice, which the script reads as `AF_<side>_ClientType`.
    fn client_type(self) -> String {
        format!("RB_{}_ClientType", self.stem())
    }

    fn bold_company(self) -> String {
        format!("CB_{}_Bold_CompanyName", self.stem())
    }
}

/// The field the summary script puts the sender into the DoR header with.
const SENDER_INFO: &str = "AF_Sender_Info_For_Header_Slot";
/// The DoR header slot the sender is printed in.
pub const SENDER_SLOT: &str = "slot2";
const SUBJECT: &str = "AF_Subject";
/// The receiver's opening line, which the summary script prints above the
/// receiver's name.
const OPENING: &str = "AF_Receiver_Opening";
const YOUR_REFERENCE: &str = "TXT_Your_Ref";
const OUR_REFERENCE: &str = "TXT_Our_Ref";
/// What every `bindRef` of the profile's letterhead opens with: the path of the
/// `Letter` element the schema walk binds the letterhead to.
const LETTER_PATH: &str = "{letter}";

/// `radiobutton` values of the person/company choice.
const PERSON: &str = "0";
const COMPANY: &str = "1";
/// A checked company-name format checkbox.
const BOLD: &str = "1";

/// Whether the tree holds a letterhead.
pub fn holds_letterhead(node: &AemNode) -> bool {
    match node {
        AemNode::Letterhead { .. } => true,
        AemNode::Root { children, .. }
        | AemNode::Panel { children, .. }
        | AemNode::Repeatable { children, .. } => children.iter().any(holds_letterhead),
        _ => false,
    }
}

/// What a letterhead's own content makes impossible to write, whatever the
/// form around it: the addresses and the texts.
/// The fields a subject text names, in the order it names them first: each
/// `{NAME}`, where `NAME` is a component name (a letter or `_`, then letters,
/// digits and `_`). `{{` and `}}` write a brace; any other brace is refused,
/// so a reference cannot be mistyped into text.
pub fn subject_references(text: &str) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(['{', '}']) {
        let (brace, after) = (&rest[at..at + 1], &rest[at + 1..]);
        if after.starts_with(brace) {
            rest = &after[1..];
            continue;
        }
        if brace == "}" {
            return Err(format!("a lone `}}` in {text:?}: write `}}}}` for a brace"));
        }
        let end = after
            .find('}')
            .ok_or_else(|| format!("an unclosed `{{` in {text:?}: write `{{{{` for a brace"))?;
        let name = &after[..end];
        let valid = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!(
                "`{{{name}}}` in {text:?} names no field: a reference is `{{FIELD_NAME}}`, \
                 and `{{{{` writes a brace"
            ));
        }
        if !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
        rest = &after[end + 1..];
    }
    Ok(names)
}

/// The fields a letterhead's subject names, over all its languages.
pub fn subject_fields(subject: &AemI18nText) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = Vec::new();
    for text in subject.0.values() {
        for name in subject_references(text)? {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    Ok(names)
}

/// The rule writing the subject (`scripts::letter_subject`) and the fields
/// it names.
pub(super) fn subject_rule(
    subject: &AemI18nText,
    config: &AemConfig,
) -> Result<(String, Vec<String>), String> {
    let fields = subject_fields(subject)?;
    let rule = super::scripts::letter_subject(
        &texts_by_code(subject, config),
        &fallback(subject, config),
        &fields,
    );
    Ok((rule, fields))
}

/// The subject rule of the tree's letterhead and the fields it names, when
/// the tree holds one: what each of those fields runs on its value commit.
pub(super) fn subject_triggers(
    root: &AemNode,
    config: &AemConfig,
) -> Result<Option<(String, Vec<String>)>, String> {
    fn find_letterhead(node: &AemNode) -> Option<&AemNode> {
        match node {
            AemNode::Letterhead { .. } => Some(node),
            AemNode::Root { children, .. }
            | AemNode::Panel { children, .. }
            | AemNode::Repeatable { children, .. } => children.iter().find_map(find_letterhead),
            _ => None,
        }
    }
    match find_letterhead(root) {
        Some(AemNode::Letterhead { subject, .. }) => subject_rule(subject, config).map(Some),
        _ => Ok(None),
    }
}

pub fn problems(
    sender: Option<&LetterParty>,
    receiver: &LetterParty,
    subject: &AemI18nText,
    references: [Option<&AemI18nText>; 2],
) -> Vec<String> {
    let mut problems = Vec::new();
    for (side, party) in [(Side::Sender, sender), (Side::Receiver, Some(receiver))] {
        if let Some(LetterParty::Fixed { address, .. }) = party {
            problems.extend(address.problems(side));
        }
    }
    if sender.and_then(LetterParty::opening).is_some() {
        problems.push(
            "the letterhead's sender has an `opening`, and only the receiver window prints one: \
             move it to the `receiver`"
                .into(),
        );
    }
    if receiver
        .opening()
        .is_some_and(|o| o.0.values().all(|v| v.trim().is_empty()))
    {
        problems.push("the receiver's `opening` is empty: leave it out instead".into());
    }
    if let Err(problem) = subject_fields(subject) {
        problems.push(format!("the letterhead's `subject` {problem}"));
    }
    if subject.0.values().all(|s| s.trim().is_empty()) {
        problems.push("the letterhead's `subject` is empty".into());
    }
    let texts = std::iter::once(("subject", Some(subject))).chain([
        ("your_reference", references[0]),
        ("our_reference", references[1]),
        ("receiver`'s `opening", receiver.opening()),
    ]);
    for (key, text) in texts {
        for (lang, value) in text.into_iter().flat_map(|t| t.0.iter()) {
            if is_markup(value) {
                problems.push(format!(
                    "the letterhead's `{key}` in {lang} holds markup, and the letter prints it as plain \
                     text: write it without tags"
                ));
            }
        }
    }
    problems
}

/// Whether `text` holds an HTML tag.
fn is_markup(text: &str) -> bool {
    text.split('<')
        .skip(1)
        .any(|rest| rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '!'))
}

// ------------------------------------------------------------------ write --

/// The profile's letterhead component (`template`, the parsed
/// `letterhead.xml`), written for `node`.
pub(super) fn render(
    template: RawJcrNode,
    node: &AemNode,
    config: &AemConfig,
) -> Result<RawJcrNode, String> {
    let AemNode::Letterhead {
        name,
        sender,
        receiver,
        subject,
        your_reference,
        our_reference,
        bind_ref,
        ..
    } = node
    else {
        return Err("not a letterhead".into());
    };
    let mut letterhead = template
        .children
        .into_iter()
        .find(|c| c.tag_name == "letterhead")
        .ok_or("the profile's letterhead template holds no `letterhead` element")?;
    letterhead.attributes.insert("name".into(), name.clone());
    bind(&mut letterhead, bind_ref.as_deref())?;

    match sender {
        None => hide_side(&mut letterhead, Side::Sender)?,
        Some(party) => {
            write_party(&mut letterhead, Side::Sender, party)?;
            let info = named(&mut letterhead, SENDER_INFO)?;
            info.attributes
                .insert("dorHeaderSlot".into(), SENDER_SLOT.into());
            info.attributes
                .insert("hideLabelAndBorder".into(), "true".into());
        }
    }
    write_party(&mut letterhead, Side::Receiver, receiver)?;
    if let Some(opening) = receiver.opening() {
        add_rule(named(&mut letterhead, OPENING)?, OPENING, opening, config)?;
    }

    let (rule, _) = subject_rule(subject, config)?;
    let rule = EventScript::event(SUBJECT, ScriptEvent::Initialize, rule);
    add_rules(
        named(&mut letterhead, SUBJECT)?,
        SUBJECT,
        ScriptEvent::Initialize,
        vec![rule],
    )?;
    for (field, text) in [
        (YOUR_REFERENCE, your_reference),
        (OUR_REFERENCE, our_reference),
    ] {
        if let Some(text) = text {
            let reference = named(&mut letterhead, field)?;
            reference.attributes.remove("visible");
            if !text.0.values().all(|t| t.trim().is_empty()) {
                add_rule(reference, field, text, config)?;
            }
        }
    }
    mirror_value_commits(&mut letterhead)?;
    Ok(letterhead)
}

/// Every value-commit rule of the letterhead run on Initialize too, for a form
/// that opens with the value already in it (a draft, a prefill): the date
/// string and the references block are otherwise empty in the DoR until the
/// person touches the date or a reference (PROBLEM-visible-on-init). A rule
/// that destroys data (a reset) is not repeated on load, and one whose text an
/// Initialize rule of the field already holds is not repeated twice. The copy
/// runs only once the field has a value, as its value-commit rule does, so the
/// date logic never reads an empty date.
fn mirror_value_commits(node: &mut RawJcrNode) -> Result<(), String> {
    const DESTRUCTIVE: &[&str] = &[
        "resetData(",
        "resetAllPanelInstances(",
        "addInstance(",
        "removeInstance(",
    ];
    let name = node.attributes.get("name").cloned();
    if let (Some(name), Some(at)) = (
        name,
        node.children
            .iter()
            .position(|c| c.tag_name == "fd:scripts"),
    ) {
        let scripts = &mut node.children[at];
        let contents = |attribute: &str| -> Result<Vec<String>, String> {
            let Some(raw) = scripts.attributes.get(attribute) else {
                return Ok(Vec::new());
            };
            let rules =
                decode_script_list(raw).map_err(|e| format!("`{name}`'s {attribute}: {e}"))?;
            Ok(rules
                .iter()
                .filter_map(|json| {
                    let rule: serde_json::Value = serde_json::from_str(json).ok()?;
                    Some(rule.pointer("/script/content")?.as_str()?.to_owned())
                })
                .collect())
        };
        let commits = contents(ScriptEvent::ValueCommit.attribute())?;
        let inits = contents(ScriptEvent::Initialize.attribute())?;
        let copies: Vec<String> = commits
            .into_iter()
            .filter(|body| !DESTRUCTIVE.iter().any(|d| body.contains(d)))
            .filter(|body| !inits.iter().any(|init| init.contains(body.as_str())))
            .map(|body| {
                EventScript::event(
                    name.as_str(),
                    ScriptEvent::Initialize,
                    format!("if (this.value) {{\n{body}\n}}"),
                )
                .to_json()
            })
            .collect();
        if !copies.is_empty() {
            let attribute = ScriptEvent::Initialize.attribute();
            let mut rules = match scripts.attributes.get(attribute) {
                Some(raw) => {
                    decode_script_list(raw).map_err(|e| format!("`{name}`'s {attribute}: {e}"))?
                }
                None => Vec::new(),
            };
            rules.extend(copies);
            scripts.attributes.insert(
                attribute.into(),
                u2s_mapper_aem::script::encode_script_list(rules),
            );
        }
    }
    node.children.iter_mut().try_for_each(mirror_value_commits)
}

/// Every `bindRef` under `node` bound below `letter`, the path the schema walk
/// gave the letterhead, or dropped when it has none (an unbound package).
fn bind(node: &mut RawJcrNode, letter: Option<&str>) -> Result<(), String> {
    if let Some(path) = node.attributes.get("bindRef") {
        let rest = path
            .strip_prefix(LETTER_PATH)
            .ok_or_else(|| {
                format!(
                    "the letterhead template binds `{path}`, which is not under `{LETTER_PATH}`"
                )
            })?
            .to_owned();
        match letter {
            Some(letter) => node
                .attributes
                .insert("bindRef".into(), format!("{letter}{rest}")),
            None => node.attributes.remove("bindRef"),
        };
    }
    node.children
        .iter_mut()
        .try_for_each(|child| bind(child, letter))
}

/// One side of the letter, as `party` has it.
fn write_party(letterhead: &mut RawJcrNode, side: Side, party: &LetterParty) -> Result<(), String> {
    let address = match party {
        LetterParty::Entered { .. } => {
            set(letterhead, &side.client_type(), "_value", PERSON)?;
            return client_type_rules(letterhead, side, true);
        }
        LetterParty::Fixed { address, .. } => address,
    };
    client_type_rules(letterhead, side, false)?;
    hide_side(letterhead, side)?;
    for (suffix, value) in address.fields() {
        if let Some(value) = value {
            set(letterhead, &side.field(suffix), "_value", value)?;
        }
    }
    let kind = if address.company.is_some() {
        COMPANY
    } else {
        PERSON
    };
    set(letterhead, &side.client_type(), "_value", kind)?;
    if address.bold_company {
        set(letterhead, &side.bold_company(), "_value", BOLD)?;
    }
    Ok(())
}

/// The side's heading and address hidden on screen: a fixed address, or a
/// letter without a sender.
fn hide_side(letterhead: &mut RawJcrNode, side: Side) -> Result<(), String> {
    for name in [side.subtitle(), side.panel()] {
        set(letterhead, &name, "visible", "{Boolean}false")?;
    }
    Ok(())
}

fn set(letterhead: &mut RawJcrNode, name: &str, key: &str, value: &str) -> Result<(), String> {
    named(letterhead, name)?
        .attributes
        .insert(key.into(), value.into());
    Ok(())
}

/// The one component of the letterhead named `name`.
fn named<'a>(letterhead: &'a mut RawJcrNode, name: &str) -> Result<&'a mut RawJcrNode, String> {
    fn count(node: &RawJcrNode, name: &str) -> usize {
        usize::from(node.attributes.get("name").is_some_and(|n| n == name))
            + node.children.iter().map(|c| count(c, name)).sum::<usize>()
    }
    fn find<'a>(node: &'a mut RawJcrNode, name: &str) -> Option<&'a mut RawJcrNode> {
        if node.attributes.get("name").is_some_and(|n| n == name) {
            return Some(node);
        }
        node.children.iter_mut().find_map(|c| find(c, name))
    }
    match count(letterhead, name) {
        1 => Ok(find(letterhead, name).expect("counted once")),
        n => Err(format!(
            "the letterhead template holds {n} components named `{name}`, not one"
        )),
    }
}

/// The Initialize rule that gives `field` the reader's language's `text`.
///
/// A field's value is no text AEM's dictionary translates (only static texts'
/// are), so the rule picks it by the form's language. It fills only an empty
/// field, so a value the reader entered or a saved draft brought back stays.
fn add_rule(
    field: &mut RawJcrNode,
    name: &str,
    text: &AemI18nText,
    config: &AemConfig,
) -> Result<(), String> {
    let rule = super::scripts::localized_value(
        name,
        &texts_by_code(text, config),
        &fallback(text, config),
    );
    add_rules(field, name, ScriptEvent::Initialize, vec![rule])
}

/// `rules` as `field`'s rules of `event`, in its `fd:scripts`, which the
/// template gives it none of.
fn add_rules(
    field: &mut RawJcrNode,
    name: &str,
    event: ScriptEvent,
    rules: Vec<EventScript>,
) -> Result<(), String> {
    let attribute = event.attribute();
    let value = super::scripts::attribute_value(&rules);
    let scripts = match field
        .children
        .iter()
        .position(|c| c.tag_name == "fd:scripts")
    {
        Some(at) => &mut field.children[at],
        None => {
            field.children.push(RawJcrNode {
                tag_name: "fd:scripts".into(),
                attributes: [("jcr:primaryType".to_owned(), "nt:unstructured".to_owned())]
                    .into_iter()
                    .collect(),
                children: Vec::new(),
            });
            field.children.last_mut().expect("just pushed")
        }
    };
    if scripts.attributes.contains_key(attribute) {
        return Err(format!(
            "the letterhead template's `{name}` already has a rule on `{attribute}`"
        ));
    }
    scripts.attributes.insert(attribute.into(), value);
    Ok(())
}

/// The person/company choice of `side`, and its rules.
///
/// It copies its value into `AF_<side>_ClientType`, which the summary script
/// reads. On an entered side it also shows the names or the company name, and
/// on a change resets that side's own two panels, so no option keeps the
/// other's data (PROBLEM-configurator-reset-on-change; the panels in the order
/// the feedback guard writes them, by name). It never resets the other side,
/// whose address may be fixed: the source prints a fixed address as it is. A
/// fixed side's choice is hidden and prefilled, so it gets no show rule and no
/// reset, and nothing can clear its values.
fn client_type_rules(letterhead: &mut RawJcrNode, side: Side, entered: bool) -> Result<(), String> {
    let choice = side.client_type();
    let copy = format!("AF_{}_ClientType.value = this.value;", side.stem());
    let (names, company) = (side.names(), side.company());
    let shown = format!(
        "if (this.value === \"{PERSON}\") {{\n  {names}.visible = true;\n  {company}.visible = false;\n}}\nelse {{\n  {names}.visible = false;\n  {company}.visible = true;\n}}\n\n{copy}"
    );
    let field = named(letterhead, &choice)?;
    if !entered {
        let init = EventScript::event(choice.as_str(), ScriptEvent::Initialize, copy);
        return add_rules(field, &choice, ScriptEvent::Initialize, vec![init]);
    }
    let rule = |event| EventScript::event(choice.as_str(), event, shown.clone());
    let targets = [company.clone(), names.clone()].map(|panel| super::scripts::ResetTarget {
        panel,
        repeats: Vec::new(),
    });
    let reset = super::scripts::configurator_reset(&choice, &targets);
    add_rules(
        field,
        &choice,
        ScriptEvent::Initialize,
        vec![rule(ScriptEvent::Initialize)],
    )?;
    add_rules(
        field,
        &choice,
        ScriptEvent::ValueCommit,
        vec![rule(ScriptEvent::ValueCommit), reset],
    )
}

/// `text` by every code the form's metadata may report a language as: each
/// language the form ships and its synonyms. A language `text` has no
/// translation for is left out, and reads the fallback.
fn texts_by_code(text: &AemI18nText, config: &AemConfig) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for lang in config.canonical_languages() {
        let family = config.language_family(&lang);
        let Some(value) = family
            .iter()
            .find_map(|l| text.get(l))
            .filter(|v| !v.trim().is_empty())
        else {
            continue;
        };
        for code in family {
            out.insert(code, value.to_owned());
        }
    }
    out
}

/// What a reader of a language `text` has no translation for reads: the form's
/// base language's text.
fn fallback(text: &AemI18nText, config: &AemConfig) -> String {
    let base = config.base_language();
    config
        .language_family(&base)
        .iter()
        .find_map(|l| text.get(l))
        .unwrap_or_else(|| text.master(&base))
        .to_owned()
}

// ------------------------------------------------------------------- read --

/// A package's letterhead (`node`, a `controls/letterhead` component) as the
/// node it was written for.
///
/// A text the package holds as a plain value names no language; it is filed
/// under the empty code, which the lift reads as the form's base language.
pub(super) fn parse(node: &JcrNode, uuid: Uuid) -> AemNode {
    let name = node.component_name().unwrap_or("PN_LetterHead").to_owned();
    let sender = read_party(node, Side::Sender);
    let opening = find(node, &|n| n.component_name() == Some(OPENING))
        .map(read_text)
        .filter(|t| t.0.values().any(|v| !v.trim().is_empty()));
    let receiver = read_party(node, Side::Receiver)
        .unwrap_or_else(LetterParty::entered)
        .with_opening(opening);
    let subject = find(node, &|n| n.component_name() == Some(SUBJECT))
        .map(read_text)
        .unwrap_or_default();
    let reference = |field: &str| {
        find(node, &|n| n.component_name() == Some(field))
            .filter(|n| visible(n))
            .map(read_text)
    };
    AemNode::Letterhead {
        uuid,
        name,
        sender,
        receiver,
        subject,
        your_reference: reference(YOUR_REFERENCE),
        our_reference: reference(OUR_REFERENCE),
        bind_ref: None,
    }
}

/// One side of a loaded letterhead: fixed where its fields carry values,
/// entered where it is shown (or, for the sender, printed in the DoR header)
/// with nothing in it, and for the sender absent otherwise.
fn read_party(letterhead: &JcrNode, side: Side) -> Option<LetterParty> {
    let value = |name: &str| {
        find(letterhead, &|n| n.component_name() == Some(name))
            .and_then(|n| n.attr("_value"))
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
    };
    // The choice decides; an older letterhead whose choice carries no value is
    // a company where only the company name is filled in.
    let person_given = ["Title", "FirstName", "LastName"]
        .iter()
        .any(|suffix| value(&side.field(suffix)).is_some());
    let company = match value(&side.client_type()) {
        Some(kind) => kind == COMPANY,
        None => !person_given && value(&side.field("CompanyName")).is_some(),
    };
    let address = ADDRESS_FIELDS
        .iter()
        .filter(|suffix| match **suffix {
            // The letter window prints the one the person/company choice picks.
            "CompanyName" => company,
            "Title" | "FirstName" | "LastName" => !company,
            _ => true,
        })
        .filter_map(|suffix| value(&side.field(suffix)).map(|v| (*suffix, v)))
        .fold(LetterAddress::default(), |address, (suffix, v)| {
            address.with(suffix, v)
        });
    if address != LetterAddress::default() {
        let bold_company = company && value(&side.bold_company()).as_deref() == Some(BOLD);
        return Some(LetterParty::Fixed {
            opening: None,
            address: Box::new(LetterAddress {
                bold_company,
                ..address
            }),
        });
    }
    // The address panel is `PN_<side>_Address` here and `CP_<side>_Address` in
    // older letterheads.
    let panel_suffix = format!("_{}_Address", side.stem());
    let shown = find(letterhead, &|n| {
        n.component_name()
            .is_some_and(|n| n.ends_with(&panel_suffix))
    })
    .is_some_and(visible);
    let in_header = side == Side::Sender
        && find(letterhead, &|n| n.component_name() == Some(SENDER_INFO))
            .is_some_and(|n| n.attr("dorHeaderSlot").is_some());
    match side {
        Side::Sender if !shown && !in_header => None,
        _ => Some(LetterParty::entered()),
    }
}

/// A letterhead text field's text: the one the writer's Initialize rule picks
/// by language, or the plain value an older letterhead carries.
fn read_text(field: &JcrNode) -> AemI18nText {
    let from_rule = field
        .child("fd:scripts")
        .and_then(|s| s.attr(ScriptEvent::Initialize.attribute()))
        .and_then(|raw| decode_script_list(raw).ok())
        .into_iter()
        .flatten()
        .find_map(|json| {
            let rule: serde_json::Value = serde_json::from_str(&json).ok()?;
            let content = rule.pointer("/script/content")?.as_str()?;
            super::scripts::localized_values(content)
                .or_else(|| super::scripts::letter_subject_values(content))
        });
    if let Some(texts) = from_rule {
        return AemI18nText(texts);
    }
    match field.attr("_value").map(crate::xsd::strip_markup) {
        Some(value) if !value.trim().is_empty() => AemI18nText::single("", value.trim()),
        _ => AemI18nText::default(),
    }
}

fn visible(node: &JcrNode) -> bool {
    !node.attr("visible").is_some_and(|v| v.contains("false"))
}

/// The first node under `node`, depth first, that `test` accepts.
fn find<'a>(node: &'a JcrNode, test: &dyn Fn(&JcrNode) -> bool) -> Option<&'a JcrNode> {
    if test(node) {
        return Some(node);
    }
    node.children.iter().find_map(|c| find(c, test))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subject_names_fields_in_braces_and_doubles_a_brace() {
        assert_eq!(
            subject_references("Polizza n.: {TXT_PolizzaN} {TXT_A} {TXT_PolizzaN} {{x}}").unwrap(),
            ["TXT_PolizzaN", "TXT_A"]
        );
        assert!(
            subject_references("{1abc}")
                .unwrap_err()
                .contains("names no field")
        );
        assert!(subject_references("{TXT a}").is_err());
        assert!(subject_references("a } b").unwrap_err().contains("lone"));
        assert!(
            subject_references("a { b")
                .unwrap_err()
                .contains("unclosed")
        );
    }

    #[test]
    fn markup_is_told_from_a_comparison() {
        assert!(is_markup("<b>Oggetto</b>"));
        assert!(is_markup("a</p>"));
        assert!(!is_markup("a < b and 3 <4"));
    }

    /// A letterhead side as an older package holds it: fields with values, and
    /// a person/company choice with `choice` as its value, if any.
    fn loaded_side(choice: Option<&str>, fields: &[(&str, &str)]) -> JcrNode {
        let mut xml = String::from("<letterhead name=\"panel_letterhead\">");
        xml.push_str(&match choice {
            Some(v) => format!("<radiobutton name=\"RB_Receiver_ClientType\" _value=\"{v}\"/>"),
            None => "<radiobutton name=\"RB_Receiver_ClientType\"/>".into(),
        });
        for (name, value) in fields {
            xml.push_str(&format!("<textbox name=\"{name}\" _value=\"{value}\"/>"));
        }
        xml.push_str("</letterhead>");
        u2s_mapper_aem::jcr::tree::parse_jcr_xml(&xml).unwrap()
    }

    /// Without a value on the choice, a side whose company name alone is filled
    /// in is a company; with person names it stays a person, and a choice that
    /// says person is believed.
    #[test]
    fn a_company_address_reads_back_without_a_choice_value() {
        let company =
            |choice, fields| match read_party(&loaded_side(choice, fields), Side::Receiver) {
                Some(LetterParty::Fixed { address, .. }) => address.company.clone(),
                other => panic!("a fixed address, got {other:?}"),
            };
        let ubs = [
            ("AF_Receiver_CompanyName", "UBS Europe SE"),
            ("AF_Receiver_City", "Milano"),
        ];
        assert_eq!(company(None, &ubs[..]), Some("UBS Europe SE".into()));
        let both = [
            ("AF_Receiver_CompanyName", "UBS Europe SE"),
            ("AF_Receiver_LastName", "Rossi"),
        ];
        assert_eq!(company(None, &both[..]), None);
        assert_eq!(company(Some(PERSON), &ubs[..]), None);
        assert_eq!(
            company(Some(COMPANY), &ubs[..]),
            Some("UBS Europe SE".into())
        );
    }

    #[test]
    fn a_company_with_a_person_is_refused() {
        let address = LetterAddress {
            company: Some("UBS".into()),
            last_name: Some("Rossi".into()),
            ..LetterAddress::default()
        };
        let problems = address.problems(Side::Receiver);
        assert!(
            problems.iter().any(|p| p.contains("company and a person")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_empty_fixed_address_and_a_bold_person_are_refused() {
        assert!(!LetterAddress::default().problems(Side::Sender).is_empty());
        let bold = LetterAddress {
            last_name: Some("Rossi".into()),
            bold_company: true,
            ..LetterAddress::default()
        };
        assert!(
            bold.problems(Side::Sender)
                .iter()
                .any(|p| p.contains("bold_company"))
        );
    }
}
