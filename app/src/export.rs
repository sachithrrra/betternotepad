//! Exports the current buffer to PDF or DOCX (not the existing .txt/.md save path).
//! Markdown is rendered like the preview; anything else is exported as the editor shows it.
//! Both use the Settings font; markdown code uses Lilex, like the preview's.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use docx_rs::{BreakType, Docx, LineSpacing, LineSpacingType, Paragraph, Run, RunFonts};
use markdown::{ParseOptions, mdast::Node};
use printpdf::{
    Color, FontId, Line, LinePoint, Mm, Op, ParsedFont, PdfDocument, PdfFontHandle, PdfPage,
    PdfSaveOptions, Point, Pt, Rgb, TextItem,
};

use crate::{LILEX_BOLD, LILEX_REGULAR};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ExportFormat {
    Pdf,
    Docx,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Pdf => "pdf",
            ExportFormat::Docx => "docx",
        }
    }

    /// `family`/`size` are the Settings font, which the editor and preview both use.
    pub fn write(
        self,
        text: &str,
        markdown: bool,
        family: &str,
        size: f32,
        path: &Path,
    ) -> Result<()> {
        let blocks = if markdown {
            markdown_blocks(text)
        } else {
            vec![Block::code(text)]
        };
        let code_family = if markdown { "Lilex" } else { family };
        match self {
            ExportFormat::Pdf => write_pdf(&blocks, family, code_family, size, path),
            ExportFormat::Docx => write_docx(&blocks, family, code_family, size, path),
        }
    }
}

// ---- Document model shared by both writers ----

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Heading(u8),
    Para,
    Code,
    Rule,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Style {
    bold: bool,
    italic: bool,
    code: bool,
}

#[derive(Debug)]
struct Span {
    text: String,
    style: Style,
}

#[derive(Debug)]
struct Block {
    kind: Kind,
    /// List/blockquote nesting depth.
    indent: u8,
    spans: Vec<Span>,
}

impl Block {
    fn code(text: &str) -> Self {
        Block {
            kind: Kind::Code,
            indent: 0,
            spans: vec![Span {
                text: text.to_string(),
                style: Style { code: true, ..Style::default() },
            }],
        }
    }
}

fn markdown_blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    match markdown::to_mdast(text, &ParseOptions::gfm()) {
        Ok(root) => collect_blocks(&root, 0, &mut out),
        // Plain markdown never fails to parse (only MDX can); fall back to raw text anyway.
        Err(_) => out.push(Block::code(text)),
    }
    out
}

fn collect_blocks(node: &Node, indent: u8, out: &mut Vec<Block>) {
    let mut push = |kind, children: &[Node]| {
        let mut spans = Vec::new();
        collect_inline(children, Style::default(), &mut spans);
        out.push(Block { kind, indent, spans });
    };
    match node {
        Node::Heading(h) => push(Kind::Heading(h.depth), &h.children),
        Node::Paragraph(p) => push(Kind::Para, &p.children),
        Node::Code(c) => out.push(Block { indent, ..Block::code(&c.value) }),
        Node::ThematicBreak(_) => out.push(Block { kind: Kind::Rule, indent, spans: vec![] }),
        Node::Blockquote(q) => q.children.iter().for_each(|c| collect_blocks(c, indent + 1, out)),
        Node::List(list) => {
            for (i, item) in list.children.iter().enumerate() {
                let marker = match item {
                    Node::ListItem(li) if li.checked == Some(true) => "[x] ".to_string(),
                    Node::ListItem(li) if li.checked == Some(false) => "[ ] ".to_string(),
                    _ if list.ordered => format!("{}. ", list.start.unwrap_or(1) as usize + i),
                    _ => "\u{2022} ".to_string(),
                };
                let first = out.len();
                for child in item.children().into_iter().flatten() {
                    collect_blocks(child, indent + 1, out);
                }
                match out.get_mut(first) {
                    Some(block) if block.kind != Kind::Code => {
                        block.spans.insert(0, Span { text: marker, style: Style::default() })
                    }
                    _ => {}
                }
            }
        }
        // ponytail: tables flatten to "a | b" rows; draw a real grid if people export tables.
        Node::Table(t) => {
            for row in &t.children {
                let mut spans = Vec::new();
                for (i, cell) in row.children().into_iter().flatten().enumerate() {
                    if i > 0 {
                        spans.push(Span { text: " | ".into(), style: Style::default() });
                    }
                    collect_inline(cell.children().map_or(&[][..], |c| c), Style::default(), &mut spans);
                }
                out.push(Block { kind: Kind::Para, indent, spans });
            }
        }
        Node::Html(_) => {}
        other => other
            .children()
            .into_iter()
            .flatten()
            .for_each(|c| collect_blocks(c, indent, out)),
    }
}

