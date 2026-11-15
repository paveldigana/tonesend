//! `tonesend` — ToneSend modem command line.

mod music_cmd;
mod probe_cmd;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use modem::sim::{resample, Channel};
use modem::util::Rng;
use modem::{decode, encode, net_bitrate, Band, BandConfig, Frame, Rate, SAMPLE_RATE};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Parser)]
#[command(
    name = "tonesend",
    version,
    about = "ToneSend: data over sound (OFDM modem)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List band/rate profiles and their throughput.
    Info,
    /// Encode a file or text into a WAV.
    Encode {
        #[arg(short, long, conflicts_with = "text")]
        input: Option<PathBuf>,
        #[arg(short, long)]
        text: Option<String>,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long, default_value = "wide")]
        band: Band,
        #[arg(long, default_value = "normal")]
        rate: Rate,
    },
    /// Decode all frames found in a WAV recording.
    Decode {
        #[arg(short, long)]
        input: PathBuf,
        /// Write payload here (frame N > 0 gets a ".N" suffix).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Only look for this band (default: all).
        #[arg(long)]
        band: Option<Band>,
    },
    /// Pass a WAV through the simulated acoustic channel.
    Channel {
        #[arg(short, long)]
        input: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[command(flatten)]
        ch: ChannelArgs,
    },
    /// One encode -> channel -> decode run with detailed stats.
    Sim {
        #[arg(long, default_value = "wide")]
        band: Band,
        #[arg(long, default_value = "normal")]
        rate: Rate,
        #[arg(long, default_value_t = 4096)]
        bytes: usize,
        #[command(flatten)]
        ch: ChannelArgs,
        /// Save the received (simulated) signal as WAV.
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// Goodput of every profile across an SNR sweep.
    Bench {
        #[arg(long, default_value_t = 2048)]
        bytes: usize,
        #[arg(long, default_value_t = 3)]
        trials: u64,
        #[arg(long, default_value = "desk")]
        preset: String,
        #[arg(long, value_delimiter = ',', default_value = "30,20,15,10,5,0")]
        snr: Vec<f32>,
        #[arg(long)]
        band: Option<Band>,
    },
    /// List audio input/output devices.
    Devices,
    /// Play a WAV on a speaker.
    Play {
        #[arg(short, long)]
        input: PathBuf,
        /// Output device name (substring), default = system default.
        #[arg(long)]
        device: Option<String>,
    },
    /// Record from a microphone to WAV (48 kHz mono). Stops after --seconds.
    Record {
        #[arg(short, long)]
        output: PathBuf,
        #[arg(short, long, default_value_t = 10.0)]
        seconds: f32,
        /// Input device name (substring), default = system default.
        #[arg(long)]
        device: Option<String>,
    },
    /// Real-time receive: from the microphone, or replay a WAV through the streaming receiver.
    Listen {
        /// Stream this WAV instead of the microphone.
        #[arg(short, long)]
        input: Option<PathBuf>,
        #[arg(long)]
        device: Option<String>,
        /// Stop after this many seconds (microphone mode).
        #[arg(short, long, default_value_t = 60.0)]
        seconds: f32,
    },
    /// Embed a short data ID in music and test it through MP3/AAC/Opus encoders.
    Music(music_cmd::MusicArgs),
    /// Embed text into a song (repeated) for the iPhone app to pick up.
    Embed(music_cmd::EmbedArgs),
    /// Power spectrum of a WAV section in 1 kHz bands (diagnostics).
    Spectrum {
        #[arg(short, long)]
        input: PathBuf,
        /// Section start / end in seconds.
        #[arg(long, default_value_t = 0.0)]
        from: f64,
        #[arg(long)]
        to: Option<f64>,
        #[arg(long, default_value_t = 1000.0)]
        width: f64,
    },
    /// Measure a real acoustic channel (phase 2): generate / analyze / run.
    Probe {
        #[command(subcommand)]
        cmd: probe_cmd::ProbeCmd,
    },
}

#[derive(Args, Clone)]
struct ChannelArgs {
    /// clean | desk | room | noisy | far
    #[arg(long, default_value = "desk")]
    preset: String,
    /// Full-band SNR in dB (overrides preset).
    #[arg(long)]
    snr: Option<f32>,
    #[arg(long)]
    rt60: Option<f32>,
    #[arg(long)]
    drr: Option<f32>,
    #[arg(long, allow_hyphen_values = true)]
    ppm: Option<f64>,
    #[arg(long)]
    seed: Option<u64>,
}

