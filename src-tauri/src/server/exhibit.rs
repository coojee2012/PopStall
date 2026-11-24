use super::catalog::Theme;

pub fn render_exhibit(theme: Theme, title: &str) -> String {
    include_str!("exhibit.html")
        .replace("{{TITLE}}", &escape_html(title))
        .replace("{{THEME}}", theme.as_str())
}

pub fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