fn collect_inline(nodes: &[Node], style: Style, out: &mut Vec<Span>) {
    for node in nodes {
        match node {
            Node::Text(t) => out.push(Span { text: t.value.replace('\n', " "), style }),
            Node::InlineCode(c) => out.push(Span {
                text: c.value.clone(),
                style: Style { code: true, ..style },
            }),
            Node::Break(_) => out.push(Span { text: "\n".into(), style }),
            Node::Strong(s) => collect_inline(&s.children, Style { bold: true, ..style }, out),
            Node::Emphasis(e) => collect_inline(&e.children, Style { italic: true, ..style }, out),
            Node::Image(i) => out.push(Span { text: i.alt.clone(), style }),
            Node::Html(_) => {}
            other => collect_inline(other.children().map_or(&[][..], |c| c), style, out),
        }
    }
}

// ---- PDF ----

const PAGE_W_MM: f32 = 210.0;
const PAGE_H_MM: f32 = 297.0;
const MARGIN_MM: f32 = 20.0;
const INDENT_PT: f32 = 18.0;

/// Heading sizes relative to the body text.
fn block_size(kind: Kind, body: f32) -> f32 {
    body * match kind {
        Kind::Heading(1) => 1.8,
        Kind::Heading(2) => 1.45,
        Kind::Heading(3) => 1.2,
        _ => 1.0,
    }
}

const LINE_HEIGHT: f32 = 1.4;

/// Space above a block (PDF and DOCX alike): headings get more, like the preview.
fn gap_before(kind: Kind, body: f32) -> f32 {
    match kind {
        Kind::Heading(_) => block_size(kind, body) * 0.9,
        _ => body * 0.6,
    }
}

/// Where a font face comes from: the bundled Lilex, or a system font file (+ face index).
#[derive(Clone, PartialEq, Debug)]
enum Source {
    Lilex { bold: bool },
    File(PathBuf, usize),
}

/// Finds `family`'s face with exactly these traits via CoreText; None if it has no such face.
fn system_face(family: &str, bold: bool, italic: bool) -> Option<Source> {
    use core_foundation::{array::CFArray, base::TCFType};
    use core_text::font_descriptor::{
        CTFontDescriptor, SymbolicTraitAccessors as _, kCTFontBoldTrait, kCTFontItalicTrait,
    };

    let descs = core_text::font_collection::create_for_family(family)?.get_descriptors()?;
    let base = core_text::font::new_from_descriptor(&*descs.get(0)?, 12.0);
    let want = (if bold { kCTFontBoldTrait } else { 0 }) | (if italic { kCTFontItalicTrait } else { 0 });
    let font = base.clone_with_symbolic_traits(want, kCTFontBoldTrait | kCTFontItalicTrait)?;
    let traits = font.symbolic_traits();
    if traits.is_bold() != bold || traits.is_italic() != italic || font.family_name() != family {
        return None;
    }
    let url = font.url()?;
    // A .ttc holds several faces; CoreText lists them in file order.
    let name = font.postscript_name();
    let faces = unsafe {
        core_text::font_manager::CTFontManagerCreateFontDescriptorsFromURL(
            url.as_concrete_TypeRef(),
        )
    };
    let index = (!faces.is_null())
        .then(|| unsafe { CFArray::<CTFontDescriptor>::wrap_under_create_rule(faces) })
        .and_then(|faces| faces.iter().position(|d| d.font_name() == name))
        .unwrap_or(0);
    // ponytail: a variable font (e.g. SF Pro) embeds its default instance, so its bold
    // prints regular; add an instancer if that matters.
    Some(Source::File(url.to_path()?, index))
}

