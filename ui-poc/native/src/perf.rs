//! Frame-time / fps / input-latency bookkeeping.

use std::collections::VecDeque;
use std::time::Duration;
use web_time::Instant;

#[derive(Default)]
pub struct Perf {
    /// (frame drawn instant, layout->drawn duration)
    pub frames: VecDeque<(Instant, Duration)>,
    /// all frame times since `mark` (for bench)
    pub all_frame_ms: Vec<f64>,
    pub all_intervals_ms: Vec<f64>,
    last_frame: Option<Instant>,
    /// pending probe: (char, key event time, echo seen time)
    pub probe: Option<(u8, Instant, Option<Instant>)>,
    pub key_to_echo_ms: Vec<f64>,
    pub key_to_paint_ms: Vec<f64>,
    pub echo_queue_ms: Vec<f64>,
    pub rows_rebuilt: u64,
    /// per frame: view render() (element tree build) ms, layout+paint (scene build) ms, GPU submit+present ms
    pub build_ms: Vec<f64>,
    pub scene_ms: Vec<f64>,
    pub gpu_ms: Vec<f64>,
    pub last_build_ms: f64,
    pub events: u64,
    pub output_bytes: u64,
    /// UI-thread time spent inside the terminal core: parsing output, rebuilding snapshot rows.
    pub feed_us: u64,
    pub snap_us: u64,
}

pub fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let i = ((s.len() as f64 - 1.) * p).round() as usize;
    s[i]
}

impl Perf {
    pub fn split(&mut self, scene: Duration, gpu: Duration) {
        self.scene_ms.push(scene.as_secs_f64() * 1000.);
        self.gpu_ms.push(gpu.as_secs_f64() * 1000.);
        self.build_ms.push(self.last_build_ms);
        self.last_build_ms = 0.;
        let cap = 20_000;
        for v in [&mut self.scene_ms, &mut self.gpu_ms, &mut self.build_ms] {
            if v.len() > cap {
                v.drain(..cap / 2);
            }
        }
    }

    pub fn frame(&mut self, now: Instant, dur: Duration) {
        self.frames.push_back((now, dur));
        while self.frames.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(2)) {
            self.frames.pop_front();
        }
        self.all_frame_ms.push(dur.as_secs_f64() * 1000.);
        if let Some(l) = self.last_frame {
            self.all_intervals_ms.push(now.duration_since(l).as_secs_f64() * 1000.);
        }
        self.last_frame = Some(now);
    }

    /// fps over the last second, frame-time p50/p99 over the last 2s.
    pub fn summary(&self) -> (f64, f64, f64) {
        let now = Instant::now();
        let fps = self.frames.iter().filter(|(t, _)| now.duration_since(*t) <= Duration::from_secs(1)).count() as f64;
        let ms: Vec<f64> = self.frames.iter().map(|(_, d)| d.as_secs_f64() * 1000.).collect();
        (fps, pct(&ms, 0.5), pct(&ms, 0.99))
    }

    pub fn reset_bench(&mut self) {
        self.all_frame_ms.clear();
        self.build_ms.clear();
        self.scene_ms.clear();
        self.gpu_ms.clear();
        self.all_intervals_ms.clear();
        self.key_to_echo_ms.clear();
        self.key_to_paint_ms.clear();
        self.echo_queue_ms.clear();
        self.rows_rebuilt = 0;
        self.events = 0;
        self.output_bytes = 0;
        self.feed_us = 0;
        self.snap_us = 0;
        self.last_frame = None;
    }
}
