//! Lightroom Classic XMP presets and photo sidecars: the Camera Raw settings of every top-level
//! `rdf:Description`, matched by namespace URI whatever prefix the file binds to it.
use super::value::{RawSetting, RawValue};
use super::{duplicate_setting, not_a_preset, unsupported};
use crate::{Error, ErrorKind};
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::HashSet;

/// The Camera Raw settings namespace. Adobe's preferred prefix is `crs`, but only the URI counts.
pub(super) const CAMERA_RAW_NS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const RDF_NS: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const XMP_META_NS: &str = "adobe:ns:meta/";
const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

/// The most XML nodes one preset may hold. A 1 MiB document of empty elements would otherwise
/// hold about 260,000.
pub const MAX_XMP_NODES: u32 = 200_000;
/// The deepest element nesting accepted. Lightroom's deepest structures, masks inside mask groups,
/// stay well under twenty levels. The XML parser descends recursively, so this is checked by a
/// scan before it runs.
pub const MAX_XMP_DEPTH: usize = 64;
/// The most attribute comparisons the parser's duplicate check may make, summed over elements.
/// That check compares each attribute with every earlier one on its element, so its cost grows
/// with the square of an element's attribute count: one element may hold about 2,000 attributes,
/// where Lightroom writes a few hundred at most.
pub const MAX_XMP_ATTRIBUTE_PAIRS: u64 = 2_000_000;
/// The most namespace declarations one document may make. The parser resolves every element and
/// attribute prefix by scanning the declarations in scope; a sidecar declares about twenty.
pub const MAX_XMP_NAMESPACES: usize = 128;

fn is(node: Node<'_, '_>, namespace: &str, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace() == Some(namespace)
        && node.tag_name().name() == name
}

fn xml_error(error: roxmltree::Error) -> Error {
    match error {
        roxmltree::Error::DtdDetected => unsupported("an XMP preset may not contain a DTD"),
        roxmltree::Error::NodesLimitReached => Error::new(
            ErrorKind::ResourceLimit,
            format!("the XMP holds more than {MAX_XMP_NODES} XML nodes"),
        ),
        other => unsupported(format!("malformed XMP: {other}")),
    }
}

fn limit(detail: String) -> Error {
    Error::new(ErrorKind::ResourceLimit, detail)
}

/// One start tag read to its closing `>` outside quoted values: where that `>` is, and how many
/// attributes and namespace declarations the tag holds. Every attribute has exactly one `=`
/// outside quotes, and its name is the bare word before that `=`.
fn start_tag(tag: &[u8]) -> (Option<usize>, u64, usize) {
    let mut quote = None;
    let mut word: Option<usize> = None;
    let mut last = (0, 0);
    let mut attributes = 0;
    let mut namespaces = 0;
    for (offset, &byte) in tag.iter().enumerate().skip(1) {
        if let Some(open) = quote {
            if byte == open {
                quote = None;
            }
            continue;
        }
        let ends_word = matches!(byte, b'"' | b'\'' | b'=' | b'>') || byte.is_ascii_whitespace();
        if ends_word && let Some(start) = word.take() {
            last = (start, offset);
        }
        match byte {
            b'"' | b'\'' => quote = Some(byte),
            b'>' => return (Some(offset), attributes, namespaces),
            b'=' => {
                attributes += 1;
                let name = &tag[last.0..last.1];
                if name == b"xmlns" || name.starts_with(b"xmlns:") {
                    namespaces += 1;
                }
            }
            _ if ends_word => {}
            _ => {
                word.get_or_insert(offset);
            }
        }
    }
    (None, attributes, namespaces)
}

/// Refuse, before the parser runs, a document whose shape would make parsing it costly: nesting
/// past [`MAX_XMP_DEPTH`], which would exhaust the stack, attribute comparisons past
/// [`MAX_XMP_ATTRIBUTE_PAIRS`] and namespace declarations past [`MAX_XMP_NAMESPACES`].
///
/// A light linear scan of the markup. It skips comments, CDATA sections, processing instructions
/// and declarations, and reads each start tag outside quoted values. On malformed input it may
/// stop early, and the parser then refuses the document at the same point.
fn prescan(text: &str) -> Result<(), Error> {
    let bytes = text.as_bytes();
    let mut at = 0;
    let mut depth = 0_usize;
    let mut pairs = 0_u64;
    let mut namespaces = 0_usize;
    let skip_to = |from: usize, end: &str| text[from..].find(end).map(|found| from + found);
    while let Some(found) = text[at..].find('<') {
        at += found;
        let rest = &bytes[at..];
        let next = if rest.starts_with(b"<!--") {
            skip_to(at + 4, "-->")
        } else if rest.starts_with(b"<![CDATA[") {
            skip_to(at + 9, "]]>")
        } else if rest.starts_with(b"<?") {
            skip_to(at + 2, "?>")
        } else if rest.starts_with(b"<!") {
            skip_to(at + 2, ">")
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            skip_to(at + 2, ">")
        } else {
            let (end, attributes, declared) = start_tag(rest);
            pairs = pairs.saturating_add(attributes * attributes.saturating_sub(1) / 2);
            if pairs > MAX_XMP_ATTRIBUTE_PAIRS {
                return Err(limit(format!(
                    "the XMP holds too many attributes per element: checking them would take \
                     more than {MAX_XMP_ATTRIBUTE_PAIRS} comparisons"
                )));
            }
            namespaces += declared;
            if namespaces > MAX_XMP_NAMESPACES {
                return Err(limit(format!(
                    "the XMP declares more than {MAX_XMP_NAMESPACES} namespaces"
                )));
            }
            let end = end.map(|end| at + end);
            if let Some(end) = end
                && bytes[end - 1] != b'/'
            {
                depth += 1;
                if depth > MAX_XMP_DEPTH {
                    return Err(limit(format!(
                        "the XMP nests elements deeper than {MAX_XMP_DEPTH} levels"
                    )));
                }
            }
            end
        };
        match next {
            Some(next) => at = next + 1,
            None => break,
        }
    }
    Ok(())
}