/// Regular, bold, italic, bold italic for `family`, falling back per face to the closest one
/// that exists, and to Lilex when the family isn't a system font (Lilex is app-bundled).
fn family_faces(family: &str) -> [Source; 4] {
    let lilex = |bold| Source::Lilex { bold };
    let Some(regular) = (family != "Lilex").then(|| system_face(family, false, false)).flatten()
    else {
        return [lilex(false), lilex(true), lilex(false), lilex(true)];
    };
    let bold = system_face(family, true, false).unwrap_or(regular.clone());
    let italic = system_face(family, false, true).unwrap_or(regular.clone());
    let bold_italic = system_face(family, true, true).unwrap_or(bold.clone());
    [regular, bold, italic, bold_italic]
}

/// The document's embedded fonts, each also parsed for glyph widths so text wraps correctly.
struct Fonts {
    faces: Vec<(FontId, ParsedFont)>,
    /// Indexes into `faces`: regular, bold, italic, bold italic, then code.
    slots: [usize; 5],
}

impl Fonts {
    fn load(doc: &mut PdfDocument, family: &str, code_family: &str) -> Result<Self> {
        let [r, b, i, bi] = family_faces(family);
        let code = family_faces(code_family)[0].clone();
        let mut sources: Vec<Source> = vec![];
        let mut faces = vec![];
        let slots = [r, b, i, bi, code].map(|src| {
            sources.iter().position(|s| *s == src).unwrap_or_else(|| {
                sources.push(src);
                sources.len() - 1
            })
        });
        for src in &sources {
            let (bytes, index) = match src {
                Source::Lilex { bold: false } => (LILEX_REGULAR.to_vec(), 0),
                Source::Lilex { bold: true } => (LILEX_BOLD.to_vec(), 0),
                Source::File(path, index) => (
                    std::fs::read(path).with_context(|| format!("reading {}", path.display()))?,
                    *index,
                ),
            };
            let parsed = ParsedFont::from_bytes(&bytes, index, &mut Vec::new())
                .with_context(|| format!("parsing font {src:?}"))?;
            faces.push((doc.add_font(&parsed), parsed));
        }
        Ok(Fonts { faces, slots })
    }

    fn face(&self, style: Style) -> usize {
        match (style.code, style.bold, style.italic) {
            (true, _, _) => self.slots[4],
            (_, bold, italic) => self.slots[bold as usize + 2 * italic as usize],
        }
    }

    fn width(&self, face: usize, size: f32, text: &str) -> f32 {
        let parsed = &self.faces[face].1;
        let upm = parsed.font_metrics.units_per_em.max(1) as f32;
        text.chars()
            .map(|c| {
                parsed
                    .lookup_glyph_index(c as u32)
                    .map_or(size * 0.6, |g| parsed.get_horizontal_advance(g) as f32 / upm * size)
            })
            .sum()
    }
}

type PdfLine = Vec<(usize, String)>;

/// Greedy word wrap over styled spans; code keeps its own line breaks and hard-wraps.
fn wrap_block(block: &Block, size: f32, max_w: f32, m: &Fonts) -> Vec<PdfLine> {
    let mut lines: Vec<PdfLine> = vec![vec![]];
    let mut width = 0.0;
    let bold = matches!(block.kind, Kind::Heading(_));
    for span in &block.spans {
        let font = m.face(Style { bold: bold || span.style.bold, ..span.style });
        let tokens: Vec<&str> = if block.kind == Kind::Code {
            span.text.split_inclusive('\n').collect()
        } else {
            span.text.split_inclusive([' ', '\n']).collect()
        };
        for token in tokens {
            let (mut rest, newline) = match token.strip_suffix('\n') {
                Some(t) => (t, true),
                None => (token, false),
            };
            while !rest.is_empty() {
                // Trailing spaces may hang past the margin; they're invisible.
                if width + m.width(font, size, rest.trim_end()) <= max_w {
                    push_run(lines.last_mut().unwrap(), font, rest);
                    width += m.width(font, size, rest);
                    break;
                }
                if width > 0.0 {
                    lines.push(vec![]);
                    width = 0.0;
                    continue;
                }
                // Alone on the line and still too wide: hard-break where it overflows.
                let mut acc = 0.0;
                let cut = rest
                    .char_indices()
                    .find(|&(i, c)| {
                        acc += m.width(font, size, c.encode_utf8(&mut [0; 4]));
                        i > 0 && acc > max_w
                    })
                    .map_or(rest.len(), |(i, _)| i);
                push_run(lines.last_mut().unwrap(), font, &rest[..cut]);
                rest = &rest[cut..];
                lines.push(vec![]);
            }
            if newline {
                lines.push(vec![]);
                width = 0.0;
            }
        }
    }
    if block.kind == Kind::Code && lines.len() > 1 && lines.last().is_some_and(Vec::is_empty) {
        lines.pop(); // trailing newline of the buffer
    }
    lines
}

