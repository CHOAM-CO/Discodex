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
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Table {
    pub source: String,
    pub rows: Vec<Vec<String>>,
    pub alignments: Vec<Alignment>,
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
            let mut cell = String::new();
            let mut inside = false;
            let mut j = i + 1;
            while j < events.len() {
                let (e, r) = &events[j];
                end = end.max(r.end);
                match e {
                    Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => row.clear(),
                    Event::Start(Tag::TableCell) => {
                        cell.clear();
                        inside = true;
                    }
                    Event::End(TagEnd::TableCell) => {
                        row.push(cell.trim().to_owned());
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
                    Event::Text(s) | Event::Code(s) | Event::Html(s) | Event::InlineHtml(s)
                        if inside =>
                    {
                        cell.push_str(s)
                    }
                    Event::SoftBreak | Event::HardBreak if inside => cell.push(' '),
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
fn wrap(s: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    for p in s.split('\n') {
        let mut line = String::new();
        let mut n = 0;
        for g in p.graphemes(true) {
            let w = UnicodeWidthStr::width(g);
            if n > 0 && n + w > max {
                out.push(std::mem::take(&mut line));
                n = 0;
            }
            line.push_str(g);
            n += w;
            if n >= max {
                out.push(std::mem::take(&mut line));
                n = 0;
            }
        }
        if !line.is_empty() || p.is_empty() {
            out.push(line);
        }
    }
    out
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn wrapped_rows(
    rows: &[Vec<String>],
    columns: std::ops::Range<usize>,
    widths: &[usize],
    line_limit: usize,
) -> Vec<Vec<Vec<String>>> {
    let mut segments = Vec::new();
    for row in rows {
        let cells: Vec<Vec<String>> = columns
            .clone()
            .map(|c| wrap(row.get(c).map(String::as_str).unwrap_or(""), widths[c]))
            .collect();
        let count = cells.iter().map(Vec::len).max().unwrap_or(1);
        for begin in (0..count).step_by(line_limit) {
            let end = (begin + line_limit).min(count);
            segments.push(
                cells
                    .iter()
                    .map(|cell| {
                        let cell_end = end.min(cell.len());
                        let part = if begin < cell_end {
                            &cell[begin..cell_end]
                        } else {
                            &[]
                        };
                        if part.is_empty() {
                            vec![String::new()]
                        } else {
                            part.to_vec()
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
                .map(|r| {
                    r.get(c)
                        .map(|s| UnicodeWidthStr::width(s.as_str()))
                        .unwrap_or(0)
                })
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
        let mut header = vec![Vec::new(); group.len()];
        for segment in header_segments {
            for (col, lines) in segment.into_iter().enumerate() {
                header[col].extend(lines);
            }
        }
        if header.iter().all(Vec::is_empty) {
            header = vec![vec![String::new()]; group.len()];
        }
        let header_h = header.iter().map(Vec::len).max().unwrap_or(1) as f32 * LH + PY * 2.0;
        let body_segments = wrapped_rows(&t.rows[1..], group.clone(), &widths, 50);
        let mut pages: Vec<Vec<Vec<Vec<String>>>> = vec![Vec::new()];
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
                    for (line_no, text) in cell_lines.iter().enumerate() {
                        write!(
                            &mut svg,
                            r##"<text x="{tx}" y="{}" text-anchor="{text_anchor}" font-family="{family}" font-size="{FONT}" fill="#17212b"{}>{}</text>"##,
                            y + PY + FONT + line_no as f32 * LH,
                            if ri == 0 { " font-weight=\"700\"" } else { "" },
                            esc(text)
                        )?;
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
        assert_eq!(t.rows[1], vec!["a|b", "x|y"]);
        assert!(t.source.contains("a\\|b"));
        assert_eq!(p[2], Part::Text("\n\nafter".into()));
    }
    #[test]
    fn multilingual_png_output_and_fenced_pipes() {
        let p = parse("```md\na | b\n```\n\n| 한국어 | 日本語 |\n| --- | --- |\n| 中文 | 👩‍👩‍👧‍👦 |\n");
        assert_eq!(p.len(), 3);
        let Part::Table(t) = &p[1] else { panic!() };
        assert_eq!(t.rows[1][0], "中文");
        assert!(wrap(&"한글語".repeat(5), 3).len() > 1);
        let png = render(t).unwrap();
        assert_eq!(png.len(), 1);
        assert!(png[0].starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(dark_pixel_count(&png[0]) > 20);
        let blank = Table {
            source: String::new(),
            rows: vec![vec![String::new()], vec![String::new()]],
            alignments: vec![Alignment::Left],
        };
        assert!(dark_pixel_count(&png[0]) > dark_pixel_count(&render(&blank).unwrap()[0]));
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
                "H1".into(),
                "H2".into(),
                "H3".into(),
                "H4".into(),
                "H5".into(),
                "H6".into(),
            ],
            vec![
                long.clone(),
                "short".into(),
                long.clone(),
                "tail".into(),
                long.clone(),
                "end".into(),
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
        assert!(segments.iter().any(|r| r[1] == vec!["short"]));
        assert!(segments.iter().any(|r| r[3] == vec!["tail"]));
        assert!(segments.iter().any(|r| r[5] == vec!["end"]));
        let text: String = segments
            .iter()
            .skip(1)
            .flat_map(|r| r[0].iter())
            .cloned()
            .collect();
        assert_eq!(text, long);
    }
    #[test]
    fn long_table_splits_pages() {
        let mut rows = vec![vec!["Header".into()]];
        for i in 0..110 {
            rows.push(vec![format!("row {i} {}", "text ".repeat(5))]);
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
