//! Repeatable subforms: occurrence limits and the Form DOM's instances.
//!
//! XFA 3.3 §9 ("The Occur Element", "Instance Manager"): a subform with an
//! `<occur>` element may appear in the Form DOM more than once, as same-named
//! siblings `Row`, `Row[1]`, ... An empty merge (no data, which is every form
//! this crate opens) creates `initial` of them; a script then adds and removes
//! instances through the instance manager, within `[min, max]`.
//!
//! This module owns the two pure halves of that:
//!
//! - [`Occur`], the limits, parsed once from the template.
//! - [`materialize_initial_instances`], which turns the template's one
//!   declaration of each repeatable subform into its `initial` sibling
//!   instances and keeps the pristine declaration as a [`Prototypes`] entry, so
//!   an instance added later is cloned from the template rather than from
//!   another instance that scripts or the user have already changed.

use std::collections::HashMap;

use boa_engine::{Context, JsObject, js_string, property::PropertyKey};

use super::scripting::som::{SomPath, child_som_path};
use super::{XfaNode, XfaNodeKind};

/// Occurrence limits of one subform (XFA 3.3 §9 "The Occur Element").
///
/// `min` defaults to 1; `max` and `initial` default to `min`. `max == None`
/// is the template's `-1`, no upper limit. A subform without `<occur>` is
/// `1/1/1`: exactly one instance, never more, never fewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occur {
    pub min: u32,
    pub max: Option<u32>,
    pub initial: u32,
}

impl Default for Occur {
    fn default() -> Self {
        Occur {
            min: 1,
            max: Some(1),
            initial: 1,
        }
    }
}

impl Occur {
    /// The limits a node declares, `None` when it has no `<occur>` child.
    ///
    /// The template must keep `min <= initial <= max` (§9, "The initial
    /// property"); a template that does not is clamped into that range with a
    /// warning rather than refused, since the form is still renderable and
    /// the clamp is what an XFA processor enforces at the first add/remove.
    pub fn of(node: &XfaNode) -> Option<Occur> {
        let occur = node.children.iter().find(
            |c| matches!(&c.kind, XfaNodeKind::Element { tag_name, .. } if tag_name == "occur"),
        )?;
        let attr = |name: &str| -> Option<i64> {
            occur
                .attributes
                .get(name)
                .and_then(|s| s.trim().parse::<i64>().ok())
        };

        let min = attr("min").map(|v| v.max(0) as u32).unwrap_or(1);
        let max = match attr("max") {
            Some(-1) => None,
            Some(v) => Some(v.max(0) as u32),
            None => Some(min),
        };
        let declared_initial = attr("initial").map(|v| v.max(0) as u32).unwrap_or(min);
        let mut parsed = Occur {
            min,
            max,
            initial: declared_initial,
        };
        parsed.initial = parsed.clamp(declared_initial);
        if parsed.initial != declared_initial || parsed.max.is_some_and(|m| m < min) {
            log::warn!(
                "<occur> on {:?} is inconsistent (min {min}, max {max:?}, initial \
                 {declared_initial}); using initial {}",
                node.name,
                parsed.initial
            );
        }
        Some(parsed)
    }

    /// `count` moved into `[min, max]`.
    pub fn clamp(&self, count: u32) -> u32 {
        let low = count.max(self.min);
        match self.max {
            Some(max) => low.min(max.max(self.min)),
            None => low,
        }
    }

    /// Whether the Form DOM can hold anything other than exactly one
    /// instance -- the only subforms that need a prototype.
    pub fn is_dynamic(&self) -> bool {
        *self != Occur::default()
    }

    /// The template's `max` attribute value: `-1` for no limit.
    pub fn max_attr(&self) -> i64 {
        self.max.map(i64::from).unwrap_or(-1)
    }
}

/// One change a script made through an instance manager, queued in JS and
/// applied to the Form DOM by the engine afterwards. `parent` is the
/// canonical path of the subform holding the instances, `name` their name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceOp {
    Insert {
        parent: SomPath,
        name: String,
        index: usize,
    },
    Remove {
        parent: SomPath,
        name: String,
        index: usize,
    },
    Move {
        parent: SomPath,
        name: String,
        from: usize,
        to: usize,
    },
    /// A call refused because it would leave `[min, max]` or named an
    /// instance that does not exist; the Form DOM is unchanged.
    Limit {
        parent: SomPath,
        name: String,
        method: String,
        reason: LimitReason,
        count: usize,
        bound: i64,
    },
}

