use supra_theme::Theme;

/// A bordered panel: title plus body lines. No border is drawn here;
/// the terminal draws them (box-drawing under the theme). This crate
/// only formats the title and the row text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panel {
    /// The title line.
    pub title: String,
    /// The body lines.
    pub body: Vec<String>,
}

impl Panel {
    /// Create a panel with a title and body lines.
    #[must_use]
    pub fn new(title: impl Into<String>, body: Vec<String>) -> Self {
        Self { title: title.into(), body }
    }

    /// Render as a newline-joined string, the panel's content without
    /// the frame - the frame belongs to the terminal layer.
    #[must_use]
    pub fn render(&self, theme: &Theme) -> String {
        let mut out = theme.paint(supra_theme::Token::Accent, &self.title);
        for line in &self.body {
            out.push('\n');
            out.push_str(theme.paint(supra_theme::Token::Plain, line).as_str());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use supra_ffi::width::Ambiguous;

    #[test]
    fn a_panel_joins_its_rows_under_the_title() {
        let theme = Theme::new(
            "probe",
            Ambiguous::Narrow,
            [
                (supra_theme::Token::Plain, &[]),
                (supra_theme::Token::Input, &[]),
                (supra_theme::Token::Answer, &[]),
                (supra_theme::Token::Thinking, &[]),
                (supra_theme::Token::Tool, &[]),
                (supra_theme::Token::Error, &[]),
                (supra_theme::Token::Muted, &[]),
                (supra_theme::Token::Accent, &[]),
            ],
        );
        let panel = Panel::new("Plugins", vec!["alpha".to_owned(), "beta".to_owned()]);
        let rendered = panel.render(&theme);
        assert_eq!(rendered, "Plugins\nalpha\nbeta");
    }

    #[test]
    fn an_empty_panel_is_its_title_alone() {
        let theme = Theme::default_dark();
        let panel = Panel::new("Errors", Vec::new());
        let rendered = panel.render(&theme);
        assert!(rendered.contains("Errors"));
        assert!(!rendered.contains('\n'));
    }
}
