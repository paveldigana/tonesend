//! `tonesend music` — embed short data IDs in music and check they survive lossy codecs.

use crate::{read_wav, write_wav};
use anyhow::{bail, Context, Result};
use clap::Args;
use modem::util::Rng;
use modem::{decode, encode, Band, Rate, SAMPLE_RATE};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Args)]
pub struct MusicArgs {
    /// Music file (WAV, or any format ffmpeg reads). Omit to use a synthetic test track.
    #[arg(short, long)]
    input: Option<PathBuf>,
    /// The data to embed (repeated through the track).
    #[arg(long, default_value = "AD-2026-0042")]
    id: String,
    #[arg(long, default_value = "inaudible")]
    band: Band,
    #[arg(long, default_value = "safe")]
    rate: Rate,
    /// Data level relative to the music's RMS, dB. Several values = sweep.
    #[arg(
        long,
        value_delimiter = ',',
        allow_hyphen_values = true,
        default_value = "-10,-20,-30"
    )]
    level: Vec<f32>,
    /// Seconds between repetitions of the ID.
    #[arg(long, default_value_t = 3.0)]
    every: f32,
    /// Codec settings to test (name:kbps), e.g. mp3:128,aac:96,opus:64,wav.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "wav,mp3:320,mp3:192,mp3:128,mp3:96,aac_at:256,aac_at:128,aac_at:64,aac:128,opus:128,opus:64"
    )]
    codecs: Vec<String>,
    /// Keep the mixed WAV and codec outputs here (to listen to them).
    #[arg(long)]
    out_dir: Option<PathBuf>,
    /// Length of the synthetic track, seconds.
    #[arg(long, default_value_t = 30.0)]
    seconds: f32,
}

