use crate::{Budget, Decoder, Encoder, Rect};

struct Random(u64);

impl Random {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
    fn below(&mut self, limit: usize) -> usize {
        self.next() as usize % limit
    }
}

const FULL: Rect = Rect {
    x: 0,
    y: 0,
    width: usize::MAX,
    height: usize::MAX,
};
const NOTHING: Rect = Rect {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
};
const TIGHT: Budget = Budget {
    now: 2000,
    steady: 2000,
};

fn canvas(rgb: u32) -> u32 {
    0xff00_0000 | ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff)
}

fn shows(decoder: &Decoder, frame: &[u32]) -> bool {
    decoder
        .pixels()
        .iter()
        .zip(frame)
        .all(|(shown, source)| *shown == canvas(*source))
}

fn page(width: usize, height: usize, random: &mut Random) -> Vec<u32> {
    let mut frame = vec![0x00ff_ffff; width * height];
    let colours = [
        0x0011_1111,
        0x0025_63eb,
        0x00d9_3025,
        0x0088_8888,
        0x00f1_f5f9,
    ];
    for _ in 0..width * height / 40 {
        let (x, y) = (random.below(width), random.below(height));
        let (w, h) = (1 + random.below(9), 1 + random.below(3));
        let colour = colours[random.below(colours.len())];
        for row in y..(y + h).min(height) {
            for column in x..(x + w).min(width) {
                frame[row * width + column] = colour;
            }
        }
    }
    frame
}

fn noise(frame: &mut [u32], width: usize, area: Rect, random: &mut Random) {
    for y in area.y..area.y + area.height {
        for x in area.x..area.x + area.width {
            frame[y * width + x] = random.next() & 0x00ff_ffff;
        }
    }
}

fn scrolled(frame: &[u32], width: usize, rows: usize, random: &mut Random) -> Vec<u32> {
    let height = frame.len() / width;
    let mut next = frame[rows * width..].to_vec();
    next.extend(page(width, rows, random));
    assert_eq!(next.len(), width * height);
    next
}

#[test]
fn every_frame_of_a_changing_page_is_shown_exactly() {
    for (width, height) in [(320, 240), (100, 75), (17, 33), (16, 16)] {
        let mut random = Random(width as u64);
        let mut encoder = Encoder::new(width, height);
        let mut decoder = Decoder::new(width, height).unwrap();
        let mut out = Vec::new();
        let mut frame = page(width, height, &mut random);
        for step in 0..12 {
            encoder.encode(&frame, FULL, &[], Budget::UNLIMITED, &mut out);
            decoder.decode(&out).unwrap();
            assert!(shows(&decoder, &frame), "{width}x{height} step {step}");
            match step % 3 {
                0 if height > 16 => {
                    frame = scrolled(&frame, width, 1 + random.below(7), &mut random);
                }
                1 => {
                    let at = random.below(frame.len());
                    frame[at] ^= 0x0001_0101;
                }
                _ => frame = page(width, height, &mut random),
            }
        }
    }
}

#[test]
fn an_unchanged_screen_sends_nothing() {
    let mut random = Random(3);
    let frame = page(200, 120, &mut random);
    let mut encoder = Encoder::new(200, 120);
    let mut out = Vec::new();
    assert!(
        encoder
            .encode(&frame, FULL, &[], Budget::UNLIMITED, &mut out)
            .changed
    );
    let stats = encoder.encode(&frame, FULL, &[], Budget::UNLIMITED, &mut out);
    assert!(!stats.changed);
    assert!(out.is_empty());
}

#[test]
fn a_scrolled_page_costs_far_less_than_a_new_page() {
    let mut random = Random(11);
    let (width, height) = (640, 480);
    let first = page(width, height, &mut random);
    let second = scrolled(&first, width, 6, &mut random);
    let mut encoder = Encoder::new(width, height);
    let mut decoder = Decoder::new(width, height).unwrap();
    let mut out = Vec::new();
    encoder.encode(&first, FULL, &[], Budget::UNLIMITED, &mut out);
    let whole = out.len();
    decoder.decode(&out).unwrap();
    let stats = encoder.encode(&second, FULL, &[], Budget::UNLIMITED, &mut out);
    decoder.decode(&out).unwrap();
    assert!(stats.moves > 0);
    assert!(out.len() * 10 < whole, "{} bytes after {whole}", out.len());
    assert!(shows(&decoder, &second));
}

#[test]
fn busy_regions_go_lossy_under_a_budget_and_end_exact() {
    for (width, height) in [(320, 240), (100, 75)] {
        let mut random = Random(5);
        let mut encoder = Encoder::new(width, height);
        let mut decoder = Decoder::new(width, height).unwrap();
        let mut out = Vec::new();
        let mut frame = page(width, height, &mut random);
        let busy = Rect {
            x: 8,
            y: 8,
            width: width - 30,
            height: height - 30,
        };
        let mut lossy = 0;
        for _ in 0..8 {
            noise(&mut frame, width, busy, &mut random);
            let stats = encoder.encode(&frame, FULL, &[], TIGHT, &mut out);
            lossy += stats.lossy_blocks;
            decoder.decode(&out).unwrap();
            assert!(shows(&decoder, encoder.reference()));
        }
        assert!(lossy > 0);
        assert!(!shows(&decoder, &frame));
        let mut rounds = 0;
        while encoder.pending() {
            let stats = encoder.encode(&frame, NOTHING, &[], TIGHT, &mut out);
            if stats.changed {
                decoder.decode(&out).unwrap();
            }
            rounds += 1;
            assert!(rounds < 10_000, "the picture never became exact");
        }
        assert!(shows(&decoder, &frame));
    }
}