/// Why an instance-manager call was refused or clamped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitReason {
    /// Adding would exceed `<occur max>`.
    Max,
    /// Removing would go below `<occur min>`.
    Min,
    /// `setInstances` asked for a count outside `[min, max]`; `bound` is
    /// the count it was clamped to.
    Clamp,
    /// The index named no instance.
    Index,
}

impl InstanceOp {
    /// Read one queued `_xfa_instance_ops` entry. The queue is written only
    /// by `js_helpers::XFA_INSTANCE_MANAGER`, so an entry of another shape
    /// is an engine bug.
    pub(crate) fn from_js(entry: &JsObject, context: &mut Context) -> Option<InstanceOp> {
        let mut text = |key: &str| -> String {
            entry
                .get(PropertyKey::from(js_string!(key)), context)
                .ok()
                .filter(|v| !v.is_undefined())
                .and_then(|v| v.to_string(context).ok())
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_default()
        };
        let op = text("op");
        let parent = SomPath::new(text("parent"));
        let name = text("name");
        let number = |s: String| -> i64 {
            s.parse::<f64>()
                .unwrap_or_else(|_| panic!("instance op field is not a number: {s:?}"))
                as i64
        };
        Some(match op.as_str() {
            "insert" => InstanceOp::Insert {
                parent,
                name,
                index: number(text("index")) as usize,
            },
            "remove" => InstanceOp::Remove {
                parent,
                name,
                index: number(text("index")) as usize,
            },
            "move" => InstanceOp::Move {
                parent,
                name,
                from: number(text("from")) as usize,
                to: number(text("to")) as usize,
            },
            "limit" => InstanceOp::Limit {
                parent,
                name,
                method: text("method"),
                reason: match text("reason").as_str() {
                    "max" => LimitReason::Max,
                    "min" => LimitReason::Min,
                    "clamp" => LimitReason::Clamp,
                    "index" => LimitReason::Index,
                    other => panic!("unknown instance limit reason {other:?}"),
                },
                count: number(text("count")).max(0) as usize,
                bound: number(text("bound")),
            },
            other => panic!("unknown instance op {other:?}"),
        })
    }
}

/// One repeatable subform's pristine declaration and limits.
#[derive(Debug, Clone)]
pub struct Prototype {
    /// The `<subform>` exactly as the template declares it, before any of its
    /// own nested repeatables were materialised.
    pub node: XfaNode,
    pub occur: Occur,
    /// The names of the named siblings declared before it in the template.
    /// An instance added when none exists goes after the last of these that
    /// is still present, which is where the template put the declaration.
    pub preceding: Vec<String>,
}

/// Every repeatable subform's [`Prototype`], keyed by its index-free template
/// path (`form1.Body.Row`, shared by `Row`, `Row[1]`, ...).
#[derive(Debug, Clone, Default)]
pub struct Prototypes {
    by_template: HashMap<SomPath, Prototype>,
    /// How many instance copies have been made from these prototypes, used
    /// to give each copy's element ids a suffix no other copy has.
    copies_made: u64,
}

impl Prototypes {
    pub fn get(&self, template: &SomPath) -> Option<&Prototype> {
        self.by_template.get(&template.index_free())
    }

    /// The repeatable subforms declared directly under `parent` (any instance
    /// path of it), as `(name, prototype)`.
    pub fn repeatables_under<'a>(
        &'a self,
        parent: &SomPath,
    ) -> impl Iterator<Item = (&'a str, &'a Prototype)> + 'a {
        let parent = parent.index_free();
        self.by_template
            .iter()
            .filter(move |(path, _)| path.parent().as_ref() == Some(&parent))
            .map(|(path, proto)| (path.name(), proto))
    }

    pub fn is_empty(&self) -> bool {
        self.by_template.is_empty()
    }

    /// A copy of `declaration` to stand as a new instance. The first
    /// instance keeps the template's element ids; every later copy gets its
    /// own (see [`localize_ids`]), so an `xfa:embed="#id"` inside it names
    /// this copy's field rather than the first instance's.
    pub fn instance_copy(&mut self, declaration: &XfaNode, first: bool) -> XfaNode {
        let mut copy = declaration.clone();
        if !first {
            self.copies_made += 1;
            localize_ids(&mut copy, self.copies_made);
        }
        copy
    }
}

