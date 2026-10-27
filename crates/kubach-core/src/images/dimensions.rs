//! Pixel size limits of downloaded images (#201).
//!
//! A few hundred bytes can describe a huge picture: an SVG with
//! `width="20000"` or a PNG whose header claims 100000×100000 pixels. GPUI
//! decodes rasters at their full size (every frame of an animation) and
//! rasterizes SVG at twice its size, so such a file would take gigabytes
//! of memory. The size is checked from the header (PNG, JPEG, GIF, WebP)
//! or by parsing the SVG before an image is cached and before a cached
//! file is handed to the UI.

use std::sync::Mutex;

use super::{ImageError, ImageKind};
use crate::sync;

/// Widest and tallest image accepted, in pixels as decoded (for SVG, as
/// rasterized by GPUI).
pub const MAX_SIDE: u32 = 4096;
/// Pixels of all frames of an animated GIF or WebP together: GPUI decodes
/// each frame at the full canvas size (four 4096×4096 frames, 256 MB as RGBA).
pub const MAX_ANIMATION_PIXELS: u64 = 4 * (MAX_SIDE as u64) * (MAX_SIDE as u64);
/// GPUI rasterizes an SVG image at twice its size (`SMOOTH_SVG_SCALE_FACTOR`).
pub const SVG_RASTER_SCALE: f32 = 2.0;

/// Fails when the image in `bytes` would decode to more pixels than the
/// limits allow, or when its size can't be read.
pub fn check(kind: ImageKind, bytes: &[u8]) -> Result<(), ImageError> {
    let (width, height) = match kind {
        ImageKind::Svg => {
            let (width, height) = svg_raster_size(bytes)?;
            // Rounded up: the pixmap is allocated with whole pixels.
            (to_pixels(width), to_pixels(height))
        }
        _ => raster_size(bytes)?,
    };
    check_side(width, height)?;
    let frames = match kind {
        ImageKind::Gif => gif_frame_pixels(bytes, width, height)?,
        ImageKind::WebP => webp_frames(bytes) * u64::from(width) * u64::from(height),
        _ => 0,
    };
    if frames > MAX_ANIMATION_PIXELS {
        return Err(ImageError::TooManyPixels { width, height });
    }
    Ok(())
}

fn to_pixels(size: f32) -> u32 {
    if size.is_finite() && size > 0.0 {
        size.ceil().min(u32::MAX as f32) as u32
    } else {
        u32::MAX
    }
}

fn check_side(width: u32, height: u32) -> Result<(), ImageError> {
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(ImageError::TooManyPixels { width, height });
    }
    Ok(())
}

/// Width and height of a PNG, JPEG, GIF or WebP image from its header.
pub fn raster_size(bytes: &[u8]) -> Result<(u32, u32), ImageError> {
    let size = imagesize::blob_size(bytes).map_err(|_| ImageError::NotAnImage)?;
    let clamp = |side: usize| u32::try_from(side).unwrap_or(u32::MAX);
    Ok((clamp(size.width), clamp(size.height)))
}

/// The size GPUI rasterizes an SVG image at, in pixels: its `width` and
/// `height` in any units (`viewBox` when they are missing) times
/// [`SVG_RASTER_SCALE`]. Raster images embedded as `data:` URLs are
/// checked too: they are decoded at their own size. An `<image>` usvg
/// would load from a file, or an embedded `data:` SVG with an external
/// reference ([`super::svg_refs`]), fails with
/// [`ImageError::ExternalReference`].
pub fn svg_raster_size(bytes: &[u8]) -> Result<(f32, f32), ImageError> {
    let refused: Mutex<Option<ImageError>> = Mutex::new(None);
    let refuse = |error: ImageError| {
        let mut refused = sync::lock(&refused);
        refused.get_or_insert(error);
    };
    let mut options = usvg::Options::default();
    // Never read local files named by `<image href>` while checking; the
    // stock resolver GPUI draws with would read them, so it's an error.
    options.image_href_resolver.resolve_string = Box::new(|href, _| {
        refuse(ImageError::ExternalReference(
            href.chars().take(100).collect(),
        ));
        None
    });
    let default_data = usvg::ImageHrefResolver::default_data_resolver();
    options.image_href_resolver.resolve_data = Box::new(move |mime, data, options| {
        let raster = imagesize::image_type(&data).is_ok_and(|kind| {
            matches!(
                kind,
                imagesize::ImageType::Png
                    | imagesize::ImageType::Jpeg
                    | imagesize::ImageType::Gif
                    | imagesize::ImageType::Webp
            )
        });
        // A nested SVG: `image/svg+xml`, or `text/plain` that isn't a raster.
        if (mime == "image/svg+xml" || (mime == "text/plain" && !raster))
            && let Err(error) = super::svg_refs::check(&data)
        {
            refuse(error);
            return None;
        }
        default_data(mime, data, options)
    });
    let parsed = usvg::Tree::from_data(bytes, &options);
    drop(options);
    if let Some(error) = sync::into_inner(refused) {
        return Err(error);
    }
    let tree = parsed.map_err(|_| ImageError::NotAnImage)?;
    check_embedded(tree.root())?;
    let size = tree.size();
    Ok((
        size.width() * SVG_RASTER_SCALE,
        size.height() * SVG_RASTER_SCALE,
    ))
}

