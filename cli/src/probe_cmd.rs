//! `tonesend probe …` — real-world channel measurement (phase 2).

use crate::{read_wav, write_wav};
use anyhow::{anyhow, Result};
use clap::Subcommand;
use modem::probe::{self, Report};
use modem::{Band, SAMPLE_RATE};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Subcommand)]
pub enum ProbeCmd {
    /// Write the probe signal to a WAV (play it on the sending device).
    Generate {
        #[arg(short, long, default_value = "probe.wav")]
        output: PathBuf,
        /// Payload bytes per test frame (9 frames: every band × rate).
        #[arg(long, default_value_t = 1024)]
        bytes: usize,
    },
    /// Analyse a recording of the probe.
    Analyze {
        #[arg(short, long)]
        input: PathBuf,
        /// Also write the full report as JSON.
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// Play the probe and record it on this machine at the same time, then analyse.
    Run {
        #[arg(long, default_value_t = 1024)]
        bytes: usize,
        /// Keep the recording.
        #[arg(long)]
        save: Option<PathBuf>,
        #[arg(long)]
        json: Option<PathBuf>,
        #[arg(long)]
        input_device: Option<String>,
        #[arg(long)]
        output_device: Option<String>,
    },
}

pub fn run(cmd: ProbeCmd) -> Result<()> {
    match cmd {
        ProbeCmd::Generate { output, bytes } => {
            let x = probe::generate(bytes);
            write_wav(&output, &x)?;
            println!(
                "probe -> {} ({:.1} s). Play it on the sender, record on the receiver \
                 (WAV or lossless — no AAC/MP3), then: tonesend probe analyze -i <recording>",
                output.display(),
                x.len() as f64 / SAMPLE_RATE as f64
            );
            Ok(())
        }
        ProbeCmd::Analyze { input, json } => analyze(&read_wav(&input)?, json.as_deref()),
        ProbeCmd::Run {
            bytes,
            save,
            json,
            input_device,
            output_device,
        } => {
            let x = probe::generate(bytes);
            let rec = audio::record(input_device.as_deref())?;
            std::thread::sleep(Duration::from_millis(700));
            let player = audio::play(&x, output_device.as_deref())?;
            println!(
                "playing {:.1} s probe on '{}' ({} Hz), recording on '{}' ({} Hz)…",
                x.len() as f64 / SAMPLE_RATE as f64,
                player.device_name,
                player.device_rate,
                rec.device_name,
                rec.device_rate
            );
            player.wait();
            std::thread::sleep(Duration::from_millis(1500));
            drop(player);
            let y = rec.finish();
            warn_if_silent(&y);
            if let Some(p) = &save {
                write_wav(p, &y)?;
                println!("recording -> {}", p.display());
            }
            analyze(&y, json.as_deref())
        }
    }
}

pub fn warn_if_silent(x: &[f32]) {
    let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak < 1e-4 {
        eprintln!(
            "WARNING: recording is silent. On macOS allow microphone access for your terminal: \
             System Settings → Privacy & Security → Microphone."
        );
    }
}

fn analyze(x: &[f32], json_out: Option<&Path>) -> Result<()> {
    let r = probe::analyze(x).map_err(|e| anyhow!(e))?;
    print_report(&r);
    if let Some(p) = json_out {
        std::fs::write(p, serde_json::to_string_pretty(&to_json(&r))?)?;
        println!("\nreport -> {}", p.display());
    }
    Ok(())
}

fn print_report(r: &Report) {
    println!("\n== Recording");
    println!("peak {:.1} dBFS, rms {:.1} dBFS", r.peak_dbfs, r.rms_dbfs);
    if r.peak_dbfs > -0.5 {
        println!("  ! clipping — lower the playback volume");
    } else if r.peak_dbfs < -40.0 {
        println!("  ! very quiet — raise volume or move the devices closer");
    }
    println!("clock drift {:+.1} ppm (receiver vs sender)", r.drift_ppm);
    println!(
        "room: RT60 {}, direct-to-reverberant {:.1} dB",
        r.rt60_s.map_or("n/a".to_string(), |t| format!("{t:.2} s")),
        r.drr_db
    );

    println!("\n== Frequency response (speaker + room + mic) and SNR");
    println!(
        "{:>8} {:>9} {:>7} {:>7}  SNR",
        "Hz", "response", "noise", "SNR"
    );
    let nmax = r.freq.iter().map(|f| f.noise_db).fold(f32::MIN, f32::max);
    for f in &r.freq {
        let bar = if f.snr_db.is_finite() {
            "#".repeat((f.snr_db.max(0.0) / 2.0).round() as usize)
        } else {
            String::new()
        };
        println!(
            "{:>8.0} {:>7.1}dB {:>5.1}dB {:>5.1}dB  {bar}",
            f.f_center,
            f.response_db,
            f.noise_db - nmax,
            f.snr_db
        );
    }

    println!("\n== Profiles: prediction from the measurement");
    println!(
        "{:<10} {:>10} {:>10} {:>10}  {:<8} {:>12}",
        "band", "noise SNR", "echo SIR", "effective", "→ rate", "predicted"
    );
    for p in &r.profiles {
        println!(
            "{:<10} {:>8.1}dB {:>8.1}dB {:>8.1}dB  {:<8} {:>8.2} kb/s",
            p.band.name(),
            p.noise_snr_db,
            p.reverb_sir_db,
            p.effective_snr_db,
            p.best_rate.map_or("none", |r| r.name()),
            p.predicted_bps / 1e3
        );
    }

    println!("\n== Test frames: actual result");
    println!(
        "{:<18} {:<8} {:>7} {:>8} {:>12}",
        "profile", "result", "blocks", "SNR", "goodput"
    );
    for f in &r.frames {
        let res = match (f.found, f.ok) {
            (false, _) => "missing",
            (true, true) => "OK",
            (true, false) => "damaged",
        };
        println!(
            "{:<18} {:<8} {:>7} {:>8} {:>8.2} kb/s",
            format!("{}/{}", f.band, f.rate),
            res,
            format!("{}/{}", f.blocks_ok, f.blocks_total),
            f.snr_db.map_or("-".to_string(), |s| format!("{s:.1}dB")),
            f.goodput_bps / 1e3
        );
    }

    println!("\n== Verdict");
    let aud = r.best_frame(&[Band::AudibleWide, Band::AudibleNarrow]);
    let ina = r.best_frame(&[Band::Inaudible]);
    match aud {
        Some(a) => println!(
            "fastest audible:   {}/{} at {:.2} kbit/s",
            a.band,
            a.rate,
            a.goodput_bps / 1e3
        ),
        None => println!("fastest audible:   none decoded"),
    }
    match ina {
        Some(i) => println!(
            "fastest inaudible: {}/{} at {:.2} kbit/s",
            i.band,
            i.rate,
            i.goodput_bps / 1e3
        ),
        None => {
            println!("fastest inaudible: none decoded — inaudible not usable on this device pair")
        }
    }
    if let (Some(a), Some(i)) = (aud, ina) {
        println!(
            "→ inaudible is {:.1}× slower than audible here",
            a.goodput_bps / i.goodput_bps
        );
    }
    println!(
        "simulator equivalent: tonesend bench --preset clean … with --rt60 {} --drr {:.0} --ppm {:.0}",
        r.rt60_s.map_or("0".into(), |t| format!("{t:.2}")),
        r.drr_db,
        r.drift_ppm
    );
}

fn to_json(r: &Report) -> serde_json::Value {
    json!({
        "peak_dbfs": r.peak_dbfs,
        "rms_dbfs": r.rms_dbfs,
        "drift_ppm": r.drift_ppm,
        "rt60_s": r.rt60_s,
        "drr_db": r.drr_db,
        "freq": r.freq.iter().map(|f| json!({
            "hz": f.f_center, "response_db": f.response_db, "noise_db": f.noise_db,
            "snr_db": if f.snr_db.is_finite() { json!(f.snr_db) } else { json!(null) },
        })).collect::<Vec<_>>(),
        "profiles": r.profiles.iter().map(|p| json!({
            "band": p.band.name(), "noise_snr_db": p.noise_snr_db, "reverb_sir_db": p.reverb_sir_db,
            "effective_snr_db": p.effective_snr_db, "best_rate": p.best_rate.map(|r| r.name()),
            "predicted_bps": p.predicted_bps,
        })).collect::<Vec<_>>(),
        "frames": r.frames.iter().map(|f| json!({
            "band": f.band.name(), "rate": f.rate.name(), "found": f.found, "ok": f.ok,
            "blocks_ok": f.blocks_ok, "blocks_total": f.blocks_total, "snr_db": f.snr_db,
            "goodput_bps": f.goodput_bps,
        })).collect::<Vec<_>>(),
    })
}