/// Give every element id inside `node` a `~serial` suffix, and point every
/// `xfa:embed="#id"` reference inside it that named one of those ids at the
/// new one. References to ids outside the subtree are left alone.
///
/// XFA ids are document-unique (XFA 3.3 §3 "Referencing Objects by Their
/// id"); copying a subform as a new instance would otherwise duplicate them.
pub fn localize_ids(node: &mut XfaNode, serial: u64) {
    fn collect(node: &XfaNode, ids: &mut Vec<String>) {
        if let Some(id) = node.attributes.get("id") {
            ids.push(id.clone());
        }
        for child in &node.children {
            collect(child, ids);
        }
    }
    fn rewrite(node: &mut XfaNode, ids: &[String], serial: u64) {
        if let Some(id) = node.attributes.get_mut("id") {
            *id = format!("{id}~{serial}");
        }
        if let Some(target) = node.attributes.get_mut("xfa:embed")
            && let Some(id) = target.strip_prefix('#')
            && ids.iter().any(|i| i == id)
        {
            *target = format!("#{id}~{serial}");
        }
        for child in &mut node.children {
            rewrite(child, ids, serial);
        }
    }
    let mut ids = Vec::new();
    collect(node, &mut ids);
    rewrite(node, &ids, serial);
}

/// Replace each repeatable subform declaration in the template with its
/// `initial` instances (XFA 3.3 §9, empty merge), recording its pristine
/// declaration in the returned [`Prototypes`].
///
/// Only the `<template>` packet is touched: the saved `<form>` packet and the
/// `<datasets>` keep the shape they were saved with. Subforms without
/// `<occur>`, or with a `1/1/1` one, are left exactly as they are, so a form
/// with no repeatable subform comes out unchanged.
pub fn materialize_initial_instances(nodes: &mut [XfaNode]) -> Prototypes {
    let mut prototypes = Prototypes::default();
    materialize_templates(nodes, &mut prototypes);
    prototypes
}

/// Find each `<template>` (the parser keeps the `<xdp:xdp>` wrapper as an
/// element around the packets) and materialise below it.
fn materialize_templates(nodes: &mut [XfaNode], prototypes: &mut Prototypes) {
    for node in nodes.iter_mut() {
        if matches!(node.kind, XfaNodeKind::Template) {
            materialize_children(&mut node.children, "", prototypes);
        } else if matches!(node.kind, XfaNodeKind::Element { .. }) {
            materialize_templates(&mut node.children, prototypes);
        }
    }
}

/// Materialise the repeatables nested inside a fresh instance copy whose
/// index-free template path is `template` (an instance added at runtime
/// starts from its declaration, with its own nested `initial` instances).
pub fn materialize_subtree(node: &mut XfaNode, template: &SomPath, prototypes: &mut Prototypes) {
    materialize_children(
        &mut node.children,
        template.index_free().as_str(),
        prototypes,
    );
}

/// Materialise every repeatable directly in `children` (whose parent's
/// index-free path is `parent`), then recurse into every named or unnamed
/// container below.
fn materialize_children(children: &mut Vec<XfaNode>, parent: &str, prototypes: &mut Prototypes) {
    let mut i = 0;
    let mut preceding: Vec<String> = Vec::new();
    while i < children.len() {
        let child = &children[i];
        let occur = matches!(child.kind, XfaNodeKind::Subform)
            .then(|| Occur::of(child))
            .flatten()
            .filter(Occur::is_dynamic);

        match (occur, child.name.clone()) {
            (Some(occur), Some(name)) => {
                let template = SomPath::new(child_som_path(parent, &name, 0));
                let declaration = children.remove(i);
                prototypes
                    .by_template
                    .entry(template)
                    .or_insert_with(|| Prototype {
                        node: declaration.clone(),
                        occur,
                        preceding: preceding.clone(),
                    });
                for n in 0..occur.initial {
                    children.insert(i, prototypes.instance_copy(&declaration, n == 0));
                    i += 1;
                }
                preceding.push(name);
            }
            (_, name) => {
                if let Some(name) = name {
                    preceding.push(name);
                }
                i += 1;
            }
        }
    }

    // Recurse after this level is final, with each child's index-free path.
    for child in children.iter_mut() {
        let path = match &child.name {
            Some(name) => child_som_path(parent, name, 0),
            None => parent.to_string(),
        };
        materialize_children(&mut child.children, &path, prototypes);
    }
}

/// Where a new instance of `name` goes among `children`: after its last
/// existing instance, or, when none exists, after the last still-present
/// sibling the template declared before it.
pub fn insertion_slot(children: &[XfaNode], name: &str, prototype: &Prototype) -> usize {
    if let Some(last) = children
        .iter()
        .rposition(|c| c.name.as_deref() == Some(name))
    {
        return last + 1;
    }
    children
        .iter()
        .rposition(|c| {
            c.name
                .as_deref()
                .is_some_and(|n| prototype.preceding.iter().any(|p| p == n))
        })
        .map(|last| last + 1)
        .unwrap_or(0)
}

