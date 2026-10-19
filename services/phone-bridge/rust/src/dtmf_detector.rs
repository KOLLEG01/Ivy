//! Call-local PCM tone detection. The call owner serializes access and delivers
//! returned detections after processing the entire input block.
use std::{collections::VecDeque, f64::consts::PI};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detection {
    pub digit: char,
    pub sample_rate: i32,
    pub confirmed_duration_ms: i32,
    pub rms: f64,
    pub tone_energy_ratio: f64,
    pub low_group_amplitude: f64,
    pub high_group_amplitude: f64,
}

/// Aggregate signal quality only; no samples or individual tones are exposed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Diagnostics {
    pub analyzed_windows: i64,
    pub audible_windows: i64,
    pub paired_tone_windows: i64,
    pub candidate_tone_windows: i64,
    pub maximum_paired_tone_energy_ratio: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetectorError {
    InvalidHop,
}

#[derive(Default)]
pub struct InBandDetector {
    samples: VecDeque<i16>,
    sample_rate: i32,
    window_samples: usize,
    hop_samples: usize,
    weights: Vec<f64>,
    weight_sum: f64,
    candidate: Option<char>,
    candidate_windows: i32,
    emitted_digit: Option<char>,
    release_windows: i32,
    diagnostics: Diagnostics,
}

impl InBandDetector {
    pub fn diagnostics(&self) -> Diagnostics {
        self.diagnostics
    }

    pub fn process(
        &mut self,
        samples: &[i16],
        sample_rate: i32,
    ) -> Result<Vec<Detection>, DetectorError> {
        if sample_rate < 4_000 || samples.is_empty() {
            return Ok(Vec::new());
        }
        if self.sample_rate != sample_rate {
            self.reset_for_rate(sample_rate);
        }
        self.samples.extend(samples);
        let mut detections = Vec::new();
        while self.samples.len() >= self.window_samples {
            let analysis = self.analyze_window();
            if let Some(detection) = self.advance(analysis) {
                detections.push(detection);
            }
            // Preserve the native range error for overflowing, non-audio rate
            // inputs; never panic or return a partial batch of detections.
            if self.hop_samples > self.samples.len() {
                return Err(DetectorError::InvalidHop);
            }
            self.samples.drain(..self.hop_samples);
        }
        Ok(detections)
    }

    fn reset_for_rate(&mut self, sample_rate: i32) {
        self.sample_rate = sample_rate;
        self.window_samples = (sample_rate.wrapping_mul(40) / 1_000).max(1) as usize;
        self.hop_samples = (sample_rate.wrapping_mul(10) / 1_000).max(1) as usize;
        self.weights = (0..self.window_samples)
            .map(|index| {
                if self.window_samples <= 1 {
                    1.0
                } else {
                    0.54 - 0.46 * (2.0 * PI * index as f64 / (self.window_samples - 1) as f64).cos()
                }
            })
            .collect();
        self.weight_sum = self.weights.iter().sum();
        self.samples.clear();
        self.candidate = None;
        self.candidate_windows = 0;
        self.emitted_digit = None;
        self.release_windows = 0;
        // Native diagnostics cover the entire call, including rate changes.
    }

    fn advance(&mut self, analysis: Option<Detection>) -> Option<Detection> {
        let Some(detection) = analysis else {
            self.candidate = None;
            self.candidate_windows = 0;
            if self.emitted_digit.is_some() {
                self.release_windows = self.release_windows.wrapping_add(1);
                if self.release_windows >= 2 {
                    self.emitted_digit = None;
                    self.release_windows = 0;
                }
            }
            return None;
        };
        self.release_windows = 0;
        if self.candidate == Some(detection.digit) {
            self.candidate_windows = self.candidate_windows.wrapping_add(1);
        } else {
            self.candidate = Some(detection.digit);
            self.candidate_windows = 1;
        }
        if self.candidate_windows < 2 || self.emitted_digit == Some(detection.digit) {
            return None;
        }
        self.emitted_digit = Some(detection.digit);
        Some(detection)
    }

