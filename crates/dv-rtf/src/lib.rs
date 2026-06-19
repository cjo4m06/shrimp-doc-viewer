//! Viewer-grade RTF -> `dv_flow::Block`s. Handles groups, character formatting
//! (\b \i \ul \strike \fs \cf with \colortbl), paragraph breaks (\par), Unicode
//! (\uN with \uc skip) and \'hh bytes; skips \fonttbl/\stylesheet/\pict/\*-groups.

use dv_flow::{Block, Span};
use dv_ir::Color;

#[derive(Clone, Copy)]
struct Fmt {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    size: f32,        // px
    color: Option<Color>,
    uc: i32,          // \uc Unicode fallback skip count
    ignore: bool,     // inside \fonttbl/\colortbl/\*-group etc.
}

impl Default for Fmt {
    fn default() -> Self {
        Fmt { bold: false, italic: false, underline: false, strike: false, size: 15.0, color: None, uc: 1, ignore: false }
    }
}

pub fn parse(input: &[u8]) -> Vec<Block> {
    let bytes = input;
    let mut i = 0usize;
    let mut stack: Vec<Fmt> = Vec::new();
    let mut f = Fmt::default();
    let mut blocks: Vec<Block> = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut colortbl: Vec<Color> = Vec::new();
    let mut in_colortbl = false;
    let mut cur_color = (0u8, 0u8, 0u8);
    let mut skip_u = 0i32; // chars to skip after a \uN fallback

    let push_text = |spans: &mut Vec<Span>, f: &Fmt, s: &str| {
        if f.ignore || s.is_empty() {
            return;
        }
        spans.push(Span {
            text: s.to_string(),
            bold: f.bold,
            italic: f.italic,
            underline: f.underline,
            strike: f.strike,
            color: f.color,
            size: Some(f.size),
            ..Default::default()
        });
    };
    let end_para = |spans: &mut Vec<Span>, blocks: &mut Vec<Block>| {
        let taken = std::mem::take(spans);
        blocks.push(Block::Para(if taken.is_empty() { vec![Span::new("")] } else { taken }));
    };

    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                stack.push(f);
                i += 1;
            }
            b'}' => {
                if in_colortbl {
                    in_colortbl = false;
                }
                f = stack.pop().unwrap_or_default();
                i += 1;
            }
            b'\\' => {
                i += 1;
                if i >= bytes.len() {
                    break;
                }
                let c = bytes[i];
                if c.is_ascii_alphabetic() {
                    // control word + optional signed number
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    let word = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
                    let mut num: Option<i32> = None;
                    if i < bytes.len() && (bytes[i] == b'-' || bytes[i].is_ascii_digit()) {
                        let ns = i;
                        if bytes[i] == b'-' {
                            i += 1;
                        }
                        while i < bytes.len() && bytes[i].is_ascii_digit() {
                            i += 1;
                        }
                        num = std::str::from_utf8(&bytes[ns..i]).ok().and_then(|s| s.parse().ok());
                    }
                    if i < bytes.len() && bytes[i] == b' ' {
                        i += 1; // a single space delimiter is consumed
                    }
                    apply_word(word, num, &mut f, &mut spans, &mut blocks, &mut colortbl, &mut in_colortbl, &mut cur_color, &mut skip_u, &push_text, &end_para);
                } else {
                    // control symbol
                    match c {
                        b'\'' => {
                            // \'hh hex byte (codepage). Treat as latin-1.
                            let hh = bytes.get(i + 1..i + 3).and_then(|b| std::str::from_utf8(b).ok()).and_then(|s| u8::from_str_radix(s, 16).ok());
                            i += 3;
                            if skip_u > 0 {
                                skip_u -= 1;
                            } else if let Some(b) = hh {
                                push_text(&mut spans, &f, &(b as char).to_string());
                            }
                        }
                        b'*' => {
                            f.ignore = true;
                            i += 1;
                        }
                        b'\\' | b'{' | b'}' => {
                            push_text(&mut spans, &f, &(c as char).to_string());
                            i += 1;
                        }
                        b'~' => {
                            push_text(&mut spans, &f, "\u{00a0}");
                            i += 1;
                        }
                        b'\n' | b'\r' => {
                            end_para(&mut spans, &mut blocks);
                            i += 1;
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }
            }
            b'\r' | b'\n' => {
                i += 1; // raw newlines are not significant in RTF
            }
            _ => {
                // run of literal text up to the next control char
                let start = i;
                while i < bytes.len() && !matches!(bytes[i], b'\\' | b'{' | b'}' | b'\r' | b'\n') {
                    i += 1;
                }
                if in_colortbl {
                    // entries are ';'-separated; commit the running triple on each ';'
                    for &b in &bytes[start..i] {
                        if b == b';' {
                            colortbl.push(Color { r: cur_color.0, g: cur_color.1, b: cur_color.2, a: 255 });
                            cur_color = (0, 0, 0);
                        }
                    }
                    continue;
                }
                if skip_u > 0 {
                    // consume fallback chars after \uN
                    let n = (i - start).min(skip_u as usize);
                    skip_u -= n as i32;
                    let rest = &bytes[start + n..i];
                    if !rest.is_empty() {
                        push_text(&mut spans, &f, &String::from_utf8_lossy(rest));
                    }
                } else {
                    push_text(&mut spans, &f, &String::from_utf8_lossy(&bytes[start..i]));
                }
            }
        }
    }
    if !spans.is_empty() {
        blocks.push(Block::Para(spans));
    }
    if blocks.is_empty() {
        blocks.push(Block::Para(vec![Span::new("")]));
    }
    let _ = (colortbl, cur_color);
    blocks
}

