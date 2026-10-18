use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use resvg::{tiny_skia, usvg};
use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Part {
    Text(String),
    Table(Table),
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Span {
    text: String,
    style: Style,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Cell(Vec<Span>);

impl Cell {
    fn plain_text(&self) -> String {
        self.0.iter().map(|span| span.text.as_str()).collect()
    }
    fn push(&mut self, text: &str, style: Style) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.0.last_mut().filter(|last| last.style == style) {
            last.text.push_str(text);
        } else {
            self.0.push(Span {
                text: text.to_owned(),
                style,
            });
        }
    }
}

impl From<&str> for Cell {
    fn from(value: &str) -> Self {
        let mut c = Cell::default();
        c.push(value, Style::default());
        c
    }
}
impl From<String> for Cell {
    fn from(value: String) -> Self {
        let mut c = Cell::default();
        c.push(&value, Style::default());
        c
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StyleKind {
    Bold,
    Underline,
    Italic,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub source: String,
    rows: Vec<Vec<Cell>>,
    alignments: Vec<Alignment>,
}

pub(crate) fn parse(text: &str) -> Vec<Part> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    let events: Vec<_> = Parser::new_ext(text, opts).into_offset_iter().collect();
    let mut out = Vec::new();
    let mut cursor = 0;
    let mut i = 0;
    while i < events.len() {
        let (ev, range) = &events[i];
        if let Event::Start(Tag::Table(align)) = ev {
            let start = range.start;
            let mut end = range.end;
            let mut rows = Vec::new();
            let mut row = Vec::new();
            let mut cell = Cell::default();
            let mut styles: Vec<StyleKind> = Vec::new();
            let mut inside = false;
            let mut j = i + 1;
            while j < events.len() {
                let (e, r) = &events[j];
                end = end.max(r.end);
                match e {
                    Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => row.clear(),
                    Event::Start(Tag::TableCell) => {
                        cell = Cell::default();
                        styles.clear();
                        inside = true;
                    }
                    Event::End(TagEnd::TableCell) => {
                        row.push(cell.clone());
                        inside = false;
                    }
                    Event::End(TagEnd::TableHead) | Event::End(TagEnd::TableRow) => {
                        if !row.is_empty() {
                            rows.push(std::mem::take(&mut row));
                        }
                    }
                    Event::End(TagEnd::Table) => {
                        end = r.end;
                        break;
                    }
                    Event::Start(Tag::Strong) if inside => {
                        let opener = text.get(r.start..).unwrap_or("");
                        styles.push(if opener.starts_with("__") {
                            StyleKind::Underline
                        } else {
                            StyleKind::Bold
                        });
                    }
                    Event::End(TagEnd::Strong) if inside => {
                        styles.pop();
                    }
                    Event::Start(Tag::Emphasis) if inside => {
                        styles.push(StyleKind::Italic);
                    }
                    Event::End(TagEnd::Emphasis) if inside => {
                        styles.pop();
                    }
                    Event::Text(value) if inside => {
                        let style = Style {
                            bold: styles.contains(&StyleKind::Bold),
                            italic: styles.contains(&StyleKind::Italic),
                            underline: styles.contains(&StyleKind::Underline),
                        };
                        cell.push(value, style);
                    }
                    Event::Code(value) if inside => cell.push(value, active_style(&styles)),
                    Event::Html(value) | Event::InlineHtml(value) if inside => {
                        cell.push(value, active_style(&styles))
                    }
                    Event::SoftBreak | Event::HardBreak if inside => {
                        cell.push(" ", active_style(&styles))
                    }
                    _ => {}
                }
                j += 1;
            }
            if start > cursor {
                out.push(Part::Text(text[cursor..start].to_owned()));
            }
            let mut table_end = end.min(text.len());
            while table_end > start && matches!(text.as_bytes()[table_end - 1], b'\n' | b'\r') {
                table_end -= 1;
            }
            out.push(Part::Table(Table {
                source: text[start..table_end].to_owned(),
                rows,
                alignments: align.clone(),
            }));
            cursor = table_end;
            i = j.saturating_add(1);
        } else {
            i += 1;
        }
    }
    if cursor < text.len() {
        out.push(Part::Text(text[cursor..].to_owned()));
    }
    if out.is_empty() {
        out.push(Part::Text(text.to_owned()));
    }
    out
}
const FONT: f32 = 15.0;
const PX: f32 = 12.0;
const PY: f32 = 9.0;
const LH: f32 = 21.0;
fn push_span(line: &mut Vec<Span>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = line.last_mut().filter(|last| last.style == style) {
        last.text.push_str(text);
    } else {
        line.push(Span {
            text: text.to_owned(),
            style,
        });
    }
}

fn wrap(cell: &Cell, max: usize) -> Vec<Vec<Span>> {
    let mut lines = vec![Vec::new()];
    let mut width = 0usize;
    for span in &cell.0 {
        for grapheme in span.text.graphemes(true) {
            if grapheme == "\n" {
                lines.push(Vec::new());
                width = 0;
                continue;
            }
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if width > 0 && width + grapheme_width > max {
                lines.push(Vec::new());
                width = 0;
            }
            push_span(lines.last_mut().unwrap(), grapheme, span.style);
            width += grapheme_width;
            if width >= max {
                lines.push(Vec::new());
                width = 0;
            }
        }
    }
    if lines.len() > 1 && lines.last().is_some_and(Vec::is_empty) {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}
fn cell_width(cell: &Cell) -> usize {
    UnicodeWidthStr::width(cell.plain_text().as_str())
}

fn active_style(styles: &[StyleKind]) -> Style {
    Style {
        bold: styles.contains(&StyleKind::Bold),
        italic: styles.contains(&StyleKind::Italic),
        underline: styles.contains(&StyleKind::Underline),
    }
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

// Many CJK fonts have no italic face. Measure the regular/bold runs with the
// same shaper used for rendering, then slant italic runs around their baseline.
// This also preserves the advance of spaces between differently styled runs.
fn write_oblique_line(
    svg: &mut String,
    spans: &[Span],
    x: f32,
    baseline: f32,
    anchor: &str,
    header: bool,
    family: &str,
    options: &usvg::Options,
) -> anyhow::Result<()> {
    let mut measurement =
        String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1">"#);
    for (index, span) in spans.iter().enumerate() {
        write!(
            &mut measurement,
            r#"<text id="run-{index}" x="0" y="0" xml:space="preserve" font-family="{family}" font-size="{FONT}" font-weight="{}">{}</text>"#,
            if header || span.style.bold { 700 } else { 400 },
            esc(&span.text),
        )?;
    }
    measurement.push_str("</svg>");
    let measured = usvg::Tree::from_str(&measurement, options)?;
    let widths: Vec<f32> = (0..spans.len())
        .map(|index| {
            measured
                .node_by_id(&format!("run-{index}"))
                .map(|node| node.bounding_box().width())
                .unwrap_or(0.0)
        })
        .collect();
    let total: f32 = widths.iter().sum();
    let mut cursor = x - match anchor {
        "middle" => total / 2.0,
        "end" => total,
        _ => 0.0,
    };
    for (span, width) in spans.iter().zip(widths) {
        write!(
            svg,
            r##"<g transform="translate({cursor} {baseline}){}"><text x="0" y="0" xml:space="preserve" font-family="{family}" font-size="{FONT}" font-weight="{}" fill="#17212b"{}>{}</text></g>"##,
            if span.style.italic { " skewX(-12)" } else { "" },
            if header || span.style.bold { 700 } else { 400 },
            if span.style.underline {
                " text-decoration=\"underline\""
            } else {
                ""
            },
            esc(&span.text),
        )?;
        cursor += width;
    }
    Ok(())
}

type StyledLine = Vec<Span>;
type StyledCellLines = Vec<StyledLine>;
type StyledRow = Vec<StyledCellLines>;

fn wrapped_rows(
    rows: &[Vec<Cell>],
    columns: std::ops::Range<usize>,
    widths: &[usize],
    line_limit: usize,
) -> Vec<StyledRow> {
    let mut segments = Vec::new();
    for row in rows {
        let empty = Cell::default();
        let cells: Vec<Vec<Vec<Span>>> = columns
            .clone()
            .map(|c| wrap(row.get(c).unwrap_or(&empty), widths[c]))
            .collect();
        let count = cells.iter().map(Vec::len).max().unwrap_or(1);
        for begin in (0..count).step_by(line_limit) {
            let end = (begin + line_limit).min(count);
            segments.push(
                cells
                    .iter()
                    .map(|cell| {
                        let cell_end = end.min(cell.len());
                        if begin < cell_end {
                            cell[begin..cell_end].to_vec()
                        } else {
                            vec![Vec::new()]
                        }
                    })
                    .collect(),
            );
        }
    }
    segments
}

pub(crate) fn render(t: &Table) -> anyhow::Result<Vec<Vec<u8>>> {
    if t.rows.is_empty() {
        anyhow::bail!("table has no rows");
    }
    let cols = t.rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            t.rows
                .iter()
                .map(|r| r.get(c).map(cell_width).unwrap_or(0))
                .max()
                .unwrap_or(0)
                .clamp(8, 40)
        })
        .collect();
    let col_px: Vec<f32> = widths
        .iter()
        .map(|w| (*w as f32 * FONT + PX * 2.0).max(110.0))
        .collect();

    static FONT_DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    let font_db = FONT_DB
        .get_or_init(|| {
            let mut database = fontdb::Database::new();
            database.load_system_fonts();
            Arc::new(database)
        })
        .clone();
    if font_db.is_empty() {
        anyhow::bail!("no system fonts are available to render the table");
    }
    let mut font_options = usvg::Options::default();
    font_options.fontdb = font_db.clone();
    let family = [
        "Menlo",
        "Consolas",
        "DejaVu Sans Mono",
        "Liberation Mono",
        "monospace",
    ]
    .into_iter()
    .find(|name| {
        font_db
            .faces()
            .any(|face| face.families.iter().any(|(family, _)| family == name))
    })
    .unwrap_or("monospace")
    .to_owned();
    font_options.font_family = family.clone();

    let mut groups = Vec::new();
    let mut start = 0;
    while start < cols {
        let mut end = start;
        let mut width = 0.0;
        while end < cols && (end == start || width + col_px[end] <= 1700.0) {
            width += col_px[end];
            end += 1;
        }
        groups.push(start..end);
        start = end;
    }

    let mut output = Vec::new();
    for group in groups {
        let group_width: f32 = col_px[group.clone()].iter().sum();
        let header_segments = wrapped_rows(&t.rows[..1], group.clone(), &widths, 64);
        let mut header: StyledRow = vec![Vec::new(); group.len()];
        for segment in header_segments {
            for (col, lines) in segment.into_iter().enumerate() {
                header[col].extend(lines);
            }
        }
        if header.iter().all(Vec::is_empty) {
            header = vec![vec![Vec::new()]; group.len()];
        }
        let header_h = header.iter().map(Vec::len).max().unwrap_or(1) as f32 * LH + PY * 2.0;
        let body_segments = wrapped_rows(&t.rows[1..], group.clone(), &widths, 50);
        let mut pages: Vec<Vec<StyledRow>> = vec![Vec::new()];
        let mut page_lines = 0usize;
        for segment in body_segments {
            let line_count = segment.iter().map(Vec::len).max().unwrap_or(1);
            if page_lines > 0 && page_lines + line_count > 50 {
                pages.push(Vec::new());
                page_lines = 0;
            }
            pages.last_mut().unwrap().push(segment);
            page_lines += line_count;
        }

        for page in pages {
            let body_h: f32 = page
                .iter()
                .map(|row| row.iter().map(Vec::len).max().unwrap_or(1) as f32 * LH + PY * 2.0)
                .sum();
            let height = (header_h + body_h + 2.0).ceil() as u32;
            let width = group_width.ceil() as u32;
            let mut svg = String::new();
            write!(
                &mut svg,
                r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}"><rect width="100%" height="100%" fill="#ffffff"/>"##
            )?;
            let mut y = 1.0f32;
            for (ri, row) in std::iter::once(&header).chain(page.iter()).enumerate() {
                let row_lines = row.iter().map(Vec::len).max().unwrap_or(1);
                let rh = row_lines as f32 * LH + PY * 2.0;
                write!(
                    &mut svg,
                    r##"<rect x="1" y="{y}" width="{}" height="{rh}" fill="{}" stroke="#b8c2cc"/>"##,
                    group_width - 2.0,
                    if ri == 0 { "#e9eef4" } else { "#ffffff" }
                )?;
                let mut x = 0.0f32;
                for (local_col, cell_lines) in row.iter().enumerate() {
                    if local_col > 0 {
                        write!(
                            &mut svg,
                            r##"<path d="M {x} {y} v {rh}" stroke="#c7d0d9"/>"##
                        )?;
                    }
                    let global_col = group.start + local_col;
                    let align = t
                        .alignments
                        .get(global_col)
                        .copied()
                        .unwrap_or(Alignment::None);
                    let text_anchor = match align {
                        Alignment::Center => "middle",
                        Alignment::Right => "end",
                        _ => "start",
                    };
                    let tx = match align {
                        Alignment::Center => x + col_px[global_col] / 2.0,
                        Alignment::Right => x + col_px[global_col] - PX,
                        _ => x + PX,
                    };
                    for (line_no, spans) in cell_lines.iter().enumerate() {
                        if spans.iter().any(|span| span.style.italic) {
                            write_oblique_line(
                                &mut svg,
                                spans,
                                tx,
                                y + PY + FONT + line_no as f32 * LH,
                                text_anchor,
                                ri == 0,
                                &family,
                                &font_options,
                            )?;
                            continue;
                        }
                        write!(
                            &mut svg,
                            r##"<text x="{tx}" y="{}" text-anchor="{text_anchor}" font-family="{family}" font-size="{FONT}" fill="#17212b"{}>"##,
                            y + PY + FONT + line_no as f32 * LH,
                            if ri == 0 { " font-weight=\"700\"" } else { "" }
                        )?;
                        for span in spans {
                            let mut attrs = String::new();
                            if span.style.bold {
                                attrs.push_str(" font-weight=\"700\"");
                            }
                            if span.style.italic {
                                attrs.push_str(" font-style=\"italic\"");
                            }
                            if span.style.underline {
                                attrs.push_str(" text-decoration=\"underline\"");
                            }
                            write!(&mut svg, "<tspan{attrs}>{}</tspan>", esc(&span.text))?;
                        }
                        svg.push_str("</text>");
                    }
                    x += col_px[global_col];
                }
                y += rh;
            }
            svg.push_str("</svg>");
            let tree = usvg::Tree::from_str(&svg, &font_options)
                .map_err(|e| anyhow::anyhow!("SVG rendering setup failed: {e}"))?;
            let mut pixmap = tiny_skia::Pixmap::new(width, height)
                .ok_or_else(|| anyhow::anyhow!("could not allocate table image"))?;
            resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
            output.push(pixmap.encode_png()?);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_preserves_table_and_surrounding_prose() {
        let s = "before\n\n| Name | Note |\n| --- | --- |\n| a\\|b | `x\\|y` |\n\nafter";
        let p = parse(s);
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], Part::Text("before\n\n".into()));
        let Part::Table(t) = &p[1] else { panic!() };
        assert_eq!(
            t.rows[1].iter().map(Cell::plain_text).collect::<Vec<_>>(),
            vec!["a|b", "x|y"]
        );
        assert!(t.source.contains("a\\|b"));
        assert_eq!(p[2], Part::Text("\n\nafter".into()));
    }
    #[test]
    fn markdown_inline_styles_survive_nesting_and_grapheme_wrapping() {
        let tick = char::from(96);
        let source = format!(
            "| Plain | Styled | Literal |\n| --- | --- | --- |\n| x | **bold** *italic* __underline__ __**한국어 _기울임_**__ | \\*literal\\* {tick}__code__{tick} |"
        );
        let Part::Table(parsed) = &parse(&source)[0] else {
            panic!()
        };
        let styled = &parsed.rows[1][1];
        assert!(
            styled
                .0
                .iter()
                .any(|span| span.text == "bold" && span.style.bold && !span.style.underline)
        );
        assert!(
            styled
                .0
                .iter()
                .any(|span| span.text == "italic" && span.style.italic)
        );
        assert!(
            styled
                .0
                .iter()
                .any(|span| span.text == "underline" && span.style.underline)
        );
        assert!(
            styled
                .0
                .iter()
                .any(|span| span.text == "한국어 " && span.style.bold && span.style.underline)
        );
        assert!(styled.0.iter().any(|span| span.text == "기울임"
            && span.style.bold
            && span.style.underline
            && span.style.italic));
        let literal = &parsed.rows[1][2];
        assert!(
            literal
                .0
                .iter()
                .any(|span| span.text.contains("*literal*") && span.style == Style::default())
        );
        assert!(literal.plain_text().contains("__code__"));
        assert!(literal.0.iter().all(|span| span.style == Style::default()));

        let nested_code =
            format!("| H |\n| --- |\n| **bold {tick}__literal__{tick} and more bold** |");
        let Part::Table(nested_table) = &parse(&nested_code)[0] else {
            panic!()
        };
        let nested_cell = &nested_table.rows[1][0];
        assert!(nested_cell.plain_text().contains("__literal__"));
        assert!(nested_cell.0.iter().all(|span| span.style.bold));

        let wrapped_source = format!(
            "| H |\n| --- |\n| **{}** |",
            "한국어 text ".repeat(12) + "wrap"
        );
        let Part::Table(wrapped_table) = &parse(&wrapped_source)[0] else {
            panic!()
        };
        let cell = &wrapped_table.rows[1][0];
        let lines = wrap(cell, 10);
        assert!(lines.len() > 1);
        assert!(lines.iter().flatten().all(|span| span.style.bold));
        let reconstructed: String = lines
            .iter()
            .flatten()
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(reconstructed, cell.plain_text());
    }

    #[test]
    fn emphasis_styles_change_rendered_png_pixels() {
        let plain_source = "| H |\n| --- |\n| styled sample |";
        let Part::Table(plain) = &parse(plain_source)[0] else {
            panic!()
        };
        let plain_png = render(plain).unwrap().remove(0);
        for markup in [
            "**styled sample**",
            "*styled sample*",
            "__styled sample__",
            "**__styled sample__**",
        ] {
            let source = format!("| H |\n| --- |\n| {markup} |");
            let Part::Table(styled) = &parse(&source)[0] else {
                panic!()
            };
            let png = render(styled).unwrap().remove(0);
            assert!(
                pixel_difference_count(&png, &plain_png) > 0,
                "rendering should preserve {markup}"
            );
            assert!(dark_pixel_count(&png) > 10);
        }
        let Part::Table(korean_plain) = &parse("| H |\n| --- |\n| 한국어 기울임 |")[0] else {
            panic!()
        };
        let Part::Table(korean_italic) = &parse("| H |\n| --- |\n| *한국어 기울임* |")[0]
        else {
            panic!()
        };
        assert!(
            pixel_difference_count(
                &render(korean_italic).unwrap()[0],
                &render(korean_plain).unwrap()[0]
            ) > 0
        );
    }

    #[test]
    fn multilingual_png_output_and_fenced_pipes() {
        let p = parse("```md\na | b\n```\n\n| 한국어 | 日本語 |\n| --- | --- |\n| 中文 | 👩‍👩‍👧‍👦 |\n");
        assert_eq!(p.len(), 3);
        let Part::Table(t) = &p[1] else { panic!() };
        assert_eq!(t.rows[1][0].plain_text(), "中文");
        assert!(wrap(&Cell::from("한글語".repeat(5)), 3).len() > 1);
        let png = render(t).unwrap();
        assert_eq!(png.len(), 1);
        assert!(png[0].starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(dark_pixel_count(&png[0]) > 20);
        let blank = Table {
            source: String::new(),
            rows: vec![vec![Cell::default()], vec![Cell::default()]],
            alignments: vec![Alignment::Left],
        };
        assert!(dark_pixel_count(&png[0]) > dark_pixel_count(&render(&blank).unwrap()[0]));
    }
    fn pixel_difference_count(a: &[u8], b: &[u8]) -> usize {
        let (aw, ah, ap) = decoded_pixels(a);
        let (bw, bh, bp) = decoded_pixels(b);
        assert_eq!((aw, ah), (bw, bh));
        ap.chunks_exact(4)
            .zip(bp.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count()
    }

    fn decoded_pixels(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut data).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgba);
        (info.width, info.height, data[..info.buffer_size()].to_vec())
    }

    fn dark_pixel_count(png: &[u8]) -> usize {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        buf[..info.buffer_size()]
            .chunks_exact(info.color_type.samples() * info.bit_depth as usize / 8)
            .filter(|px| px.get(0).copied().unwrap_or(255) < 180)
            .count()
    }
    #[test]
    fn wide_and_tall_tables_keep_all_cells_and_split() {
        let long = "中文".repeat(1200);
        let rows = vec![
            vec![
                Cell::from("H1"),
                Cell::from("H2"),
                Cell::from("H3"),
                Cell::from("H4"),
                Cell::from("H5"),
                Cell::from("H6"),
            ],
            vec![
                Cell::from(long.clone()),
                Cell::from("short"),
                Cell::from(long.clone()),
                Cell::from("tail"),
                Cell::from(long.clone()),
                Cell::from("end"),
            ],
        ];
        let t = Table {
            source: String::new(),
            rows,
            alignments: vec![Alignment::Left; 6],
        };
        let pngs = render(&t).unwrap();
        assert!(pngs.len() >= 4);
        let segments = wrapped_rows(&t.rows, 0..6, &[40, 8, 40, 8, 40, 8], 50);
        assert!(segments.iter().any(|r| {
            r[1].iter()
                .flatten()
                .map(|s| s.text.as_str())
                .collect::<String>()
                == "short"
        }));
        assert!(segments.iter().any(|r| {
            r[3].iter()
                .flatten()
                .map(|s| s.text.as_str())
                .collect::<String>()
                == "tail"
        }));
        assert!(segments.iter().any(|r| {
            r[5].iter()
                .flatten()
                .map(|s| s.text.as_str())
                .collect::<String>()
                == "end"
        }));
        let text: String = segments
            .iter()
            .skip(1)
            .flat_map(|r| r[0].iter().flatten())
            .map(|span| span.text.as_str())
            .collect();
        assert_eq!(text, long);
    }
    #[test]
    fn long_table_splits_pages() {
        let mut rows = vec![vec!["Header".into()]];
        for i in 0..110 {
            rows.push(vec![Cell::from(format!("row {i} {}", "text ".repeat(5)))]);
        }
        let t = Table {
            source: String::new(),
            rows,
            alignments: vec![Alignment::Left],
        };
        let png = render(&t).unwrap();
        assert!(png.len() > 1);
        assert!(png.iter().all(|p| p.starts_with(b"\x89PNG\r\n\x1a\n")));
    }
}
