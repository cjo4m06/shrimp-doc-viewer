//! Viewer-grade RTF -> `dv_flow::Block`s. Handles groups, character formatting
//! (\b \i \ul \strike \fs \cf with \colortbl), paragraph breaks (\par), Unicode
//! (\uN with \uc skip) and \'hh bytes decoded via the active codepage
//! (\ansicpg + per-font \fcharset, so CJK Word/WordPad RTF is not mojibake).
//! Skips \fonttbl text/\stylesheet/\pict/\*-groups and \bin binary data.

use std::collections::HashMap;

use dv_flow::{Block, Span};
use dv_ir::Color;
use encoding_rs::{Encoding, BIG5, EUC_KR, GBK, SHIFT_JIS, WINDOWS_1250, WINDOWS_1251, WINDOWS_1252, WINDOWS_1253, WINDOWS_1254, WINDOWS_1255, WINDOWS_1256, WINDOWS_1257, WINDOWS_1258, WINDOWS_874};

#[derive(Clone, Copy)]
struct Fmt {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    size: f32,    // px
    color: Option<Color>,
    uc: i32,      // \uc Unicode fallback skip count
    ignore: bool, // inside \fonttbl/\stylesheet/\*-group etc.
}

impl Default for Fmt {
    fn default() -> Self {
        Fmt { bold: false, italic: false, underline: false, strike: false, size: 15.0, color: None, uc: 1, ignore: false }
    }
}

/// Windows codepage number -> encoding_rs codec (default Windows-1252).
fn codec(cp: u32) -> &'static Encoding {
    match cp {
        932 => SHIFT_JIS,
        936 | 54936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        874 => WINDOWS_874,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        _ => WINDOWS_1252,
    }
}

