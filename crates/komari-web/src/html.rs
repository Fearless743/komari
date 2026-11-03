//! Single-pass HTML replacements for the SPA entry point.
//!
//! The Go backend builds a `strings.NewReplacer` for the title, description,
//! custom head and custom body. A single pass matters: injected content must
//! not be re-scanned by later replacements. This module reproduces that
//! behaviour.

use crate::server::SiteMeta;

pub const DEFAULT_TITLE: &str = "<title>Komari Monitor</title>";
pub const DEFAULT_DESCRIPTION: &str = "A simple server monitor tool.";

/// Apply the four site replacements in one left-to-right pass.
pub fn apply_site_replacements(input: &str, meta: &SiteMeta) -> String {
    let pairs: [(&str, String); 4] = [
        (DEFAULT_TITLE, format!("<title>{}</title>", meta.sitename)),
        (DEFAULT_DESCRIPTION, meta.description.clone()),
        ("</head>", format!("{}</head>", meta.custom_head)),
        ("</body>", format!("{}</body>", meta.custom_body)),
    ];

    let mut output = String::with_capacity(input.len() + 64);
    let mut index = 0;

    while index < input.len() {
        let rest = &input[index..];
        if let Some((pattern, replacement)) =
            pairs.iter().find(|(pattern, _)| rest.starts_with(*pattern))
        {
            output.push_str(replacement);
            index += pattern.len();
            continue;
        }

        let character = rest
            .chars()
            .next()
            .expect("index always points at a character boundary");
        output.push(character);
        index += character.len_utf8();
    }

    output
}