/// Read every Camera Raw setting of an XMP document, in document order.
pub(super) fn read(text: &str) -> Result<Vec<RawSetting>, Error> {
    prescan(text)?;
    let options = ParsingOptions {
        allow_dtd: false,
        nodes_limit: MAX_XMP_NODES,
    };
    let document = Document::parse_with_options(text, options).map_err(xml_error)?;
    if !document
        .descendants()
        .any(|node| is(node, XMP_META_NS, "xmpmeta") || is(node, RDF_NS, "RDF"))
    {
        return Err(not_a_preset());
    }
    let mut settings = Vec::new();
    for rdf in document
        .descendants()
        .filter(|node| is(*node, RDF_NS, "RDF"))
    {
        for description in rdf
            .children()
            .filter(|node| is(*node, RDF_NS, "Description"))
        {
            for attribute in description.attributes() {
                if attribute.namespace() == Some(CAMERA_RAW_NS) {
                    settings.push(RawSetting {
                        name: attribute.name().to_owned(),
                        value: RawValue::Text(attribute.value().to_owned()),
                    });
                }
            }
            for child in description.children().filter(Node::is_element) {
                if child.tag_name().namespace() == Some(CAMERA_RAW_NS) {
                    settings.push(RawSetting {
                        name: child.tag_name().name().to_owned(),
                        value: element_value(child),
                    });
                }
            }
        }
    }
    if settings.is_empty() {
        return Err(unsupported("the XMP holds no Camera Raw settings"));
    }
    let mut seen = HashSet::new();
    for setting in &settings {
        if !seen.insert(setting.name.as_str()) {
            return Err(duplicate_setting(&setting.name));
        }
    }
    Ok(settings)
}