impl ChannelArgs {
    fn build(&self) -> Result<Channel> {
        let mut c = preset(&self.preset)?;
        if self.snr.is_some() {
            c.snr_db = self.snr;
        }
        if let Some(v) = self.rt60 {
            c.rt60 = v;
        }
        if let Some(v) = self.drr {
            c.drr_db = v;
        }
        if let Some(v) = self.ppm {
            c.ppm = v;
        }
        if let Some(v) = self.seed {
            c.seed = v;
        }
        Ok(c)
    }
}

fn preset(name: &str) -> Result<Channel> {
    Channel::preset(name)
        .with_context(|| format!("unknown preset '{name}' ({})", Channel::PRESETS.join(" | ")))
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Info => info(),
        Cmd::Encode {
            input,
            text,
            output,
            band,
            rate,
        } => {
            // Same envelope + compression as the apps (framework content layer).
            let (data, info) = match (input, text) {
                (Some(p), _) => {
                    let bytes =
                        std::fs::read(&p).with_context(|| format!("reading {}", p.display()))?;
                    let name = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    modem::content::pack_file(&name, &bytes)
                }
                (None, Some(t)) => modem::content::pack_text(&t),
                (None, None) => bail!("give --input FILE or --text TEXT"),
            };
            println!(
                "content {} B -> {} B on air{}",
                info.original_len,
                info.packed_len,
                if info.compressed { " (Brotli)" } else { "" }
            );
            let sig = encode(&data, &band.config(), rate);
            write_wav(&output, &sig)?;
            println!(
                "{} bytes -> {} ({:.2} s, {band}/{rate}, {:.0} bit/s net)",
                data.len(),
                output.display(),
                sig.len() as f64 / SAMPLE_RATE as f64,
                data.len() as f64 * 8.0 * SAMPLE_RATE as f64 / sig.len() as f64
            );
            Ok(())
        }
        Cmd::Decode {
            input,
            output,
            band,
        } => {
            let x = read_wav(&input)?;
            let cfgs: Vec<BandConfig> = match band {
                Some(b) => vec![b.config()],
                None => Band::ALL.iter().map(|b| b.config()).collect(),
            };
            let t = Instant::now();
            let frames = decode(&x, &cfgs);
            if frames.is_empty() {
                bail!("no frames found");
            }
            for (i, f) in frames.iter().enumerate() {
                print_frame(i, f);
                let c = modem::content::unpack(&f.payload);
                if c.compressed {
                    println!(
                        "    unpacked {} B -> {} B (Brotli)",
                        c.wire_len,
                        c.data.len()
                    );
                }
                if let Some(o) = &output {
                    let path = if i == 0 {
                        o.clone()
                    } else {
                        PathBuf::from(format!("{}.{i}", o.display()))
                    };
                    std::fs::write(&path, &c.data)?;
                    println!("    -> {}", path.display());
                } else if f.ok() {
                    match c.kind {
                        modem::content::Kind::Text => {
                            let s = String::from_utf8_lossy(&c.data);
                            if s.len() <= 2000 {
                                println!("    text: {s}");
                            }
                        }
                        k => println!(
                            "    {k:?} \"{}\" ({} B) — use -o to save",
                            c.name,
                            c.data.len()
                        ),
                    }
                }
            }
            eprintln!(
                "decoded {:.1} s of audio in {:.0} ms",
                x.len() as f64 / SAMPLE_RATE as f64,
                t.elapsed().as_secs_f64() * 1e3
            );
            Ok(())
        }
        Cmd::Channel { input, output, ch } => {
            let y = ch.build()?.apply(&read_wav(&input)?, SAMPLE_RATE);
            write_wav(&output, &normalize(y))
        }
        Cmd::Sim {
            band,
            rate,
            bytes,
            ch,
            save,
        } => sim(band, rate, bytes, &ch.build()?, save.as_deref()),
        Cmd::Bench {
            bytes,
            trials,
            preset: p,
            snr,
            band,
        } => bench(bytes, trials, &p, &snr, band),
        Cmd::Devices => {
            for d in audio::list_devices()? {
                println!("{d}");
            }
            Ok(())
        }
        Cmd::Play { input, device } => {
            let x = read_wav(&input)?;
            let p = audio::play(&x, device.as_deref())?;
            println!(
                "playing {:.1} s on '{}' ({} Hz)…",
                x.len() as f64 / SAMPLE_RATE as f64,
                p.device_name,
                p.device_rate
            );
            p.wait();
            Ok(())
        }
        Cmd::Record {
            output,
            seconds,
            device,
        } => {
            let r = audio::record(device.as_deref())?;
            println!(
                "recording {seconds:.1} s from '{}' ({} Hz)…",
                r.device_name, r.device_rate
            );
            std::thread::sleep(std::time::Duration::from_secs_f32(seconds));
            let x = r.finish();
            probe_cmd::warn_if_silent(&x);
            write_wav(&output, &x)?;
            println!("-> {}", output.display());
            Ok(())
        }
        Cmd::Probe { cmd } => probe_cmd::run(cmd),
        Cmd::Music(args) => music_cmd::run(args),
        Cmd::Embed(args) => music_cmd::embed(args),
        Cmd::Listen {
            input,
            device,
            seconds,
        } => {
            let mut rx = modem::Receiver::new(&Band::ALL);
            let mut n = 0;
            let mut report = |frames: Vec<Frame>| {
                for f in frames {
                    print_frame(n, &f);
                    n += 1;
                }
            };
            match input {
                Some(path) => {
                    let x = read_wav(&path)?;
                    let t = Instant::now();
                    for chunk in x.chunks(1024) {
                        report(rx.push(chunk));
                    }
                    report(rx.push(&vec![0.0; SAMPLE_RATE as usize]));
                    eprintln!(
                        "streamed {:.1} s in {:.0} ms",
                        x.len() as f64 / SAMPLE_RATE as f64,
                        t.elapsed().as_secs_f64() * 1e3
                    );
                }
                None => {
                    let rec = audio::record(device.as_deref())?;
                    println!("listening on '{}' for {seconds:.0} s…", rec.device_name);
                    let mut consumed = 0;
                    let t = Instant::now();
                    while t.elapsed().as_secs_f32() < seconds {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        let new = rec.take_new(&mut consumed);
                        report(rx.push(&new));
                    }
                }
            }
            Ok(())
        }
        Cmd::Spectrum {
            input,
            from,
            to,
            width,
        } => {
            let x = read_wav(&input)?;
            let fs = SAMPLE_RATE as f64;
            let a = ((from * fs) as usize).min(x.len());
            let b = to
                .map_or(x.len(), |t| ((t * fs) as usize).min(x.len()))
                .max(a);
            for (f, p) in modem::probe::band_powers(&x[a..b], width) {
                println!(
                    "{:>6.0} Hz {:>7.1} dB  {}",
                    f,
                    p,
                    "#".repeat(((p + 110.0).max(0.0) / 2.0) as usize)
                );
            }
            Ok(())
        }
    }
}

