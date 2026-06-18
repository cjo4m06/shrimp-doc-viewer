//! Minimal raster image decoder for doc-viewer: PNG + JPEG → straight RGBA8.
//!
//! Uses the `zune-png` / `zune-jpeg` crates (pure-Rust, wasm-friendly, minimal
//! deps) rather than the umbrella `image` crate, to keep the WASM payload small.
//! Both DOCX (`w:drawing`) and PPTX (`p:pic`) frontends call [`decode`] and push
//! a `dv_ir::Command::Image` with the result.

/// A decoded image as straight (un-premultiplied) RGBA8, row-major (`w*h*4`).
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode PNG or JPEG bytes. Returns `None` for unsupported formats
/// (EMF/WMF/GIF/WebP/BMP/TIFF) or on decode failure.
pub fn decode(bytes: &[u8]) -> Option<DecodedImage> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'] {
        decode_png(bytes)
    } else if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        decode_jpeg(bytes)
    } else {
        None
    }
}

fn decode_png(bytes: &[u8]) -> Option<DecodedImage> {
    use zune_png::zune_core::result::DecodingResult;
    let mut dec = zune_png::PngDecoder::new(bytes);
    let result = dec.decode().ok()?;
    let (w, h) = dec.get_dimensions()?;
    let channels = dec.get_colorspace().map(|c| c.num_components()).unwrap_or(0);

    let bytes8: Vec<u8> = match result {
        DecodingResult::U8(v) => v,
        DecodingResult::U16(v) => v.iter().map(|&x| (x >> 8) as u8).collect(),
        _ => return None,
    };
    let rgba = to_rgba(&bytes8, channels, w.checked_mul(h)?)?;
    Some(DecodedImage { width: w as u32, height: h as u32, rgba })
}

fn decode_jpeg(bytes: &[u8]) -> Option<DecodedImage> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let opts = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
    let px = dec.decode().ok()?;
    let (w, h) = dec.dimensions()?;
    if px.len() < w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    Some(DecodedImage { width: w as u32, height: h as u32, rgba: px })
}

/// Expand `channels`-channel pixels to RGBA8.
fn to_rgba(src: &[u8], channels: usize, px_count: usize) -> Option<Vec<u8>> {
    if px_count == 0 || src.len() < px_count.checked_mul(channels)? {
        return None;
    }
    let mut out = Vec::with_capacity(px_count * 4);
    match channels {
        4 => out.extend_from_slice(&src[..px_count * 4]),
        3 => {
            for i in 0..px_count {
                let o = i * 3;
                out.extend_from_slice(&[src[o], src[o + 1], src[o + 2], 255]);
            }
        }
        2 => {
            for i in 0..px_count {
                let o = i * 2;
                let v = src[o];
                out.extend_from_slice(&[v, v, v, src[o + 1]]);
            }
        }
        1 => {
            for i in 0..px_count {
                let v = src[i];
                out.extend_from_slice(&[v, v, v, 255]);
            }
        }
        _ => return None,
    }
    Some(out)
}