/// The sibling positions of the instances of `name` among `children`, in
/// instance order: `positions[i]` is where instance `i` sits.
pub fn instance_positions(children: &[XfaNode], name: &str) -> Vec<usize> {
    children
        .iter()
        .enumerate()
        .filter(|(_, c)| c.name.as_deref() == Some(name))
        .map(|(pos, _)| pos)
        .collect()
}

/// Apply one structural instance-manager change to the Form DOM tree.
///
/// `Limit` changes nothing. Anything else names a parent and a prototype the
/// engine registered itself, so not finding them means the JS side and the
/// tree have diverged: that is an engine bug and is reported, not skipped.
pub fn apply_to_tree(
    nodes: &mut [XfaNode],
    prototypes: &mut Prototypes,
    op: &InstanceOp,
) -> Result<(), String> {
    let (parent, name) = match op {
        InstanceOp::Limit { .. } => return Ok(()),
        InstanceOp::Insert { parent, name, .. }
        | InstanceOp::Remove { parent, name, .. }
        | InstanceOp::Move { parent, name, .. } => (parent, name),
    };
    let template = SomPath::new(child_som_path(parent.index_free().as_str(), name, 0));
    let parent_node = super::scripting::som::walk_som_path_mut(nodes, parent.as_str())
        .ok_or_else(|| format!("instance op on {parent}.{name}: no such parent in the Form DOM"))?;
    let children = &mut parent_node.children;
    let positions = instance_positions(children, name);
    let out_of_range = |index: usize| {
        format!(
            "instance op on {parent}.{name}: index {index} but {} instance(s) exist",
            positions.len()
        )
    };

    match op {
        InstanceOp::Insert { index, .. } => {
            let prototype = prototypes
                .get(&template)
                .ok_or_else(|| format!("{template} has no prototype; it is not repeatable"))?
                .clone();
            let slot = match positions.get(*index) {
                Some(&pos) => pos,
                None if *index == positions.len() => insertion_slot(children, name, &prototype),
                None => return Err(out_of_range(*index)),
            };
            let mut instance = prototypes.instance_copy(&prototype.node, positions.is_empty());
            materialize_subtree(&mut instance, &template, prototypes);
            children.insert(slot, instance);
        }
        InstanceOp::Remove { index, .. } => {
            let &pos = positions.get(*index).ok_or_else(|| out_of_range(*index))?;
            children.remove(pos);
        }
        InstanceOp::Move { from, to, .. } => {
            let &from_pos = positions.get(*from).ok_or_else(|| out_of_range(*from))?;
            if *to >= positions.len() {
                return Err(out_of_range(*to));
            }
            let moved = children.remove(from_pos);
            let remaining = instance_positions(children, name);
            let slot = match remaining.get(*to) {
                Some(&pos) => pos,
                None => remaining.last().map(|&p| p + 1).unwrap_or(from_pos),
            };
            children.insert(slot, moved);
        }
        InstanceOp::Limit { .. } => unreachable!("handled above"),
    }
    Ok(())
}

impl InstanceOp {
    /// The `(parent, name)` instance array this op touched.
    pub fn array(&self) -> (&SomPath, &str) {
        match self {
            InstanceOp::Insert { parent, name, .. }
            | InstanceOp::Remove { parent, name, .. }
            | InstanceOp::Move { parent, name, .. }
            | InstanceOp::Limit { parent, name, .. } => (parent, name),
        }
    }

