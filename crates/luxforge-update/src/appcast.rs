//! The Sparkle-format RSS appcast, one per download target: written by the feed generator and read
//! by the Linux updater.
//!
//! An item carries its title, release page link and `pubDate`; `sparkle:version` (the build
//! number) and `sparkle:shortVersionString` (the version); `sparkle:minimumSystemVersion` on
//! macOS; `sparkle:channel` only for a pre-release; `luxforge:catalogFormat` in the
//! [`LUXFORGE_NAMESPACE`]; and one enclosure with its URL, length, `type` and
//! `sparkle:edSignature`. Reading tolerates every Sparkle element and attribute this crate does not
//! use; an item it cannot use is skipped with its reason rather than failing the feed.
use crate::{
    error::{Error, Result},
    key::Signature,
};
use std::fmt::Write as _;

/// Sparkle's XML namespace.
pub const SPARKLE_NAMESPACE: &str = "http://www.andymatuschak.org/xml-namespaces/sparkle";
/// Luxforge's XML namespace, for `luxforge:catalogFormat`.
pub const LUXFORGE_NAMESPACE: &str = "https://luxforge.app/xmlns/update";
/// Every enclosure's `type`, as Sparkle's own `generate_appcast` writes it.
pub const ENCLOSURE_TYPE: &str = "application/octet-stream";
/// The largest feed read; the client caps its download at this.
pub const MAX_APPCAST_BYTES: usize = 4 * 1024 * 1024;
/// The most XML nodes a feed may parse into.
const MAX_NODES: u32 = 200_000;

/// An appcast: its channel's title, link and description, and its items in document order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Appcast {
    pub title: String,
    pub link: Option<String>,
    pub description: Option<String>,
    pub items: Vec<AppcastItem>,
}

/// One release in a feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppcastItem {
    pub title: Option<String>,
    /// The release page.
    pub link: Option<String>,
    /// RFC 822 text, as written.
    pub pub_date: Option<String>,
    /// `sparkle:version`, the build number, which orders releases.
    pub build: u64,
    /// `sparkle:shortVersionString`, the version; `sparkle:version`'s text when absent.
    pub version: String,
    pub minimum_system_version: Option<String>,
    /// `sparkle:channel`; `None` is the default channel.
    pub channel: Option<String>,
    /// `luxforge:catalogFormat`; `None` when the item does not state it.
    pub catalog_format: Option<u32>,
    pub enclosure: Enclosure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Enclosure {
    pub url: String,
    pub length: u64,
    pub mime_type: Option<String>,
    pub ed_signature: Signature,
}

/// An item a read feed skipped: its position among the channel's items and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedItem {
    pub index: usize,
    pub reason: String,
}

/// A read feed and the items it skipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedAppcast {
    pub appcast: Appcast,
    pub skipped: Vec<SkippedItem>,
}

impl Appcast {
    /// The feed's XML. The same appcast always writes the same bytes.
    pub fn to_xml(&self) -> String {
        let mut xml = String::new();
        xml.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
        let _ = writeln!(
            xml,
            "<rss version=\"2.0\" xmlns:sparkle=\"{SPARKLE_NAMESPACE}\" \
             xmlns:luxforge=\"{LUXFORGE_NAMESPACE}\">"
        );
        xml.push_str("  <channel>\n");
        element(&mut xml, 4, "title", &self.title);
        if let Some(link) = &self.link {
            element(&mut xml, 4, "link", link);
        }
        if let Some(description) = &self.description {
            element(&mut xml, 4, "description", description);
        }
        element(&mut xml, 4, "language", "en");
        for item in &self.items {
            xml.push_str("    <item>\n");
            if let Some(title) = &item.title {
                element(&mut xml, 6, "title", title);
            }
            if let Some(link) = &item.link {
                element(&mut xml, 6, "link", link);
            }
            if let Some(date) = &item.pub_date {
                element(&mut xml, 6, "pubDate", date);
            }
            element(&mut xml, 6, "sparkle:version", &item.build.to_string());
            element(&mut xml, 6, "sparkle:shortVersionString", &item.version);
            if let Some(minimum) = &item.minimum_system_version {
                element(&mut xml, 6, "sparkle:minimumSystemVersion", minimum);
            }
            if let Some(channel) = &item.channel {
                element(&mut xml, 6, "sparkle:channel", channel);
            }
            if let Some(format) = item.catalog_format {
                element(&mut xml, 6, "luxforge:catalogFormat", &format.to_string());
            }
            let enclosure = &item.enclosure;
            let _ = write!(
                xml,
                "      <enclosure url=\"{}\" length=\"{}\"",
                escape(&enclosure.url),
                enclosure.length
            );
            if let Some(mime_type) = &enclosure.mime_type {
                let _ = write!(xml, " type=\"{}\"", escape(mime_type));
            }
            let _ = writeln!(
                xml,
                " sparkle:edSignature=\"{}\"/>",
                enclosure.ed_signature.to_base64()
            );
            xml.push_str("    </item>\n");
        }
        xml.push_str("  </channel>\n</rss>\n");
        xml
    }