/// RTF \fcharset value -> Windows codepage.
fn charset_cp(cs: i32) -> u32 {
    match cs {
        128 => 932,
        134 => 936,
        136 => 950,
        129 => 949,
        161 => 1253,
        162 => 1254,
        163 => 1258,
        177 => 1255,
        178 => 1256,
        186 => 1257,
        204 => 1251,
        222 => 874,
        238 => 1250,
        _ => 1252,
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
    let mut skip_bin = 0usize; // raw bytes to skip after \binN

    // codepage state
    let mut ansicpg: u32 = 1252;
    let mut font_cp: HashMap<i32, u32> = HashMap::new();
    let mut cur_fontdef: i32 = -1; // font being defined in \fonttbl
    let mut cur_cp: u32 = 1252;
    let mut pending: Vec<u8> = Vec::new(); // buffered \'hh bytes for codepage decode

    macro_rules! flush_pending {
        () => {
            if !pending.is_empty() {
                if !f.ignore {
                    let (s, _, _) = codec(cur_cp).decode(&pending);
                    push_span(&mut spans, &f, &s);
                }
                pending.clear();
            }
        };
    }

    while i < bytes.len() {
        // \bin raw bytes
        if skip_bin > 0 {
            let n = skip_bin.min(bytes.len() - i);
            i += n;
            skip_bin -= n;
            continue;
        }
        match bytes[i] {
            b'{' => {
                flush_pending!();
                stack.push(f);
                i += 1;
            }
            b'}' => {
                flush_pending!();
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
                        i += 1;
                    }
                    // \'hh runs must be decoded together, so flush before any other word
                    // except the ones that don't emit text.
                    if word != "u" {
                        flush_pending!();
                    }
                    match word {
                        "bin" => skip_bin = num.unwrap_or(0).max(0) as usize,
                        "ansicpg" => {
                            ansicpg = num.unwrap_or(1252) as u32;
                            cur_cp = ansicpg;
                        }
                        "f" => {
                            if let Some(n) = num {
                                cur_fontdef = n;
                                if let Some(&cp) = font_cp.get(&n) {
                                    cur_cp = cp;
                                }
                            }
                        }
                        "fcharset" => {
                            if let Some(cs) = num {
                                let cp = charset_cp(cs);
                                if cur_fontdef >= 0 {
                                    font_cp.insert(cur_fontdef, cp);
                                }
                                cur_cp = cp;
                            }
                        }
                        _ => apply_word(word, num, &mut f, &mut spans, &mut blocks, &mut colortbl, &mut in_colortbl, &mut cur_color, &mut skip_u, ansicpg, &mut cur_cp),
                    }
                } else {
                    match c {
                        b'\'' => {
                            let hh = bytes.get(i + 1..i + 3).and_then(|b| std::str::from_utf8(b).ok()).and_then(|s| u8::from_str_radix(s, 16).ok());
                            i += 3;
                            if skip_u > 0 {
                                skip_u -= 1;
                            } else if let Some(b) = hh {
                                pending.push(b); // buffered; decoded as a run by flush_pending
                            }
                        }
                        b'*' => {
                            f.ignore = true;
                            i += 1;
                        }
                        b'\\' | b'{' | b'}' => {
                            push_span(&mut spans, &f, &(c as char).to_string());
                            i += 1;
                        }
                        b'~' => {
                            push_span(&mut spans, &f, "\u{00a0}");
                            i += 1;
                        }
                        b'_' => {
                            push_span(&mut spans, &f, "\u{2011}"); // non-breaking hyphen
                            i += 1;
                        }
                        b'\n' | b'\r' => {
                            end_para(&mut spans, &mut blocks);
                            i += 1;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'\r' | b'\n' => {
                i += 1; // raw newlines are insignificant in RTF
            }
            _ => {
                flush_pending!();
                let start = i;
                while i < bytes.len() && !matches!(bytes[i], b'\\' | b'{' | b'}' | b'\r' | b'\n') {
                    i += 1;
                }
                if in_colortbl {
                    for &b in &bytes[start..i] {
                        if b == b';' {
                            colortbl.push(Color { r: cur_color.0, g: cur_color.1, b: cur_color.2, a: 255 });
                            cur_color = (0, 0, 0);
                        }
                    }
                    continue;
                }
                // ASCII literal text (codepage-agnostic for <0x80)
                if skip_u > 0 {
                    let n = (i - start).min(skip_u as usize);
                    skip_u -= n as i32;
                    let rest = &bytes[start + n..i];
                    if !rest.is_empty() {
                        push_span(&mut spans, &f, &String::from_utf8_lossy(rest));
                    }
                } else {
                    push_span(&mut spans, &f, &String::from_utf8_lossy(&bytes[start..i]));
                }
            }
        }
    }
    flush_pending!();
    if !spans.is_empty() {
        blocks.push(Block::Para(spans));
    }
    if blocks.is_empty() {
        blocks.push(Block::Para(vec![Span::new("")]));
    }
    blocks
}

fn push_span(spans: &mut Vec<Span>, f: &Fmt, s: &str) {
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
}

fn end_para(spans: &mut Vec<Span>, blocks: &mut Vec<Block>) {
    let taken = std::mem::take(spans);
    blocks.push(Block::Para(if taken.is_empty() { vec![Span::new("")] } else { taken }));
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
    ansicpg: u32,
    cur_cp: &mut u32,
) {
    match word {
        "par" | "line" => end_para(spans, blocks),
        "pard" => {
            f.bold = false;
            f.italic = false;
            f.underline = false;
            f.strike = false;
            f.size = 15.0;
            f.color = None;
        }
        "plain" => *f = Fmt { uc: f.uc, ignore: f.ignore, ..Fmt::default() },
        "b" => f.bold = num != Some(0),
        "i" => f.italic = num != Some(0),
        "ul" => f.underline = num != Some(0),
        "ulnone" => f.underline = false,
        "strike" => f.strike = num != Some(0),
        "fs" => {
            if let Some(n) = num {
                f.size = (n as f32 / 2.0) * (96.0 / 72.0);
            }
        }
        "uc" => f.uc = num.unwrap_or(1).max(0),
        "u" => {
            if let Some(n) = num {
                let cp = if n < 0 { (n as i64 + 65536) as u32 } else { n as u32 };
                if let Some(ch) = char::from_u32(cp) {
                    push_span(spans, f, &ch.to_string());
                }
                *skip_u = f.uc;
            }
        }
        "cf" => f.color = num.and_then(|n| colortbl.get(n as usize).copied()),
        "cf0" => f.color = None,
        "colortbl" => {
            *in_colortbl = true;
            *cur_color = (0, 0, 0);
        }
        "red" => cur_color.0 = num.unwrap_or(0) as u8,
        "green" => cur_color.1 = num.unwrap_or(0) as u8,
        "blue" => cur_color.2 = num.unwrap_or(0) as u8,
        "fonttbl" | "stylesheet" | "info" | "pict" | "header" | "footer" | "footnote" | "themedata" | "colorschememapping" | "datastore" | "latentstyles" => f.ignore = true,
        "tab" => push_span(spans, f, "\t"),
        "ansi" => *cur_cp = ansicpg,
        _ => {}
    }
}