    /// What a refused call reports to the host, `None` for a change.
    pub fn limit_message(&self) -> Option<String> {
        let InstanceOp::Limit {
            parent,
            name,
            method,
            reason,
            count,
            bound,
        } = self
        else {
            return None;
        };
        let manager = format!("{parent}._{name}");
        Some(match reason {
            LimitReason::Max => format!(
                "{manager}.{method} did nothing: {name} already has {count} instance(s), \
                 the most its <occur max=\"{bound}\"> allows"
            ),
            LimitReason::Min => format!(
                "{manager}.{method} did nothing: {name} has {count} instance(s), \
                 the fewest its <occur min=\"{bound}\"> allows"
            ),
            LimitReason::Clamp => format!(
                "{manager}.setInstances was held to {bound} instance(s), \
                 the limit its <occur> allows"
            ),
            LimitReason::Index => format!(
                "{manager}.{method} did nothing: no instance at that index \
                 ({name} has {count})"
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> Vec<XfaNode> {
        let xml = format!(
            r#"<xdp:xdp xmlns:xdp="http://ns.adobe.com/xdp/"><template xmlns="http://www.xfa.org/schema/xfa-template/3.3/"><subform name="Root">{body}</subform></template></xdp:xdp>"#
        );
        XfaNode::parse(xml.as_bytes()).expect("parse")
    }

    fn occur_of(attrs: &str) -> Option<Occur> {
        let nodes = parse(&format!(
            r#"<subform name="Row"><occur {attrs}/></subform>"#
        ));
        let row = crate::xfa::scripting::som::walk_som_path(&nodes, "Root.Row").expect("Row");
        Occur::of(row)
    }

    #[test]
    fn occur_defaults_follow_xfa_3_3() {
        // §9: min defaults to 1, max and initial to min, -1 is unlimited.
        assert_eq!(
            occur_of(""),
            Some(Occur {
                min: 1,
                max: Some(1),
                initial: 1
            })
        );
        assert_eq!(
            occur_of(r#"max="-1""#),
            Some(Occur {
                min: 1,
                max: None,
                initial: 1
            })
        );
        assert_eq!(
            occur_of(r#"min="0" max="5""#),
            Some(Occur {
                min: 0,
                max: Some(5),
                initial: 0
            })
        );
        assert_eq!(
            occur_of(r#"min="2""#),
            Some(Occur {
                min: 2,
                max: Some(2),
                initial: 2
            })
        );
        // An initial outside [min, max] is clamped into it.
        assert_eq!(
            occur_of(r#"min="1" max="3" initial="7""#).unwrap().initial,
            3
        );
        let nodes = parse(r#"<subform name="Row"/>"#);
        let row = crate::xfa::scripting::som::walk_som_path(&nodes, "Root.Row").unwrap();
        assert_eq!(Occur::of(row), None);
        assert!(!Occur::default().is_dynamic());
    }

    fn names_under_root(nodes: &[XfaNode]) -> Vec<String> {
        let root = crate::xfa::scripting::som::walk_som_path(nodes, "Root").expect("Root");
        root.children
            .iter()
            .filter_map(|c| c.name.clone())
            .collect()
    }

    #[test]
    fn initial_instances_replace_the_declaration_and_nested_ones_repeat_per_instance() {
        let mut nodes = parse(
            r#"<field name="Before"/><subform name="Row"><occur max="-1" initial="2"/><subform name="Cell"><occur max="3" initial="3"/></subform></subform><field name="After"/>"#,
        );
        let prototypes = materialize_initial_instances(&mut nodes);
        assert_eq!(names_under_root(&nodes), ["Before", "Row", "Row", "After"]);

        for row in ["Root.Row", "Root.Row[1]"] {
            let row = crate::xfa::scripting::som::walk_som_path(&nodes, row).expect("row");
            let cells = row
                .children
                .iter()
                .filter(|c| c.name.as_deref() == Some("Cell"));
            assert_eq!(cells.count(), 3, "each Row has its own three Cells");
        }
        assert!(prototypes.get(&SomPath::new("Root.Row")).is_some());
        assert!(
            prototypes
                .get(&SomPath::new("Root.Row[1].Cell[2]"))
                .is_some()
        );
        assert_eq!(
            prototypes
                .repeatables_under(&SomPath::new("Root"))
                .map(|(name, _)| name.to_string())
                .collect::<Vec<_>>(),
            ["Row"]
        );
    }

    #[test]
    fn initial_zero_leaves_no_instance_but_remembers_where_one_goes() {
        let mut nodes = parse(
            r#"<field name="Before"/><subform name="Row"><occur min="0" max="-1" initial="0"/></subform><field name="After"/>"#,
        );
        let prototypes = materialize_initial_instances(&mut nodes);
        assert_eq!(names_under_root(&nodes), ["Before", "After"]);

        let prototype = prototypes
            .get(&SomPath::new("Root.Row"))
            .expect("prototype");
        let root = crate::xfa::scripting::som::walk_som_path(&nodes, "Root").unwrap();
        // After `Before`, the sibling the template declared before it.
        let slot = insertion_slot(&root.children, "Row", prototype);
        assert_eq!(root.children[slot - 1].name.as_deref(), Some("Before"));
    }

    #[test]
    fn a_form_without_repeatables_is_left_unchanged() {
        let mut nodes = parse(
            r#"<subform name="A"><field name="F"/></subform><subform name="B"><occur min="1" max="1"/></subform>"#,
        );
        let before = format!("{nodes:?}");
        let prototypes = materialize_initial_instances(&mut nodes);
        assert!(prototypes.is_empty());
        assert_eq!(format!("{nodes:?}"), before);
    }
}