    /// Reads a feed. A document that is not an RSS appcast, is larger than
    /// [`MAX_APPCAST_BYTES`] or declares a DTD is refused; an item without a numeric
    /// `sparkle:version`, an enclosure with a URL, length and `sparkle:edSignature`, or with a
    /// malformed `luxforge:catalogFormat`, or one marked `sparkle:informationalUpdate`, is skipped.
    pub fn parse(xml: &str) -> Result<ParsedAppcast> {
        if xml.len() > MAX_APPCAST_BYTES {
            return Err(Error::MalformedAppcast(format!(
                "a feed is at most {MAX_APPCAST_BYTES} bytes"
            )));
        }
        let document = roxmltree::Document::parse_with_options(
            xml,
            roxmltree::ParsingOptions {
                allow_dtd: false,
                nodes_limit: MAX_NODES,
            },
        )
        .map_err(|error| Error::MalformedAppcast(error.to_string()))?;
        let rss = document.root_element();
        if !plain(rss, "rss") {
            return Err(Error::MalformedAppcast("the root is not <rss>".into()));
        }
        let channel = rss
            .children()
            .find(|node| plain(*node, "channel"))
            .ok_or_else(|| Error::MalformedAppcast("no <channel>".into()))?;
        let mut appcast = Appcast {
            title: child_text(channel, None, "title").unwrap_or_default(),
            link: child_text(channel, None, "link"),
            description: child_text(channel, None, "description"),
            items: Vec::new(),
        };
        let mut skipped = Vec::new();
        for (index, node) in channel
            .children()
            .filter(|node| plain(*node, "item"))
            .enumerate()
        {
            match item(node) {
                Ok(item) => appcast.items.push(item),
                Err(reason) => skipped.push(SkippedItem { index, reason }),
            }
        }
        Ok(ParsedAppcast { appcast, skipped })
    }
}

fn item(node: roxmltree::Node) -> std::result::Result<AppcastItem, String> {
    let sparkle = Some(SPARKLE_NAMESPACE);
    if child(node, sparkle, "informationalUpdate").is_some() {
        return Err("an informational update has nothing to install".into());
    }
    let enclosure = child(node, None, "enclosure").ok_or("no enclosure")?;
    // Sparkle's older form puts the versions on the enclosure.
    let build_text = child_text(node, sparkle, "version")
        .or_else(|| attribute(enclosure, sparkle, "version"))
        .ok_or("no sparkle:version")?;
    let build = decimal(&build_text).ok_or("sparkle:version is not a build number")?;
    let version = child_text(node, sparkle, "shortVersionString")
        .or_else(|| attribute(enclosure, sparkle, "shortVersionString"))
        .unwrap_or(build_text);
    let catalog_format = match child_text(node, Some(LUXFORGE_NAMESPACE), "catalogFormat") {
        Some(text) => Some(
            decimal(&text)
                .and_then(|format| u32::try_from(format).ok())
                .ok_or("luxforge:catalogFormat is not a number")?,
        ),
        None => None,
    };
    let url = attribute(enclosure, None, "url").ok_or("the enclosure has no url")?;
    let length = attribute(enclosure, None, "length")
        .as_deref()
        .and_then(decimal)
        .ok_or("the enclosure has no numeric length")?;
    let ed_signature = attribute(enclosure, sparkle, "edSignature")
        .ok_or("the enclosure has no sparkle:edSignature")?;
    let ed_signature = Signature::from_base64(&ed_signature)
        .map_err(|_| "the enclosure's sparkle:edSignature is not 64 bytes of base64")?;
    Ok(AppcastItem {
        title: child_text(node, None, "title"),
        link: child_text(node, None, "link"),
        pub_date: child_text(node, None, "pubDate"),
        build,
        version,
        minimum_system_version: child_text(node, sparkle, "minimumSystemVersion"),
        channel: child_text(node, sparkle, "channel"),
        catalog_format,
        enclosure: Enclosure {
            url,
            length,
            mime_type: attribute(enclosure, None, "type"),
            ed_signature,
        },
    })
}

/// Whether `node` is the namespace-less RSS element `name`.
fn plain(node: roxmltree::Node, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.tag_name().name() == name
}

fn child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    namespace: Option<&str>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|child| {
        child.is_element()
            && child.tag_name().namespace() == namespace
            && child.tag_name().name() == name
    })
}

/// The trimmed text of `node`'s first child element `name`, when it has any.
fn child_text(node: roxmltree::Node, namespace: Option<&str>, name: &str) -> Option<String> {
    let text = child(node, namespace, name)?.text()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn attribute(node: roxmltree::Node, namespace: Option<&str>, name: &str) -> Option<String> {
    let value = match namespace {
        Some(namespace) => node.attribute((namespace, name)),
        None => node.attribute(name),
    }?
    .trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// A plain decimal integer: digits only, no sign.
fn decimal(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn element(xml: &mut String, indent: usize, name: &str, text: &str) {
    let _ = writeln!(
        xml,
        "{:indent$}<{name}>{}</{name}>",
        "",
        escape(text),
        indent = indent
    );
}

/// `text` with XML's five special characters escaped, for element text and attribute values.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            c => escaped.push(c),
        }
    }
    escaped
}
