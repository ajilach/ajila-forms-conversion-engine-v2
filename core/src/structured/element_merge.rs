//! Element merging rules for the structured editor.
//!
//! This module provides rules for merging adjacent or non-adjacent structured
//! nodes in the editor UI. Different node type combinations have different
//! merge behaviors.

use super::{FieldNode, FieldType, HeadingLevel, ListItem, ParagraphNode, StructuredNode};

/// Errors that can occur during element merging.
#[derive(Debug, Clone, PartialEq)]
pub enum MergeError {
    /// Cannot merge these node types together.
    IncompatibleTypes {
        source_type: &'static str,
        target_type: &'static str,
    },
    /// Need at least two nodes to merge.
    NotEnoughNodes,
    /// Cannot merge nodes that contain form fields.
    CannotMergeFields,
    /// Cannot merge conditional or repeatable nodes.
    CannotMergeStructural,
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeError::IncompatibleTypes {
                source_type,
                target_type,
            } => {
                write!(f, "Cannot merge {source_type} into {target_type}")
            }
            MergeError::NotEnoughNodes => write!(f, "Need at least two nodes to merge"),
            MergeError::CannotMergeFields => write!(f, "Cannot merge form fields"),
            MergeError::CannotMergeStructural => {
                write!(f, "Cannot merge conditional or repeatable nodes")
            }
        }
    }
}

impl std::error::Error for MergeError {}

/// Returns the type name of a structured node for error messages.
fn node_type_name(node: &StructuredNode) -> &'static str {
    match node {
        StructuredNode::Heading(_) => "Heading",
        StructuredNode::Paragraph(_) => "Paragraph",
        StructuredNode::Image(_) => "Image",
        StructuredNode::Table(_) => "Table",
        StructuredNode::Field(_) => "Field",
        StructuredNode::Repeatable(_) => "Repeatable",
        StructuredNode::Group(_) => "Group",
        StructuredNode::Conditional(_) => "Conditional",
        StructuredNode::Empty => "Empty",
        StructuredNode::GridLayout(_) => "GridLayout",
        StructuredNode::List(_) => "List",
        StructuredNode::Footnote(_) => "Footnote",
        StructuredNode::Html(_) => "Html",
        StructuredNode::Notice(_) => "Notice",
    }
}

/// Check whether two fields are option groups of the same kind (both radio
/// groups or both checkbox groups), which makes them mergeable by combining
/// their options into a single group.
fn fields_are_mergeable_option_groups(a: &FieldNode, b: &FieldNode) -> bool {
    matches!(
        (&a.input_type, &b.input_type),
        (FieldType::Radio { .. }, FieldType::Radio { .. })
            | (FieldType::CheckboxGroup { .. }, FieldType::CheckboxGroup { .. })
    )
}

/// Check if two node types can be merged together.
///
/// Returns `Ok(())` if the nodes can be merged, or an error explaining why not.
pub fn can_merge(source: &StructuredNode, target: &StructuredNode) -> Result<(), MergeError> {
    use StructuredNode::*;

    match (source, target) {
        // Radio groups and checkbox groups of the same kind can be merged by
        // combining their options into a single group.
        (Field(a), Field(b)) if fields_are_mergeable_option_groups(a, b) => Ok(()),

        // Other fields cannot be merged
        (Field(_), _) | (_, Field(_)) => Err(MergeError::CannotMergeFields),

        // Structural nodes (Conditional, Repeatable) cannot be merged
        (Conditional(_), _) | (_, Conditional(_)) | (Repeatable(_), _) | (_, Repeatable(_)) => {
            Err(MergeError::CannotMergeStructural)
        }

        // Empty nodes can always be merged (they just disappear)
        (Empty, _) | (_, Empty) => Ok(()),

        // Same types can always merge
        (Paragraph(_), Paragraph(_))
        | (Heading(_), Heading(_))
        | (List(_), List(_))
        | (Table(_), Table(_))
        | (Group(_), Group(_))
        | (GridLayout(_), GridLayout(_)) => Ok(()),

        // Cross-type merges for text-like content
        (Paragraph(_), List(_))
        | (List(_), Paragraph(_))
        | (Heading(_), Paragraph(_))
        | (Paragraph(_), Heading(_))
        | (Heading(_), List(_))
        | (List(_), Heading(_)) => Ok(()),

        // Images can be merged into groups
        (Image(_), Group(_)) | (Group(_), Image(_)) => Ok(()),

        // Everything else is incompatible
        _ => Err(MergeError::IncompatibleTypes {
            source_type: node_type_name(source),
            target_type: node_type_name(target),
        }),
    }
}

