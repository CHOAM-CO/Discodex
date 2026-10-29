//! In-memory storage for the original Markdown inside table PNG attachments.
//!
//! The image attachment remains the sole durable source of truth. Discord
//! hosts it, and the view-source button fetches the original attachment URL.
use std::io::Cursor;

use anyhow::{Context as _, Result, bail};
use png::{Decoder, Encoder, text_metadata::ITXtChunk};

const SOURCE_KEYWORD: &str = "Discodex Markdown";

/// Re-encode a PNG with the original UTF-8 Markdown in a standard iTXt chunk.
/// Pixel samples and image dimensions are preserved; no image or source is written
/// to local storage.
pub(crate) fn embed(png_bytes: &[u8], source: &str) -> Result<Vec<u8>> {
    let mut reader = Decoder::new(Cursor::new(png_bytes))
        .read_info()
        .context("read table PNG header")?;
    let mut info = reader.info().clone();
    let buffer_size = reader
        .output_buffer_size()
        .context("table PNG exceeds the available address space")?;
    let mut pixels = vec![0; buffer_size];
    let frame = reader
        .next_frame(&mut pixels)
        .context("decode table PNG pixels")?;
    let pixel_bytes = pixels[..frame.buffer_size()].to_vec();
    info.width = frame.width;
    info.height = frame.height;
    info.color_type = frame.color_type;
    info.bit_depth = frame.bit_depth;
    if info.color_type != png::ColorType::Indexed {
        info.palette = None;
    }
    info.utf8_text
        .retain(|chunk| chunk.keyword != SOURCE_KEYWORD);
    info.utf8_text
        .push(ITXtChunk::new(SOURCE_KEYWORD.to_owned(), source.to_owned()));
    reader.finish().context("finish reading table PNG")?;

    let mut output = Vec::new();
    let mut writer = Encoder::with_info(&mut output, info)
        .context("prepare table PNG metadata")?
        .write_header()
        .context("write table PNG header")?;
    writer
        .write_image_data(&pixel_bytes)
        .context("write table PNG pixels")?;
    writer.finish().context("finish table PNG")?;
    Ok(output)
}

/// Extract the original UTF-8 Markdown from a table PNG.
pub(crate) fn extract(png_bytes: &[u8]) -> Result<String> {
    let reader = Decoder::new(Cursor::new(png_bytes))
        .read_info()
        .context("read table PNG metadata")?;
    for chunk in reader.info().utf8_text.iter().rev() {
        if chunk.keyword == SOURCE_KEYWORD {
            return chunk.get_text().context("decode table Markdown metadata");
        }
    }
    bail!("table PNG does not contain original Markdown")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_png() -> (Vec<u8>, Vec<u8>) {
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 12, 34, 56, 0,
        ];
        let mut bytes = Vec::new();
        let mut encoder = Encoder::new(&mut bytes, 2, 2);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&pixels).unwrap();
        writer.finish().unwrap();
        (bytes, pixels)
    }

    fn decoded_pixels(bytes: &[u8]) -> (u32, u32, png::ColorType, png::BitDepth, Vec<u8>) {
        let mut reader = Decoder::new(Cursor::new(bytes)).read_info().unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut data).unwrap();
        let pixels = data[..frame.buffer_size()].to_vec();
        reader.finish().unwrap();
        (
            frame.width,
            frame.height,
            frame.color_type,
            frame.bit_depth,
            pixels,
        )
    }

    #[test]
    fn utf8_markdown_round_trips_without_changing_pixels() {
        let (png, pixels) = test_png();
        let source = "| 名前 | 값 |\n| --- | --- |\n| 猫 🐈 | 한글 |\n";
        let embedded = embed(&png, source).unwrap();

        assert_eq!(extract(&embedded).unwrap(), source);
        assert_eq!(
            decoded_pixels(&embedded),
            (2, 2, png::ColorType::Rgba, png::BitDepth::Eight, pixels)
        );
    }

    #[test]
    fn extraction_reports_missing_metadata_and_invalid_png() {
        let (png, _) = test_png();
        assert!(extract(&png).is_err());
        assert!(extract(b"not a PNG").is_err());
    }
}
