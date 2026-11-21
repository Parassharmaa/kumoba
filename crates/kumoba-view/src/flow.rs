use kumoba_view_codec::Budget;
use std::collections::VecDeque;
use std::time::Duration;

const MIN_RATE: f64 = 400_000.0;
const START_RATE: f64 = 10_000_000.0;
const BURST: f64 = 0.25;
const HISTORY: Duration = Duration::from_secs(10);
const RECENT: usize = 3;
const CUT: f64 = 0.8;
const RAISE: f64 = 1.08;
const RAISE_EVERY: Duration = Duration::from_millis(250);
const CALM: Duration = Duration::from_millis(500);
const MAX_WINDOW: usize = 180;

pub struct Flow {
    cap: f64,
    rate: f64,
    tokens: f64,
    updated: Duration,
    interval: Duration,
    trips: VecDeque<(Duration, Duration)>,
    recent: VecDeque<Duration>,
    smoothed: Duration,
    last_cut: Duration,
    last_raise: Duration,
    limited: bool,
}

impl Flow {
    pub fn new(max_mbps: u32, fps: u32, now: Duration) -> Self {
        let cap = (f64::from(max_mbps.max(1)) * 1_000_000.0).max(MIN_RATE);
        let rate = cap.min(START_RATE);
        Self {
            cap,
            rate,
            tokens: rate / 8.0 * BURST,
            updated: now,
            interval: Duration::from_nanos(1_000_000_000 / u64::from(fps.max(1))),
            trips: VecDeque::new(),
            recent: VecDeque::new(),
            smoothed: Duration::from_millis(50),
            last_cut: now,
            last_raise: now,
            limited: false,
        }
    }

    fn refill(&mut self, now: Duration) {
        let elapsed = now.saturating_sub(self.updated).as_secs_f64();
        self.updated = now;
        let limit = self.rate / 8.0 * BURST;
        self.tokens = (self.tokens + self.rate / 8.0 * elapsed).min(limit);
    }

    #[cfg(test)]
    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn window(&self) -> usize {
        let frames = self.smoothed.as_secs_f64() / self.interval.as_secs_f64();
        (frames.ceil() as usize + 2).clamp(2, MAX_WINDOW)
    }

    pub fn budget(&mut self, now: Duration) -> Option<Budget> {
        self.refill(now);
        if self.tokens < 0.0 {
            self.limited = true;
            return None;
        }
        Some(Budget {
            now: self.tokens as usize,
            steady: (self.rate / 8.0 * self.interval.as_secs_f64()) as usize,
        })
    }

    pub fn wait(&self) -> Duration {
        if self.tokens >= 0.0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(-self.tokens / (self.rate / 8.0))
    }

    pub fn sent(&mut self, bytes: usize, limited: bool) {
        self.tokens -= bytes as f64;
        self.limited |= limited;
    }

    fn fastest(&self) -> Option<Duration> {
        self.trips.iter().map(|&(_, trip)| trip).min()
    }

    pub fn acked(&mut self, trip: Duration, now: Duration) {
        self.smoothed = (self.smoothed * 7 + trip) / 8;
        while self
            .trips
            .front()
            .is_some_and(|&(at, _)| now.saturating_sub(at) > HISTORY)
        {
            self.trips.pop_front();
        }
        self.trips.push_back((now, trip));
        self.recent.push_back(trip);
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        let Some(fastest) = self.fastest() else {
            return;
        };
        let allowance = (fastest / 2).max(Duration::from_millis(25));
        let standing = self.recent.len() == RECENT
            && self
                .recent
                .iter()
                .all(|&sample| sample > fastest + allowance);
        let pause = (fastest * 2).max(Duration::from_millis(200));
        if standing {
            if now.saturating_sub(self.last_cut) >= pause {
                self.rate = (self.rate * CUT).max(MIN_RATE);
                self.last_cut = now;
                self.refill(now);
            }
            return;
        }
        if self.limited
            && now.saturating_sub(self.last_cut) >= CALM
            && now.saturating_sub(self.last_raise) >= RAISE_EVERY
        {
            self.rate = (self.rate * RAISE).min(self.cap);
            self.last_raise = now;
            self.limited = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_large_frame_is_paid_back_before_the_next_one() {
        let mut flow = Flow::new(8, 60, Duration::ZERO);
        let budget = flow.budget(Duration::ZERO).unwrap();
        assert_eq!(budget.now, 250_000);
        flow.sent(1_250_000, false);
        assert!(flow.budget(MS * 10).is_none());
        assert!(flow.wait() > MS * 900);
        assert!(flow.budget(MS * 1100).is_some());
    }

    #[test]
    fn a_standing_delay_lowers_the_rate() {
        let mut flow = Flow::new(30, 60, Duration::ZERO);
        let start = flow.rate();
        for step in 0..20 {
            flow.acked(MS * 20, MS * 16 * step);
        }
        assert!((flow.rate() - start).abs() < 1.0);
        for step in 20..60 {
            flow.acked(MS * 120, MS * 16 * step);
        }
        assert!(flow.rate() < start);
    }

    #[test]
    fn one_late_frame_does_not_lower_the_rate() {
        let mut flow = Flow::new(30, 60, Duration::ZERO);
        let start = flow.rate();
        for step in 0..60 {
            let trip = if step % 5 == 0 { MS * 90 } else { MS * 20 };
            flow.acked(trip, MS * 16 * step);
        }
        assert!((flow.rate() - start).abs() < 1.0);
    }

    #[test]
    fn the_rate_rises_only_while_the_encoder_wants_more() {
        let mut flow = Flow::new(30, 60, Duration::ZERO);
        let start = flow.rate();
        for step in 0..120 {
            flow.acked(MS * 20, MS * 16 * step);
        }
        assert!((flow.rate() - start).abs() < 1.0);
        for step in 120..600 {
            flow.sent(1000, true);
            flow.acked(MS * 20, MS * 16 * step);
        }
        assert!(flow.rate() > start);
        assert!(flow.rate() <= 30_000_000.0);
    }

    #[test]
    fn a_longer_round_trip_allows_more_frames_in_flight() {
        let mut near = Flow::new(30, 60, Duration::ZERO);
        let mut far = Flow::new(30, 60, Duration::ZERO);
        for step in 0..100 {
            near.acked(MS * 5, MS * 16 * step);
            far.acked(MS * 160, MS * 16 * step);
        }
        assert!(near.window() <= 3);
        assert!(far.window() >= 10);
    }
}