/// Embedded raster images anywhere in `group`, nested SVG, masks,
/// patterns and clip paths included.
fn check_embedded(group: &usvg::Group) -> Result<(), ImageError> {
    for node in group.children() {
        match node {
            usvg::Node::Group(group) => check_embedded(group)?,
            usvg::Node::Image(image) => match image.kind() {
                usvg::ImageKind::JPEG(data)
                | usvg::ImageKind::PNG(data)
                | usvg::ImageKind::GIF(data)
                | usvg::ImageKind::WEBP(data) => {
                    let (width, height) = raster_size(data)?;
                    check_side(width, height)?;
                }
                // Walked below as a subroot.
                usvg::ImageKind::SVG(_) => {}
            },
            _ => {}
        }
        let mut result = Ok(());
        node.subroots(|sub| {
            if result.is_ok() {
                result = check_embedded(sub);
            }
        });
        result?;
    }
    Ok(())
}

/// Pixels of all frames of a GIF as decoded: each frame takes the whole
/// `width`×`height` canvas, or its own size when larger. Fails when a frame
/// is over [`MAX_SIDE`]. Stops at the end of the data, like the decoder.
fn gif_frame_pixels(bytes: &[u8], width: u32, height: u32) -> Result<u64, ImageError> {
    let canvas = u64::from(width) * u64::from(height);
    let mut total = 0u64;
    let Some(&flags) = bytes.get(10) else {
        return Ok(total);
    };
    let mut ix = 13;
    if flags & 0x80 != 0 {
        ix += 3 << ((flags & 7) + 1);
    }
    let le16 = |at: usize| -> Option<u32> {
        Some(u32::from(u16::from_le_bytes([
            *bytes.get(at)?,
            *bytes.get(at + 1)?,
        ])))
    };
    loop {
        match bytes.get(ix) {
            // Image descriptor: position, size, flags, then the data.
            Some(0x2C) => {
                let (Some(w), Some(h), Some(&local)) =
                    (le16(ix + 5), le16(ix + 7), bytes.get(ix + 9))
                else {
                    return Ok(total);
                };
                check_side(w, h)?;
                total = total.saturating_add(canvas.max(u64::from(w) * u64::from(h)));
                ix += 10;
                if local & 0x80 != 0 {
                    ix += 3 << ((local & 7) + 1);
                }
                // LZW minimum code size.
                ix += 1;
                match skip_sub_blocks(bytes, ix) {
                    Some(next) => ix = next,
                    None => return Ok(total),
                }
            }
            // Extension: label, then sub-blocks.
            Some(0x21) => match skip_sub_blocks(bytes, ix + 2) {
                Some(next) => ix = next,
                None => return Ok(total),
            },
            // Trailer, garbage or the end of the data.
            _ => return Ok(total),
        }
    }
}

/// The index after the GIF data sub-blocks starting at `ix`.
fn skip_sub_blocks(bytes: &[u8], mut ix: usize) -> Option<usize> {
    loop {
        let len = usize::from(*bytes.get(ix)?);
        ix += 1;
        if len == 0 {
            return Some(ix);
        }
        ix += len;
    }
}

/// Frames of a WebP image: its `ANMF` chunks, 1 for a still image.
fn webp_frames(bytes: &[u8]) -> u64 {
    let mut frames = 0u64;
    let mut ix = 12;
    while let Some(header) = bytes.get(ix..ix + 8) {
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if &header[..4] == b"ANMF" {
            frames += 1;
        }
        // Chunks are padded to an even length.
        ix = ix.saturating_add(8).saturating_add(len + (len & 1));
    }
    frames.max(1)
}

