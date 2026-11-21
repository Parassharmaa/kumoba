#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use kumoba_view_codec::{Budget, Decoder, Encoder, Rect};
use std::io::Read;
use std::time::Instant;

fn load(path: &str) -> (usize, usize, Vec<Vec<u32>>) {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    let word = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
    let (w, h, n) = (word(0), word(4), word(8));
    let frames = (0..n)
        .map(|f| {
            let base = 12 + f * w * h * 4;
            bytes[base..base + w * h * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| u32::from_le_bytes([p[0], p[1], p[2], 0]))
                .collect()
        })
        .collect();
    (w, h, frames)
}

fn canvas(rgb: u32) -> u32 {
    0xff00_0000 | ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff)
}

fn damage(previous: &[u32], current: &[u32], w: usize, h: usize) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        let (a, b) = (&previous[y * w..(y + 1) * w], &current[y * w..(y + 1) * w]);
        if a == b {
            continue;
        }
        y0 = y0.min(y);
        y1 = y + 1;
        x0 = x0.min(a.iter().zip(b).position(|(p, q)| p != q).unwrap());
        x1 = x1.max(a.iter().zip(b).rposition(|(p, q)| p != q).unwrap() + 1);
    }
    if y1 == 0 {
        return Rect {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
        };
    }
    Rect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    }
}

fn psnr(decoded: &[u32], frame: &[u32]) -> f64 {
    let mut error = 0u64;
    for (a, b) in decoded.iter().zip(frame) {
        let b = canvas(*b);
        for shift in [0, 8, 16] {
            let d = i64::from((a >> shift) & 0xff) - i64::from((b >> shift) & 0xff);
            error += (d * d) as u64;
        }
    }
    if error == 0 {
        return 99.0;
    }
    10.0 * (255.0f64 * 255.0 * 3.0 * frame.len() as f64 / error as f64).log10()
}

fn main() {
    let mut paths = Vec::new();
    let mut mbps = 0.0f64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--mbps" {
            mbps = args
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or(0.0);
        } else {
            paths.push(arg);
        }
    }
    let steady = if mbps > 0.0 {
        (mbps * 1e6 / 8.0 / 60.0) as usize
    } else {
        usize::MAX
    };
    let burst = if mbps > 0.0 {
        (mbps * 1e6 / 8.0 / 4.0) as usize
    } else {
        usize::MAX
    };
    println!(
        "{:<8} {:>9} {:>8} {:>8} {:>7} {:>7} {:>7} {:>7} {:>6} {:>6} {:>6}",
        "seq",
        "bytes/f",
        "Mbps@60",
        "max f",
        "enc ms",
        "enc max",
        "dec ms",
        "dec max",
        "moves",
        "lossy",
        "psnr"
    );
    for path in paths {
        let (w, h, frames) = load(&path);
        let mut encoder = Encoder::new(w, h);
        let mut decoder = Decoder::new(w, h).unwrap();
        let mut out = Vec::new();
        let (mut bytes, mut largest, mut moves, mut sent) = (0usize, 0usize, 0usize, 0usize);
        let (mut enc, mut enc_max, mut dec, mut dec_max) = (0f64, 0f64, 0f64, 0f64);
        let (mut lossy, mut quality, mut worst) = (0usize, 0f64, 99f64);
        let mut key_bytes = 0;
        let mut bucket = burst;
        for (index, frame) in frames.iter().enumerate() {
            let area = if index == 0 {
                Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                }
            } else {
                damage(&frames[index - 1], frame, w, h)
            };
            bucket = bucket.saturating_add(steady).min(burst);
            let budget = Budget {
                now: bucket,
                steady,
            };
            let started = Instant::now();
            let stats = encoder.encode(frame, area, &[], budget, &mut out);
            let took = started.elapsed().as_secs_f64() * 1000.0;
            if !stats.changed {
                continue;
            }
            bucket = bucket.saturating_sub(out.len());
            let started = Instant::now();
            decoder.decode(&out).unwrap();
            let decode_took = started.elapsed().as_secs_f64() * 1000.0;
            assert!(
                decoder
                    .pixels()
                    .iter()
                    .zip(encoder.reference())
                    .all(|(a, b)| *a == canvas(*b)),
                "frame {index} of {path}: decoder and encoder disagree"
            );
            if stats.pending_blocks == 0 {
                assert!(
                    encoder.reference() == &frame[..],
                    "frame {index} of {path} is not exact"
                );
            }
            if index == 0 {
                key_bytes = out.len();
                continue;
            }
            sent += 1;
            bytes += out.len();
            largest = largest.max(out.len());
            moves += stats.moves;
            lossy += stats.lossy_blocks;
            let score = psnr(decoder.pixels(), frame);
            quality += score;
            worst = worst.min(score);
            enc += took;
            enc_max = enc_max.max(took);
            dec += decode_took;
            dec_max = dec_max.max(decode_took);
        }
        let last = frames.last().unwrap();
        let (mut settle_frames, mut settle_bytes) = (0, 0);
        while encoder.pending() {
            bucket = bucket.saturating_add(steady).min(burst);
            let stats = encoder.encode(
                last,
                Rect {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 0,
                },
                &[],
                Budget {
                    now: bucket,
                    steady,
                },
                &mut out,
            );
            if stats.changed {
                bucket = bucket.saturating_sub(out.len());
                decoder.decode(&out).unwrap();
                settle_bytes += out.len();
            }
            settle_frames += 1;
            assert!(settle_frames < 10_000, "{path} never settled");
        }
        assert!(
            decoder
                .pixels()
                .iter()
                .zip(last)
                .all(|(a, b)| *a == canvas(*b)),
            "{path} did not end exact"
        );
        let n = sent.max(1) as f64;
        let name = path.rsplit('/').next().unwrap().trim_end_matches(".raw");
        println!(
            "{:<8} {:>9.0} {:>8.2} {:>8} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>6.1} {:>6.0} {:>6.1}   key {} B, worst psnr {:.1}, exact after {} frames / {} B",
            name,
            bytes as f64 / n,
            bytes as f64 / n * 8.0 * 60.0 / 1e6,
            largest,
            enc / n,
            enc_max,
            dec / n,
            dec_max,
            moves as f64 / n,
            lossy as f64 / n,
            quality / n,
            key_bytes,
            worst,
            settle_frames,
            settle_bytes
        );
    }
}
