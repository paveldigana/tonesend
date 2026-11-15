//! C ABI for mobile apps. See `include/tonesend.h` — keep the two in sync.
#![allow(clippy::missing_safety_doc)]

use modem::transfer::{self, Event, TransferReceiver, TransferStats};
use modem::{Activity, Band, Rate};
use std::collections::VecDeque;

fn band(id: u8) -> Band {
    Band::ALL
        .get(id as usize)
        .copied()
        .unwrap_or(Band::AudibleWide)
}

fn band_id(b: Band) -> u8 {
    Band::ALL.iter().position(|&x| x == b).unwrap_or(0) as u8
}

fn rate(id: u8) -> Rate {
    Rate::from_id(id).unwrap_or(Rate::Normal)
}

unsafe fn bytes<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if len == 0 || data.is_null() {
        &[]
    } else {
        std::slice::from_raw_parts(data, len)
    }
}

fn leak_samples(v: Vec<f32>, out_len: *mut usize) -> *mut f32 {
    let mut b = v.into_boxed_slice();
    if !out_len.is_null() {
        unsafe { *out_len = b.len() };
    }
    let p = b.as_mut_ptr();
    std::mem::forget(b);
    p
}

#[no_mangle]
pub unsafe extern "C" fn ts_encode(
    data: *const u8,
    len: usize,
    band_id: u8,
    rate_id: u8,
    out_len: *mut usize,
) -> *mut f32 {
    let d = bytes(data, len.min(modem::modem::MAX_PAYLOAD));
    leak_samples(
        modem::encode(d, &band(band_id).config(), rate(rate_id)),
        out_len,
    )
}

#[no_mangle]
pub unsafe extern "C" fn ts_probe_generate(frame_bytes: usize, out_len: *mut usize) -> *mut f32 {
    leak_samples(modem::probe::generate(frame_bytes), out_len)
}

#[no_mangle]
pub unsafe extern "C" fn ts_samples_free(samples: *mut f32, len: usize) {
    if !samples.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            samples, len,
        )));
    }
}

#[no_mangle]
pub extern "C" fn ts_net_bitrate(band_id: u8, rate_id: u8, len: usize) -> f64 {
    modem::net_bitrate(&band(band_id).params(), rate(rate_id), len)
}

#[no_mangle]
pub extern "C" fn ts_max_payload() -> usize {
    modem::modem::MAX_PAYLOAD
}

#[no_mangle]
pub unsafe extern "C" fn ts_transfer_encode(
    data: *const u8,
    len: usize,
    band_id: u8,
    rate_id: u8,
    id: u32,
    redundancy: f32,
    out_len: *mut usize,
) -> *mut f32 {
    let d = bytes(data, len);
    leak_samples(
        transfer::encode(d, &band(band_id).config(), rate(rate_id), id, redundancy),
        out_len,
    )
}

#[no_mangle]
pub unsafe extern "C" fn ts_transfer_repair(
    data: *const u8,
    len: usize,
    band_id: u8,
    rate_id: u8,
    id: u32,
    sent: f32,
    extra: f32,
    out_len: *mut usize,
) -> *mut f32 {
    let d = bytes(data, len);
    leak_samples(
        transfer::encode_repair(d, &band(band_id).config(), rate(rate_id), id, sent, extra),
        out_len,
    )
}

#[no_mangle]
pub extern "C" fn ts_transfer_seconds(
    len: usize,
    band_id: u8,
    rate_id: u8,
    redundancy: f32,
) -> f64 {
    transfer::air_time(len, &band(band_id).config(), rate(rate_id), redundancy)
}

// ------------------------------------------------------------------ content

use modem::content::{self, Kind, PackInfo};

#[repr(C)]
pub struct TsPackInfo {
    original_len: u64,
    packed_len: u64,
    compressed: u8,
}

fn leak_bytes(v: Vec<u8>, out_len: *mut usize) -> *mut u8 {
    let mut b = v.into_boxed_slice();
    if !out_len.is_null() {
        unsafe { *out_len = b.len() };
    }
    let p = b.as_mut_ptr();
    std::mem::forget(b);
    p
}

unsafe fn put_info(info: *mut TsPackInfo, i: PackInfo) {
    if let Some(o) = info.as_mut() {
        o.original_len = i.original_len as u64;
        o.packed_len = i.packed_len as u64;
        o.compressed = i.compressed as u8;
    }
}

unsafe fn str_arg<'a>(p: *const std::os::raw::c_char, len: usize) -> std::borrow::Cow<'a, str> {
    String::from_utf8_lossy(bytes(p as *const u8, len))
}

#[no_mangle]
pub unsafe extern "C" fn ts_pack_text(
    utf8: *const std::os::raw::c_char,
    len: usize,
    info: *mut TsPackInfo,
    out_len: *mut usize,
) -> *mut u8 {
    let (v, i) = content::pack_text(&str_arg(utf8, len));
    put_info(info, i);
    leak_bytes(v, out_len)
}

