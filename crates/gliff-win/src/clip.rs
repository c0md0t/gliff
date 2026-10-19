//! The local clipboard, on a thread of its own: it reports local changes
//! (text, or an image as PNG) and writes what the server offers. Content
//! it wrote itself is recognised by fingerprint and not reported back, so
//! a copy does not bounce between the two machines.
//!
//! On Windows the clipboard's sequence number is polled, and the
//! clipboard opened only when it moves; elsewhere the content itself is
//! polled.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self as std_mpsc, RecvTimeoutError};
use std::time::Duration;

use bytes::Bytes;
use gliff_proto::clipboard::{is_text_mime, MAX_ITEM, TEXT_MIME};
use tokio::sync::mpsc::UnboundedSender;

pub const PNG_MIME: &str = "image/png";

const POLL: Duration = Duration::from_millis(250);

/// What one side's clipboard holds, in the form it crosses the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Png(Bytes),
}

impl Content {
    pub fn mime(&self) -> &'static str {
        match self {
            Content::Text(_) => TEXT_MIME,
            Content::Png(_) => PNG_MIME,
        }
    }

    pub fn bytes(&self) -> Bytes {
        match self {
            Content::Text(t) => Bytes::copy_from_slice(t.as_bytes()),
            Content::Png(p) => p.clone(),
        }
    }

    /// The peer may ask for any text flavour of offered text.
    pub fn serves(&self, mime: &str) -> bool {
        match self {
            Content::Text(_) => is_text_mime(mime),
            Content::Png(_) => mime == PNG_MIME,
        }
    }
}

/// Start the clipboard thread. Local changes arrive on `local`; send
/// content to the returned sender to put it on the local clipboard.
/// `None` when there is no clipboard to use.
pub fn spawn(local: UnboundedSender<Content>) -> Option<std_mpsc::Sender<Content>> {
    let clipboard = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gliff: no local clipboard ({e}); clipboard sharing is off");
            return None;
        }
    };
    let (tx, rx) = std_mpsc::channel();
    std::thread::Builder::new()
        .name("gliff-clipboard".into())
        .spawn(move || run(clipboard, rx, local))
        .ok()?;
    Some(tx)
}