fn info() -> Result<()> {
    println!(
        "{:<10} {:<7} {:<6} {:>11} {:>5} {:>8} {:>9} {:>11} {:>13}",
        "band", "rate", "mod", "band Hz", "sc", "sym/s", "FEC", "raw bit/s", "net bit/s*"
    );
    for b in Band::ALL {
        let p = b.params();
        for r in Rate::ALL {
            let raw = p.nsc() as f64 * r.bits_per_carrier() as f64 * p.symbol_rate();
            println!(
                "{:<10} {:<7} {:<6} {:>5.0}-{:<5.0} {:>5} {:>8.2} {:>9} {:>11.0} {:>13.0}",
                b.name(),
                r.name(),
                r.modulation(),
                p.k_lo() as f32 * p.bin_hz(),
                p.k_hi() as f32 * p.bin_hz(),
                p.nsc(),
                p.symbol_rate(),
                r.code_rate().name(),
                raw,
                net_bitrate(&p, r, 4096)
            );
        }
    }
    println!(
        "\n* net = 4 KiB payload incl. preamble, header, CRC and FEC overhead, error-free channel"
    );
    Ok(())
}

fn print_frame(i: usize, f: &Frame) {
    let ok = f.blocks_ok.iter().filter(|&&b| b).count();
    println!(
        "frame {i}: {}/{} @ {:.3}s  {} bytes  blocks {ok}/{}  SNR {:.1} dB  drift {:+.0} ppm  corr {:.2}  {:.2}s  goodput {:.0} bit/s{}",
        f.band,
        f.rate,
        f.start as f64 / f.sample_rate as f64,
        f.payload.len(),
        f.blocks_ok.len(),
        f.snr_db,
        f.drift_ppm,
        f.detect_score,
        f.duration_s(),
        f.goodput_bps(),
        if f.ok() { "" } else { "  [DAMAGED]" }
    );
}