fn push_run(line: &mut PdfLine, font: usize, text: &str) {
    match line.last_mut() {
        Some((f, s)) if *f == font => s.push_str(text),
        _ => line.push((font, text.to_string())),
    }
}

fn write_pdf(
    blocks: &[Block],
    family: &str,
    code_family: &str,
    body: f32,
    path: &Path,
) -> Result<()> {
    let mut doc = PdfDocument::new("Better Notepad Export");
    let m = Fonts::load(&mut doc, family, code_family)?;
    let margin = Mm(MARGIN_MM).into_pt().0;
    let page_w = Mm(PAGE_W_MM).into_pt().0;
    let page_h = Mm(PAGE_H_MM).into_pt().0;
    let black = Color::Rgb(Rgb { r: 0.0, g: 0.0, b: 0.0, icc_profile: None });

    let mut pages: Vec<Vec<Op>> = vec![vec![]];
    let mut y = page_h - margin;
    let mut first = true;
    for block in blocks {
        let size = block_size(block.kind, body);
        let line_h = size * LINE_HEIGHT;
        let x = margin + block.indent as f32 * INDENT_PT;
        let gap = if first { 0.0 } else { gap_before(block.kind, body) };
        first = false;
        y -= gap;
        if block.kind == Kind::Rule {
            y -= body * 0.6;
            pages.last_mut().unwrap().push(Op::DrawLine {
                line: Line {
                    points: [x, page_w - margin]
                        .map(|px| LinePoint { p: Point { x: Pt(px), y: Pt(y) }, bezier: false })
                        .to_vec(),
                    is_closed: false,
                },
            });
            continue;
        }
        for line in wrap_block(block, size, page_w - margin - x, &m) {
            if y - line_h < margin {
                pages.push(vec![]);
                y = page_h - margin;
            }
            y -= line_h;
            let ops = pages.last_mut().unwrap();
            ops.push(Op::StartTextSection);
            ops.push(Op::SetTextCursor { pos: Point { x: Pt(x), y: Pt(y + line_h - size) } });
            ops.push(Op::SetFillColor { col: black.clone() });
            // Each ShowText advances the cursor, so styled runs sit side by side.
            for (font, text) in line {
                ops.push(Op::SetFont { font: PdfFontHandle::External(m.faces[font].0.clone()), size: Pt(size) });
                ops.push(Op::ShowText { items: vec![TextItem::Text(text)] });
            }
            ops.push(Op::EndTextSection);
        }
    }

    let pages = pages
        .into_iter()
        .map(|ops| PdfPage::new(Mm(PAGE_W_MM), Mm(PAGE_H_MM), ops))
        .collect();
    let mut warnings = Vec::new();
    let bytes = doc
        .with_pages(pages)
        .save(&PdfSaveOptions::default(), &mut warnings);
    std::fs::write(path, bytes).context("writing PDF file")
}

// ---- DOCX ----

