use mux_core::protocol::FileKind;
use serde_json::Value;

// how a file is previewed, from what the server found in it and its extension
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Code,
    Markdown,
    // with its delimiter
    Table(u8),
    Notebook,
    Mermaid,
    Image,
    Svg,
    Font,
    Pdf,
    Office,
    Hex,
}

pub fn extension(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn shape(path: &str, kind: FileKind, head: &[u8]) -> Shape {
    let ext = extension(path);
    match kind {
        FileKind::Pdf => Shape::Pdf,
        FileKind::Image if ext == "svg" || head.trim_ascii_start().starts_with(b"<") => Shape::Svg,
        FileKind::Image => Shape::Image,
        FileKind::Binary => match ext.as_str() {
            "ttf" | "otf" => Shape::Font,
            "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" | "key"
            | "pages" | "numbers" | "rtf" => Shape::Office,
            _ => Shape::Hex,
        },
        FileKind::Text => match ext.as_str() {
            "md" | "markdown" | "mdx" => Shape::Markdown,
            "csv" => Shape::Table(b','),
            "tsv" => Shape::Table(b'\t'),
            "ipynb" => Shape::Notebook,
            "mmd" | "mermaid" => Shape::Mermaid,
            "rtf" => Shape::Office,
            _ => Shape::Code,
        },
    }
}

// rows of fields, with quoted fields holding delimiters, doubled quotes and line breaks
pub fn parse_table(text: &str, delimiter: u8, max_rows: usize) -> Vec<Vec<String>> {
    let delimiter = delimiter as char;
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    // a row with nothing typed yet, so a trailing line break doesn't add an empty row
    let mut started = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            match c {
                '"' if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                '"' => quoted = false,
                c => field.push(c),
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => {
                quoted = true;
                started = true;
            }
            c if c == delimiter => {
                row.push(std::mem::take(&mut field));
                started = true;
            }
            '\r' if chars.peek() == Some(&'\n') => {}
            '\n' | '\r' => {
                if started || !field.is_empty() {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                    if rows.len() >= max_rows {
                        return rows;
                    }
                }
                started = false;
            }
            c => {
                field.push(c);
                started = true;
            }
        }
    }
    if started || !field.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notebook {
    pub language: String,
    pub cells: Vec<Cell>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Markdown(String),
    Code {
        source: String,
        outputs: Vec<Output>,
    },
    Raw(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Text(String),
    Png(Vec<u8>),
    Error(String),
}

// notebooks keep text as one string or a list of lines
fn joined(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(lines) => lines.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

// tracebacks come coloured for a terminal
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn parse_notebook(text: &str) -> Result<Notebook, String> {
    use base64::Engine;
    let doc: Value =
        serde_json::from_str(text).map_err(|e| format!("This notebook isn't valid JSON: {e}"))?;
    let cells = doc
        .get("cells")
        .and_then(Value::as_array)
        .ok_or("This notebook has no cells.")?;
    let meta = doc.get("metadata");
    let language = meta
        .and_then(|m| m.pointer("/language_info/name"))
        .or_else(|| meta.and_then(|m| m.pointer("/kernelspec/language")))
        .and_then(Value::as_str)
        .unwrap_or("python")
        .to_owned();
    let cells = cells
        .iter()
        .map(|cell| {
            let source = joined(cell.get("source").unwrap_or(&Value::Null));
            match cell.get("cell_type").and_then(Value::as_str) {
                Some("markdown") => Cell::Markdown(source),
                Some("code") => {
                    let outputs = cell
                        .get("outputs")
                        .and_then(Value::as_array)
                        .map(|outputs| outputs.iter().filter_map(output).collect())
                        .unwrap_or_default();
                    Cell::Code { source, outputs }
                }
                _ => Cell::Raw(source),
            }
        })
        .collect();
    return Ok(Notebook { language, cells });

    fn output(out: &Value) -> Option<Output> {
        match out.get("output_type")?.as_str()? {
            "stream" => Some(Output::Text(joined(out.get("text")?))),
            "error" => {
                let trace = out
                    .get("traceback")
                    .and_then(Value::as_array)
                    .map(|lines| {
                        lines
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let head = format!(
                    "{}: {}",
                    out.get("ename").and_then(Value::as_str).unwrap_or("Error"),
                    out.get("evalue").and_then(Value::as_str).unwrap_or("")
                );
                Some(Output::Error(strip_ansi(if trace.is_empty() {
                    &head
                } else {
                    &trace
                })))
            }
            _ => {
                let data = out.get("data")?;
                if let Some(png) = data.get("image/png") {
                    let encoded: String = joined(png).split_whitespace().collect();
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded) {
                        return Some(Output::Png(bytes));
                    }
                }
                data.get("text/plain").map(|t| Output::Text(joined(t)))
            }
        }
    }
}

// what a binary file is, from its first bytes, for the hex view's heading
pub fn describe(head: &[u8], path: &str) -> &'static str {
    const KNOWN: [(&[u8], &str); 9] = [
        (b"PK\x03\x04", "ZIP archive"),
        (b"\x7fELF", "ELF executable"),
        (b"MZ", "Windows executable"),
        (b"\x1f\x8b", "gzip archive"),
        (b"\0asm", "WebAssembly module"),
        (b"SQLite format 3\0", "SQLite database"),
        (b"\xcf\xfa\xed\xfe", "Mach-O executable"),
        (b"wOFF", "WOFF font"),
        (b"wOF2", "WOFF2 font"),
    ];
    if let Some((_, name)) = KNOWN.iter().find(|(magic, _)| head.starts_with(magic)) {
        return name;
    }
    match extension(path).as_str() {
        "ttf" | "otf" => "Font",
        _ => "Binary data",
    }
}

// one hex dump line: offset, sixteen bytes and what they read as
pub fn hex_line(bytes: &[u8], offset: usize) -> String {
    let mut line = format!("{offset:08x}  ");
    for i in 0..16 {
        match bytes.get(i) {
            Some(b) => line.push_str(&format!("{b:02x} ")),
            None => line.push_str("   "),
        }
        if i == 7 {
            line.push(' ');
        }
    }
    line.push(' ');
    line.extend(bytes.iter().map(|&b| {
        if b.is_ascii_graphic() || b == b' ' {
            b as char
        } else {
            '.'
        }
    }));
    line
}

pub fn size_label(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_fields() {
        let text = "name,note\r\nann,\"likes, commas\"\nbob,\"said \"\"hi\"\"\"\n\"two\nlines\",\n";
        assert_eq!(
            parse_table(text, b',', 100),
            [
                vec!["name", "note"],
                vec!["ann", "likes, commas"],
                vec!["bob", "said \"hi\""],
                vec!["two\nlines", ""],
            ]
        );
        assert_eq!(
            parse_table("a\tb\n1\t2", b'\t', 100),
            [["a", "b"], ["1", "2"]]
        );
        assert_eq!(parse_table("a\n\nb\n", b',', 100), [["a"], ["b"]]);
        assert_eq!(parse_table("1\n2\n3\n", b',', 2).len(), 2);
        assert!(parse_table("", b',', 10).is_empty());
    }

    #[test]
    fn reads_notebook_cells_and_outputs() {
        let text = r##"{
            "metadata": {"language_info": {"name": "python"}},
            "cells": [
                {"cell_type": "markdown", "source": ["# Title\n", "text"]},
                {"cell_type": "code", "source": "print(1)", "outputs": [
                    {"output_type": "stream", "name": "stdout", "text": ["1\n"]},
                    {"output_type": "execute_result", "data": {"text/plain": "2"}},
                    {"output_type": "display_data", "data": {"image/png": "iVBORw0K\nGgo=", "text/plain": "<Figure>"}},
                    {"output_type": "error", "ename": "ValueError", "evalue": "bad", "traceback": ["\u001b[0;31mValueError\u001b[0m: bad"]}
                ]},
                {"cell_type": "raw", "source": "x"}
            ]
        }"##;
        let notebook = parse_notebook(text).unwrap();
        assert_eq!(notebook.language, "python");
        assert_eq!(
            notebook.cells[0],
            Cell::Markdown("# Title\ntext".to_owned())
        );
        assert_eq!(
            notebook.cells[1],
            Cell::Code {
                source: "print(1)".to_owned(),
                outputs: vec![
                    Output::Text("1\n".to_owned()),
                    Output::Text("2".to_owned()),
                    Output::Png(b"\x89PNG\r\n\x1a\n".to_vec()),
                    Output::Error("ValueError: bad".to_owned()),
                ],
            }
        );
        assert_eq!(notebook.cells[2], Cell::Raw("x".to_owned()));
        assert!(parse_notebook("{}").is_err());
        assert!(parse_notebook("not json").is_err());
    }

    #[test]
    fn picks_a_preview_from_kind_and_extension() {
        assert_eq!(shape("a/b.rs", FileKind::Text, b""), Shape::Code);
        assert_eq!(shape("README.MD", FileKind::Text, b""), Shape::Markdown);
        assert_eq!(shape("x.tsv", FileKind::Text, b""), Shape::Table(b'\t'));
        assert_eq!(shape("x.ipynb", FileKind::Text, b""), Shape::Notebook);
        assert_eq!(shape("logo", FileKind::Image, b"  <svg>"), Shape::Svg);
        assert_eq!(shape("a.png", FileKind::Image, b"\x89PNG"), Shape::Image);
        assert_eq!(shape("f.ttf", FileKind::Binary, b""), Shape::Font);
        assert_eq!(shape("r.docx", FileKind::Binary, b"PK"), Shape::Office);
        assert_eq!(shape("a.out", FileKind::Binary, b""), Shape::Hex);
        assert_eq!(shape("a.txt", FileKind::Pdf, b""), Shape::Pdf);
    }

    #[test]
    fn dumps_bytes_as_hex() {
        assert_eq!(
            hex_line(b"Hi\0", 16),
            "00000010  48 69 00                                          Hi."
        );
        assert_eq!(describe(b"PK\x03\x04", "a.zip"), "ZIP archive");
        assert_eq!(size_label(1536), "1.5 KB");
    }
}
