use super::{MediaError, SAMPLE_RATE};

/// Bounded mono PCM ring. The route serializes access, including status reads.
/// No allocation occurs after construction, including overflow and discontinuity blending.
pub struct AudioQueue {
    samples: Box<[f32]>,
    head: usize,
    count: usize,
    dropped: u64,
    underruns: u64,
    prebuffer: usize,
    rebuffer_after_underrun: bool,
    smooth_discontinuities: bool,
    smooth_pending: bool,
    last_sample: f32,
    started: bool,
}

impl AudioQueue {
    pub fn new(
        capacity: usize,
        prebuffer: usize,
        rebuffer_after_underrun: bool,
        smooth_discontinuities: bool,
    ) -> Result<Self, MediaError> {
        if capacity == 0 || capacity > SAMPLE_RATE / 5 {
            return Err(MediaError::InvalidCapacity);
        }
        if prebuffer > capacity {
            return Err(MediaError::InvalidPrebuffer);
        }
        Ok(Self {
            samples: vec![0.0; capacity].into_boxed_slice(),
            head: 0,
            count: 0,
            dropped: 0,
            underruns: 0,
            prebuffer,
            rebuffer_after_underrun,
            smooth_discontinuities,
            smooth_pending: false,
            last_sample: 0.0,
            started: false,
        })
    }

    pub fn count(&self) -> usize {
        self.count
    }
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    pub fn clear(&mut self) {
        self.samples.fill(0.0);
        self.count = 0;
        self.head = 0;
        self.started = false;
        self.last_sample = 0.0;
        self.smooth_pending = false;
    }

    pub fn write(&mut self, input: &[f32]) {
        let capacity = self.samples.len();
        if input.len() >= capacity {
            self.dropped += (self.count + input.len() - capacity) as u64;
            if self.count > 0 || input.len() > capacity {
                self.smooth_pending = true;
            }
            self.samples
                .copy_from_slice(&input[input.len() - capacity..]);
            self.head = 0;
            self.count = capacity;
            return;
        }
        let discard = (self.count + input.len()).saturating_sub(capacity);
        if discard > 0 {
            self.smooth_pending = true;
        }
        self.head = (self.head + discard) % capacity;
        self.count -= discard;
        self.dropped += discard as u64;
        let tail = (self.head + self.count) % capacity;
        let first = input.len().min(capacity - tail);
        self.samples[tail..tail + first].copy_from_slice(&input[..first]);
        self.samples[..input.len() - first].copy_from_slice(&input[first..]);
        self.count += input.len();
    }

    pub fn read(&mut self, output: &mut [f32]) -> usize {
        if output.is_empty() {
            return 0;
        }
        if !self.started {
            if self.count < self.prebuffer {
                output.fill(0.0);
                return 0;
            }
            self.started = true;
            self.smooth_pending = true;
        }
        let length = self.count.min(output.len());
        let first = length.min(self.samples.len() - self.head);
        output[..first].copy_from_slice(&self.samples[self.head..self.head + first]);
        output[first..length].copy_from_slice(&self.samples[..length - first]);
        self.samples[self.head..self.head + first].fill(0.0);
        self.samples[..length - first].fill(0.0);
        self.head = (self.head + length) % self.samples.len();
        self.count -= length;
        if length < output.len() {
            self.underruns += 1;
            if self.rebuffer_after_underrun {
                self.started = false;
            }
        }
        output[length..].fill(0.0);
        if self.smooth_discontinuities {
            self.smooth(output, length);
        }
        length
    }

    fn smooth(&mut self, output: &mut [f32], length: usize) {
        const BLEND: usize = SAMPLE_RATE / 200;
        if self.smooth_pending && length > 0 {
            let limit = BLEND.min(length);
            for (index, sample) in output[..limit].iter_mut().enumerate() {
                let weight = (index + 1) as f32 / limit as f32;
                *sample = self.last_sample * (1.0 - weight) + *sample * weight;
            }
            self.smooth_pending = false;
        }
        if length < output.len() {
            if length > 0 {
                let limit = BLEND.min(length);
                for (index, sample) in output[length - limit..length].iter_mut().enumerate() {
                    *sample *= 1.0 - (index + 1) as f32 / limit as f32;
                }
            } else {
                let limit = BLEND.min(output.len());
                for (index, sample) in output[..limit].iter_mut().enumerate() {
                    *sample = self.last_sample * (1.0 - (index + 1) as f32 / limit as f32);
                }
            }
        }
        self.last_sample = output[output.len() - 1];
    }
}

impl Drop for AudioQueue {
    fn drop(&mut self) {
        self.clear();
    }
}
