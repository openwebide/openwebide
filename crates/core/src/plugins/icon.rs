//! Structured, bounded drawing metadata rendered with fixed theme styling.
use super::{PluginError, invalid};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use std::fmt::Write;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginIcon {
    pub elements: Vec<PluginIconElement>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum PluginIconElement {
    Path {
        d: String,
    },
    Circle {
        cx: Number,
        cy: Number,
        r: Number,
    },
    Ellipse {
        cx: Number,
        cy: Number,
        rx: Number,
        ry: Number,
    },
    Rect {
        x: Number,
        y: Number,
        width: Number,
        height: Number,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rx: Option<Number>,
    },
    Line {
        x1: Number,
        y1: Number,
        x2: Number,
        y2: Number,
    },
    Polyline {
        points: Vec<[Number; 2]>,
    },
    Polygon {
        points: Vec<[Number; 2]>,
    },
}

/// Only schema-defined attributes reach markup; publisher XML is never accepted.
pub fn render(icon: &PluginIcon) -> Result<String, PluginError> {
    if icon.elements.is_empty()
        || icon.elements.len() > 64
        || serde_json::to_vec(icon)
            .map_err(|_| invalid("Invalid plugin icon."))?
            .len()
            > 8192
    {
        return Err(invalid("Plugin icons require 1–64 elements within 8 KiB."));
    }
    let mut output = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"2\" stroke-linecap=\"round\" stroke-linejoin=\"round\">",
    );
    for element in &icon.elements {
        let value = serde_json::to_value(element).map_err(|_| invalid("Invalid plugin icon."))?;
        let fields = value
            .as_object()
            .expect("drawing elements serialize as objects");
        let kind = fields["type"].as_str().expect("drawing type is a string");
        write!(output, "<{kind}").expect("write to String");
        for (attribute, value) in fields {
            if attribute == "type" {
                continue;
            }
            let text = if attribute == "d" {
                let path = value.as_str().expect("path data is a string");
                if path.trim().is_empty()
                    || !path.bytes().all(|byte| {
                        byte.is_ascii_digit()
                            || b" .,+-\t\r\nMmLlHhVvCcSsQqTtAaZzEe".contains(&byte)
                    })
                {
                    return Err(invalid("Plugin icon path contains unsupported characters."));
                }
                path.to_owned()
            } else if attribute == "points" {
                let points = value.as_array().expect("points are an array");
                if !(2..=128).contains(&points.len()) {
                    return Err(invalid("Plugin icon requires 2–128 points."));
                }
                let mut coordinates = Vec::new();
                for point in points {
                    let pair = point.as_array().expect("point is a coordinate pair");
                    for number in pair {
                        validate_number(number, false)?;
                    }
                    coordinates.push(format!("{},{}", pair[0], pair[1]));
                }
                coordinates.join(" ")
            } else {
                validate_number(
                    value,
                    matches!(attribute.as_str(), "r" | "rx" | "ry" | "width" | "height"),
                )?;
                value.to_string()
            };
            write!(output, " {attribute}=\"{text}\"").expect("write to String");
        }
        write!(output, "></{kind}>").expect("write to String");
    }
    output.push_str("</svg>");
    Ok(output)
}

fn validate_number(value: &serde_json::Value, nonnegative: bool) -> Result<(), PluginError> {
    if value.as_f64().is_some_and(|number| {
        number.is_finite() && number.abs() <= 1024.0 && (!nonnegative || number >= 0.0)
    }) {
        Ok(())
    } else {
        Err(invalid("Plugin icon geometry is out of bounds."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_drawings_inherit_theme_color() {
        let icon: PluginIcon =
            serde_json::from_str(r#"{"elements":[{"type":"circle","cx":12,"cy":12,"r":10}]}"#)
                .unwrap();
        let rendered = render(&icon).unwrap();
        assert!(rendered.contains("stroke=\"currentColor\""));
        assert!(rendered.contains("viewBox=\"0 0 24 24\""));
        assert!(rendered.contains("<circle cx=\"12\" cy=\"12\" r=\"10\"></circle>"));
    }
    #[test]
    fn schemas_reject_markup_unknown_elements_and_attributes() {
        for source in [
            r#""<svg/>""#,
            r#"{"elements":[{"type":"script"}]}"#,
            r#"{"elements":[{"type":"path","d":"M0 0","onload":"alert(1)"}]}"#,
            r#"{"elements":[{"type":"circle","cx":"12","cy":12,"r":10}]}"#,
        ] {
            assert!(
                serde_json::from_str::<PluginIcon>(source).is_err(),
                "{source}"
            );
        }
    }
    #[test]
    fn manifest_and_catalog_validate_bounded_geometry_and_paths() {
        let mut receipt = crate::plugins::testing::receipt();
        receipt.validate().unwrap();
        for source in [
            r#"{"elements":[]}"#,
            r#"{"elements":[{"type":"circle","cx":12,"cy":12,"r":-1}]}"#,
            r#"{"elements":[{"type":"path","d":"M0 0<script>"}]}"#,
            r#"{"elements":[{"type":"line","x1":1025,"x2":0,"y1":0,"y2":0}]}"#,
        ] {
            receipt.manifest.icon = Some(serde_json::from_str(source).unwrap());
            assert!(receipt.validate().is_err());
            let mut catalog = crate::plugins::testing::catalog().catalog;
            catalog.plugins[0].icon.clone_from(&receipt.manifest.icon);
            assert!(catalog.validate().is_err());
        }
        let element = PluginIconElement::Path { d: "M0 0".into() };
        assert!(
            render(&PluginIcon {
                elements: vec![element.clone(); 65]
            })
            .is_err()
        );
        assert!(
            render(&PluginIcon {
                elements: vec![PluginIconElement::Path {
                    d: "M0 0 ".repeat(2000)
                }]
            })
            .is_err()
        );
    }
}