    fn analyze_window(&mut self) -> Option<Detection> {
        self.diagnostics.analyzed_windows = self.diagnostics.analyzed_windows.wrapping_add(1);
        let count = self.window_samples;
        let mean = self
            .samples
            .iter()
            .take(count)
            .map(|sample| f64::from(*sample))
            .sum::<f64>()
            / count as f64;
        let sum_squares: f64 = self
            .samples
            .iter()
            .take(count)
            .map(|sample| {
                let centered = f64::from(*sample) - mean;
                centered * centered
            })
            .sum();
        let rms = (sum_squares / count as f64).sqrt();
        if rms < 120.0 {
            return None;
        }
        self.diagnostics.audible_windows = self.diagnostics.audible_windows.wrapping_add(1);
        let low = self.measure_group(mean, [697.0, 770.0, 852.0, 941.0]);
        let high = self.measure_group(mean, [1209.0, 1336.0, 1477.0, 1633.0]);
        let low_index = index_of_maximum(&low);
        let high_index = index_of_maximum(&high);
        let low_amplitude = low[low_index];
        let high_amplitude = high[high_index];
        let minimum_amplitude = 150.0_f64.max(rms * 0.30);
        let twist = low_amplitude / 1.0_f64.max(high_amplitude);
        let energy_ratio = (low_amplitude * low_amplitude + high_amplitude * high_amplitude)
            / 1.0_f64.max(2.0 * rms * rms);
        if low_amplitude >= minimum_amplitude && high_amplitude >= minimum_amplitude {
            self.diagnostics.paired_tone_windows =
                self.diagnostics.paired_tone_windows.wrapping_add(1);
            self.diagnostics.maximum_paired_tone_energy_ratio = self
                .diagnostics
                .maximum_paired_tone_energy_ratio
                .max(energy_ratio);
        }
        if low_amplitude < minimum_amplitude
            || high_amplitude < minimum_amplitude
            || low_amplitude / 1.0_f64.max(second_largest(&low, low_index)) < 2.0
            || high_amplitude / 1.0_f64.max(second_largest(&high, high_index)) < 2.0
            || energy_ratio < 0.68
            || !(0.28..=3.55).contains(&twist)
        {
            return None;
        }
        self.diagnostics.candidate_tone_windows =
            self.diagnostics.candidate_tone_windows.wrapping_add(1);
        const DIGITS: [[char; 4]; 4] = [
            ['1', '2', '3', 'A'],
            ['4', '5', '6', 'B'],
            ['7', '8', '9', 'C'],
            ['*', '0', '#', 'D'],
        ];
        Some(Detection {
            digit: DIGITS[low_index][high_index],
            sample_rate: self.sample_rate,
            confirmed_duration_ms: 50,
            rms,
            tone_energy_ratio: energy_ratio,
            low_group_amplitude: low_amplitude,
            high_group_amplitude: high_amplitude,
        })
    }

    fn measure_group(&self, mean: f64, frequencies: [f64; 4]) -> [f64; 4] {
        frequencies.map(|frequency| {
            let mut maximum_amplitude = 0.0_f64;
            for scale in [0.985, 1.0, 1.015] {
                let coefficient =
                    2.0 * (2.0 * PI * frequency * scale / f64::from(self.sample_rate)).cos();
                let (mut previous, mut previous_previous) = (0.0, 0.0);
                for (sample, weight) in self.samples.iter().zip(&self.weights) {
                    let current = (f64::from(*sample) - mean) * weight + coefficient * previous
                        - previous_previous;
                    previous_previous = previous;
                    previous = current;
                }
                let power = previous * previous + previous_previous * previous_previous
                    - coefficient * previous * previous_previous;
                let amplitude = 2.0 * 0.0_f64.max(power).sqrt() / self.weight_sum;
                maximum_amplitude = maximum_amplitude.max(amplitude);
            }
            maximum_amplitude
        })
    }
}

impl Drop for InBandDetector {
    fn drop(&mut self) {
        self.samples.iter_mut().for_each(|sample| *sample = 0);
        self.weights.fill(0.0);
    }
}

fn index_of_maximum(values: &[f64; 4]) -> usize {
    let mut maximum = 0;
    for index in 1..values.len() {
        if values[index] > values[maximum] {
            maximum = index;
        }
    }
    maximum
}

fn second_largest(values: &[f64; 4], maximum: usize) -> f64 {
    values
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != maximum)
        .map(|(_, value)| *value)
        .fold(0.0, f64::max)
}