pub fn run(a: MusicArgs) -> Result<()> {
    let work = std::env::temp_dir().join(format!("tonesend-music-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    let out_dir = a.out_dir.clone().unwrap_or_else(|| work.clone());
    std::fs::create_dir_all(&out_dir)?;

    let (music, name) = match &a.input {
        Some(p) => (
            load_any(p, &work)?,
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
        ),
        None => (synth_track(a.seconds), "synthetic track".to_string()),
    };
    let fs = SAMPLE_RATE as f64;
    let music_rms = rms(&music);
    let cfg = a.band.config();
    let (lo, hi) = (cfg.params.f_lo as f64, cfg.params.f_hi as f64);
    println!("music: {name}, {:.1} s, RMS {:.1} dBFS, its own energy in {:.1}–{:.1} kHz: {:.1} dB below its total",
        music.len() as f64 / fs, db(music_rms), lo / 1e3, hi / 1e3, -band_share_db(&music, lo, hi));

    let id = a.id.as_bytes();
    let frame = encode(id, &cfg, a.rate);
    let frame_rms = rms(&frame);
    println!(
        "data: {:?} ({} bytes) on {}/{} — {:.2} s per repetition, every {:.1} s\n",
        a.id,
        id.len(),
        a.band,
        a.rate,
        frame.len() as f64 / fs,
        a.every
    );

    // Header row.
    print!("{:<12}", "codec");
    for l in &a.level {
        print!(" {:>20}", format!("level {l:+} dB"));
    }
    println!();

    let mut results: Vec<Vec<String>> = vec![Vec::new(); a.codecs.len()];
    for &level in &a.level {
        // Mix the ID every `every` seconds.
        let gain = (music_rms / frame_rms) as f32 * 10f32.powf(level / 20.0);
        let mut mix = music.clone();
        let step = (a.every as f64 * fs) as usize;
        let mut pos = (0.5 * fs) as usize;
        let mut sent = 0;
        while pos + frame.len() <= mix.len() {
            for (m, f) in mix[pos..].iter_mut().zip(&frame) {
                *m += f * gain;
            }
            sent += 1;
            pos += step.max(frame.len() + 4800);
        }
        // Avoid clipping without changing the music/data ratio.
        let peak = mix.iter().fold(0f32, |m, v| m.max(v.abs()));
        if peak > 0.98 {
            mix.iter_mut().for_each(|v| *v *= 0.98 / peak);
        }
        let mixed = out_dir.join(format!(
            "mixed_{}{}.wav",
            if level < 0.0 { "m" } else { "p" },
            level.abs()
        ));
        write_wav(&mixed, &mix)?;

        for (ci, codec) in a.codecs.iter().enumerate() {
            let cell = match roundtrip(&mixed, codec, &work, &out_dir, level) {
                Ok(y) => {
                    let frames = decode(&y, &[cfg]);
                    let ok: Vec<_> = frames
                        .iter()
                        .filter(|f| f.ok() && f.payload == id)
                        .collect();
                    let snr = if ok.is_empty() {
                        f32::NAN
                    } else {
                        ok.iter().map(|f| f.snr_db).sum::<f32>() / ok.len() as f32
                    };
                    if ok.is_empty() {
                        format!("{}/{} —", ok.len(), sent)
                    } else {
                        format!("{}/{} SNR {:>4.1}", ok.len(), sent, snr)
                    }
                }
                Err(e) => format!("error: {e}").chars().take(20).collect(),
            };
            results[ci].push(cell);
        }
    }
    for (codec, row) in a.codecs.iter().zip(&results) {
        print!("{codec:<12}");
        for c in row {
            print!(" {c:>20}");
        }
        println!();
    }
    println!("\ncells: IDs decoded / IDs embedded, mean decoder SNR (dB)");
    if a.out_dir.is_some() {
        println!("mixed tracks and codec outputs: {}", out_dir.display());
    }
    if a.out_dir.is_none() {
        let _ = std::fs::remove_dir_all(&work);
    }
    Ok(())
}

/// Encode `wav` with a codec and decode back to 48 kHz mono samples.
fn roundtrip(wav: &Path, codec: &str, work: &Path, out_dir: &Path, level: f32) -> Result<Vec<f32>> {
    if codec == "wav" {
        return read_wav(wav);
    }
    let (name, kbps) = codec
        .split_once(':')
        .context("codec must look like mp3:128")?;
    let (enc, ext) = match name {
        "mp3" => ("libmp3lame", "mp3"),
        "aac" => ("aac", "m4a"),
        "aac_at" => ("aac_at", "m4a"),
        "opus" => ("libopus", "opus"),
        other => bail!("unknown codec {other}"),
    };
    let tag = format!(
        "{}_{}k_{}{}",
        name,
        kbps,
        if level < 0.0 { "m" } else { "p" },
        level.abs()
    );
    let coded = out_dir.join(format!("{tag}.{ext}"));
    // Stereo at 48 kHz like real music distribution (encoders behave differently in mono).
    ffmpeg(&[
        "-i",
        &wav.to_string_lossy(),
        "-ac",
        "2",
        "-ar",
        "48000",
        "-c:a",
        enc,
        "-b:a",
        &format!("{kbps}k"),
        &coded.to_string_lossy(),
    ])?;
    let back = work.join(format!("{tag}_decoded.wav"));
    ffmpeg(&[
        "-i",
        &coded.to_string_lossy(),
        "-ac",
        "1",
        "-ar",
        "48000",
        "-c:a",
        "pcm_f32le",
        &back.to_string_lossy(),
    ])?;
    read_wav(&back)
}

fn ffmpeg(args: &[&str]) -> Result<()> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .context("running ffmpeg (brew install ffmpeg)")?;
    if !out.status.success() {
        bail!(
            "ffmpeg: {}",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("")
        );
    }
    Ok(())
}

fn load_any(p: &Path, work: &Path) -> Result<Vec<f32>> {
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")) {
        return read_wav(p);
    }
    let w = work.join("input.wav");
    ffmpeg(&[
        "-i",
        &p.to_string_lossy(),
        "-ac",
        "1",
        "-ar",
        "48000",
        "-c:a",
        "pcm_f32le",
        &w.to_string_lossy(),
    ])?;
    read_wav(&w)
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

/// How far (dB) the music's energy in [lo, hi] is below its total energy.
fn band_share_db(x: &[f32], lo: f64, hi: f64) -> f64 {
    let bands = modem::probe::band_powers(x, 250.0);
    let lin = |d: f32| 10f64.powf(d as f64 / 10.0);
    let total: f64 = bands.iter().map(|b| lin(b.1)).sum();
    let inband: f64 = bands
        .iter()
        .filter(|b| (b.0 as f64) >= lo && (b.0 as f64) < hi)
        .map(|b| lin(b.1))
        .sum();
    10.0 * (total / inband.max(1e-30)).log10()
}

/// A simple pop-style test track: bass, chords, kick, snare, bright hi-hats.
pub fn synth_track(seconds: f32) -> Vec<f32> {
    let fs = SAMPLE_RATE as f32;
    let n = (seconds * fs) as usize;
    let mut x = vec![0f32; n];
    let mut rng = Rng::new(2026);
    let beat = 0.5; // 120 bpm
    let chords: [[f32; 3]; 4] = [
        [261.6, 329.6, 392.0],
        [220.0, 261.6, 329.6],
        [174.6, 220.0, 261.6],
        [196.0, 246.9, 293.7],
    ];
    let tau = std::f32::consts::TAU;
    let mut hp_prev = 0f32;
    let mut hp_y = 0f32;
    for (i, out) in x.iter_mut().enumerate() {
        let t = i as f32 / fs;
        let bar = (t / (4.0 * beat)) as usize % 4;
        let tb = t % beat; // time since beat
        let beat_idx = (t / beat) as usize;
        let t8 = t % (beat / 2.0);
        let mut v = 0.0;
        // Chords: bright additive saw (harmonics up to ~12 kHz), soft attack each bar.
        let tbar = t % (4.0 * beat);
        let env_c = (tbar * 8.0).min(1.0) * (-tbar * 0.6).exp();
        for &f in &chords[bar] {
            let mut h = 1.0;
            while f * h < 12_000.0 {
                v += 0.05 * env_c * (tau * f * h * t).sin() / h;
                h += 1.0;
            }
        }
        // Bass: root two octaves down.
        let fb = chords[bar][0] / 4.0;
        v += 0.25 * (-tb * 3.0).exp() * ((tau * fb * t).sin() + 0.3 * (tau * 2.0 * fb * t).sin());
        // Kick on every beat.
        let fk = 50.0 + 70.0 * (-tb * 30.0).exp();
        v += 0.5 * (-tb * 12.0).exp() * (tau * fk * tb).sin();
        // Snare on 2 and 4.
        let noise = rng.gauss() as f32;
        if beat_idx % 2 == 1 {
            v += 0.18 * (-tb * 25.0).exp() * noise;
        }
        // Hi-hat on 8th notes: high-passed noise (lots of energy up to 20+ kHz).
        hp_y = 0.6 * (hp_y + noise - hp_prev);
        hp_prev = noise;
        v += 0.12 * (-t8 * 60.0).exp() * hp_y;
        *out = v;
    }
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    x.iter_mut().for_each(|v| *v *= 0.89 / peak);
    x
}

#[derive(Args)]
pub struct EmbedArgs {
    /// Song to embed into (any format ffmpeg reads). Omit for the synthetic test track.
    #[arg(short, long)]
    input: Option<PathBuf>,
    /// Text the iPhone app will show.
    #[arg(short, long)]
    text: String,
    /// Output file: .wav, .mp3 or .m4a.
    #[arg(short, long)]
    output: PathBuf,
    #[arg(long, default_value = "high")]
    band: Band,
    #[arg(long, default_value = "safe")]
    rate: Rate,
    /// Data level relative to the music's RMS, dB.
    #[arg(long, allow_hyphen_values = true, default_value_t = -18.0)]
    level: f32,
    /// Seconds between repetitions.
    #[arg(long, default_value_t = 3.0)]
    every: f32,
    /// Bitrate for .mp3/.m4a output, kbit/s.
    #[arg(long, default_value_t = 192)]
    kbps: u32,
    /// Length of the synthetic track, seconds.
    #[arg(long, default_value_t = 60.0)]
    seconds: f32,
}

/// Write `text` (wrapped in the iPhone app's text envelope) into a song, repeated.
pub fn embed(a: EmbedArgs) -> Result<()> {
    let work = std::env::temp_dir().join(format!("tonesend-embed-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    let (mut left, mut right) = match &a.input {
        Some(p) => load_stereo(p, &work)?,
        None => {
            let m = synth_track(a.seconds);
            (m.clone(), m)
        }
    };
    let fs = SAMPLE_RATE as f64;
    let (payload, _) = modem::content::pack_text(&a.text);
    let frame = encode(&payload, &a.band.config(), a.rate);
    let music_rms = ((rms(&left).powi(2) + rms(&right).powi(2)) / 2.0).sqrt();
    let gain = (music_rms / rms(&frame)) as f32 * 10f32.powf(a.level / 20.0);
    let step = ((a.every as f64 * fs) as usize).max(frame.len() + 4800);
    let mut pos = (0.5 * fs) as usize;
    let mut n = 0;
    while pos + frame.len() <= left.len() {
        for (i, f) in frame.iter().enumerate() {
            left[pos + i] += f * gain;
            right[pos + i] += f * gain;
        }
        n += 1;
        pos += step;
    }
    let peak = left.iter().chain(&right).fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.98 {
        left.iter_mut()
            .chain(right.iter_mut())
            .for_each(|v| *v *= 0.98 / peak);
    }
    let wav = work.join("embedded.wav");
    write_stereo_wav(&wav, &left, &right)?;
    let ext = a
        .output
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "wav" => {
            std::fs::copy(&wav, &a.output)?;
        }
        "mp3" | "m4a" => {
            let enc = if ext == "mp3" { "libmp3lame" } else { "aac_at" };
            ffmpeg(&[
                "-i",
                &wav.to_string_lossy(),
                "-c:a",
                enc,
                "-b:a",
                &format!("{}k", a.kbps),
                &a.output.to_string_lossy(),
            ])?;
        }
        _ => bail!("output must be .wav, .mp3 or .m4a"),
    }
    let _ = std::fs::remove_dir_all(&work);
    println!(
        "{} — {:.0} s, \"{}\" embedded {n}× ({}/{} band {:.1}–{:.1} kHz, {:+.0} dB under the music, every {:.0} s)",
        a.output.display(),
        left.len() as f64 / fs,
        a.text,
        a.band,
        a.rate,
        a.band.params().f_lo / 1000.0,
        a.band.params().f_hi / 1000.0,
        a.level,
        step as f64 / fs
    );
    println!("Play it on the computer; on the iPhone open Receive and tap the ear.");
    Ok(())
}

fn load_stereo(p: &Path, work: &Path) -> Result<(Vec<f32>, Vec<f32>)> {
    let w = work.join("input_stereo.wav");
    ffmpeg(&[
        "-i",
        &p.to_string_lossy(),
        "-ac",
        "2",
        "-ar",
        "48000",
        "-c:a",
        "pcm_f32le",
        &w.to_string_lossy(),
    ])?;
    let mut r = hound::WavReader::open(&w)?;
    let s: Vec<f32> = r.samples::<f32>().collect::<Result<_, _>>()?;
    Ok((
        s.iter().step_by(2).copied().collect(),
        s.iter().skip(1).step_by(2).copied().collect(),
    ))
}

fn write_stereo_wav(path: &Path, l: &[f32], r: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for (a, b) in l.iter().zip(r) {
        w.write_sample((a.clamp(-1.0, 1.0) * 32767.0) as i16)?;
        w.write_sample((b.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}
