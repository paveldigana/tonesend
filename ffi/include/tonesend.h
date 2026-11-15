// C interface to the ToneSend OFDM modem (Rust, crate `tonesend`).
// Audio is mono float32 at TS_SAMPLE_RATE.
// Band ids: 0 wide, 1 narrow, 2 inaudible, 3 max, 4 high (append-only).
// Rate ids: 0 safe, 1 normal, 2 fast, 3 turbo.
#pragma once
#include <stddef.h>
#include <stdint.h>

#define TS_SAMPLE_RATE 48000

typedef struct TsReceiver TsReceiver;

// One finished (or stalled) transfer.
typedef struct {
    uint8_t kind;            // 1 = complete (checksum verified), 2 = incomplete (needs repair data),
                             // 3 = repeat of a looped message already delivered (no data)
    uint8_t band;
    uint8_t rate;
    uint8_t _pad;
    uint32_t transfer_id;
    uint32_t crc32;          // whole-file CRC-32 announced by the sender
    uint32_t total_len;
    uint32_t frames;
    uint32_t blocks_ok;
    uint32_t blocks_total;
    uint32_t shards_have;    // distinct useful shards received
    uint32_t shards_needed;  // shards needed to rebuild the file
    uint32_t repaired;       // data shards rebuilt from repair (parity) data
    uint32_t repeat_count;   // kind 3: times this message has been heard so far
    float air_time;          // seconds of audio carrying this transfer
    float goodput_bps;       // file bits per second of air time
    float snr_db;
    float drift_ppm;
    float f_lo;              // band edges of carrier_snr_db (Hz)
    float f_hi;
    const uint8_t *data;     // complete only
    size_t data_len;
    const float *carrier_snr_db;
    size_t carrier_count;
} TsEvent;

typedef struct {
    uint8_t activity;        // 0 listening, 1 preamble detected, 2 receiving a frame
    uint8_t band;            // valid when activity >= 1
    uint8_t rate;            // valid when activity == 2
    uint8_t transfer_active; // 1 if a transfer is being assembled
    uint32_t bytes;          // current frame payload size, valid when activity == 2
    float progress;          // current frame 0..1, valid when activity == 2
    float level_db;          // input RMS, dBFS
    uint32_t transfer_id;
    uint32_t transfer_len;
    uint32_t shards_have;
    uint32_t shards_needed;
    uint32_t blocks_lost;
} TsStatus;

// --- content: envelope, compression, images -----------------------------------
// Build the bytes to send with ts_transfer_encode. Text and files are Brotli-compressed
// (with a built-in dictionary) when that makes them smaller; images become WebP.

typedef struct {
    uint64_t original_len;   // content size before compression (image: raw RGBA bytes)
    uint64_t packed_len;     // envelope + body as sent
    uint8_t compressed;
} TsPackInfo;

// Content kinds: 0 text, 1 file, 2 image (WebP/JPEG/PNG file), 15 raw (no envelope).
typedef struct {
    uint8_t kind;
    uint8_t compressed;      // body was Brotli-compressed on air
    const char *name;        // UTF-8, not NUL-terminated
    size_t name_len;
    const uint8_t *data;     // decompressed body
    size_t data_len;
    size_t wire_len;         // bytes on air
} TsContent;

uint8_t *ts_pack_text(const char *utf8, size_t len, TsPackInfo *info, size_t *out_len);
uint8_t *ts_pack_file(const char *name, size_t name_len, const uint8_t *data, size_t len,
                      TsPackInfo *info, size_t *out_len);
// Already-encoded image file (e.g. from ts_image_encode_rgba).
uint8_t *ts_pack_image(const char *name, size_t name_len, const uint8_t *data, size_t len,
                       TsPackInfo *info, size_t *out_len);
// RGBA8 pixels (row-major, w*h*4) -> downscaled to max_side -> lossy WebP (quality 0..100).
uint8_t *ts_image_encode_rgba(const uint8_t *rgba, uint32_t w, uint32_t h, uint32_t max_side,
                              float quality, uint32_t *out_w, uint32_t *out_h, size_t *out_len);
// WebP -> RGBA8 pixels. NULL if not decodable.
uint8_t *ts_image_decode(const uint8_t *data, size_t len, uint32_t *w, uint32_t *h, size_t *out_len);
void ts_bytes_free(uint8_t *bytes, size_t len);

TsContent *ts_unpack(const uint8_t *bytes, size_t len);
void ts_content_free(TsContent *content);

// --- single raw frame (no transfer layer) -------------------------------------
float *ts_encode(const uint8_t *data, size_t len, uint8_t band, uint8_t rate, size_t *out_len);
// Channel-measurement probe signal (see `tonesend probe analyze` on desktop).
float *ts_probe_generate(size_t frame_bytes, size_t *out_len);
void ts_samples_free(float *samples, size_t len);
double ts_net_bitrate(uint8_t band, uint8_t rate, size_t len);
size_t ts_max_payload(void);

// --- transfers: checksum + Reed-Solomon repair data ---------------------------
// redundancy: extra repair data as a fraction (0.25 = 25 %).
float *ts_transfer_encode(const uint8_t *data, size_t len, uint8_t band, uint8_t rate,
                          uint32_t transfer_id, float redundancy, size_t *out_len);
// Additional, previously unsent repair data. `sent` = redundancy already sent.
float *ts_transfer_repair(const uint8_t *data, size_t len, uint8_t band, uint8_t rate,
                          uint32_t transfer_id, float sent, float extra, size_t *out_len);
double ts_transfer_seconds(size_t len, uint8_t band, uint8_t rate, float redundancy);

// Streaming receiver. band_mask: bit i set = listen for band id i.
TsReceiver *ts_receiver_new(uint32_t band_mask);
void ts_receiver_free(TsReceiver *rx);
// Feed samples; returns the number of events waiting in the queue.
size_t ts_receiver_push(TsReceiver *rx, const float *samples, size_t n);
// Next event or NULL. Free with ts_event_free.
TsEvent *ts_receiver_next_event(TsReceiver *rx);
void ts_event_free(TsEvent *event);
TsStatus ts_receiver_status(const TsReceiver *rx);
// Frame-level diagnostics since the last call, UTF-8 JSON Lines: one object per detected
// preamble, {"type":"rx_frame",...} (band, rate, at_s, dur_s, bytes, blocks_ok, blocks,
// snr_db, slice_snr_db[8], drift_ppm, score, copies, attempts, header_assumed) or
// {"type":"rx_rejected",...} (band, at_s, score, header, header_assumed).
// NULL if nothing new. Free with ts_bytes_free.
uint8_t *ts_receiver_take_log(TsReceiver *rx, size_t *out_len);
