//! Markdown + plain-text -> `dv_flow::Block`s. Viewer-grade CommonMark subset:
//! ATX headings, fenced + indented code, blockquotes, ordered/unordered lists,
//! thematic breaks, and inline `**bold** *italic* `code` ~~strike~~ [text](url)`.

use dv_flow::{Block, Span};

/// Plain text: blank-line-separated paragraphs; lines within a block are joined
/// (prose wrapping). No inline markup.
pub fn parse_text(input: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut para = String::new();
    for line in input.replace('\r', "").lines() {
        if line.trim().is_empty() {
            flush_text(&mut para, &mut blocks);
        } else {
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(line.trim_end());
        }
    }
    flush_text(&mut para, &mut blocks);
    if blocks.is_empty() {
        blocks.push(Block::Para(vec![Span::new("")]));
    }
    blocks
}

fn flush_text(para: &mut String, blocks: &mut Vec<Block>) {
    if !para.trim().is_empty() {
        blocks.push(Block::Para(vec![Span::new(std::mem::take(para))]));
    } else {
        para.clear();
    }
}

/// Markdown -> blocks.
pub fn parse_markdown(input: &str) -> Vec<Block> {
    let text = input.replace('\r', "");
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut i = 0;

    let flush_para = |para: &mut Vec<&str>, blocks: &mut Vec<Block>| {
        if !para.is_empty() {
            let joined = para.join(" ");
            blocks.push(Block::Para(parse_inline(joined.trim())));
            para.clear();
        }
    };

    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();

        // fenced code block
        if let Some(fence) = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~")).map(|_| &t[..3]) {
            flush_para(&mut para, &mut blocks);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with(fence) {
                code.push(lines[i].to_string());
                i += 1;
            }
            i += 1; // closing fence
            blocks.push(Block::Code(code));
            continue;
        }
        // blank line ends a paragraph
        if t.is_empty() {
            flush_para(&mut para, &mut blocks);
            i += 1;
            continue;
        }
        // setext heading: a paragraph line underlined by ===/--- becomes a heading
        if !para.is_empty() {
            let u: String = t.chars().filter(|c| !c.is_whitespace()).collect();
            if !u.is_empty() && (u.chars().all(|c| c == '=') || u.chars().all(|c| c == '-')) {
                let level = if u.starts_with('=') { 1 } else { 2 };
                let joined = para.join(" ");
                blocks.push(Block::Heading(level, parse_inline(joined.trim())));
                para.clear();
                i += 1;
                continue;
            }
        }
        // indented (4-space / tab) code block — only when not continuing a paragraph
        if para.is_empty() && (line.starts_with("    ") || line.starts_with('\t')) {
            let mut code = Vec::new();
            while i < lines.len() && (lines[i].starts_with("    ") || lines[i].starts_with('\t') || lines[i].trim().is_empty()) {
                if lines[i].trim().is_empty() && (i + 1 >= lines.len() || !(lines[i + 1].starts_with("    ") || lines[i + 1].starts_with('\t'))) {
                    break;
                }
                let stripped = lines[i].strip_prefix("    ").or_else(|| lines[i].strip_prefix('\t')).unwrap_or(lines[i]);
                code.push(stripped.to_string());
                i += 1;
            }
            blocks.push(Block::Code(code));
            continue;
        }
        // thematic break
        if is_rule(t) {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::Rule);
            i += 1;
            continue;
        }
        // ATX heading
        if let Some((level, rest)) = atx_heading(t) {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::Heading(level, parse_inline(rest)));
            i += 1;
            continue;
        }
        // blockquote
        if let Some(rest) = t.strip_prefix('>') {
            flush_para(&mut para, &mut blocks);
            let mut q = String::from(rest.trim_start());
            i += 1;
            while i < lines.len() {
                let lt = lines[i].trim_start();
                if let Some(r) = lt.strip_prefix('>') {
                    q.push(' ');
                    q.push_str(r.trim_start());
                    i += 1;
                } else if lt.is_empty() {
                    break;
                } else {
                    q.push(' ');
                    q.push_str(lt);
                    i += 1;
                }
            }
            blocks.push(Block::Quote(parse_inline(q.trim())));
            continue;
        }
        // list item
        if let Some((marker, level, content)) = list_item(line) {
            flush_para(&mut para, &mut blocks);
            blocks.push(Block::ListItem { level, marker, spans: parse_inline(content) });
            i += 1;
            continue;
        }
        // paragraph text
        para.push(line.trim_end());
        i += 1;
    }
    flush_para(&mut para, &mut blocks);
    if blocks.is_empty() {
        blocks.push(Block::Para(vec![Span::new("")]));
    }
    blocks
}

fn is_rule(t: &str) -> bool {
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && (s.chars().all(|c| c == '-') || s.chars().all(|c| c == '*') || s.chars().all(|c| c == '_'))
}

fn atx_heading(t: &str) -> Option<(u8, &str)> {
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
        Some((hashes as u8, t[hashes..].trim_start().trim_end_matches('#').trim_end()))
    } else {
        None
    }
}