#[allow(clippy::too_many_arguments)]
fn apply_word(
    word: &str,
    num: Option<i32>,
    f: &mut Fmt,
    spans: &mut Vec<Span>,
    blocks: &mut Vec<Block>,
    colortbl: &mut Vec<Color>,
    in_colortbl: &mut bool,
    cur_color: &mut (u8, u8, u8),
    skip_u: &mut i32,
    push_text: &impl Fn(&mut Vec<Span>, &Fmt, &str),
    end_para: &impl Fn(&mut Vec<Span>, &mut Vec<Block>),
) {
    match word {
        "par" | "line" => end_para(spans, blocks),
        "pard" => {
            f.bold = false;
            f.italic = false;
            f.underline = false;
            f.strike = false;
        }
        "b" => f.bold = num != Some(0),
        "i" => f.italic = num != Some(0),
        "ul" => f.underline = num != Some(0),
        "ulnone" => f.underline = false,
        "strike" => f.strike = num != Some(0),
        "fs" => {
            if let Some(n) = num {
                f.size = (n as f32 / 2.0) * (96.0 / 72.0); // half-points -> px
            }
        }
        "uc" => f.uc = num.unwrap_or(1).max(0),
        "u" => {
            if let Some(n) = num {
                let cp = if n < 0 { (n + 65536) as u32 } else { n as u32 };
                if let Some(ch) = char::from_u32(cp) {
                    push_text(spans, f, &ch.to_string());
                }
                *skip_u = f.uc;
            }
        }
        "cf" => {
            if let Some(n) = num {
                f.color = colortbl.get(n as usize).copied();
            }
        }
        "colortbl" => {
            *in_colortbl = true;
            *cur_color = (0, 0, 0);
        }
        "red" => cur_color.0 = num.unwrap_or(0) as u8,
        "green" => cur_color.1 = num.unwrap_or(0) as u8,
        "blue" => {
            cur_color.2 = num.unwrap_or(0) as u8;
        }
        "fonttbl" | "stylesheet" | "info" | "pict" | "header" | "footer" | "themedata" | "colorschememapping" => f.ignore = true,
        "plain" => *f = Fmt { uc: f.uc, ignore: f.ignore, ..Fmt::default() },
        "tab" => push_text(spans, f, "\t"),
        _ => {}
    }
}
