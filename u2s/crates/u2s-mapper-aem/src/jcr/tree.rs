//! The untyped JCR node tree: what a `.content.xml` file parses into before
//! anything decides what any of it *means*.
//!
//! `JcrNode` itself, `parse_jcr_xml` and `serialize_jcr_node` are ported
//! from `ajila-forms-conversion-engine/core/src/aem/parser.rs` (see
//! `PORTING.md`). An intermediate typed tree is required rather than
//! parsing straight into `AemForm`, for three reasons stated in the design
//! doc: `Passthrough` is a set complement and cannot be computed without
//! first materialising the full set of attributes a node carries; the
//! round-trip comparison (`canonical.rs`) needs a tree on both sides; and
//! this tree is the only uniform handle on the ZIP entries that are not the
//! form page at all (folder `.content.xml`s, the DAM asset, dictionaries).
#![allow(
    dead_code,
    reason = "consumed once decode::form (Phase 4a) wires up the decode() entry point; \
              remove this line when it does"
)]

use std::io::Cursor;

use quick_xml::events::{BytesEnd, BytesStart, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;

/// One JCR XML element: its tag, its attributes in source order, and its
/// children in source order. Order matters on both axes -- attribute order
/// for nothing here, but child order is semantic (a wizard step's position
/// among its siblings, an option's position in its list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JcrNode {
    pub tag_name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<JcrNode>,
}

impl JcrNode {
    /// A childless node with the given tag and attributes -- for tests and
    /// for callers building a tree in memory rather than parsing one.
    pub fn leaf<'a, I>(tag_name: &str, attributes: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        Self {
            tag_name: tag_name.to_owned(),
            attributes: attributes
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            children: Vec::new(),
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// The JCR `sling:resourceType` attribute -- which component kind this
    /// node is, in AEM's own vocabulary.
    pub fn resource_type(&self) -> Option<&str> {
        self.attr("sling:resourceType")
    }

    /// The `name` attribute: the AEM component name a script or a `fd:rules`
    /// reference addresses this node by. Distinct from the *element* name
    /// (`self.tag_name`), which is what a `Common.jcr_name` round-trips --
    /// see that field's own doc for why the two are not interchangeable.
    pub fn component_name(&self) -> Option<&str> {
        self.attr("name")
    }

    /// The first direct child with this tag name, if any.
    pub fn child(&self, tag_name: &str) -> Option<&JcrNode> {
        self.children.iter().find(|c| c.tag_name == tag_name)
    }

    /// Every direct child *except* the ones [`xml_writer`](crate::xml_writer)
    /// regenerates from typed fields -- a node's own `items` wrapper, the
    /// grid `layout`, and `cq:responsive` (reproduced from a colspan). A
    /// component-kind decoder walks this, not `children` directly, so a
    /// child the writer will re-synthesize never becomes double-counted
    /// `Passthrough`.
    pub fn passthrough_children(&self) -> impl Iterator<Item = &JcrNode> {
        const REGENERATED: &[&str] = &["items", "layout", "cq:responsive"];
        self.children
            .iter()
            .filter(|c| !REGENERATED.contains(&c.tag_name.as_str()))
    }
}

/// Recursively searches for a node by its `sling:resourceType`, depth-first,
/// self before children. Ported (`parser.rs:555-570`).
pub fn find_node_by_resource_type<'a>(
    node: &'a JcrNode,
    resource_type: &str,
) -> Option<&'a JcrNode> {
    if node.resource_type() == Some(resource_type) {
        return Some(node);
    }
    for child in &node.children {
        if let Some(found) = find_node_by_resource_type(child, resource_type) {
            return Some(found);
        }
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub enum JcrXmlError {
    #[error("XML parse error: {0}")]
    Parse(#[from] quick_xml::Error),
    #[error("mismatched or unexpected XML end tag")]
    Mismatched,
    #[error("the document is empty")]
    Empty,
    /// `quick_xml`'s reader reaches `Eof` without error on a truncated
    /// document (an unclosed tag simply never gets its `End` event) --
    /// this is the one well-formedness check this function has to make
    /// itself, since the parse loop alone cannot tell "ended" from
    /// "silently truncated".
    #[error("the document ends with {0} tag(s) still open")]
    UnclosedTags(usize),
}

/// Parses a `.content.xml` document into a [`JcrNode`] tree. Ported
/// (`parser.rs:495-548`), adjusted to return a typed error rather than a
/// bare `String` and to keep attribute order (the reference discards it;
/// this crate's own round-trip comparison does not need to, and keeping it
/// costs nothing).
pub fn parse_jcr_xml(xml: &str) -> Result<JcrNode, JcrXmlError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut stack: Vec<JcrNode> = Vec::new();
    let mut root: Option<JcrNode> = None;

    loop {
        match reader.read_event()? {
            Event::Start(ref e) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let attributes = read_attributes(e)?;
                stack.push(JcrNode {
                    tag_name,
                    attributes,
                    children: Vec::new(),
                });
            }
            Event::Empty(ref e) => {
                let tag_name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let attributes = read_attributes(e)?;
                let node = JcrNode {
                    tag_name,
                    attributes,
                    children: Vec::new(),
                };
                place(&mut stack, &mut root, node);
            }
            Event::End(_) => {
                let node = stack.pop().ok_or(JcrXmlError::Mismatched)?;
                place(&mut stack, &mut root, node);
            }
            Event::Eof => break,
            _ => {} // skip text, comments, PIs, doctype
        }
    }

    if !stack.is_empty() {
        return Err(JcrXmlError::UnclosedTags(stack.len()));
    }
    root.ok_or(JcrXmlError::Empty)
}