fn texture(x: f64, y: f64) -> u32 {
    let wave = |a: f64, b: f64, phase: f64| {
        (128.0 + 90.0 * ((x * a + y * b + phase).sin() + (x * b * 0.7 - y * a * 1.3).cos()) / 2.0)
            as u32
    };
    wave(0.21, 0.13, 0.0) << 16 | wave(0.17, 0.29, 1.0) << 8 | wave(0.33, 0.11, 2.0)
}

fn drifting(width: usize, height: usize, step: f64, random: &mut Random) -> Vec<Vec<u32>> {
    (0..10)
        .map(|frame| {
            let mut pixels = vec![0x00ff_ffff; width * height];
            for y in 4..height - 4 {
                for x in 4..width - 4 {
                    let grain = (random.next() & 0x0003_0303) ^ 0x0001_0101;
                    let shifted = texture(
                        x as f64 - step * f64::from(frame),
                        y as f64 - step * f64::from(frame) / 2.0,
                    );
                    pixels[y * width + x] = shifted ^ grain;
                }
            }
            pixels
        })
        .collect()
}

#[test]
fn drifting_busy_content_is_predicted_from_the_last_frame_and_ends_exact() {
    for (width, height, step) in [
        (320, 240, 2.0),
        (320, 240, 0.5),
        (100, 75, 3.0),
        (33, 49, 1.5),
    ] {
        let mut random = Random(17);
        let frames = drifting(width, height, step, &mut random);
        let mut encoder = Encoder::new(width, height);
        let mut decoder = Decoder::new(width, height).unwrap();
        let mut out = Vec::new();
        let (mut lossy, mut moved) = (0, 0);
        for frame in &frames {
            let stats = encoder.encode(frame, FULL, &[], TIGHT, &mut out);
            lossy += stats.lossy_blocks;
            moved += stats.moved_blocks;
            decoder.decode(&out).unwrap();
            assert!(
                shows(&decoder, encoder.reference()),
                "{width}x{height} step {step}"
            );
        }
        assert!(lossy > 0, "{width}x{height} step {step}");
        assert!(moved > 0, "{width}x{height} step {step}");
        let last = frames.last().unwrap();
        let mut rounds = 0;
        while encoder.pending() {
            if encoder.encode(last, NOTHING, &[], TIGHT, &mut out).changed {
                decoder.decode(&out).unwrap();
            }
            rounds += 1;
            assert!(rounds < 10_000);
        }
        assert!(shows(&decoder, last));
    }
}

#[test]
fn a_change_next_to_a_lossy_region_keeps_both_correct() {
    let (width, height) = (160, 96);
    let mut random = Random(9);
    let mut encoder = Encoder::new(width, height);
    let mut decoder = Decoder::new(width, height).unwrap();
    let mut out = Vec::new();
    let mut frame = page(width, height, &mut random);
    noise(
        &mut frame,
        width,
        Rect {
            x: 0,
            y: 0,
            width: 64,
            height: 64,
        },
        &mut random,
    );
    encoder.encode(&frame, FULL, &[], TIGHT, &mut out);
    decoder.decode(&out).unwrap();
    for step in 0..6 {
        frame[70 * width + 20 + step] = 0x00ab_cdef;
        frame[10 * width + 70 + step] = 0x0012_3456;
        let area = Rect {
            x: 20,
            y: 10,
            width: 60,
            height: 61,
        };
        let stats = encoder.encode(&frame, area, &[], TIGHT, &mut out);
        if stats.changed {
            decoder.decode(&out).unwrap();
        }
        assert!(shows(&decoder, encoder.reference()));
    }
    while encoder.pending() {
        if encoder
            .encode(&frame, NOTHING, &[], Budget::UNLIMITED, &mut out)
            .changed
        {
            decoder.decode(&out).unwrap();
        }
    }
    assert!(shows(&decoder, &frame));
}

#[test]
fn damaged_frames_are_refused_without_a_crash() {
    let (width, height) = (160, 96);
    let mut random = Random(21);
    let mut encoder = Encoder::new(width, height);
    let mut out = Vec::new();
    let mut frame = page(width, height, &mut random);
    let mut frames = Vec::new();
    for step in 0..6 {
        let budget = if step % 2 == 0 {
            Budget::UNLIMITED
        } else {
            TIGHT
        };
        encoder.encode(&frame, FULL, &[], budget, &mut out);
        frames.push(out.clone());
        frame = scrolled(&frame, width, 3, &mut random);
        noise(
            &mut frame,
            width,
            Rect {
                x: 16,
                y: 16,
                width: 64,
                height: 48,
            },
            &mut random,
        );
    }
    for round in 0..4000 {
        let mut decoder = Decoder::new(width, height).unwrap();
        for (index, good) in frames.iter().enumerate() {
            let mut bytes = good.clone();
            if index == round % frames.len() {
                match round % 3 {
                    0 => bytes.truncate(random.below(bytes.len())),
                    1 => {
                        for _ in 0..1 + random.below(4) {
                            let at = random.below(bytes.len());
                            bytes[at] = random.next() as u8;
                        }
                    }
                    _ => {
                        bytes = (0..random.below(300))
                            .map(|_| random.next() as u8)
                            .collect();
                    }
                }
            }
            if decoder.decode(&bytes).is_err() {
                break;
            }
        }
    }
}
