//! Acoustic OFDM data modem — pure DSP, no audio I/O.
//!
//! ```
//! use modem::{Band, Rate};
//! let cfg = Band::AudibleWide.config();
//! let audio = modem::encode(b"hello", &cfg, Rate::Normal);
//! let frames = modem::decode(&audio, &[cfg]);
//! assert_eq!(frames[0].payload, b"hello");
//! ```

pub mod content;
pub mod fec;
pub mod modem;
pub mod ofdm;
pub mod probe;
pub mod profiles;
pub mod rs;
pub mod sim;
pub mod stream;
pub mod sync;
pub mod transfer;
pub mod util;

pub use modem::{decode, encode, frame_samples, layout, net_bitrate, Frame, Hints, BLOCK_BYTES, MAX_DRIFT_PPM};
pub use profiles::{Band, BandConfig, OfdmParams, Rate, SAMPLE_RATE};
pub use stream::{Activity, Diag, Receiver, Status};