#[cfg(test)]
mod tests {
    use super::{ImageError, ImageKind, MAX_SIDE, check, raster_size, svg_raster_size};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }

    fn gif(width: u16, height: u16, frames: usize) -> Vec<u8> {
        let mut bytes = b"GIF89a".to_vec();
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0]);
        for _ in 0..frames {
            // Graphic control extension, then the frame with one sub-block.
            bytes.extend_from_slice(&[0x21, 0xF9, 4, 0, 0, 0, 0, 0]);
            bytes.push(0x2C);
            bytes.extend_from_slice(&[0, 0, 0, 0]);
            bytes.extend_from_slice(&width.to_le_bytes());
            bytes.extend_from_slice(&height.to_le_bytes());
            bytes.extend_from_slice(&[0, 2, 2, 0x4C, 0x01, 0]);
        }
        bytes.push(0x3B);
        bytes
    }

    fn svg(attributes: &str) -> Vec<u8> {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" {attributes}><rect width=\"1\" height=\"1\"/></svg>")
            .into_bytes()
    }

    fn too_big(result: Result<(), ImageError>) -> bool {
        matches!(result, Err(ImageError::TooManyPixels { .. }))
    }

    #[test]
    fn png_size_from_the_header() {
        assert_eq!(raster_size(&png(64, 32)).unwrap(), (64, 32));
        assert!(check(ImageKind::Png, &png(MAX_SIDE, MAX_SIDE)).is_ok());
        assert!(too_big(check(ImageKind::Png, &png(MAX_SIDE + 1, 16))));
        assert!(too_big(check(ImageKind::Png, &png(100_000, 100_000))));
        assert!(matches!(
            check(ImageKind::Png, b"\x89PNG\r\n\x1a\n"),
            Err(ImageError::NotAnImage)
        ));
    }

    #[test]
    fn gif_frames_count_together() {
        assert!(check(ImageKind::Gif, &gif(256, 256, 10)).is_ok());
        assert!(too_big(check(ImageKind::Gif, &gif(5000, 10, 1))));
        // 2048×2048: one frame is fine, 17 of them are over 4×4096².
        assert!(check(ImageKind::Gif, &gif(2048, 2048, 16)).is_ok());
        assert!(too_big(check(ImageKind::Gif, &gif(2048, 2048, 17))));
    }

    #[test]
    fn svg_width_and_height_are_doubled() {
        let (w, h) = svg_raster_size(&svg("width=\"100\" height=\"50\"")).unwrap();
        assert_eq!((w, h), (200.0, 100.0));
        assert!(check(ImageKind::Svg, &svg("width=\"2048\" height=\"2048\"")).is_ok());
        // GPUI would rasterize it at 4098 pixels.
        assert!(too_big(check(
            ImageKind::Svg,
            &svg("width=\"2049\" height=\"10\"")
        )));
        assert!(too_big(check(
            ImageKind::Svg,
            &svg("width=\"20000\" height=\"20000\"")
        )));
    }

    #[test]
    fn svg_without_size_uses_the_view_box() {
        let (w, h) = svg_raster_size(&svg("viewBox=\"0 0 300 150\"")).unwrap();
        assert_eq!((w, h), (600.0, 300.0));
        assert!(too_big(check(
            ImageKind::Svg,
            &svg("viewBox=\"0 0 30000 10\"")
        )));
        // A small width with a huge viewBox is drawn small.
        assert!(
            check(
                ImageKind::Svg,
                &svg("width=\"64\" height=\"64\" viewBox=\"0 0 99999 99999\"")
            )
            .is_ok()
        );
    }

    #[test]
    fn svg_units_are_converted() {
        // 1pt = 4/3 px.
        let (w, _) = svg_raster_size(&svg("width=\"75pt\" height=\"75pt\"")).unwrap();
        assert!((w - 200.0).abs() < 0.01, "{w}");
        // 1cm = 96/2.54 px: 60cm ≈ 2268 px, rasterized at ≈ 4535.
        let (w, _) = svg_raster_size(&svg("width=\"60cm\" height=\"1cm\"")).unwrap();
        assert!((w - 4535.43).abs() < 0.1, "{w}");
        assert!(too_big(check(
            ImageKind::Svg,
            &svg("width=\"60cm\" height=\"1cm\"")
        )));
        assert!(check(ImageKind::Svg, &svg("width=\"20cm\" height=\"20cm\"")).is_ok());
    }

    #[test]
    fn svg_embedded_rasters_are_checked() {
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD.encode(png(50_000, 50_000));
        let bomb = svg(&format!(
            "width=\"16\" height=\"16\"><image width=\"16\" height=\"16\" href=\"data:image/png;base64,{data}\"/"
        ));
        assert!(too_big(check(ImageKind::Svg, &bomb)));
    }

    #[test]
    fn svg_file_images_are_refused() {
        let refused = |result: Result<(f32, f32), ImageError>| {
            matches!(result, Err(ImageError::ExternalReference(_)))
        };
        // usvg would hand this to the file resolver.
        assert!(refused(svg_raster_size(&svg(
            "width=\"16\" height=\"16\"><image width=\"16\" height=\"16\" href=\"/etc/passwd\"/"
        ))));
        // A nested data: SVG with an external reference.
        use base64::Engine as _;
        let nested = base64::engine::general_purpose::STANDARD.encode(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\">\
             <use href=\"other.svg#x\"/></svg>",
        );
        assert!(refused(svg_raster_size(&svg(&format!(
            "width=\"16\" height=\"16\"><image width=\"16\" height=\"16\" href=\"data:image/svg+xml;base64,{nested}\"/"
        )))));
        let fine = base64::engine::general_purpose::STANDARD
            .encode("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"8\" height=\"8\"/>");
        assert!(
            svg_raster_size(&svg(&format!(
                "width=\"16\" height=\"16\"><image width=\"16\" height=\"16\" href=\"data:image/svg+xml;base64,{fine}\"/"
            )))
            .is_ok()
        );
    }

    #[test]
    fn broken_svg_is_not_an_image() {
        assert!(matches!(
            check(ImageKind::Svg, b"<svg"),
            Err(ImageError::NotAnImage)
        ));
    }
}