#[no_mangle]
pub unsafe extern "C" fn ts_pack_file(
    name: *const std::os::raw::c_char,
    name_len: usize,
    data: *const u8,
    len: usize,
    info: *mut TsPackInfo,
    out_len: *mut usize,
) -> *mut u8 {
    let (v, i) = content::pack_file(&str_arg(name, name_len), bytes(data, len));
    put_info(info, i);
    leak_bytes(v, out_len)
}

#[no_mangle]
pub unsafe extern "C" fn ts_pack_image(
    name: *const std::os::raw::c_char,
    name_len: usize,
    data: *const u8,
    len: usize,
    info: *mut TsPackInfo,
    out_len: *mut usize,
) -> *mut u8 {
    let (v, i) = content::pack_image_file(&str_arg(name, name_len), bytes(data, len));
    put_info(info, i);
    leak_bytes(v, out_len)
}

#[no_mangle]
pub unsafe extern "C" fn ts_image_encode_rgba(
    rgba: *const u8,
    w: u32,
    h: u32,
    max_side: u32,
    quality: f32,
    out_w: *mut u32,
    out_h: *mut u32,
    out_len: *mut usize,
) -> *mut u8 {
    let px = bytes(rgba, w as usize * h as usize * 4);
    match content::encode_image_rgba(px, w as usize, h as usize, max_side as usize, quality) {
        Some((v, nw, nh)) => {
            if let Some(o) = out_w.as_mut() {
                *o = nw as u32
            }
            if let Some(o) = out_h.as_mut() {
                *o = nh as u32
            }
            leak_bytes(v, out_len)
        }
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn ts_image_decode(
    data: *const u8,
    len: usize,
    w: *mut u32,
    h: *mut u32,
    out_len: *mut usize,
) -> *mut u8 {
    match content::decode_image(bytes(data, len)) {
        Some((iw, ih, px)) => {
            if let Some(o) = w.as_mut() {
                *o = iw as u32
            }
            if let Some(o) = h.as_mut() {
                *o = ih as u32
            }
            leak_bytes(px, out_len)
        }
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn ts_bytes_free(p: *mut u8, len: usize) {
    if !p.is_null() {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(p, len)));
    }
}

#[repr(C)]
pub struct TsContent {
    kind: u8,
    compressed: u8,
    name: *const u8,
    name_len: usize,
    data: *const u8,
    data_len: usize,
    wire_len: usize,
}

#[repr(C)]
struct ContentBox {
    c: TsContent,
    name: String,
    data: Vec<u8>,
}

#[no_mangle]
pub unsafe extern "C" fn ts_unpack(data: *const u8, len: usize) -> *mut TsContent {
    let u = content::unpack(bytes(data, len));
    let kind = match u.kind {
        Kind::Text => 0,
        Kind::File => 1,
        Kind::Image => 2,
        Kind::Raw => 15,
    };
    let mut b = Box::new(ContentBox {
        c: TsContent {
            kind,
            compressed: u.compressed as u8,
            name: std::ptr::null(),
            name_len: 0,
            data: std::ptr::null(),
            data_len: 0,
            wire_len: u.wire_len,
        },
        name: u.name,
        data: u.data,
    });
    b.c.name = b.name.as_ptr();
    b.c.name_len = b.name.len();
    b.c.data = b.data.as_ptr();
    b.c.data_len = b.data.len();
    Box::into_raw(b) as *mut TsContent
}

#[no_mangle]
pub unsafe extern "C" fn ts_content_free(c: *mut TsContent) {
    if !c.is_null() {
        drop(Box::from_raw(c as *mut ContentBox));
    }
}

pub struct TsReceiver {
    rx: TransferReceiver,
    queue: VecDeque<Event>,
}

#[no_mangle]
pub extern "C" fn ts_receiver_new(band_mask: u32) -> *mut TsReceiver {
    let bands: Vec<Band> = Band::ALL
        .iter()
        .enumerate()
        .filter(|(i, _)| band_mask & (1 << i) != 0)
        .map(|(_, &b)| b)
        .collect();
    let bands = if bands.is_empty() {
        Band::ALL.to_vec()
    } else {
        bands
    };
    Box::into_raw(Box::new(TsReceiver {
        rx: TransferReceiver::new(&bands),
        queue: VecDeque::new(),
    }))
}

#[no_mangle]
pub unsafe extern "C" fn ts_receiver_free(rx: *mut TsReceiver) {
    if !rx.is_null() {
        drop(Box::from_raw(rx));
    }
}

#[no_mangle]
pub unsafe extern "C" fn ts_receiver_push(
    rx: *mut TsReceiver,
    samples: *const f32,
    n: usize,
) -> usize {
    let Some(r) = rx.as_mut() else { return 0 };
    if n > 0 && !samples.is_null() {
        let ev = r.rx.push(std::slice::from_raw_parts(samples, n));
        r.queue.extend(ev);
    }
    r.queue.len()
}

#[repr(C)]
pub struct TsEvent {
    kind: u8,
    band: u8,
    rate: u8,
    _pad: u8,
    transfer_id: u32,
    crc32: u32,
    total_len: u32,
    frames: u32,
    blocks_ok: u32,
    blocks_total: u32,
    shards_have: u32,
    shards_needed: u32,
    repaired: u32,
    repeat_count: u32,
    air_time: f32,
    goodput_bps: f32,
    snr_db: f32,
    drift_ppm: f32,
    f_lo: f32,
    f_hi: f32,
    data: *const u8,
    data_len: usize,
    carrier_snr_db: *const f32,
    carrier_count: usize,
}

/// `c` must stay the first field: the C side only sees a pointer to it.
#[repr(C)]
struct EventBox {
    c: TsEvent,
    data: Vec<u8>,
    snr: Vec<f32>,
}

fn c_event(kind: u8, id: u32, s: &TransferStats) -> TsEvent {
    TsEvent {
        kind,
        band: s.band.map_or(0, band_id),
        rate: s.rate.map_or(1, |r| r.id()),
        _pad: 0,
        transfer_id: id,
        crc32: s.crc,
        total_len: s.total_len as u32,
        frames: s.frames as u32,
        blocks_ok: s.blocks_ok as u32,
        blocks_total: s.blocks_total as u32,
        shards_have: s.shards_have as u32,
        shards_needed: s.shards_needed as u32,
        repaired: s.repaired as u32,
        repeat_count: 1,
        air_time: s.air_time as f32,
        goodput_bps: s.goodput_bps() as f32,
        snr_db: s.snr_db,
        drift_ppm: s.drift_ppm as f32,
        f_lo: s.f_lo,
        f_hi: s.f_hi,
        data: std::ptr::null(),
        data_len: 0,
        carrier_snr_db: std::ptr::null(),
        carrier_count: 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn ts_receiver_next_event(rx: *mut TsReceiver) -> *mut TsEvent {
    let Some(r) = rx.as_mut() else {
        return std::ptr::null_mut();
    };
    let Some(ev) = r.queue.pop_front() else {
        return std::ptr::null_mut();
    };
    let (c, data, snr) = match ev {
        Event::Complete { id, data, stats } => (c_event(1, id, &stats), data, stats.carrier_snr_db),
        Event::Incomplete { id, stats } => {
            (c_event(2, id, &stats), Vec::new(), stats.carrier_snr_db)
        }
        Event::Repeat { id, count, stats } => {
            let mut c = c_event(3, id, &stats);
            c.repeat_count = count as u32;
            (c, Vec::new(), stats.carrier_snr_db)
        }
    };
    let mut b = Box::new(EventBox { c, data, snr });
    b.c.data = b.data.as_ptr();
    b.c.data_len = b.data.len();
    b.c.carrier_snr_db = b.snr.as_ptr();
    b.c.carrier_count = b.snr.len();
    Box::into_raw(b) as *mut TsEvent
}

#[no_mangle]
pub unsafe extern "C" fn ts_event_free(ev: *mut TsEvent) {
    if !ev.is_null() {
        drop(Box::from_raw(ev as *mut EventBox));
    }
}

#[repr(C)]
pub struct TsStatus {
    activity: u8,
    band: u8,
    rate: u8,
    transfer_active: u8,
    bytes: u32,
    progress: f32,
    level_db: f32,
    transfer_id: u32,
    transfer_len: u32,
    shards_have: u32,
    shards_needed: u32,
    blocks_lost: u32,
}

/// Frame-level diagnostics since the last call as UTF-8 JSON Lines (one object per
/// detected preamble: `rx_frame` or `rx_rejected`). NULL when there is nothing new.
/// Free with `ts_bytes_free`.
#[no_mangle]
pub unsafe extern "C" fn ts_receiver_take_log(rx: *mut TsReceiver, out_len: *mut usize) -> *mut u8 {
    let Some(r) = rx.as_mut() else {
        return std::ptr::null_mut();
    };
    let diag = r.rx.take_diag();
    if diag.is_empty() {
        return std::ptr::null_mut();
    }
    let mut s = String::new();
    for d in diag {
        s.push_str(&d.to_json());
        s.push('\n');
    }
    let mut b = s.into_bytes().into_boxed_slice();
    if !out_len.is_null() {
        *out_len = b.len();
    }
    let p = b.as_mut_ptr();
    std::mem::forget(b);
    p
}

#[no_mangle]
pub unsafe extern "C" fn ts_receiver_status(rx: *const TsReceiver) -> TsStatus {
    let mut s = TsStatus {
        activity: 0,
        band: 0,
        rate: 0,
        transfer_active: 0,
        bytes: 0,
        progress: 0.0,
        level_db: -120.0,
        transfer_id: 0,
        transfer_len: 0,
        shards_have: 0,
        shards_needed: 0,
        blocks_lost: 0,
    };
    let Some(r) = rx.as_ref() else { return s };
    let st = r.rx.status();
    s.level_db = st.level_db;
    match st.activity {
        Activity::Listening => {}
        Activity::Detected { band } => {
            s.activity = 1;
            s.band = band_id(band);
        }
        Activity::Receiving {
            band,
            rate,
            bytes,
            progress,
        } => {
            s.activity = 2;
            s.band = band_id(band);
            s.rate = rate.id();
            s.bytes = bytes as u32;
            s.progress = progress;
        }
    }
    if let Some((id, t)) = r.rx.active() {
        s.transfer_active = 1;
        s.transfer_id = id;
        s.transfer_len = t.total_len as u32;
        s.shards_have = t.shards_have as u32;
        s.shards_needed = t.shards_needed as u32;
        s.blocks_lost = (t.blocks_total - t.blocks_ok) as u32;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_api_content_roundtrip() {
        unsafe {
            let text = "https://www.example.com/a/very/long/path?utm_source=sound&utm_source=sound";
            let mut info = TsPackInfo {
                original_len: 0,
                packed_len: 0,
                compressed: 0,
            };
            let mut n = 0;
            let p = ts_pack_text(text.as_ptr() as *const _, text.len(), &mut info, &mut n);
            assert_eq!(info.packed_len as usize, n);
            let c = ts_unpack(p, n);
            assert_eq!(
                (
                    (*c).kind,
                    std::slice::from_raw_parts((*c).data, (*c).data_len)
                ),
                (0, text.as_bytes())
            );
            ts_content_free(c);
            ts_bytes_free(p, n);
            // image encode/decode
            let rgba = vec![200u8; 64 * 48 * 4];
            let (mut w, mut h) = (0, 0);
            let img = ts_image_encode_rgba(rgba.as_ptr(), 64, 48, 32, 60.0, &mut w, &mut h, &mut n);
            assert_eq!((w, h), (32, 24));
            let (mut dw, mut dh, mut dn) = (0, 0, 0);
            let px = ts_image_decode(img, n, &mut dw, &mut dh, &mut dn);
            assert_eq!((dw, dh, dn), (32, 24, 32 * 24 * 4));
            ts_bytes_free(px, dn);
            ts_bytes_free(img, n);
        }
    }

    #[test]
    fn c_api_transfer_roundtrip() {
        unsafe {
            let msg: Vec<u8> = (0..3000u32).map(|i| (i * 7) as u8).collect();
            let mut n = 0usize;
            let s = ts_transfer_encode(msg.as_ptr(), msg.len(), 0, 2, 77, 0.25, &mut n);
            let est = ts_transfer_seconds(msg.len(), 0, 2, 0.25);
            assert!((est - n as f64 / 48_000.0).abs() < 1e-6);
            let rx = ts_receiver_new(0b111);
            let mut padded = vec![0f32; 10_000];
            padded.extend_from_slice(std::slice::from_raw_parts(s, n));
            padded.extend(vec![0f32; 48_000 * 4]);
            let mut saw_transfer = false;
            for c in padded.chunks(480) {
                ts_receiver_push(rx, c.as_ptr(), c.len());
                saw_transfer |= ts_receiver_status(rx).activity == 2;
            }
            let e = ts_receiver_next_event(rx);
            assert!(!e.is_null());
            let ev = &*e;
            assert_eq!((ev.kind, ev.transfer_id), (1, 77));
            assert_eq!(std::slice::from_raw_parts(ev.data, ev.data_len), &msg[..]);
            assert!(ev.frames >= 1 && ev.goodput_bps > 0.0 && ev.carrier_count > 100);
            assert!(saw_transfer);
            ts_event_free(e);
            assert!(ts_receiver_next_event(rx).is_null());
            let mut n = 0;
            let log = ts_receiver_take_log(rx, &mut n);
            assert!(!log.is_null());
            let text = std::str::from_utf8(std::slice::from_raw_parts(log, n)).unwrap().to_owned();
            ts_bytes_free(log, n);
            assert!(text.lines().count() >= 1);
            assert!(text.lines().all(|l| l.starts_with("{\"type\":\"rx_")));
            assert!(text.contains("\"type\":\"rx_frame\""), "{text}");
            assert!(ts_receiver_take_log(rx, &mut n).is_null());
            ts_receiver_free(rx);
            ts_samples_free(s, n);
        }
    }
}
