# User guide: ToneSend for iPhone

## Installing on your iPhones

The app is installed from Xcode (it is not on the App Store).

**On the Mac (once):** Xcode, Rust and xcodegen. See [development.md → Setup](development.md#setup).

```sh
cd ios
xcodegen generate
open ToneSend.xcodeproj
```

1. Select the **ToneSend** target → **Signing & Capabilities** → choose your **Team**.
   The bundle identifier is `cz.ittailors.soundtransfer`.
2. Connect the iPhone with a cable, **unlock it**, select it as the run destination, press **Run** (⌘R).
3. Repeat for the second iPhone.

**Important: the phone needs internet the first time it launches a newly installed app.**
iOS verifies the developer profile with Apple once. Without internet the launch fails with
*"The application could not be launched because the Developer App Certificate is not trusted"*
or *"invalid code signature… profile has not been explicitly trusted"*, even though nothing is
wrong. Connect the phone to Wi‑Fi or cellular once, launch the app, then it works offline.
See [Troubleshooting](#troubleshooting).

## The three tabs

| Tab | Purpose |
|---|---|
| **Send** | Type text or pick a photo or file, choose a sound profile, send it |
| **Receive** | Listen, watch incoming transfers, open received items |
| **Lab** | Measurement tools: play the probe signal, record raw audio, choose bands to listen for |

## Your first transfer

1. **Phone A (receiver):** open **Receive** and tap the big **ear** button. Allow microphone
   access when asked. The rings react to sound and the level meter moves.
2. **Phone B (sender):** open **Send**, keep the default message, choose **Max** and **Fast**,
   and tap **Send as sound**.
3. Hold the phones **10–30 cm apart**, with phone B's **bottom edge (speaker)** pointing at
   phone A's **bottom edge (microphone)**. Turn phone B's **volume up**.
4. Phone A shows **Incoming** with a progress bar, then the message appears in the list
   with a green ✓ (**Checksum OK**).

## Sending

### Content
- **Text:** anything you type.
- **Photo:** picked from the library, downscaled and encoded as **WebP** by the framework. The
  **Resolution** slider trades quality for transfer time; pixel size and WebP size are shown live.
  The preview shows exactly what will be sent.
- **File:** any file from the Files app. Large files take long (see the estimate).

### Sound profile (band)

| Band | Frequencies | Choose when |
|---|---|---|
| **Max** | 1–13 kHz | Default. Fastest; phones close together |
| **Audible** | 1–8 kHz | Max loses blocks (weak high frequencies) |
| **Robust** | 1.5–5 kHz, long symbols | Echoey room, longer distance, noise |
| **Silent** | 17.5–20.5 kHz | You need it (nearly) inaudible; slower, shorter range |

### Speed

| Speed | Needs | Notes |
|---|---|---|
| **Safe** | weak signal OK | Slowest, most robust |
| **Normal** | moderate | 2× Safe |
| **Fast** | moderate | 1.5× Normal. **Recommended** |
| **Turbo** | strong, clean signal | 1.5× Fast. Phones side by side, quiet room |

### Repair data

**Off / 10 % / 25 % / 50 %** extra blocks that let the receiver rebuild lost blocks without
anything being resent. **25 %** is the default. Use 50 % in difficult conditions, Off for the
fastest possible transfer when the link is clean.

The **On air / Air time / Speed** tiles show the estimate for the current choices. Text and
files are compressed automatically when that helps; the line below the tiles shows the result
(e.g. *Compressed 6.1 KB → 2.6 KB (−57 %, Brotli)*). Speed counts the content delivered, so
compression makes it higher than the link rate.

While sending, a progress bar shows elapsed and total time; **Stop** cancels.
**Recently sent** lists your transfers; tap one for details and repair.

## Receiving

- Tap the **ear** to start or stop listening. Listening continues while the app is open
  (keep the screen on and the app in the foreground).
- **Incoming** shows the profile, **blocks received / needed**, how many were **lost**, and
  the progress of the current packet.
- Each received item shows its size, effective speed and air time, and a status icon:
  - green ✓: complete, whole-file checksum verified;
  - orange ⚠ **Incomplete — N of M blocks missing**: too much was lost; see [Repair](#repairing-an-incomplete-transfer).
- Tap an item for the details screen.

### Details screen

| Item | Meaning |
|---|---|
| Content | Text (with Copy), image preview, or file with **Share / Save** |
| Effective speed | Content bits per second of air time, including all overhead and repair data; compression can push it above the link rate |
| Link rate | Raw capability of the chosen profile for large files |
| Air time | Seconds of sound that carried this transfer |
| Content / On air / Compression | Original size, bytes actually sent, and the saving (Brotli or WebP) |
| Checksum (CRC-32) | Whole-file checksum; green when verified |
| Repaired blocks | Blocks rebuilt from repair data instead of being received |
| Blocks received | Good / total blocks heard (lost blocks are normal; repair covers them) |
| Packets | Number of short packets the file was split into |
| Signal (SNR) | Signal quality. Green ≥ 12 dB, yellow ≥ 8 dB, orange below |
| Clock drift | Difference between the two phones' audio clocks; handled automatically |
| Signal per frequency | Quality at each frequency of the last packet. The dashed line is what the chosen speed needs. If the curve drops below it at high frequencies, use **Audible** instead of **Max**, or a slower speed |

## Repairing an incomplete transfer

1. Keep the receiver **listening** (don't clear the item).
2. On the **sender**, open the transfer in **Recently sent** and tap
   **Send repair data (+30 %)**.
3. The receiver merges the new blocks; the item turns green when complete.
   Tap again if it still isn't complete. Each tap sends *new* repair blocks.

## Tips for best speed

- Distance **10–30 cm**, speaker (bottom edge) towards microphone (bottom edge).
- Volume high but not distorted. If Turbo fails at full volume, try a bit lower.
- Quiet room; soft surfaces reduce echo. Don't cover the speaker or microphone.
- Start with **Max + Fast + 25 %**. If all blocks arrive (no repaired blocks), try **Turbo**.
  If many blocks are lost, use **Audible** or a slower speed.

## Reading data hidden in music

The app can pick up a message embedded in a song or ad played by another device.

1. On the Mac, create a song with your text inside (High band, 14.6–16.4 kHz, 18 dB under the
   music, repeated every 3 s):
   ```sh
   tonesend embed -i song.mp3 -t "20 % off with code SOUND20" -o song_with_id.mp3
   tonesend embed -t "Hello from the song" -o music/song_with_id.mp3   # synthetic test track
   ```
2. Play `song_with_id.mp3` in any player (Music, QuickTime, a browser) at normal listening
   volume. Make sure the Mac is not muted.
3. On the iPhone: **Receive** → tap the ear, hold the phone near the speaker. The text appears
   as one item; each further repetition increases its **heard N×** counter (the detail screen
   shows the last time). The same message heard again after a pause of more than 60 s counts as a
   new reception.

Options: `--level -15` (stronger, more robust) … `-25` (quieter, less audible), `--band inaudible`
(17.5–20.5 kHz, only for uncompressed/high-bitrate audio), `--kbps` for the MP3/M4A bitrate.
Laptop and small speakers are weak at 15–16 kHz; if nothing arrives, use `--level -12` or move
the phone closer.

## Sharing a test log

The app keeps a diagnostics log on each phone. It records sends, receptions, signal quality of
every packet (including packets that failed), the audio route and errors. **Message contents are
not recorded**, only sizes and checksums. Logs are kept for 14 days.

1. Before a test, open **Lab → Diagnostics log**, type a short note and tap **Add**, e.g.
   *"30 cm, kitchen, phones bottom to bottom, Max + Fast"*. One note per setup makes the log
   easy to read later.
2. Run your transfers as usual.
3. Tap **Share log** on **both** phones and AirDrop the files to the Mac
   (`tonesend-log-<model>-<date>.jsonl`). Send them to the developer together with what you
   observed ("photo stopped at 80 %", "Turbo never worked").
4. **Clear log** removes everything on that phone, e.g. before a new test series.

## Lab tab

- **Play probe signal:** a ~54 s measurement signal (sweeps, test tones, one test packet per
  profile).
- **Record raw audio:** records the microphone unprocessed (48 kHz WAV). Use it on the
  other phone while the probe plays, then share the WAV to a Mac (AirDrop) and run
  `tonesend probe analyze -i recording.wav` for a full report (frequency response, echo, noise,
  real speed of every profile). See [development.md → Measuring](development.md#measuring-real-devices).
- **Diagnostics log:** add notes, share or clear the log. See [Sharing a test log](#sharing-a-test-log).
- **Listen for:** which bands the receiver searches for (all by default).
- **Link rate per profile:** reference table.

## Troubleshooting

| Problem | Fix |
|---|---|
| *"Developer App Certificate is not trusted"* / *"invalid code signature"* on launch | Give the phone **internet once** and launch again. If it persists: delete the app, restart the phone, **Product → Clean Build Folder**, Run again. With a free Apple ID: Settings → General → VPN & Device Management → trust the developer |
| Nothing received | Is the receiver listening (blue orb, level meter moving)? Microphone permission on? Sender volume up? Try Audible + Safe |
| Microphone access is off | Settings → ToneSend → Microphone |
| Many lost blocks / incomplete | Move closer, reduce echo and noise, slower speed or **Audible**/**Robust**, more repair data, then **Send repair data** |
| Silent band never works | Some speakers/mics can't do 17–20 kHz; use an audible band |
| The receiver picks up its own sound | It doesn't: receiving pauses automatically while the same phone is sending |
| App stops working after 7 days | Free Apple ID builds expire; press Run in Xcode again |
