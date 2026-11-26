//! Package icons for `[[contributes.markerIcons]]` and `[[contributes.fileIcons]]`
//! (T2-5).
//!
//! The SVGs are embedded so the tests validate exactly the bytes the package
//! ships: 16×16, no scripts, no external references, under the 64 KiB limit.
//! The navigation markers and the mapper file icon are the IntelliJ edition's
//! own artwork (`mapperStatement` / `mapperMethod` / `mapperXml`), copied as-is:
//! the two markers only exist in a light variant upstream, so the dark slot
//! points at the same file.

/// XML-side statement marker (`<select>` / `<insert>` / …), cyan.
pub const NAVIGATE_XML: &str = include_str!("../../icons/toolbar/mapperStatement.svg");
/// Java-side mapper method marker, same artwork in the method colour.
pub const NAVIGATE_JAVA: &str = include_str!("../../icons/toolbar/mapperMethod.svg");
/// File icon for mapper XML files (the IntelliJ edition's `mapperXml`).
pub const MAPPER_XML: &str = include_str!("../../icons/mapper-xml.svg");
pub const MAPPER_XML_DARK: &str = include_str!("../../icons/mapper-xml-dark.svg");

/// `(id, light, dark)` for every declared icon, in registration order.
pub const ALL: &[(&str, &str, &str)] = &[
    ("sqlv-nav-xml", NAVIGATE_XML, NAVIGATE_XML),
    ("sqlv-nav-java", NAVIGATE_JAVA, NAVIGATE_JAVA),
    ("mapper-xml", MAPPER_XML, MAPPER_XML_DARK),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Fragments the host's SVG sanitiser rejects (or that would make an icon
    /// depend on anything outside the package). Internal gradient references
    /// (`url(#id)`) are allowed and checked separately.
    const FORBIDDEN: &[&str] = &[
        "<script",
        "<image",
        "foreignObject",
        "onload",
        "onclick",
        "xlink:href",
        "href=",
        "javascript:",
    ];

    #[test]
    fn every_icon_is_a_16px_svg() {
        for (id, light, dark) in ALL {
            for (variant, svg) in [("light", *light), ("dark", *dark)] {
                // Upstream files start with a licence or design comment.
                assert!(
                    svg.trim_start().starts_with("<svg") || svg.trim_start().starts_with("<!--"),
                    "{id} ({variant}) is an SVG document"
                );
                assert!(svg.contains("<svg"), "{id} ({variant})");
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
    fn icons_carry_no_scripts_or_external_references() {
        for (id, light, dark) in ALL {
            for (variant, svg) in [("light", *light), ("dark", *dark)] {
                for forbidden in FORBIDDEN {
                    assert!(
                        !svg.contains(forbidden),
                        "{id} ({variant}) must not contain {forbidden}"
                    );
                }
                // `url(...)` is fine only when it names a gradient defined in the
                // same document; anything else is an external dependency.
                for (at, _) in svg.match_indices("url(") {
                    assert!(
                        svg[at..].starts_with("url(#"),
                        "{id} ({variant}) references something outside the file"
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
