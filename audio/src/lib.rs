//! Audio playback and capture via cpal (CoreAudio / AAudio / WASAPI / ALSA / WebAudio).
//!
//! Everything is exposed as mono f32 at `modem::SAMPLE_RATE`; device rates and
//! channel counts are converted here.

use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use modem::sim::resample;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const RATE: u32 = modem::SAMPLE_RATE;

/// Human-readable list of input and output devices with their default formats.
pub fn list_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let def_in = host.default_input_device().and_then(|d| d.name().ok());
    let def_out = host.default_output_device().and_then(|d| d.name().ok());
    let mut out = Vec::new();
    for d in host.input_devices()? {
        let name = d.name().unwrap_or_default();
        let cfg = d.default_input_config().map(|c| {
            format!(
                "{} Hz, {} ch, {:?}",
                c.sample_rate().0,
                c.channels(),
                c.sample_format()
            )
        });
        let mark = if Some(&name) == def_in.as_ref() {
            " (default)"
        } else {
            ""
        };
        out.push(format!(
            "in : {name}{mark} — {}",
            cfg.unwrap_or_else(|e| e.to_string())
        ));
    }
    for d in host.output_devices()? {
        let name = d.name().unwrap_or_default();
        let cfg = d.default_output_config().map(|c| {
            format!(
                "{} Hz, {} ch, {:?}",
                c.sample_rate().0,
                c.channels(),
                c.sample_format()
            )
        });
        let mark = if Some(&name) == def_out.as_ref() {
            " (default)"
        } else {
            ""
        };
        out.push(format!(
            "out: {name}{mark} — {}",
            cfg.unwrap_or_else(|e| e.to_string())
        ));
    }
    Ok(out)
}

fn find_device(input: bool, name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    match name {
        None => if input {
            host.default_input_device()
        } else {
            host.default_output_device()
        }
        .context(if input {
            "no default input device"
        } else {
            "no default output device"
        }),
        Some(want) => {
            let want = want.to_lowercase();
            let mut devs = if input {
                host.input_devices()?
            } else {
                host.output_devices()?
            };
            devs.find(|d| {
                d.name()
                    .map(|n| n.to_lowercase().contains(&want))
                    .unwrap_or(false)
            })
            .with_context(|| {
                format!(
                    "no {} device matching '{want}' (see `st devices`)",
                    if input { "input" } else { "output" }
                )
            })
        }
    }
}

// ------------------------------------------------------------------ playback

pub struct Player {
    _stream: cpal::Stream,
    pos: Arc<AtomicUsize>,
    len: usize,
    pub device_name: String,
    pub device_rate: u32,
}

impl Player {
    /// Block until all samples have been handed to the device (+ a short drain).
    pub fn wait(&self) {
        while self.pos.load(Ordering::Relaxed) < self.len {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(300));
    }

    pub fn progress(&self) -> f32 {
        (self.pos.load(Ordering::Relaxed) as f32 / self.len.max(1) as f32).min(1.0)
    }
}

/// Start playing mono `samples` (at `RATE`) on all channels of the output device.
pub fn play(samples: &[f32], device: Option<&str>) -> Result<Player> {
    let dev = find_device(false, device)?;
    let sc = dev.default_output_config()?;
    let sr = sc.sample_rate().0;
    let ch = sc.channels() as usize;
    let data = Arc::new(if sr == RATE {
        samples.to_vec()
    } else {
        resample(samples, RATE as f64 / sr as f64)
    });
    let len = data.len();
    let pos = Arc::new(AtomicUsize::new(0));
    let cfg: cpal::StreamConfig = sc.config();
    let stream = match sc.sample_format() {
        SampleFormat::F32 => out_stream::<f32>(&dev, &cfg, ch, data, pos.clone())?,
        SampleFormat::I16 => out_stream::<i16>(&dev, &cfg, ch, data, pos.clone())?,
        SampleFormat::I32 => out_stream::<i32>(&dev, &cfg, ch, data, pos.clone())?,
        SampleFormat::U16 => out_stream::<u16>(&dev, &cfg, ch, data, pos.clone())?,
        f => bail!("unsupported output sample format {f:?}"),
    };
    stream.play()?;
    Ok(Player {
        _stream: stream,
        pos,
        len,
        device_name: dev.name().unwrap_or_default(),
        device_rate: sr,
    })
}