/// Check if a list of nodes can all be merged together.
///
/// This checks pairwise compatibility: each node must be mergeable with the next.
pub fn can_merge_all(nodes: &[&StructuredNode]) -> Result<(), MergeError> {
    if nodes.len() < 2 {
        return Err(MergeError::NotEnoughNodes);
    }

    // Check that all pairs can merge
    for window in nodes.windows(2) {
        can_merge(window[0], window[1])?;
    }

    Ok(())
}

/// Merge two nodes together.
///
/// The source node is merged into the target node. The result replaces the target.
/// Returns the merged node.
pub fn merge_two(
    source: StructuredNode,
    target: StructuredNode,
) -> Result<StructuredNode, MergeError> {
    can_merge(&source, &target)?;

    use StructuredNode::*;

    Ok(match (source, target) {
        // Empty nodes disappear
        (Empty, other) | (other, Empty) => other,

        // Radio + Radio / CheckboxGroup + CheckboxGroup: combine their options
        // into the target field, preserving target metadata (name, label, ...).
        (Field(src), Field(mut tgt))
            if fields_are_mergeable_option_groups(&src, &tgt) =>
        {
            let src_options = match src.input_type {
                FieldType::Radio { options } | FieldType::CheckboxGroup { options } => options,
                // Unreachable: guard guarantees an option-group field type.
                _ => Vec::new(),
            };
            match &mut tgt.input_type {
                FieldType::Radio { options } | FieldType::CheckboxGroup { options } => {
                    options.extend(src_options);
                }
                _ => {}
            }
            Field(tgt)
        }

        // Paragraph + Paragraph: join text content
        (Paragraph(src), Paragraph(mut tgt)) => {
            tgt.content.concat(src.content);
            Paragraph(tgt)
        }

        // Heading + Heading: join text, keep lower level (larger number = lower in hierarchy)
        (Heading(src), Heading(mut tgt)) => {
            let new_level = std::cmp::max(src.level.as_u8(), tgt.level.as_u8());
            tgt.level = HeadingLevel::from_u8(new_level);
            tgt.content.concat(src.content);
            Heading(tgt)
        }

        // Heading + Paragraph: convert heading to paragraph, then join
        (Heading(src), Paragraph(mut tgt)) => {
            tgt.content.concat(src.content);
            Paragraph(tgt)
        }

        // Paragraph + Heading: convert to paragraph, heading content first
        (Paragraph(src), Heading(tgt)) => {
            let mut result = ParagraphNode {
                content: tgt.content,
                som_path: tgt.som_path,
                source_name: tgt.source_name,
            };
            result.content.concat(src.content);
            Paragraph(result)
        }

        // List + List: append items
        (List(src), List(mut tgt)) => {
            tgt.items.extend(src.items);
            List(tgt)
        }

        // Paragraph + List: add paragraph as new list item
        (Paragraph(src), List(mut tgt)) => {
            tgt.items.push(ListItem::simple(src.content));
            List(tgt)
        }

        // List + Paragraph: prepend paragraph as first list item
        (List(mut src), Paragraph(tgt)) => {
            src.items.insert(0, ListItem::simple(tgt.content));
            List(src)
        }

        // Heading + List: convert heading to list item, prepend
        (Heading(src), List(mut tgt)) => {
            tgt.items.insert(0, ListItem::simple(src.content));
            List(tgt)
        }

        // List + Heading: convert heading to list item, append
        (List(mut src), Heading(tgt)) => {
            src.items.push(ListItem::simple(tgt.content));
            List(src)
        }

        // Table + Table: append rows
        (Table(src), Table(mut tgt)) => {
            tgt.rows.extend(src.rows);
            // If source has a caption and target doesn't, use source's
            if tgt.caption.is_none() && src.caption.is_some() {
                tgt.caption = src.caption;
            }
            Table(tgt)
        }

        // Group + Group: merge children
        (Group(src), Group(mut tgt)) => {
            tgt.children.extend(src.children);
            Group(tgt)
        }

        // Image + Group: add image to group
        (Image(src), Group(mut tgt)) => {
            tgt.children.push(Image(src));
            Group(tgt)
        }

        // Group + Image: add image to group
        (Group(mut src), Image(tgt)) => {
            src.children.insert(0, Image(tgt));
            Group(src)
        }

        // GridLayout + GridLayout: merge elements
        (GridLayout(src), GridLayout(mut tgt)) => {
            tgt.elements.extend(src.elements);
            GridLayout(tgt)
        }

        // Should not reach here due to can_merge check
        (src, tgt) => {
            return Err(MergeError::IncompatibleTypes {
                source_type: node_type_name(&src),
                target_type: node_type_name(&tgt),
            });
        }
    })
}