fn read_attributes(e: &BytesStart) -> Result<Vec<(String, String)>, JcrXmlError> {
    let mut attributes = Vec::new();
    for attr in e.attributes() {
        let attr = attr.map_err(quick_xml::Error::from)?;
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
        let value = attr
            .unescape_value()
            .map(|v| v.to_string())
            .unwrap_or_default();
        attributes.push((key, value));
    }
    Ok(attributes)
}

fn place(stack: &mut [JcrNode], root: &mut Option<JcrNode>, node: JcrNode) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        *root = Some(node);
    }
}

/// Re-serializes a [`JcrNode`] subtree to XML -- the inverse of
/// [`parse_jcr_xml`]. Attribute values are re-escaped (they were decoded on
/// parse); a childless node serializes as an empty element. A `.content.xml`
/// document carries no element text, so this emits attributes and children
/// only, matching what a real AEM package's own writer produces. Ported
/// (`parser.rs:458-477`), using `quick_xml`'s own writer rather than hand
/// building the string, since this crate's other writers already do.
pub fn serialize_jcr_node(node: &JcrNode) -> String {
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    write_node(&mut writer, node).expect("writing to an in-memory buffer cannot fail");
    String::from_utf8(writer.into_inner().into_inner()).expect("quick-xml writes valid UTF-8")
}

fn write_node(writer: &mut Writer<Cursor<Vec<u8>>>, node: &JcrNode) -> quick_xml::Result<()> {
    if node.children.is_empty() {
        let mut start = BytesStart::new(node.tag_name.as_str());
        for (k, v) in &node.attributes {
            start.push_attribute((k.as_str(), v.as_str()));
        }
        writer.write_event(Event::Empty(start))
    } else {
        let mut start = BytesStart::new(node.tag_name.as_str());
        for (k, v) in &node.attributes {
            start.push_attribute((k.as_str(), v.as_str()));
        }
        writer.write_event(Event::Start(start))?;
        for child in &node.children {
            write_node(writer, child)?;
        }
        writer.write_event(Event::End(BytesEnd::new(node.tag_name.as_str())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_single_empty_element() {
        let tree = parse_jcr_xml(r#"<jcr:root jcr:primaryType="nt:unstructured"/>"#).unwrap();
        assert_eq!(tree.tag_name, "jcr:root");
        assert_eq!(tree.attr("jcr:primaryType"), Some("nt:unstructured"));
        assert!(tree.children.is_empty());
    }

    #[test]
    fn parses_nested_children_in_order() {
        let xml = r#"<root><a name="1"/><b name="2"/></root>"#;
        let tree = parse_jcr_xml(xml).unwrap();
        assert_eq!(tree.children.len(), 2);
        assert_eq!(tree.children[0].attr("name"), Some("1"));
        assert_eq!(tree.children[1].attr("name"), Some("2"));
    }

    #[test]
    fn unescapes_attribute_values() {
        let xml = r#"<root title="A &amp; B"/>"#;
        let tree = parse_jcr_xml(xml).unwrap();
        assert_eq!(tree.attr("title"), Some("A & B"));
    }

    #[test]
    fn an_unclosed_tag_is_a_typed_error_not_a_panic() {
        let err = parse_jcr_xml("<root>").unwrap_err();
        assert!(matches!(err, JcrXmlError::UnclosedTags(1)), "got {err:?}");
    }

    #[test]
    fn an_empty_document_is_a_typed_error() {
        let err = parse_jcr_xml("").unwrap_err();
        assert!(matches!(err, JcrXmlError::Empty));
    }

    #[test]
    fn find_node_by_resource_type_searches_depth_first() {
        let mut root = JcrNode::leaf("root", []);
        let mut panel = JcrNode::leaf("panel", [("sling:resourceType", "fd/af/components/panel")]);
        panel
            .children
            .push(JcrNode::leaf("field", [("sling:resourceType", "fd/af/components/controls/textbox")]));
        root.children.push(panel);

        let found = find_node_by_resource_type(&root, "fd/af/components/controls/textbox")
            .expect("found");
        assert_eq!(found.tag_name, "field");
        assert!(find_node_by_resource_type(&root, "no/such/type").is_none());
    }

    #[test]
    fn serialize_then_parse_round_trips_a_tree() {
        let mut root = JcrNode::leaf("jcr:root", [("jcr:primaryType", "cq:Page")]);
        root.children
            .push(JcrNode::leaf("child", [("name", "a & b"), ("title", "<x>")]));

        let xml = serialize_jcr_node(&root);
        let reparsed = parse_jcr_xml(&xml).unwrap();
        assert_eq!(reparsed, root);
    }

    #[test]
    fn passthrough_children_excludes_regenerated_wrappers() {
        let mut node = JcrNode::leaf("panel", []);
        node.children.push(JcrNode::leaf("items", []));
        node.children.push(JcrNode::leaf("layout", []));
        node.children.push(JcrNode::leaf("cq:responsive", []));
        node.children.push(JcrNode::leaf("fd:rules", []));

        let kept: Vec<&str> = node
            .passthrough_children()
            .map(|c| c.tag_name.as_str())
            .collect();
        assert_eq!(kept, vec!["fd:rules"]);
    }
}
