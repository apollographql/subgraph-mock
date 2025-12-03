//! Simple latency generation
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;
use tokio::time::{Duration, Instant};
use tracing::trace;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LatencyConfig {
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    pub base: Duration,
    pub saw: Option<Shape>,
    pub sine: Option<Shape>,
    pub square: Option<Shape>,
    pub triangle: Option<Shape>,
}

impl Default for LatencyConfig {
    fn default() -> Self {
        Self {
            base: Duration::from_millis(5),
            saw: None,
            sine: Some(Shape {
                amplitude: Duration::from_millis(2),
                period: Duration::from_secs(10),
            }),
            square: None,
            triangle: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Shape {
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    pub amplitude: Duration,
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    pub period: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct LatencyGenerator {
    start: Instant,
    cfg: LatencyConfig,
}

impl LatencyGenerator {
    pub fn new(cfg: LatencyConfig) -> Self {
        Self {
            start: Instant::now(),
            cfg,
        }
    }

    pub fn generate(&self, when: Instant) -> Duration {
        let mut latency_ms = self.cfg.base.as_millis() as u64;
        let elapsed_ms = when.duration_since(self.start).as_millis() as u64;

        trace!("Base latency: {latency_ms}");
        trace!("Elapsed: {elapsed_ms}");

        if let Some(saw) = self.cfg.saw {
            latency_ms += saw_ms(saw, elapsed_ms);
        }
        if let Some(sine) = self.cfg.sine {
            latency_ms += sine_ms(sine, elapsed_ms);
        }
        if let Some(square) = self.cfg.square {
            latency_ms += square_ms(square, elapsed_ms);
        }
        if let Some(triangle) = self.cfg.triangle {
            latency_ms += triangle_ms(triangle, elapsed_ms);
        }

        trace!("Final latency: {latency_ms}");
        Duration::from_millis(latency_ms)
    }
}

#[inline(always)]
fn saw_ms(Shape { amplitude, period }: Shape, elapsed: u64) -> u64 {
    let amplitude = amplitude.as_millis() as u64;
    let period = period.as_millis() as u64;

    (((elapsed + period / 2) % period) / period * amplitude * 2) - amplitude
}

#[inline(always)]
fn sine_ms(Shape { amplitude, period }: Shape, elapsed: u64) -> u64 {
    let amplitude = amplitude.as_millis() as u64;
    let period = period.as_millis() as u64;

    trace!(
        amplitude = amplitude,
        period = period,
        elapsed = elapsed,
        "Computing sine value",
    );

    let sine_value = ((elapsed as f64) / (period as f64) * PI * 2.0).sin(); // -1.0 to 1.0
    let normalized = (sine_value + 1.0) / 2.0; // 0.0 to 1.0
    let result = (normalized * amplitude as f64).round() as u64; // 0 to amplitude (in integer steps)

    trace!(
        sine_value = sine_value,
        normalized = normalized,
        result = result,
        "Sine value computed"
    );

    result
}

#[inline(always)]
fn square_ms(Shape { amplitude, period }: Shape, elapsed: u64) -> u64 {
    let amplitude = amplitude.as_millis() as u64;
    let period = period.as_millis() as u64;

    trace!(
        amplitude = amplitude,
        period = period,
        elapsed = elapsed,
        "Computing square value",
    );

    let result = if elapsed % period < period / 2 {
        amplitude
    } else {
        0
    };

    trace!(result = result, "Square value computed");

    result
}

#[inline(always)]
fn triangle_ms(Shape { amplitude, period }: Shape, elapsed: u64) -> u64 {
    let amplitude = amplitude.as_millis() as u64;
    let period = period.as_millis() as u64;

    // time.Duration(4*a/p*math.Abs(math.Mod(((math.Mod((x-p/4), p))+p), p)-p/2) - a)
    4 * amplitude / (((((elapsed - period / 4) % period) + period) % period) - period / 2)
        - amplitude
}
