use super::theme::UiTheme;
use gpui::*;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

pub fn render_markdown(theme: UiTheme, content: &str) -> Div {
    let parser = Parser::new_ext(content, Options::all());
    let elements = build_document(parser, theme);
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .children(elements)
}

fn build_document<'a>(parser: Parser<'a>, theme: UiTheme) -> Vec<Div> {
    let mut blocks: Vec<Div> = Vec::new();
    let mut current_block: Vec<Div> = Vec::new();
    let mut inline_stack: Vec<InlineStyle> = Vec::new();
    let mut buffer = String::new();
    let mut in_code_block = false;
    let mut code_block_buf = String::new();
    let mut heading_level: Option<u8> = None;
    let mut in_blockquote = false;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading {
                    level,
                    id: _,
                    classes: _,
                    attrs: _,
                } => {
                    heading_level = Some(level as u8);
                }
                Tag::CodeBlock(kind) => {
                    in_code_block = true;
                    code_block_buf.clear();
                    if let CodeBlockKind::Fenced(lang) = kind {
                        current_block.push(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .font(UiTheme::mono_font())
                                .mb(px(2.0))
                                .child(lang.to_string()),
                        );
                    }
                }
                Tag::BlockQuote(_) => {
                    in_blockquote = true;
                }
                Tag::List(_) => {}
                Tag::Item => {
                    buffer.clear();
                }
                Tag::Emphasis => inline_stack.push(InlineStyle::Italic),
                Tag::Strong => inline_stack.push(InlineStyle::Bold),
                Tag::Strikethrough => inline_stack.push(InlineStyle::Strikethrough),
                _ => {}
            },

            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    if !buffer.trim().is_empty() {
                        let block = build_text_block(
                            &buffer, &inline_stack, theme,
                            heading_level.take(), in_blockquote,
                        );
                        current_block.push(block);
                    }
                    buffer.clear();
                    inline_stack.clear();
                }
                TagEnd::Heading(_) => {
                    if !buffer.trim().is_empty() {
                        let block = build_text_block(
                            &buffer, &inline_stack, theme,
                            heading_level.take(), in_blockquote,
                        );
                        current_block.push(block);
                    }
                    buffer.clear();
                    inline_stack.clear();
                }
                TagEnd::CodeBlock => {
                    in_code_block = false;
                    let code = code_block_buf.trim_end().to_string();
                    current_block.push(
                        div()
                            .w_full()
                            .min_w(px(0.0))
                            .rounded(px(UiTheme::RADIUS))
                            .bg(theme.accent)
                            .border_1()
                            .border_color(theme.border)
                            .px(px(12.0))
                            .py(px(8.0))
                            .font(UiTheme::mono_font())
                            .text_xs()
                            .whitespace_normal()
                            .child(code),
                    );
                    code_block_buf.clear();
                }
                TagEnd::BlockQuote(_) => {
                    in_blockquote = false;
                }
                TagEnd::List(_) => {
                    if !buffer.trim().is_empty() {
                        blocks.push(build_text_block(&buffer, &[], theme, None, false));
                        buffer.clear();
                    }
                    if !current_block.is_empty() {
                        blocks.push(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .children(std::mem::take(&mut current_block)),
                        );
                    }
                }
                TagEnd::Item => {
                    if !buffer.trim().is_empty() {
                        current_block.push(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(
                                    div()
                                        .text_color(theme.muted_foreground)
                                        .flex_none()
                                        .child("\u{2022}"),
                                )
                                .child(build_text_block(
                                    &buffer, &inline_stack, theme, None, false,
                                )),
                        );
                        buffer.clear();
                    }
                }
                TagEnd::Emphasis => {
                    inline_stack.retain(|s| *s != InlineStyle::Italic);
                }
                TagEnd::Strong => {
                    inline_stack.retain(|s| *s != InlineStyle::Bold);
                }
                TagEnd::Strikethrough => {
                    inline_stack.retain(|s| *s != InlineStyle::Strikethrough);
                }
                _ => {}
            },

            Event::Text(text) => {
                if in_code_block {
                    code_block_buf.push_str(&text);
                } else {
                    buffer.push_str(&text);
                }
            }

            Event::Code(code) => {
                if !buffer.is_empty() {
                    current_block.push(build_text_block(
                        &std::mem::take(&mut buffer), &inline_stack, theme, heading_level, in_blockquote,
                    ));
                }
                current_block.push(
                    div()
                        .font(UiTheme::mono_font())
                        .text_xs()
                        .bg(theme.accent)
                        .rounded(px(3.0))
                        .px(px(4.0))
                        .py(px(1.0))
                        .child(code.trim().to_string()),
                );
            }

            Event::SoftBreak | Event::HardBreak => {
                if in_code_block {
                    code_block_buf.push('\n');
                } else {
                    buffer.push('\n');
                }
            }

            Event::Rule => {
                if !buffer.trim().is_empty() {
                    current_block.push(build_text_block(
                        &buffer, &inline_stack, theme, None, false,
                    ));
                    buffer.clear();
                }
                current_block.push(
                    div()
                        .w_full()
                        .h(px(1.0))
                        .bg(theme.border)
                        .my(px(8.0)),
                );
            }

            _ => {}
        }
    }

    if !buffer.trim().is_empty() {
        current_block.push(build_text_block(
            &buffer, &inline_stack, theme, None, false,
        ));
    }

    if !current_block.is_empty() {
        blocks.extend(current_block);
    }

    blocks
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineStyle {
    Bold,
    Italic,
    Strikethrough,
}

fn build_text_block(
    text: &str,
    styles: &[InlineStyle],
    theme: UiTheme,
    heading: Option<u8>,
    in_blockquote: bool,
) -> Div {
    let is_bold = styles.contains(&InlineStyle::Bold);
    let is_italic = styles.contains(&InlineStyle::Italic);
    let is_strikethrough = styles.contains(&InlineStyle::Strikethrough);

    let mut block = div()
        .w_full()
        .min_w(px(0.0))
        .text_color(theme.foreground)
        .font(UiTheme::ui_font());

    if heading.is_some() {
        block = block.font_weight(FontWeight::BOLD);
        if heading == Some(1) {
            block = block.text_size(px(20.0)).pt(px(8.0));
        } else if heading == Some(2) {
            block = block.text_size(px(16.0)).pt(px(6.0));
        } else {
            block = block.text_size(px(14.0)).pt(px(4.0));
        }
    } else {
        block = block.text_sm();
    }

    if is_bold && heading.is_none() {
        block = block.font_weight(FontWeight::BOLD);
    }
    if is_italic {
        block = block.italic();
    }
    if is_strikethrough {
        block = block.line_through();
    }

    if in_blockquote {
        return div()
            .flex()
            .flex_row()
            .w_full()
            .min_w(px(0.0))
            .child(
                div()
                    .w(px(3.0))
                    .flex_none()
                    .bg(theme.brand_magenta)
                    .rounded(px(2.0)),
            )
            .child(
                block
                    .pl(px(10.0))
                    .text_color(theme.muted_foreground),
            );
    }

    let lines: Vec<Div> = text
        .lines()
        .map(|line| {
            div()
                .w_full()
                .min_w(px(0.0))
                .whitespace_normal()
                .child(line.to_string())
        })
        .collect();

    block.children(lines)
}