fn out_stream<T: SizedSample + FromSample<f32>>(
    dev: &cpal::Device,
    cfg: &cpal::StreamConfig,
    ch: usize,
    data: Arc<Vec<f32>>,
    pos: Arc<AtomicUsize>,
) -> Result<cpal::Stream> {
    Ok(dev.build_output_stream(
        cfg,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut p = pos.load(Ordering::Relaxed);
            for frame in out.chunks_mut(ch) {
                let v = T::from_sample(data.get(p).copied().unwrap_or(0.0));
                frame.iter_mut().for_each(|s| *s = v);
                p += 1;
            }
            pos.store(p, Ordering::Relaxed);
        },
        |e| eprintln!("audio output error: {e}"),
        None,
    )?)
}

// ------------------------------------------------------------------ capture

pub struct Recorder {
    stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    pub device_name: String,
    pub device_rate: u32,
}

impl Recorder {
    /// Seconds captured so far (device clock).
    pub fn seconds(&self) -> f32 {
        self.buf.lock().map(|b| b.len()).unwrap_or(0) as f32 / self.device_rate as f32
    }

    /// Samples captured since `*consumed` (device rate → `RATE`, block-wise resampled).
    pub fn take_new(&self, consumed: &mut usize) -> Vec<f32> {
        let b = self.buf.lock().unwrap();
        let new = b[*consumed..].to_vec();
        *consumed = b.len();
        if self.device_rate == RATE {
            new
        } else {
            resample(&new, self.device_rate as f64 / RATE as f64)
        }
    }

    /// Stop and return mono samples at `RATE`.
    pub fn finish(self) -> Vec<f32> {
        drop(self.stream);
        let b = std::mem::take(&mut *self.buf.lock().unwrap());
        if self.device_rate == RATE {
            b
        } else {
            resample(&b, self.device_rate as f64 / RATE as f64)
        }
    }
}

/// Start capturing the first channel of the input device.
///
/// Note: OS-level voice processing (AGC / noise suppression) must be off for the
/// modem to work. On macOS a plain CoreAudio input like this one is unprocessed.
pub fn record(device: Option<&str>) -> Result<Recorder> {
    let dev = find_device(true, device)?;
    let sc = dev.default_input_config()?;
    let sr = sc.sample_rate().0;
    let ch = sc.channels() as usize;
    let buf = Arc::new(Mutex::new(Vec::with_capacity(sr as usize * 60)));
    let cfg: cpal::StreamConfig = sc.config();
    let stream = match sc.sample_format() {
        SampleFormat::F32 => in_stream::<f32>(&dev, &cfg, ch, buf.clone())?,
        SampleFormat::I16 => in_stream::<i16>(&dev, &cfg, ch, buf.clone())?,
        SampleFormat::I32 => in_stream::<i32>(&dev, &cfg, ch, buf.clone())?,
        SampleFormat::U16 => in_stream::<u16>(&dev, &cfg, ch, buf.clone())?,
        f => bail!("unsupported input sample format {f:?}"),
    };
    stream.play()?;
    Ok(Recorder {
        stream,
        buf,
        device_name: dev.name().unwrap_or_default(),
        device_rate: sr,
    })
}

fn in_stream<T: SizedSample>(
    dev: &cpal::Device,
    cfg: &cpal::StreamConfig,
    ch: usize,
    buf: Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream>
where
    f32: FromSample<T>,
{
    Ok(dev.build_input_stream(
        cfg,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            if let Ok(mut b) = buf.lock() {
                b.extend(data.chunks(ch).map(|f| f[0].to_sample::<f32>()));
            }
        },
        |e| eprintln!("audio input error: {e}"),
        None,
    )?)
}