/// The concatenated text of an element, without the indentation around it.
fn text_of(element: Node<'_, '_>) -> String {
    element
        .children()
        .filter(Node::is_text)
        .filter_map(|node| node.text())
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Whether an element carries attributes of its own beyond RDF syntax and `xml:lang`, which in
/// RDF makes it a structure written in attribute form.
fn has_fields(element: Node<'_, '_>) -> bool {
    element
        .attributes()
        .any(|attribute| !matches!(attribute.namespace(), Some(RDF_NS | XML_NS)))
}

/// The fields of a structure: its own attributes, then its child elements, by local name.
fn fields_of(element: Node<'_, '_>) -> Vec<(String, RawValue)> {
    let attributes = element
        .attributes()
        .filter(|attribute| !matches!(attribute.namespace(), Some(RDF_NS | XML_NS)))
        .map(|attribute| {
            (
                attribute.name().to_owned(),
                RawValue::Text(attribute.value().to_owned()),
            )
        });
    let children = element
        .children()
        .filter(|node| node.is_element() && node.tag_name().namespace() != Some(RDF_NS))
        .map(|child| (child.tag_name().name().to_owned(), element_value(child)));
    attributes.chain(children).collect()
}

/// A property element's value: the `x-default` item of an `rdf:Alt`, the items of an `rdf:Seq`
/// or `rdf:Bag`, a nested `rdf:Description` or `rdf:parseType="Resource"` structure, an
/// `rdf:resource` reference, or the element's text.
fn element_value(element: Node<'_, '_>) -> RawValue {
    if element.attribute((RDF_NS, "parseType")) == Some("Resource") {
        return RawValue::Struct(fields_of(element));
    }
    if let Some(resource) = element.attribute((RDF_NS, "resource")) {
        return RawValue::Text(resource.to_owned());
    }
    match element.children().find(Node::is_element) {
        Some(child) if is(child, RDF_NS, "Alt") => {
            let items = || child.children().filter(|node| is(*node, RDF_NS, "li"));
            let default = items()
                .find(|item| item.attribute((XML_NS, "lang")) == Some("x-default"))
                .or_else(|| items().next());
            RawValue::Text(default.map(text_of).unwrap_or_default())
        }
        Some(child) if is(child, RDF_NS, "Seq") || is(child, RDF_NS, "Bag") => RawValue::List(
            child
                .children()
                .filter(|node| is(*node, RDF_NS, "li"))
                .map(element_value)
                .collect(),
        ),
        Some(child) if is(child, RDF_NS, "Description") => RawValue::Struct(fields_of(child)),
        Some(_) => RawValue::Struct(fields_of(element)),
        None if has_fields(element) => RawValue::Struct(fields_of(element)),
        None => RawValue::Text(text_of(element)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(inner: &str) -> String {
        format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
             xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">{inner}</rdf:RDF></x:xmpmeta>"
        )
    }

    #[test]
    fn the_depth_scan_ignores_markup_that_is_not_an_element() {
        let nested = |levels: usize| {
            format!(
                "{}{}",
                "<a b='>' c=\"/>\">".repeat(levels),
                "</a>".repeat(levels)
            )
        };
        assert!(prescan(&nested(MAX_XMP_DEPTH)).is_ok());
        let error = prescan(&nested(MAX_XMP_DEPTH + 1)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        let flat = format!(
            "<?xml version=\"1.0\"?><!-- <a><a> --><r>{}<![CDATA[<a><a>]]></r>",
            "<e/><f></f>".repeat(1000)
        );
        assert!(prescan(&flat).is_ok());
    }

    #[test]
    fn the_scan_counts_attributes_and_namespace_declarations_outside_quoted_values() {
        assert_eq!(start_tag(b"<a>"), (Some(2), 0, 0));
        assert_eq!(
            start_tag(b"<a xmlns='u' xmlns:b = \"v=w\" b:c='x>y' d=\"'\"/>"),
            (Some(45), 4, 2)
        );
        assert_eq!(start_tag(b"<a b='open"), (None, 1, 0));
        // An element's comparisons grow with the square of its attributes.
        let element = |attributes: usize| {
            let mut tag = "<e".to_owned();
            for index in 0..attributes {
                tag.push_str(&format!(" a{index}=\"\""));
            }
            tag + "/>"
        };
        assert!(prescan(&element(2000)).is_ok());
        let error = prescan(&element(2001)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert!(error.detail.contains("2000000 comparisons"), "{error}");
        // Many small elements stay far inside the budget.
        assert!(prescan(&format!("<r>{}</r>", element(10).repeat(40_000))).is_ok());
        let declarations = |count: usize| {
            let mut tag = "<r".to_owned();
            for index in 0..count {
                tag.push_str(&format!(" xmlns:n{index}=\"urn:{index}\""));
            }
            tag + "/>"
        };
        assert!(prescan(&declarations(MAX_XMP_NAMESPACES)).is_ok());
        let error = prescan(&declarations(MAX_XMP_NAMESPACES + 1)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert!(error.detail.contains("more than 128 namespaces"), "{error}");
    }

    #[test]
    fn element_forms_read_as_text_lists_and_structures() {
        let text = wrap(
            "<rdf:Description xmlns:c='http://ns.adobe.com/camera-raw-settings/1.0/' \
             xmlns:o='http://example.invalid/other/' c:Exposure2012='+0.35' o:Sharpness='90'>\
               <c:Contrast2012> 12 </c:Contrast2012>\
               <c:Name><rdf:Alt><rdf:li xml:lang='fr'>Doux</rdf:li>\
                 <rdf:li xml:lang='x-default'>Soft</rdf:li></rdf:Alt></c:Name>\
               <c:Bag><rdf:Bag><rdf:li>a</rdf:li><rdf:li>b</rdf:li></rdf:Bag></c:Bag>\
               <c:Look rdf:parseType='Resource'><c:Name>X</c:Name></c:Look>\
               <c:Shorthand c:Name='Y' rdf:about=''/>\
               <c:Ref rdf:resource='urn:z'/>\
               <o:Clarity2012>50</o:Clarity2012>\
             </rdf:Description>",
        );
        let settings = read(&text).unwrap();
        let text = |value: &str| RawValue::Text(value.to_owned());
        assert_eq!(
            settings,
            vec![
                RawSetting {
                    name: "Exposure2012".into(),
                    value: text("+0.35")
                },
                RawSetting {
                    name: "Contrast2012".into(),
                    value: text("12")
                },
                RawSetting {
                    name: "Name".into(),
                    value: text("Soft")
                },
                RawSetting {
                    name: "Bag".into(),
                    value: RawValue::List(vec![text("a"), text("b")])
                },
                RawSetting {
                    name: "Look".into(),
                    value: RawValue::Struct(vec![("Name".into(), text("X"))])
                },
                RawSetting {
                    name: "Shorthand".into(),
                    value: RawValue::Struct(vec![("Name".into(), text("Y"))])
                },
                RawSetting {
                    name: "Ref".into(),
                    value: text("urn:z")
                },
            ]
        );
    }

    #[test]
    fn a_repeated_setting_is_refused_rather_than_one_copy_dropped() {
        let text = wrap(
            "<rdf:Description xmlns:crs='http://ns.adobe.com/camera-raw-settings/1.0/' \
             crs:Exposure2012='1'/>\
             <rdf:Description xmlns:crs='http://ns.adobe.com/camera-raw-settings/1.0/' \
             crs:Exposure2012='2'/>",
        );
        let error = read(&text).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedInput);
        assert!(error.detail.contains("Exposure2012"), "{error}");
    }
}