/// Word gets the font by name; it substitutes when the reader doesn't have it installed.
fn write_docx(
    blocks: &[Block],
    family: &str,
    code_family: &str,
    body: f32,
    path: &Path,
) -> Result<()> {
    let file = std::fs::File::create(path).context("creating DOCX file")?;
    let font = |name: &str| RunFonts::new().ascii(name).hi_ansi(name).cs(name);
    let half_pts = |pt: f32| (pt * 2.0).round() as usize;
    // Word's units: spacing in twentieths of a point, line height in 240ths of a line.
    let spacing = |before: f32| {
        LineSpacing::new()
            .before((before * 20.0).round() as u32)
            .after(0)
            .line((LINE_HEIGHT * 240.0) as i32)
            .line_rule(LineSpacingType::Auto)
    };
    let mut docx = Docx::new();
    for block in blocks {
        let gap = gap_before(block.kind, body);
        // One paragraph per code line keeps Word's spacing from double-spacing code.
        if block.kind == Kind::Code {
            let text = &block.spans[0].text;
            for (i, line) in text.strip_suffix('\n').unwrap_or(text).split('\n').enumerate() {
                let p = Paragraph::new()
                    .line_spacing(spacing(if i == 0 { gap } else { 0.0 }))
                    .add_run(Run::new().add_text(line).fonts(font(code_family)).size(half_pts(body)))
                    .indent(Some(block.indent as i32 * 360), None, None, None);
                docx = docx.add_paragraph(p);
            }
            continue;
        }
        let mut p = Paragraph::new()
            .line_spacing(spacing(gap))
            .indent(Some(block.indent as i32 * 360), None, None, None);
        if block.kind == Kind::Rule {
            p = p.add_run(Run::new().add_text("\u{2014}".repeat(20)).fonts(font(family)));
        }
        let heading = matches!(block.kind, Kind::Heading(_));
        for span in &block.spans {
            let mut run = if span.text == "\n" {
                Run::new().add_break(BreakType::TextWrapping)
            } else {
                Run::new().add_text(&span.text)
            };
            run = run
                .size(half_pts(block_size(block.kind, body)))
                .fonts(font(if span.style.code { code_family } else { family }));
            if heading || span.style.bold {
                run = run.bold();
            }
            if span.style.italic {
                run = run.italic();
            }
            p = p.add_run(run);
        }
        docx = docx.add_paragraph(p);
    }
    docx.build()
        .pack(file)
        .map_err(|err| anyhow::anyhow!("{err:?}"))
        .context("writing DOCX file")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_styled_blocks() {
        let blocks = markdown_blocks("# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n\n---\n");
        assert_eq!(blocks[0].kind, Kind::Heading(1));
        assert_eq!(blocks[0].spans[0].text, "Title");
        assert!(blocks[1].spans.iter().any(|s| s.text == "bold" && s.style.bold));
        assert!(blocks[1].spans.iter().any(|s| s.text == "code" && s.style.code));
        assert_eq!(blocks[2].spans[0].text, "\u{2022} ");
        assert_eq!(blocks[2].indent, 1);
        assert_eq!(blocks[4].kind, Kind::Rule);
        // No raw markdown syntax leaks through.
        assert!(!blocks.iter().flat_map(|b| &b.spans).any(|s| s.text.contains(['#', '*', '`'])));
    }

    #[test]
    fn wraps_within_width() {
        let m = Fonts::load(&mut PdfDocument::new("t"), "Lilex", "Lilex").unwrap();
        let block = markdown_blocks(&("word ".repeat(200) + &"x".repeat(300))).remove(0);
        let lines = wrap_block(&block, 11.0, 400.0, &m);
        assert!(lines.len() > 5);
        for line in &lines {
            let w: f32 = line.iter().map(|(f, t)| m.width(*f, 11.0, t.trim_end())).sum();
            assert!(w <= 400.0, "line too wide: {w}");
        }
    }

    #[test]
    fn finds_system_font_faces() {
        // Menlo ships as a .ttc; its bold must be a different face than regular.
        let [r, b, ..] = family_faces("Menlo");
        assert!(matches!(r, Source::File(..)));
        assert_ne!(r, b);
        assert_eq!(family_faces("No Such Font")[0], Source::Lilex { bold: false });
    }

    #[test]
    fn exports_pdf_and_docx() {
        let dir = std::env::temp_dir();
        let text = "# Hello\n\nBetter *Notepad*!\n".to_string() + &"x".repeat(500) + "\n\nlast line";
        for (markdown, family) in [(false, "Menlo"), (true, "Helvetica Neue"), (true, "Lilex"), (false, "No Such Font")] {
            let write = |format: ExportFormat, path| format.write(&text, markdown, family, 12.0, path);
            let pdf_path = dir.join("betternotepad_export_test.pdf");
            write(ExportFormat::Pdf, &pdf_path).unwrap();
            assert!(std::fs::read(&pdf_path).unwrap().starts_with(b"%PDF"));
            std::fs::remove_file(&pdf_path).unwrap();

            let docx_path = dir.join("betternotepad_export_test.docx");
            write(ExportFormat::Docx, &docx_path).unwrap();
            assert!(std::fs::read(&docx_path).unwrap().starts_with(b"PK")); // docx is a zip archive
            std::fs::remove_file(&docx_path).unwrap();
        }
    }
}
