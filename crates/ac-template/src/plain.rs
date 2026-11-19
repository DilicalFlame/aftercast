//! Markdown to YouTube plain text (specs/08-readme-and-description.md).

use ac_core::config::LinkStyle;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Converts rendered markdown to a sanitised plain-text description.
pub fn to_plain(markdown: &str, link_style: LinkStyle) -> String {
    let mut out = String::new();
    // Ordered-list counters (None = bullet list), innermost last.
    let mut lists: Vec<Option<u64>> = Vec::new();
    // (line start, text start) of open list items, to drop empty ones.
    let mut items: Vec<(usize, usize)> = Vec::new();
    let mut links: Vec<(String, usize)> = Vec::new();
    let mut in_code = false;

    for event in Parser::new_ext(
        markdown,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    ) {
        match event {
            Event::Start(Tag::List(start)) => {
                end_line(&mut out);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    out.push('\n');
                }
            }
            Event::Start(Tag::Item) => {
                end_line(&mut out);
                let line_start = out.len();
                let indent = "  ".repeat(lists.len().saturating_sub(1));
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => "• ".to_owned(),
                };
                out.push_str(&indent);
                out.push_str(&marker);
                items.push((line_start, out.len()));
            }
            Event::End(TagEnd::Item) => {
                let (line_start, text_start) = items.pop().unwrap_or((out.len(), out.len()));
                if out[text_start..].trim().is_empty() {
                    out.truncate(line_start); // empty item: drop its marker too
                } else {
                    end_line(&mut out);
                }
            }
            Event::End(TagEnd::Paragraph) => {
                out.push('\n');
                if lists.is_empty() {
                    out.push('\n');
                }
            }
            Event::End(TagEnd::Heading(_)) => out.push_str("\n\n"),
            Event::Start(Tag::CodeBlock(kind)) => {
                end_line(&mut out);
                in_code = matches!(kind, CodeBlockKind::Fenced(_) | CodeBlockKind::Indented);
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                out.push('\n');
            }
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                links.push((dest_url.into_string(), out.len()));
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                if let Some((url, start)) = links.pop() {
                    let text = out[start..].to_owned();
                    if text.is_empty() {
                        out.push_str(&url);
                    } else if text != url {
                        match link_style {
                            LinkStyle::TextColonUrl => out.push_str(&format!(": {url}")),
                            LinkStyle::TextParenUrl => out.push_str(&format!(" ({url})")),
                        }
                    }
                }
            }
            Event::End(TagEnd::TableCell) => out.push_str(" | "),
            Event::End(TagEnd::TableHead | TagEnd::TableRow) => {
                let trimmed = out.trim_end_matches(" | ").len();
                out.truncate(trimmed);
                out.push('\n');
            }
            Event::End(TagEnd::Table) | Event::Rule => out.push('\n'),
            Event::Text(text) if in_code => {
                for line in text.lines() {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            Event::Text(text) | Event::Code(text) => out.push_str(&text),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            // HTML (including `<!-- private: ... -->` notes) is removed.
            _ => {}
        }
    }
    sanitize(&out)
}

/// Starts a new line unless already at one.
fn end_line(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Replaces `<`/`>` (rejected by the API), trims lines and collapses 3+ newlines to 2.
fn sanitize(text: &str) -> String {
    let text = text
        .replace("\r\n", "\n")
        .replace('<', "‹")
        .replace('>', "›");
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_start_matches('\n').trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(md: &str) -> String {
        to_plain(md, LinkStyle::TextColonUrl)
    }

    #[test]
    fn headings_paragraphs_and_inline() {
        assert_eq!(
            plain("# Title\n\nSome **bold** and *it* `code`.\nNext line.\n\n## Sub\ntext"),
            "Title\n\nSome bold and it code.\nNext line.\n\nSub\n\ntext"
        );
    }

    #[test]
    fn links_images_and_bare_urls() {
        assert_eq!(plain("[GitHub](https://g.h/x)"), "GitHub: https://g.h/x");
        assert_eq!(
            to_plain("[GitHub](https://g.h/x)", LinkStyle::TextParenUrl),
            "GitHub (https://g.h/x)"
        );
        assert_eq!(plain("![logo](https://i/l.png)"), "logo: https://i/l.png");
        assert_eq!(
            plain("see https://example.com now"),
            "see https://example.com now"
        );
        assert_eq!(plain("<https://example.com>"), "https://example.com");
    }

    #[test]
    fn lists_and_empty_items() {
        assert_eq!(
            plain("- a\n- b\n  - c\n\n1. one\n2. two"),
            "• a\n• b\n  • c\n\n1. one\n2. two"
        );
        assert_eq!(plain("## Doing\n\n- \n\n## Next"), "Doing\n\nNext");
    }

    #[test]
    fn code_blocks_are_indented() {
        assert_eq!(
            plain("```rust\nfn main() {}\nlet x = 1;\n```\nafter"),
            "  fn main() {}\n  let x = 1;\n\nafter"
        );
    }

    #[test]
    fn html_and_private_comments_are_removed() {
        assert_eq!(
            plain("<!-- private: secret plan -->\nvisible <b>bold</b>"),
            "visible bold"
        );
        let out = plain("a < b and c > d, <b>tag</b>");
        assert!(!out.contains(['<', '>']), "{out}");
        assert_eq!(out, "a ‹ b and c › d, tag");
    }

    #[test]
    fn tables_join_cells() {
        assert_eq!(plain("| a | b |\n|---|---|\n| 1 | 2 |"), "a | b\n1 | 2");
    }

    #[test]
    fn blank_lines_collapse() {
        assert_eq!(plain("a\n\n\n\n\nb\r\n"), "a\n\nb");
    }
}
