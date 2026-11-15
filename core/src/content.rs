//! Content layer: what a transfer carries, and making it small.
//!
//! - **Envelope** (shared by every app): text, named file or image, plus flags.
//! - **Lossless compression**: Brotli with a built-in shared dictionary of common
//!   short-message content (URLs, Wi‑Fi codes, contact cards, everyday English/Czech),
//!   used only when it actually makes the payload smaller.
//! - **Images**: downscaled and encoded as lossy WebP here, so every platform sends
//!   identical, compact images; decoding back to RGBA is provided too.
//!
//! Envelope v2: `"ST" 0x02 | flags | [name_len | name] | body`
//! - flags bits 0–3: kind (0 text, 1 file, 2 image); bit 4: body is Brotli-compressed
//!   with [`DICTIONARY`]; bits 5–7 reserved.
//! - name (files and images only): length byte (≤ 200) + UTF-8.
//! Envelope v1 (`"ST" 0x01 | kind | …`, no compression) is still decoded.

use brotli::enc::{BrotliEncoderParams, StandardAlloc};
use brotli::{BrotliCompressCustomIoCustomDict, IoReaderWrapper, IoWriterWrapper};
use std::io::{Cursor, Error, ErrorKind};

const MAGIC: [u8; 2] = [0x53, 0x54];
const FLAG_BROTLI: u8 = 0x10;
const KIND_MASK: u8 = 0x0F;

/// Shared Brotli dictionary (part of envelope v2 — never change, add a new version instead).
/// Most frequent material last: it is closest to the data and cheapest to reference.
pub const DICTIONARY: &[u8] = concat!(
    // contact cards, calendar, structured data
    "BEGIN:VCARD\nVERSION:3.0\nN:;\nFN:\nORG:\nTITLE:\nTEL;TYPE=CELL:\nTEL:\nEMAIL:\nURL:\nADR:\nNOTE:\nEND:VCARD\n",
    "BEGIN:VEVENT\nSUMMARY:\nDTSTART:\nDTEND:\nLOCATION:\nEND:VEVENT\n",
    "{\"id\":\"\",\"name\":\"\",\"type\":\"\",\"value\":\"\",\"url\":\"\",\"code\":\"\",\"price\":\"\",\"true\",\"false\",null}",
    "geo:\nmailto:\ntel:+420 \ntel:+1 \nsms:\n",
    // Czech everyday
    "Ahoj, Dobrý den, Děkuji, Díky, prosím, ano, ne, dnes, zítra, včera, ráno, večer, hodin, ",
    "sleva, slevový kód, zdarma, akce, nabídka, cena, Kč, heslo, síť, adresa, telefon, schůzka, ",
    " a  je  na  se  v  s  že  to  pro  do  ze  jsem  jsme  bude  máme  není ",
    // English everyday
    "Hello, Hi, Thanks, Thank you, please, yes, no, today, tomorrow, yesterday, morning, evening, ",
    "meeting, message, address, phone, password, network, price, free, offer, discount, code, ",
    "% off, Use code , Your code is , Order , Ticket , Seat , Gate , Room , Table , Invoice ",
    " the  and  to  of  for  in  is  on  you  your  with  this  that  at  from  are  will ",
    // links and Wi‑Fi (most common in short transfers)
    "WIFI:T:WPA;S:;P:;H:false;;WIFI:T:WPA;S:",
    ".html .php .pdf .jpg .png ?id= &utm_source= /index ",
    ".com/.org/.net/.io/.eu/.cz/.sk/.de/",
    "https://www.http://www.https://",
)
.as_bytes();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    File,
    Image,
    /// Not an envelope (or unknown version): raw bytes.
    Raw,
}

impl Kind {
    fn id(self) -> u8 {
        match self {
            Kind::Text => 0,
            Kind::File => 1,
            Kind::Image => 2,
            Kind::Raw => 15,
        }
    }
}

/// Unpacked content.
#[derive(Clone, Debug, PartialEq)]
pub struct Content {
    pub kind: Kind,
    pub name: String,
    /// Body after decompression (UTF-8 text, file bytes, or WebP/JPEG image file).
    pub data: Vec<u8>,
    /// Size on air (envelope + possibly compressed body).
    pub wire_len: usize,
    pub compressed: bool,
}

/// What packing did, for display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PackInfo {
    /// Content size before compression (image: raw RGBA size).
    pub original_len: usize,
    /// Envelope + body as sent.
    pub packed_len: usize,
    pub compressed: bool,
}

// ------------------------------------------------------------------ compression

fn params(len: usize) -> BrotliEncoderParams {
    let mut p = BrotliEncoderParams::default();
    // Max quality for small/medium payloads (radio time is precious), faster above 4 MB.
    p.quality = if len <= 4 << 20 { 11 } else { 9 };
    p.lgwin = 22;
    p.size_hint = len;
    p
}

