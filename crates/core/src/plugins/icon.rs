//! Small static SVG icons, canonicalized before insertion into browser markup.
use super::{PluginError, invalid};
use std::fmt::Write;

const SVG: &str = "http://www.w3.org/2000/svg";

/// Render only approved drawing primitives. Never return publisher markup intact.
/// Icons share the app's current color and fixed 24-unit coordinate system.
pub fn render(source: &str) -> Result<String, PluginError> {
    if source.len() > 8192 || source.contains("<!") || source.contains("<?") {
        return Err(invalid(
            "Plugin icons must be static SVG snippets of at most 8 KiB.",
        ));
    }
    let document = roxmltree::Document::parse(source)
        .map_err(|_| invalid("Plugin icon must be a static SVG drawing."))?;
    let root = document.root_element();
    if root.tag_name().name() != "svg"
        || document.descendants().count() > 128
        || document.descendants().any(|node| node.is_pi())
    {
        return Err(invalid("Plugin icon must be a bounded SVG drawing."));
    }
    let mut output = format!(
        "<svg xmlns=\"{SVG}\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">"
    );
    append(root, &mut output, 0)?;
    output.push_str("</svg>");
    Ok(output)
}

fn append(
    node: roxmltree::Node<'_, '_>,
    output: &mut String,
    depth: usize,
) -> Result<(), PluginError> {
    let name = node.tag_name().name();
    if depth > 12
        || !matches!(
            name,
            "svg" | "g" | "path" | "circle" | "ellipse" | "rect" | "line" | "polyline" | "polygon"
        )
        || node
            .tag_name()
            .namespace()
            .is_some_and(|namespace| namespace != SVG)
        || (depth > 0 && name == "svg")
    {
        return Err(invalid(
            "Plugin icons support only static SVG drawing primitives.",
        ));
    }
    if depth > 0 {
        write!(output, "<{name}").expect("write to String");
    }
    for attribute in node.attributes() {
        let name = attribute.name();
        let value = attribute.value();
        if attribute.namespace().is_some() || !valid_attribute(name, value) {
            return Err(invalid("Plugin icon contains an unsupported attribute."));
        }
        // The outer SVG has fixed layout attributes. Validate publisher defaults
        // but apply them on a drawing group rather than duplicating root fields.
    }
    if depth == 0 {
        output.push_str("<g");
    }
    for attribute in node
        .attributes()
        .filter(|attribute| attribute.name() != "viewBox")
    {
        // Values have been restricted to finite numbers, path characters or
        // fixed enums: no XML delimiters or references survive canonicalization.
        write!(output, " {}=\"{}\"", attribute.name(), attribute.value()).expect("write to String");
    }
    output.push('>');
    for child in node.children() {
        if child.is_element() {
            append(child, output, depth + 1)?;
        } else if child.is_text() && !child.text().unwrap_or_default().trim().is_empty() {
            return Err(invalid(
                "Plugin icons cannot contain text or executable content.",
            ));
        }
    }
    write!(output, "</{}>", if depth == 0 { "g" } else { name }).expect("write to String");
    Ok(())
}

fn valid_attribute(name: &str, value: &str) -> bool {
    match name {
        "viewBox" => value.split_whitespace().collect::<Vec<_>>() == ["0", "0", "24", "24"],
        "fill" | "stroke" => matches!(value, "none" | "currentColor"),
        "stroke-linecap" => matches!(value, "butt" | "round" | "square"),
        "stroke-linejoin" => matches!(value, "miter" | "round" | "bevel"),
        "fill-rule" | "clip-rule" => matches!(value, "nonzero" | "evenodd"),
        "stroke-width" => value
            .parse::<f32>()
            .is_ok_and(|number| number.is_finite() && (0.0..=4.0).contains(&number)),
        "opacity" | "fill-opacity" | "stroke-opacity" => value
            .parse::<f32>()
            .is_ok_and(|number| number.is_finite() && (0.0..=1.0).contains(&number)),
        "width" | "height" => value
            .parse::<f32>()
            .is_ok_and(|number| number.is_finite() && (0.0..=1024.0).contains(&number)),
        "x" | "y" | "x1" | "y1" | "x2" | "y2" | "cx" | "cy" | "r" | "rx" | "ry" => value
            .parse::<f32>()
            .is_ok_and(|number| number.is_finite() && number.abs() <= 1024.0),
        "d" | "points" => {
            !value.trim().is_empty()
                && value.bytes().all(|byte| {
                    byte.is_ascii_digit()
                        || b" .,+-\t\r\n".contains(&byte)
                        || if name == "d" {
                            b"MmLlHhVvCcSsQqTtAaZzEe".contains(&byte)
                        } else {
                            b"eE".contains(&byte)
                        }
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_art_is_canonical_and_inherits_theme_color() {
        let icon = render(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><circle cx="12" cy="12" r="10"/><path d="M12 8v4l3 3"/></svg>"#).unwrap();
        assert!(icon.contains("stroke=\"currentColor\""));
        assert!(icon.contains("width=\"24\" height=\"24\""));
        assert!(icon.contains("<circle cx=\"12\" cy=\"12\" r=\"10\"></circle>"));
    }
    #[test]
    fn manifest_and_catalog_reject_invalid_icons_without_changing_legacy_receipts() {
        let mut receipt = crate::plugins::testing::receipt();
        receipt.validate().unwrap();
        receipt.manifest.icon = Some("<svg><script/></svg>".into());
        assert!(receipt.validate().is_err());
        let mut catalog = crate::plugins::testing::catalog().catalog;
        catalog.validate().unwrap();
        catalog.plugins[0].icon = receipt.manifest.icon;
        assert!(catalog.validate().is_err());
    }
    #[test]
    fn active_external_and_malformed_markup_is_rejected() {
        for content in [
            r#"<script>alert(1)</script>"#,
            r#"<foreignObject><div>text</div></foreignObject>"#,
            r#"<use href="https://example.com/icon.svg"/>"#,
            r#"<path onload="alert(1)" d="M0 0"/>"#,
            r#"<path style="fill:url(https://example.com)"/>"#,
            r#"<path fill="url(#paint)"/>"#,
            r#"<circle r="NaN"/>"#,
            r#"<path xmlns="http://www.w3.org/1999/xhtml"/>"#,
            r#"<path d="M0 0&quot; onload=&quot;alert(1)"/>"#,
            "<?execute data?>",
        ] {
            assert!(
                render(&format!("<svg>{content}</svg>")).is_err(),
                "{content}"
            );
        }
        assert!(render("<!DOCTYPE svg [<!ENTITY x 'bad'>]><svg>&x;</svg>").is_err());
        assert!(render("<svg><path></svg>").is_err());
        assert!(render(&format!("<svg>{}</svg>", "<g/>".repeat(129))).is_err());
        assert!(render(&format!("<svg>{}</svg>", " ".repeat(8192))).is_err());
    }
}
