use std::collections::VecDeque;

/// Longest selectable chart window; older samples are discarded.
pub const MAX_WINDOW_SECS: f64 = 600.0;

/// Time series of (seconds since start, value).
#[derive(Default, Clone)]
pub struct Series {
    pts: VecDeque<(f64, f64)>,
}

impl Series {
    pub fn push(&mut self, t: f64, v: f64) {
        self.pts.push_back((t, v));
        while self
            .pts
            .front()
            .is_some_and(|&(t0, _)| t - t0 > MAX_WINDOW_SECS)
        {
            self.pts.pop_front();
        }
    }

    pub fn last(&self) -> Option<f64> {
        self.pts.back().map(|&(_, v)| v)
    }

    /// Points inside the window, with x rebased so that `now` is 0.
    pub fn window(&self, now: f64, window: f64) -> Vec<(f64, f64)> {
        self.pts
            .iter()
            .filter(|&&(t, _)| now - t <= window)
            .map(|&(t, v)| (t - now, v))
            .collect()
    }

    pub fn max_in(&self, now: f64, window: f64) -> f64 {
        self.pts
            .iter()
            .filter(|&&(t, _)| now - t <= window)
            .map(|&(_, v)| v)
            .fold(0.0, f64::max)
    }
}