/// Merge multiple nodes together.
///
/// Nodes are merged sequentially from first to last. The first node becomes the
/// "base" and subsequent nodes are merged into it.
///
/// Returns the single merged node.
pub fn merge_nodes(nodes: Vec<StructuredNode>) -> Result<StructuredNode, MergeError> {
    if nodes.len() < 2 {
        return Err(MergeError::NotEnoughNodes);
    }

    let mut iter = nodes.into_iter();
    let mut result = iter.next().unwrap();

    for node in iter {
        result = merge_two(node, result)?;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::ListStyleType;
    use crate::structured::{HeadingNode, ListNode, TranslatedText};

    #[test]
    fn test_merge_paragraphs() {
        let p1 = StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("Hello"),
            som_path: None,
            source_name: None,
        });
        let p2 = StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("World"),
            som_path: None,
            source_name: None,
        });

        let merged = merge_two(p1, p2).unwrap();
        if let StructuredNode::Paragraph(p) = merged {
            assert_eq!(p.content.as_plain_text(), "World Hello");
        } else {
            panic!("Expected Paragraph");
        }
    }

    #[test]
    fn test_merge_lists() {
        let l1 = StructuredNode::List(ListNode {
            list_style: ListStyleType::Disc,
            items: vec![ListItem::simple(TranslatedText::plain("Item 1"))],
        });
        let l2 = StructuredNode::List(ListNode {
            list_style: ListStyleType::Disc,
            items: vec![ListItem::simple(TranslatedText::plain("Item 2"))],
        });

        let merged = merge_two(l1, l2).unwrap();
        if let StructuredNode::List(l) = merged {
            assert_eq!(l.items.len(), 2);
        } else {
            panic!("Expected List");
        }
    }

    #[test]
    fn test_merge_paragraph_into_list() {
        let p = StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("New item"),
            som_path: None,
            source_name: None,
        });
        let l = StructuredNode::List(ListNode {
            list_style: ListStyleType::Disc,
            items: vec![ListItem::simple(TranslatedText::plain("Existing item"))],
        });

        let merged = merge_two(p, l).unwrap();
        if let StructuredNode::List(list) = merged {
            assert_eq!(list.items.len(), 2);
            assert_eq!(list.items[1].as_plain_text(), "New item");
        } else {
            panic!("Expected List");
        }
    }

    #[test]
    fn test_cannot_merge_fields() {
        use crate::structured::{FieldId, FieldNode, FieldType};
        use crate::xfa::scripting::SomPath;

        let f = StructuredNode::Field(FieldNode {
            name: FieldId::from_som_path(&SomPath::new("test")),
            som_path: None,
            label: None,
            input_type: FieldType::Text {
                regex: None,
                max_length: None,
                min_length: None,
            },
            value: None,
            placeholder: None,
            required: false,
        });
        let p = StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("Text"),
            som_path: None,
            source_name: None,
        });

        assert!(matches!(
            can_merge(&f, &p),
            Err(MergeError::CannotMergeFields)
        ));
    }

    fn option(name: &str, value: &str) -> crate::structured::NameValue {
        use crate::structured::{InputValue, NameValue, TranslatableString};
        NameValue {
            name: TranslatableString::Plain(name.to_string()),
            value: InputValue::Text(value.to_string()),
        }
    }

    fn option_field(name: &str, input_type: FieldType) -> StructuredNode {
        use crate::structured::{FieldId, FieldNode};
        use crate::xfa::scripting::SomPath;
        StructuredNode::Field(FieldNode {
            name: FieldId::from_som_path(&SomPath::new(name)),
            som_path: None,
            label: None,
            input_type,
            value: None,
            placeholder: None,
            required: false,
        })
    }

    #[test]
    fn test_merge_radio_groups_combines_options() {
        let r1 = option_field(
            "r1",
            FieldType::Radio {
                options: vec![option("Yes", "y")],
            },
        );
        let r2 = option_field(
            "r2",
            FieldType::Radio {
                options: vec![option("No", "n")],
            },
        );

        let merged = merge_two(r2, r1).unwrap();
        if let StructuredNode::Field(f) = merged {
            match f.input_type {
                FieldType::Radio { options } => {
                    assert_eq!(options.len(), 2);
                    assert_eq!(options[0].name.as_str(), "Yes");
                    assert_eq!(options[1].name.as_str(), "No");
                }
                _ => panic!("Expected Radio"),
            }
        } else {
            panic!("Expected Field");
        }
    }

    #[test]
    fn test_merge_checkbox_groups_combines_options() {
        let c1 = option_field(
            "c1",
            FieldType::CheckboxGroup {
                options: vec![option("A", "a")],
            },
        );
        let c2 = option_field(
            "c2",
            FieldType::CheckboxGroup {
                options: vec![option("B", "b"), option("C", "c")],
            },
        );

        let merged = merge_nodes(vec![c1, c2]).unwrap();
        if let StructuredNode::Field(f) = merged {
            match f.input_type {
                FieldType::CheckboxGroup { options } => {
                    assert_eq!(options.len(), 3);
                    assert_eq!(options[0].name.as_str(), "A");
                    assert_eq!(options[1].name.as_str(), "B");
                    assert_eq!(options[2].name.as_str(), "C");
                }
                _ => panic!("Expected CheckboxGroup"),
            }
        } else {
            panic!("Expected Field");
        }
    }

    #[test]
    fn test_cannot_merge_radio_with_checkbox() {
        let r = option_field(
            "r",
            FieldType::Radio {
                options: vec![option("Yes", "y")],
            },
        );
        let c = option_field(
            "c",
            FieldType::CheckboxGroup {
                options: vec![option("A", "a")],
            },
        );

        assert!(matches!(
            can_merge(&r, &c),
            Err(MergeError::CannotMergeFields)
        ));
    }

    #[test]
    fn test_merge_with_empty() {
        let p = StructuredNode::Paragraph(ParagraphNode {
            content: TranslatedText::plain("Keep me"),
            som_path: None,
            source_name: None,
        });
        let empty = StructuredNode::Empty;

        let merged = merge_two(empty, p.clone()).unwrap();
        if let StructuredNode::Paragraph(para) = merged {
            assert_eq!(para.content.as_plain_text(), "Keep me");
        } else {
            panic!("Expected Paragraph");
        }
    }

    #[test]
    fn test_merge_headings_keeps_lower_level() {
        let h1 = StructuredNode::Heading(HeadingNode {
            level: HeadingLevel::H1,
            content: TranslatedText::plain("Big"),
            som_path: None,
            source_name: None,
        });
        let h3 = StructuredNode::Heading(HeadingNode {
            level: HeadingLevel::H3,
            content: TranslatedText::plain("Small"),
            som_path: None,
            source_name: None,
        });

        let merged = merge_two(h1, h3).unwrap();
        if let StructuredNode::Heading(h) = merged {
            assert_eq!(h.level.as_u8(), 3); // Lower level = higher number
        } else {
            panic!("Expected Heading");
        }
    }
}