/// Brotli with the shared dictionary.
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 16);
    let mut input = Cursor::new(data);
    let (mut ib, mut ob) = ([0u8; 4096], [0u8; 4096]);
    let mut nop =
        |_: &mut brotli::interface::PredictionModeContextMap<brotli::InputReferenceMut>,
         _: &mut [brotli::interface::StaticCommand],
         _: brotli::InputPair,
         _: &mut StandardAlloc| ();
    BrotliCompressCustomIoCustomDict(
        &mut IoReaderWrapper(&mut input),
        &mut IoWriterWrapper(&mut out),
        &mut ib,
        &mut ob,
        &params(data.len()),
        StandardAlloc::default(),
        &mut nop,
        DICTIONARY,
        Error::new(ErrorKind::UnexpectedEof, "eof"),
    )
    .expect("in-memory brotli cannot fail");
    out
}

pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut ib, mut ob) = ([0u8; 4096], [0u8; 4096]);
    brotli_decompressor::BrotliDecompressCustomDict(
        &mut Cursor::new(data),
        &mut out,
        &mut ib,
        &mut ob,
        DICTIONARY.to_vec(),
    )
    .ok()?;
    Some(out)
}

// ------------------------------------------------------------------ envelope

fn envelope(kind: Kind, name: &str, body: &[u8], compressed: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + name.len() + 5);
    v.extend_from_slice(&MAGIC);
    v.push(2);
    v.push(kind.id() | if compressed { FLAG_BROTLI } else { 0 });
    if kind != Kind::Text {
        let mut n = name.as_bytes();
        if n.len() > 200 {
            // cut on a char boundary
            let mut cut = 200;
            while !name.is_char_boundary(cut) {
                cut -= 1;
            }
            n = &name.as_bytes()[..cut];
        }
        v.push(n.len() as u8);
        v.extend_from_slice(n);
    }
    v.extend_from_slice(body);
    v
}

/// Pack with compression only when it helps.
fn pack_body(kind: Kind, name: &str, body: &[u8]) -> (Vec<u8>, PackInfo) {
    let c = compress(body);
    let use_c = c.len() < body.len();
    let env = envelope(kind, name, if use_c { &c } else { body }, use_c);
    let info = PackInfo {
        original_len: body.len(),
        packed_len: env.len(),
        compressed: use_c,
    };
    (env, info)
}

pub fn pack_text(text: &str) -> (Vec<u8>, PackInfo) {
    pack_body(Kind::Text, "", text.as_bytes())
}

pub fn pack_file(name: &str, data: &[u8]) -> (Vec<u8>, PackInfo) {
    pack_body(Kind::File, name, data)
}

/// Pack an already-encoded image file (WebP/JPEG/PNG) as-is.
pub fn pack_image_file(name: &str, data: &[u8]) -> (Vec<u8>, PackInfo) {
    let env = envelope(Kind::Image, name, data, false);
    let info = PackInfo {
        original_len: data.len(),
        packed_len: env.len(),
        compressed: false,
    };
    (env, info)
}

pub fn unpack(bytes: &[u8]) -> Content {
    let raw = || Content {
        kind: Kind::Raw,
        name: String::new(),
        data: bytes.to_vec(),
        wire_len: bytes.len(),
        compressed: false,
    };
    if bytes.len() < 4 || bytes[0..2] != MAGIC {
        return raw();
    }
    let (version, flags) = (bytes[2], bytes[3]);
    let kind = match flags & KIND_MASK {
        0 => Kind::Text,
        1 => Kind::File,
        2 if version >= 2 => Kind::Image,
        _ => return raw(),
    };
    let compressed = version >= 2 && flags & FLAG_BROTLI != 0;
    if version != 1 && version != 2 {
        return raw();
    }
    let mut i = 4;
    let mut name = String::new();
    if kind != Kind::Text {
        let Some(&n) = bytes.get(4) else { return raw() };
        let Some(nb) = bytes.get(5..5 + n as usize) else {
            return raw();
        };
        name = String::from_utf8_lossy(nb).into_owned();
        i = 5 + n as usize;
    }
    let body = &bytes[i..];
    let data = if compressed {
        match decompress(body) {
            Some(d) => d,
            None => return raw(),
        }
    } else {
        body.to_vec()
    };
    if kind == Kind::Text && std::str::from_utf8(&data).is_err() {
        return raw();
    }
    Content {
        kind,
        name,
        data,
        wire_len: bytes.len(),
        compressed,
    }
}

// ------------------------------------------------------------------ images