fn sim(band: Band, rate: Rate, bytes: usize, ch: &Channel, save: Option<&Path>) -> Result<()> {
    let cfg = band.config();
    let mut data = vec![0u8; bytes];
    Rng::new(ch.seed).fill_bytes(&mut data);
    let t = Instant::now();
    let tx = encode(&data, &cfg, rate);
    let t_enc = t.elapsed();
    let rx = ch.apply(&tx, SAMPLE_RATE);
    if let Some(p) = save {
        write_wav(p, &normalize(rx.clone()))?;
    }
    let t = Instant::now();
    let frames = decode(&rx, &[cfg]);
    let t_dec = t.elapsed();
    println!("channel: {ch:?}");
    println!(
        "encode {:.1} ms, decode {:.1} ms for {:.2} s of audio",
        t_enc.as_secs_f64() * 1e3,
        t_dec.as_secs_f64() * 1e3,
        rx.len() as f64 / SAMPLE_RATE as f64
    );
    match frames.first() {
        None => println!("RESULT: no frame decoded"),
        Some(f) => {
            print_frame(0, f);
            let byte_err = f.payload.iter().zip(&data).filter(|(a, b)| a != b).count();
            println!(
                "RESULT: {} — {byte_err} wrong bytes of {bytes}",
                if f.payload == data { "OK" } else { "FAIL" }
            );
            let snr = &f.carrier_snr_db;
            let q = snr.len() / 4;
            println!(
                "carrier SNR dB (low→high, quartile means): {:.1} {:.1} {:.1} {:.1}",
                mean(&snr[..q]),
                mean(&snr[q..2 * q]),
                mean(&snr[2 * q..3 * q]),
                mean(&snr[3 * q..])
            );
        }
    }
    Ok(())
}

fn bench(
    bytes: usize,
    trials: u64,
    preset_name: &str,
    snrs: &[f32],
    only: Option<Band>,
) -> Result<()> {
    let base = preset(preset_name)?;
    println!(
        "preset '{preset_name}' (rt60 {}s, DRR {} dB, {} ppm), {bytes} B payload, {trials} trial(s)/cell",
        base.rt60, base.drr_db, base.ppm
    );
    println!("cells: mean goodput kbit/s ('*' = some blocks lost, '-' = nothing decoded)\n");
    print!("{:<18} {:>8}", "profile", "ideal");
    for s in snrs {
        print!(" {:>8}", format!("{s} dB"));
    }
    println!();
    let t = Instant::now();
    for band in Band::ALL
        .into_iter()
        .filter(|b| only.is_none_or(|o| o == *b))
    {
        let cfg = band.config();
        for rate in Rate::ALL {
            print!(
                "{:<18} {:>8.2}",
                format!("{band}/{rate}"),
                net_bitrate(&cfg.params, rate, bytes) / 1e3
            );
            for &snr in snrs {
                let (mut gp, mut lost, mut none) = (0.0, false, 0);
                for trial in 0..trials {
                    let mut data = vec![0u8; bytes];
                    Rng::new(trial * 7919 + 1).fill_bytes(&mut data);
                    let ch = Channel {
                        snr_db: Some(snr),
                        seed: trial + 1,
                        ..base.clone()
                    };
                    let rx = ch.apply(&encode(&data, &cfg, rate), SAMPLE_RATE);
                    match decode(&rx, &[cfg]).first() {
                        Some(f) => {
                            gp += f.goodput_bps();
                            lost |= !f.ok();
                        }
                        None => none += 1,
                    }
                }
                let cell = if none == trials {
                    "-".to_string()
                } else {
                    format!(
                        "{:.2}{}",
                        gp / trials as f64 / 1e3,
                        if lost || none > 0 { "*" } else { "" }
                    )
                };
                print!(" {cell:>8}");
            }
            println!();
        }
    }
    eprintln!("\n({:.1} s)", t.elapsed().as_secs_f64());
    Ok(())
}

fn mean(x: &[f32]) -> f32 {
    if x.is_empty() {
        0.0
    } else {
        x.iter().sum::<f32>() / x.len() as f32
    }
}

fn normalize(mut y: Vec<f32>) -> Vec<f32> {
    let peak = y.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.99 {
        let g = 0.99 / peak;
        y.iter_mut().for_each(|v| *v *= g);
    }
    y
}

pub(crate) fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut r =
        hound::WavReader::open(path).with_context(|| format!("opening {}", path.display()))?;
    let spec = r.spec();
    let all: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<_, _>>()?
        }
    };
    let mono: Vec<f32> = all
        .iter()
        .step_by(spec.channels.max(1) as usize)
        .copied()
        .collect();
    Ok(if spec.sample_rate == SAMPLE_RATE {
        mono
    } else {
        resample(&mono, spec.sample_rate as f64 / SAMPLE_RATE as f64)
    })
}

pub(crate) fn write_wav(path: &Path, x: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)
        .with_context(|| format!("creating {}", path.display()))?;
    for &v in x {
        w.write_sample((v.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    Ok(())
}
