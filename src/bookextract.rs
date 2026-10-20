use anyhow::{Context, Result};
use std::fmt;
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceKind {
    Epub,
    Pdf,
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceKind::Epub => f.write_str("epub"),
            SourceKind::Pdf => f.write_str("pdf"),
        }
    }
}

impl SourceKind {
    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "epub" => Some(SourceKind::Epub),
            "pdf" => Some(SourceKind::Pdf),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SourceKind::Epub => "EPUB",
            SourceKind::Pdf => "PDF",
        }
    }
}

pub struct Chapter {
    pub title: String,
    pub text: String,
}

pub struct Extracted {
    pub title: String,
    pub chapters: Vec<Chapter>,
}

/// Magic bytes : un EPUB est une archive ZIP (PK), un PDF commence par %PDF.
pub fn magic_matches(kind: SourceKind, data: &[u8]) -> bool {
    match kind {
        SourceKind::Epub => data.len() >= 2 && &data[..2] == b"PK",
        SourceKind::Pdf => data.len() >= 4 && &data[..4] == b"%PDF",
    }
}

pub fn extract(path: &Path, kind: SourceKind) -> Result<Extracted> {
    match kind {
        SourceKind::Epub => extract_epub(path),
        SourceKind::Pdf => extract_pdf(path),
    }
}

fn extract_epub(path: &Path) -> Result<Extracted> {
    let mut doc = epub::doc::EpubDoc::new(path)
        .with_context(|| format!("ouverture EPUB {}", path.display()))?;
    let title = doc
        .mdata("title")
        .map(|m| m.value.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "EPUB".into())
        });
    let mut chapters = Vec::new();
    let mut guard = 0;
    loop {
        if let Some((html, _mime)) = doc.get_current_str() {
            let title_ch = extract_html_title(&html)
                .unwrap_or_else(|| format!("Chapitre {}", chapters.len() + 1));
            let text = html_to_text(&html);
            if !text.trim().is_empty() {
                chapters.push(Chapter {
                    title: title_ch,
                    text,
                });
            }
        } else {
            break;
        }
        if !doc.go_next() {
            break;
        }
        guard += 1;
        if guard > 5000 {
            break;
        }
    }
    if chapters.is_empty() {
        anyhow::bail!("aucun chapitre lisible dans cet EPUB");
    }
    Ok(Extracted { title, chapters })
}

fn extract_pdf(path: &Path) -> Result<Extracted> {
    let pages = pdf_extract::extract_text_by_pages(path)
        .with_context(|| format!("extraction du texte PDF {}", path.display()))?;
    let title = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "PDF".into());
    let chapters = pages
        .into_iter()
        .enumerate()
        .filter(|(_, t)| !t.trim().is_empty())
        .map(|(i, t)| Chapter {
            title: format!("Page {}", i + 1),
            text: t,
        })
        .collect::<Vec<_>>();
    Ok(Extracted { title, chapters })
}

/// Supprime les blocs <style> et <script> avant toute extraction de texte :
/// leur contenu n'est pas du texte utile et pollue l'affichage.
fn strip_hidden_blocks(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        let rest_lower = &lower[i..];
        let cut = if rest_lower.starts_with("<style") {
            rest.find("</style>").map(|p| p + 8)
        } else if rest_lower.starts_with("<script") {
            rest.find("</script>").map(|p| p + 9)
        } else {
            None
        };
        match cut {
            Some(n) => i += n,
            None => {
                let next = rest.find('<').unwrap_or(rest.len()).max(1);
                out.push_str(&rest[..next.min(rest.len())]);
                i += next;
            }
        }
    }
    out
}

fn extract_html_title(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let rest = &html[start..];
    let gt = rest.find('>')?;
    let close = rest[gt..].find("</title>")?;
    let t = html_unescape(rest[gt + 1..gt + close].trim());
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}

pub fn html_to_text(html: &str) -> String {
    let html = &strip_hidden_blocks(html);
    let mut out = String::new();
    let mut i = 0;
    let mut list_depth = 0usize;
    while i < html.len() {
        if html[i..].starts_with('<') {
            let rest = &html[i..];
            let close = rest.find('>').unwrap_or(rest.len());
            let tag_full = rest[1..close].to_string();
            let tag = tag_full
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            match tag.as_str() {
                "p" | "div" | "section" | "br" | "tr" => out.push_str("\n\n"),
                "li" => {
                    out.push('\n');
                    for _ in 0..list_depth {
                        out.push_str("  ");
                    }
                    out.push_str("- ");
                }
                "ul" | "ol" => list_depth += 1,
                "/ul" | "/ol" => list_depth = list_depth.saturating_sub(1),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => out.push_str("\n\n"),
                "img" => {
                    let lower = tag_full.to_ascii_lowercase();
                    if let Some(pos) = lower.find("alt=") {
                        let rem = tag_full[pos + 4..].trim_matches('"');
                        out.push_str(&format!("[image: {}]", html_unescape(rem)));
                    }
                }
                _ => {}
            }
            i += close + 1;
        } else {
            let rest = &html[i..];
            let next = rest.find('<').unwrap_or(rest.len());
            out.push_str(&html_unescape(&rest[..next]));
            i += next;
        }
    }
    out
}

/// Markdown → HTML rendu et assaini (ammonia : liste blanche de balises,
/// neutralise XSS). Utilisé pour l'aperçu côté serveur.
pub fn markdown_to_html(md: &str) -> String {
    use pulldown_cmark::{html, Options, Parser};
    let parser = Parser::new_ext(md, Options::all());
    let mut buf = String::new();
    html::push_html(&mut buf, parser);
    let mut builder = ammonia::Builder::new();
    builder.url_relative(ammonia::UrlRelative::PassThrough);
    builder.clean(&buf).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_style_and_script() {
        let html = "<html><head><style>.a{color:red}</style></head><body><p>OK</p><script>x()</script></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("OK"));
        assert!(!text.contains("color"));
        assert!(!text.contains("x()"));
    }

    #[test]
    fn magic_bytes() {
        assert!(magic_matches(SourceKind::Epub, b"PK\x03\x04"));
        assert!(magic_matches(SourceKind::Pdf, b"%PDF-1.7"));
        assert!(!magic_matches(SourceKind::Pdf, b"PK\x03\x04"));
    }

    #[test]
    fn markdown_sanitized() {
        let html = markdown_to_html("salut <script>alert(1)</script> **toi**");
        assert!(html.contains("<strong>toi</strong>"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn title_extracted() {
        assert_eq!(
            extract_html_title("<title>Chap 1</title>").as_deref(),
            Some("Chap 1")
        );
    }
}
