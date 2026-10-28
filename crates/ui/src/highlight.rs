use std::sync::OnceLock;

use egui::text::{ByteIndex, LayoutJob, LayoutSection, TextFormat};
use egui::{Color32, FontId};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::theme::Palette;

// code is coloured from the app theme's terminal colours, so it matches whichever theme is picked
fn code_theme(p: &Palette, dark: bool) -> Theme {
    // the bright terminal colours read better on dark backgrounds and the normal ones on light
    let ansi = |i: usize| p.ansi[if dark { i + 8 } else { i }];
    let color = |c: Color32| Color {
        r: c.r(),
        g: c.g(),
        b: c.b(),
        a: 255,
    };
    let rules: [(&str, Color32, FontStyle); 12] = [
        (
            "comment, punctuation.definition.comment",
            p.text_faint,
            FontStyle::ITALIC,
        ),
        (
            "string, constant.character, markup.inline.raw, markup.raw",
            ansi(2),
            FontStyle::empty(),
        ),
        (
            "constant.numeric, constant.language, constant.character.escape, constant.other",
            ansi(6),
            FontStyle::empty(),
        ),
        (
            "keyword, storage, keyword.operator.new, keyword.operator.word, variable.language",
            ansi(5),
            FontStyle::empty(),
        ),
        (
            "keyword.operator, punctuation",
            p.text_muted,
            FontStyle::empty(),
        ),
        (
            "entity.name.function, support.function, meta.function-call variable.function, variable.function",
            ansi(4),
            FontStyle::empty(),
        ),
        (
            "entity.name.type, entity.name.class, entity.other.inherited-class, support.type, support.class, storage.type.primitive",
            ansi(3),
            FontStyle::empty(),
        ),
        (
            "entity.name.tag, support.type.property-name, meta.mapping.key string, entity.name.section",
            ansi(1),
            FontStyle::empty(),
        ),
        ("entity.other.attribute-name", ansi(3), FontStyle::empty()),
        ("markup.heading", ansi(4), FontStyle::BOLD),
        ("markup.bold", p.text, FontStyle::BOLD),
        ("invalid, markup.deleted", p.danger, FontStyle::empty()),
    ];
    Theme {
        settings: ThemeSettings {
            foreground: Some(color(p.text)),
            ..ThemeSettings::default()
        },
        scopes: rules
            .into_iter()
            .filter_map(|(scope, c, style)| {
                Some(ThemeItem {
                    scope: scope.parse::<ScopeSelectors>().ok()?,
                    style: StyleModifier {
                        foreground: Some(color(c)),
                        background: None,
                        font_style: Some(style),
                    },
                })
            })
            .collect(),
        ..Theme::default()
    }
}

// colours code named by a file extension or a language name, built here rather than with egui_extras so it can run on another thread
pub fn colorize(
    text: &str,
    lang: &str,
    p: &Palette,
    dark: bool,
    font: FontId,
) -> Option<LayoutJob> {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    let syntaxes = SYNTAXES.get_or_init(two_face::syntax::extra_newlines);
    let syntax = syntaxes.find_syntax_by_token(lang)?;
    let theme = code_theme(p, dark);
    let mut highlighter = HighlightLines::new(syntax, &theme);
    let mut job = LayoutJob {
        text: text.to_owned(),
        ..Default::default()
    };
    let base = text.as_ptr() as usize;
    for line in LinesWithEndings::from(text) {
        for (style, piece) in highlighter.highlight_line(line, syntaxes).ok()? {
            let start = piece.as_ptr() as usize - base;
            let fg = style.foreground;
            job.sections.push(LayoutSection {
                leading_space: 0.0,
                byte_range: ByteIndex(start)..ByteIndex(start + piece.len()),
                format: TextFormat {
                    font_id: font.clone(),
                    color: Color32::from_rgb(fg.r, fg.g, fg.b),
                    italics: style.font_style.contains(FontStyle::ITALIC),
                    ..Default::default()
                },
            });
        }
    }
    Some(job)
}

#[cfg(test)]
mod tests {
    use super::{code_theme, colorize};
    use crate::theme::{DARK, font};

    #[test]
    fn typescript_is_coloured_from_the_theme() {
        assert_eq!(code_theme(&DARK, true).scopes.len(), 12);
        let job = colorize(
            "const a: string = \"x\";\n",
            "ts",
            &DARK,
            true,
            font::mono(12.0),
        )
        .unwrap();
        let keyword = job.sections.first().unwrap().format.color;
        assert_eq!(keyword, DARK.ansi[13]);
        assert!(job.sections.iter().any(|s| s.format.color == DARK.ansi[10]));
        for ext in ["tsx", "toml", "rs", "py", "go", "md", "json", "yaml", "sh"] {
            assert!(
                colorize("x\n", ext, &DARK, true, font::mono(12.0)).is_some(),
                "{ext}"
            );
        }
    }
}
