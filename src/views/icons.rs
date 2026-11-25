//! Package icons for `[[contributes.markerIcons]]` and `[[contributes.fileIcons]]`
//! (T2-5).
//!
//! The SVGs are embedded so the tests validate exactly the bytes the package
//! ships: 16×16, no scripts, no external references, under the 64 KiB limit.

/// Mapper marker (a mapper interface or XML namespace).
pub const MARKER_MAPPER: &str = include_str!("../../icons/marker-mapper.svg");
pub const MARKER_MAPPER_DARK: &str = include_str!("../../icons/marker-mapper-dark.svg");
/// Statement marker (`<select>` / `<insert>` / …).
pub const MARKER_STATEMENT: &str = include_str!("../../icons/marker-statement.svg");
pub const MARKER_STATEMENT_DARK: &str = include_str!("../../icons/marker-statement-dark.svg");
/// Result-map marker.
pub const MARKER_RESULTMAP: &str = include_str!("../../icons/marker-resultmap.svg");
pub const MARKER_RESULTMAP_DARK: &str = include_str!("../../icons/marker-resultmap-dark.svg");
/// File icon for mapper XML files.
pub const MAPPER_XML: &str = include_str!("../../icons/mapper-xml.svg");
pub const MAPPER_XML_DARK: &str = include_str!("../../icons/mapper-xml-dark.svg");

/// `(id, light, dark)` for every declared icon, in registration order.
pub const ALL: &[(&str, &str, &str)] = &[
    ("marker-mapper", MARKER_MAPPER, MARKER_MAPPER_DARK),
    ("marker-statement", MARKER_STATEMENT, MARKER_STATEMENT_DARK),
    ("marker-resultmap", MARKER_RESULTMAP, MARKER_RESULTMAP_DARK),
    ("mapper-xml", MAPPER_XML, MAPPER_XML_DARK),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Fragments the host's SVG sanitiser rejects (or that would make an icon
    /// depend on anything outside the package).
    const FORBIDDEN: &[&str] = &[
        "<script",
        "<image",
        "foreignObject",
        "onload",
        "onclick",
        "xlink:href",
        "href=",
        "url(",
        "javascript:",
    ];

    #[test]
    fn every_icon_is_a_16px_svg() {
        for (id, light, dark) in ALL {
            for (variant, svg) in [("light", *light), ("dark", *dark)] {
                assert!(
                    svg.trim_start().starts_with("<svg"),
                    "{id} ({variant}) is an SVG document"
                );
                assert!(svg.contains("xmlns=\"http://www.w3.org/2000/svg\""), "{id}");
                assert!(
                    svg.contains("width=\"16\""),
                    "{id} ({variant}) is 16px wide"
                );
                assert!(
                    svg.contains("height=\"16\""),
                    "{id} ({variant}) is 16px tall"
                );
                assert!(
                    svg.contains("viewBox=\"0 0 16 16\""),
                    "{id} ({variant}) scales from a 16x16 view box"
                );
            }
        }
    }

    #[test]
    fn every_icon_has_a_distinct_dark_variant() {
        for (id, light, dark) in ALL {
            assert_ne!(light, dark, "{id} ships a separate dark variant");
        }
    }

    #[test]
    fn icons_carry_no_scripts_or_external_references() {
        for (id, light, dark) in ALL {
            for (variant, svg) in [("light", *light), ("dark", *dark)] {
                for forbidden in FORBIDDEN {
                    assert!(
                        !svg.contains(forbidden),
                        "{id} ({variant}) must not contain {forbidden}"
                    );
                }
            }
        }
    }

    #[test]
    fn icons_stay_under_the_host_size_limit() {
        for (id, light, dark) in ALL {
            for svg in [*light, *dark] {
                assert!(
                    svg.len() < 64 * 1024,
                    "{id} is under the 64 KiB marker-icon limit"
                );
            }
        }
    }
}