/// Area-average downscale of RGBA so the longer side is ≤ `max_side` (never upscales).
pub fn downscale_rgba(rgba: &[u8], w: usize, h: usize, max_side: usize) -> (Vec<u8>, usize, usize) {
    let scale = (max_side as f64 / w.max(h) as f64).min(1.0);
    let (nw, nh) = (
        ((w as f64 * scale).round() as usize).max(1),
        ((h as f64 * scale).round() as usize).max(1),
    );
    if (nw, nh) == (w, h) {
        return (rgba.to_vec(), w, h);
    }
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        let (y0, y1) = (y * h / nh, ((y + 1) * h / nh).max(y * h / nh + 1));
        for x in 0..nw {
            let (x0, x1) = (x * w / nw, ((x + 1) * w / nw).max(x * w / nw + 1));
            let mut acc = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = (sy * w + sx) * 4;
                    for c in 0..4 {
                        acc[c] += rgba[p + c] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let o = (y * nw + x) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / n) as u8;
            }
        }
    }
    (out, nw, nh)
}

/// Downscale to `max_side` and encode lossy WebP at `quality` (0–100, ~50–70 typical).
pub fn encode_image_rgba(
    rgba: &[u8],
    w: usize,
    h: usize,
    max_side: usize,
    quality: f32,
) -> Option<(Vec<u8>, usize, usize)> {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let (px, nw, nh) = downscale_rgba(&rgba[..w * h * 4], w, h, max_side.max(16));
    let opaque = px.chunks(4).all(|p| p[3] == 255);
    let mem = if opaque {
        // RGB saves the alpha plane.
        let rgb: Vec<u8> = px.chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
        webp::Encoder::from_rgb(&rgb, nw as u32, nh as u32).encode(quality.clamp(0.0, 100.0))
    } else {
        webp::Encoder::from_rgba(&px, nw as u32, nh as u32).encode(quality.clamp(0.0, 100.0))
    };
    Some((mem.to_vec(), nw, nh))
}

/// Downscale + WebP + envelope, in one step.
pub fn pack_image_rgba(
    rgba: &[u8],
    w: usize,
    h: usize,
    max_side: usize,
    quality: f32,
) -> Option<(Vec<u8>, PackInfo)> {
    let (webp, _, _) = encode_image_rgba(rgba, w, h, max_side, quality)?;
    let env = envelope(Kind::Image, "photo.webp", &webp, false);
    Some((
        env.clone(),
        PackInfo {
            original_len: w * h * 4,
            packed_len: env.len(),
            compressed: true,
        },
    ))
}

/// Decode a WebP image to RGBA (width, height, pixels).
pub fn decode_image(data: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let img = webp::Decoder::new(data).decode()?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let px: &[u8] = &img;
    let rgba = if img.is_alpha() {
        px.to_vec()
    } else {
        px.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()
    };
    Some((w, h, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_roundtrip_and_compression() {
        let long = "The quick brown fox jumps over the lazy dog. ".repeat(40);
        let (p, info) = pack_text(&long);
        assert!(
            info.compressed && info.packed_len * 5 < long.len(),
            "{info:?}"
        );
        let c = unpack(&p);
        assert_eq!((c.kind, c.data.as_slice()), (Kind::Text, long.as_bytes()));

        for short in [
            "https://www.example.com/offer?id=42",
            "WIFI:T:WPA;S:Office;P:secret123;;",
            "Ahoj, zítra v 10 hodin?",
        ] {
            let (p, info) = pack_text(short);
            assert_eq!(unpack(&p).data, short.as_bytes());
            assert!(info.packed_len <= short.len() + 4, "{short}: {info:?}");
        }
    }

    #[test]
    fn incompressible_file_is_sent_raw() {
        let mut r = crate::util::Rng::new(1);
        let mut data = vec![0u8; 3000];
        r.fill_bytes(&mut data);
        let (p, info) = pack_file("noise.bin", &data);
        assert!(!info.compressed);
        let c = unpack(&p);
        assert_eq!(
            (c.kind, c.name.as_str(), c.data),
            (Kind::File, "noise.bin", data)
        );
    }

    #[test]
    fn v1_envelope_still_decodes() {
        let mut v1 = vec![0x53, 0x54, 0x01, 0x00];
        v1.extend_from_slice("old".as_bytes());
        assert_eq!(unpack(&v1).data, b"old");
        assert_eq!(unpack(b"no envelope").kind, Kind::Raw);
    }

    #[test]
    fn image_roundtrip() {
        // Smooth gradient 800×600 → ≤ 320 px WebP.
        let (w, h) = (800, 600);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [(x * 255 / w) as u8, (y * 255 / h) as u8, 128, 255]
            })
            .collect();
        let (p, info) = pack_image_rgba(&rgba, w, h, 320, 60.0).unwrap();
        assert!(info.packed_len < 8000, "{info:?}");
        let c = unpack(&p);
        assert_eq!(c.kind, Kind::Image);
        let (dw, dh, px) = decode_image(&c.data).unwrap();
        assert_eq!((dw, dh), (320, 240));
        // centre pixel close to the source colour
        let o = (120 * 320 + 160) * 4;
        assert!(
            (px[o] as i32 - 127).abs() < 12 && (px[o + 1] as i32 - 127).abs() < 12,
            "{:?}",
            &px[o..o + 4]
        );
    }
}