fn list_item(line: &str) -> Option<(String, u8, &str)> {
    let indent = line.len() - line.trim_start().len();
    let level = (indent / 2).min(5) as u8;
    let t = line.trim_start();
    // unordered
    for b in ['-', '*', '+'] {
        if let Some(rest) = t.strip_prefix(b) {
            if rest.starts_with(' ') {
                return Some(("•".to_string(), level, rest.trim_start()));
            }
        }
    }
    // ordered: "<digits>." or "<digits>)"
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let after = &t[digits.len()..];
        if let Some(rest) = after.strip_prefix('.').or_else(|| after.strip_prefix(')')) {
            if rest.starts_with(' ') {
                return Some((format!("{}.", digits), level, rest.trim_start()));
            }
        }
    }
    None
}

/// Inline parser: **bold**, *italic* / _italic_, `code`, ~~strike~~, [text](url).
fn parse_inline(s: &str) -> Vec<Span> {
    let chars: Vec<char> = s.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut i = 0;

    let push = |spans: &mut Vec<Span>, buf: &mut String, bold: bool, italic: bool| {
        if !buf.is_empty() {
            spans.push(Span { text: std::mem::take(buf), bold, italic, ..Default::default() });
        }
    };

    while i < chars.len() {
        let c = chars[i];
        let two = chars.get(i + 1).map(|&n| (c, n));
        match () {
            // backslash escape: the next char is literal
            _ if c == '\\' => {
                if let Some(&n) = chars.get(i + 1) {
                    buf.push(n);
                    i += 2;
                } else {
                    buf.push('\\');
                    i += 1;
                }
            }
            _ if two == Some(('*', '*')) || two == Some(('_', '_')) => {
                push(&mut spans, &mut buf, bold, italic);
                bold = !bold;
                i += 2;
            }
            _ if two == Some(('~', '~')) => {
                // strikethrough run; if never closed, keep the text (don't drop the last char)
                push(&mut spans, &mut buf, bold, italic);
                i += 2;
                let start = i;
                while i + 1 < chars.len() && !(chars[i] == '~' && chars[i + 1] == '~') {
                    i += 1;
                }
                let closed = i + 1 < chars.len();
                let end = if closed { i } else { chars.len() };
                let text: String = chars[start..end].iter().collect();
                spans.push(Span { text, bold, italic, strike: true, ..Default::default() });
                i = if closed { i + 2 } else { chars.len() };
            }
            // emphasis: '_' between word chars (snake_case) is NOT a delimiter
            _ if c == '*' || (c == '_' && !between_word(&chars, i)) => {
                push(&mut spans, &mut buf, bold, italic);
                italic = !italic;
                i += 1;
            }
            _ if c == '`' => {
                push(&mut spans, &mut buf, bold, italic);
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '`' {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                spans.push(Span { text, mono: true, color: Some(dv_ir::Color { r: 0xc7, g: 0x25, b: 0x4e, a: 255 }), ..Default::default() });
                i += 1;
            }
            // image ![alt](url) -> render the alt text (no url in a viewer)
            _ if c == '!' && chars.get(i + 1) == Some(&'[') => {
                if let Some((text, consumed)) = parse_link(&chars[i + 1..]) {
                    push(&mut spans, &mut buf, bold, italic);
                    spans.push(Span { text, bold, italic, color: Some(dv_ir::Color { r: 0x6a, g: 0x6f, b: 0x76, a: 255 }), ..Default::default() });
                    i += 1 + consumed;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            _ if c == '[' => {
                // [text](url) -> render text as a link (url dropped in a viewer)
                if let Some((text, consumed)) = parse_link(&chars[i..]) {
                    push(&mut spans, &mut buf, bold, italic);
                    spans.push(Span { text, bold, italic, underline: true, color: Some(dv_ir::Color { r: 0x1a, g: 0x5f, b: 0xb4, a: 255 }), ..Default::default() });
                    i += consumed;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    push(&mut spans, &mut buf, bold, italic);
    if spans.is_empty() {
        spans.push(Span::new(""));
    }
    spans
}

/// True when `chars[i]` sits between two alphanumerics (e.g. `_` in snake_case),
/// where `_` must not act as an emphasis delimiter (CommonMark intraword rule).
fn between_word(chars: &[char], i: usize) -> bool {
    let prev = i.checked_sub(1).and_then(|j| chars.get(j)).map(|c| c.is_alphanumeric()).unwrap_or(false);
    let next = chars.get(i + 1).map(|c| c.is_alphanumeric()).unwrap_or(false);
    prev && next
}

/// Parse `[text](url)` starting at `chars[0] == '['`; returns (text, chars consumed).
fn parse_link(chars: &[char]) -> Option<(String, usize)> {
    let close = chars.iter().position(|&c| c == ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let paren = chars[close + 2..].iter().position(|&c| c == ')')? + close + 2;
    let text: String = chars[1..close].iter().collect();
    Some((text, paren + 1))
}
