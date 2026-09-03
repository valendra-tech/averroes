//! Convert GitHub-flavored Markdown into Telegram HTML.
//!
//! Telegram does not render GFM. `parse_mode=HTML` accepts a small tag set, so
//! this module walks CommonMark events and emits only those tags.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

pub fn markdown_to_telegram_html(markdown: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(markdown, options);
    let mut out = String::with_capacity(markdown.len());
    let mut lists = Vec::new();
    let mut skip_image_alt = false;

    for event in parser {
        if skip_image_alt {
            match event {
                Event::End(TagEnd::Image) => skip_image_alt = false,
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(tag) => start_tag(&mut out, &mut lists, &mut skip_image_alt, tag),
            Event::End(tag) => end_tag(&mut out, tag),
            Event::Text(text) => push_escaped(&mut out, &text),
            Event::Code(text) => {
                out.push_str("<code>");
                push_escaped(&mut out, &text);
                out.push_str("</code>");
            }
            Event::Html(html) | Event::InlineHtml(html) => push_escaped(&mut out, &html),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => out.push_str("────────\n"),
            Event::TaskListMarker(checked) => {
                out.push_str(if checked { "✅ " } else { "☐ " });
            }
            Event::FootnoteReference(label) => {
                out.push('[');
                push_escaped(&mut out, &label);
                out.push(']');
            }
        }
    }

    collapse_blank_lines(out.trim_end())
}

pub fn telegram_html_chunks(markdown: &str, max_chars: usize) -> Vec<String> {
    if markdown.is_empty() {
        return Vec::new();
    }
    let full = markdown_to_telegram_html(markdown);
    if char_len(&full) <= max_chars {
        return vec![full];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for part in markdown.split_inclusive("\n\n") {
        let candidate = format!("{current}{part}");
        let html = markdown_to_telegram_html(&candidate);
        if char_len(&html) <= max_chars || current.is_empty() {
            current = candidate;
            if char_len(&html) > max_chars {
                if !current.trim().is_empty() {
                    chunks.extend(split_chars(&html, max_chars));
                }
                current.clear();
            }
        } else {
            chunks.push(markdown_to_telegram_html(&current));
            current = part.to_string();
        }
    }
    if !current.trim().is_empty() {
        let html = markdown_to_telegram_html(&current);
        if char_len(&html) <= max_chars {
            chunks.push(html);
        } else {
            chunks.extend(split_chars(&html, max_chars));
        }
    }
    chunks
}

fn start_tag(
    out: &mut String,
    lists: &mut Vec<ListState>,
    skip_image_alt: &mut bool,
    tag: Tag<'_>,
) {
    match tag {
        Tag::Paragraph | Tag::HtmlBlock => {}
        Tag::Heading { .. } => out.push_str("<b>"),
        Tag::BlockQuote(_) => out.push_str("<blockquote>"),
        Tag::CodeBlock(kind) => {
            out.push_str("<pre>");
            if let CodeBlockKind::Fenced(lang) = kind {
                let lang = lang.trim();
                if !lang.is_empty()
                    && lang
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '+' || ch == '-' || ch == '_')
                {
                    out.push_str("<code class=\"language-");
                    out.push_str(lang);
                    out.push_str("\">");
                    return;
                }
            }
            out.push_str("<code>");
        }
        Tag::List(start) => lists.push(ListState {
            next: start.unwrap_or(0),
            ordered: start.is_some(),
        }),
        Tag::Item => {
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            match lists.last_mut() {
                Some(list) if list.ordered => {
                    out.push_str(&format!("{}. ", list.next));
                    list.next += 1;
                }
                _ => out.push_str("• "),
            }
        }
        Tag::Emphasis => out.push_str("<i>"),
        Tag::Strong => out.push_str("<b>"),
        Tag::Strikethrough => out.push_str("<s>"),
        Tag::Link { dest_url, .. } => {
            out.push_str("<a href=\"");
            push_attr(out, dest_url.as_ref());
            out.push_str("\">");
        }
        Tag::Image { dest_url, .. } => {
            out.push_str("<a href=\"");
            push_attr(out, dest_url.as_ref());
            out.push_str("\">");
            push_escaped(out, dest_url.as_ref());
            out.push_str("</a>");
            *skip_image_alt = true;
        }
        Tag::Table(_)
        | Tag::TableHead
        | Tag::TableRow
        | Tag::TableCell
        | Tag::FootnoteDefinition(_)
        | Tag::DefinitionList
        | Tag::DefinitionListTitle
        | Tag::DefinitionListDefinition
        | Tag::MetadataBlock(_)
        | Tag::Superscript
        | Tag::Subscript => {}
    }
}

fn end_tag(out: &mut String, tag: TagEnd) {
    match tag {
        TagEnd::Paragraph => {
            if !out.ends_with("\n\n") {
                out.push_str("\n\n");
            }
        }
        TagEnd::Heading(_) => out.push_str("</b>\n\n"),
        TagEnd::BlockQuote(_) => out.push_str("</blockquote>\n"),
        TagEnd::CodeBlock => out.push_str("</code></pre>\n\n"),
        TagEnd::List(_) => out.push('\n'),
        TagEnd::Item => {}
        TagEnd::Emphasis => out.push_str("</i>"),
        TagEnd::Strong => out.push_str("</b>"),
        TagEnd::Strikethrough => out.push_str("</s>"),
        TagEnd::Link => out.push_str("</a>"),
        TagEnd::HtmlBlock => out.push('\n'),
        _ => {}
    }
}

struct ListState {
    next: u64,
    ordered: bool,
}

fn push_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

fn push_attr(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

fn collapse_blank_lines(text: &str) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut newlines = 0;
    for ch in text.chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines <= 2 {
                collapsed.push(ch);
            }
        } else {
            newlines = 0;
            collapsed.push(ch);
        }
    }
    collapsed
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn split_chars(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if current.chars().count() >= max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(character);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::{markdown_to_telegram_html, telegram_html_chunks};

    #[test]
    fn converts_common_github_markdown() {
        let html = markdown_to_telegram_html(
            "# Title\n\nHello **world** and *italics* and `code`.\n\n- one\n- two\n\n```rust\nlet x = 1;\n```\n\nSee [docs](https://example.com).",
        );
        assert!(html.contains("<b>Title</b>"));
        assert!(html.contains("<b>world</b>"));
        assert!(html.contains("<i>italics</i>"));
        assert!(html.contains("<code>code</code>"));
        assert!(html.contains("• one"));
        assert!(html.contains("<pre><code class=\"language-rust\">"));
        assert!(html.contains("let x = 1;"));
        assert!(html.contains("<a href=\"https://example.com\">docs</a>"));
    }

    #[test]
    fn escapes_html_from_user_text() {
        let html = markdown_to_telegram_html("use <script> & tags");
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&amp;"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn chunks_stay_within_limit() {
        let markdown = "# A\n\npara one\n\n# B\n\npara two";
        let chunks = telegram_html_chunks(markdown, 24);
        assert!(chunks.len() >= 2);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 24));
    }
}