fn run(
    mut clipboard: arboard::Clipboard,
    remote: std_mpsc::Receiver<Content>,
    local: UnboundedSender<Content>,
) {
    // Fingerprint of what the clipboard held when last read or written.
    let mut seen: Option<u64> = None;
    #[cfg(windows)]
    let mut sequence: Option<u32> = None;
    loop {
        match remote.recv_timeout(POLL) {
            Ok(content) => {
                // Rewriting what the clipboard already holds would still
                // count as a new selection, which the server (sharing this
                // clipboard in a loopback test) would offer straight back.
                if seen.is_some() && fingerprint(&content) == seen {
                    continue;
                }
                match write(&mut clipboard, &content) {
                    Ok(fingerprint) => seen = Some(fingerprint),
                    Err(e) => tracing::warn!(error = %e, "cannot set the local clipboard"),
                }
                // Our own write moves the sequence number; skip it.
                #[cfg(windows)]
                {
                    sequence = Some(clipboard_sequence());
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                #[cfg(windows)]
                {
                    let now = clipboard_sequence();
                    if sequence == Some(now) {
                        continue;
                    }
                    sequence = Some(now);
                }
                let Some((fingerprint, content)) = read(&mut clipboard) else {
                    continue;
                };
                if seen == Some(fingerprint) {
                    continue;
                }
                seen = Some(fingerprint);
                if local.send(content).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(windows)]
fn clipboard_sequence() -> u32 {
    // SAFETY: no preconditions.
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// The clipboard's text, or else its image as PNG, with a fingerprint of
/// what was read.
fn read(clipboard: &mut arboard::Clipboard) -> Option<(u64, Content)> {
    if let Ok(text) = clipboard.get_text() {
        if !text.is_empty() && text.len() as u64 <= MAX_ITEM {
            return Some((text_fingerprint(&text), Content::Text(text)));
        }
    }
    let image = clipboard.get_image().ok()?;
    let fingerprint = image_fingerprint(image.width, image.height, &image.bytes);
    let png = encode_png(image.width as u32, image.height as u32, &image.bytes)?;
    if png.len() as u64 > MAX_ITEM {
        return None;
    }
    Some((fingerprint, Content::Png(png.into())))
}

/// Put `content` on the clipboard; the fingerprint is of what was written,
/// as `read` would compute it.
fn write(clipboard: &mut arboard::Clipboard, content: &Content) -> anyhow::Result<u64> {
    match content {
        Content::Text(text) => {
            clipboard.set_text(text.clone())?;
            Ok(text_fingerprint(text))
        }
        Content::Png(png) => {
            let (width, height, rgba) = decode_png(png)?;
            let fingerprint = image_fingerprint(width as usize, height as usize, &rgba);
            clipboard.set_image(arboard::ImageData {
                width: width as usize,
                height: height as usize,
                bytes: rgba.into(),
            })?;
            Ok(fingerprint)
        }
    }
}

/// The fingerprint `content` would have once written.
fn fingerprint(content: &Content) -> Option<u64> {
    match content {
        Content::Text(text) => Some(text_fingerprint(text)),
        Content::Png(png) => {
            let (width, height, rgba) = decode_png(png).ok()?;
            Some(image_fingerprint(width as usize, height as usize, &rgba))
        }
    }
}

fn text_fingerprint(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    "text".hash(&mut h);
    text.hash(&mut h);
    h.finish()
}

fn image_fingerprint(width: usize, height: usize, rgba: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    "image".hash(&mut h);
    (width, height).hash(&mut h);
    rgba.hash(&mut h);
    h.finish()
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

/// Decode any 8-bit-able PNG to RGBA.
fn decode_png(data: &[u8]) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf)?;
    let pixels = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => anyhow::bail!("indexed PNG left unexpanded"),
    };
    Ok((info.width, info.height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip_keeps_pixels() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 9, 9, 9, 9];
        let png = encode_png(2, 2, &rgba).unwrap();
        let (w, h, back) = decode_png(&png).unwrap();
        assert_eq!((w, h), (2, 2));
        assert_eq!(back, rgba);
    }

    #[test]
    fn text_is_served_in_any_text_flavour() {
        let t = Content::Text("hi".into());
        assert!(t.serves(TEXT_MIME));
        assert!(t.serves("text/plain"));
        assert!(!t.serves(PNG_MIME));
        assert!(Content::Png(Bytes::new()).serves(PNG_MIME));
    }

    /// What we write, read back, must fingerprint the same, or a remote
    /// copy would be offered straight back. Uses the real clipboard, so it
    /// runs on Windows CI only.
    #[cfg(windows)]
    #[test]
    fn windows_clipboard_reads_back_what_we_wrote() {
        let mut clipboard = arboard::Clipboard::new().unwrap();
        let text = Content::Text("gliff clipboard test\r\nline two £".into());
        let wrote = write(&mut clipboard, &text).unwrap();
        let (read_fp, read_back) = read(&mut clipboard).unwrap();
        assert_eq!(read_back, text);
        assert_eq!(read_fp, wrote);

        let rgba: Vec<u8> = (0..16 * 8).flat_map(|i| [i as u8, 50, 200, 255]).collect();
        let png = Content::Png(encode_png(16, 8, &rgba).unwrap().into());
        let wrote = write(&mut clipboard, &png).unwrap();
        let (read_fp, _) = read(&mut clipboard).unwrap();
        assert_eq!(
            read_fp, wrote,
            "an opaque image survives the clipboard unchanged"
        );
    }

    #[test]
    fn fingerprints_tell_text_from_images() {
        assert_ne!(text_fingerprint(""), image_fingerprint(0, 0, &[]));
        assert_eq!(text_fingerprint("a"), text_fingerprint("a"));
    }
}
