use super::CodecError;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, Resizable, SincInterpolationParameters, WindowFunction,
};

/// One direction's fixed-rate, mono PCM stream. Packet boundaries never reset
/// the filter. The 48 kHz identity path has neither filter delay nor history.
pub struct PcmResampler {
    filter: Option<Filtered>,
    maximum_input: usize,
    maximum_output: usize,
}

struct Filtered {
    engine: Async<f32>,
    samples: Box<[f32]>,
    pending: usize,
    source: usize,
    target: usize,
    phase: usize,
    initial_skip: usize,
    skip: usize,
    initial_holdback: usize,
    holdback: usize,
}

impl PcmResampler {
    pub fn new(source_rate: usize, target_rate: usize) -> Result<Self, CodecError> {
        if !matches!(
            (source_rate, target_rate),
            (48000, 48000 | 16000 | 8000) | (16000 | 8000, 48000)
        ) {
            return Err(CodecError::InvalidFormat);
        }
        let maximum_input = source_rate * 120 / 1000;
        let filter = if source_rate == target_rate {
            None
        } else {
            // Keep the same transition width at the lower clock in either
            // direction. Longer input-side filters are needed for decimation.
            let taps = 64 * (source_rate / target_rate).max(1);
            let parameters = SincInterpolationParameters::new(taps, WindowFunction::BlackmanHarris)
                .oversampling_factor(32)
                .f_cutoff(0.94);
            let engine = Async::new_sinc(
                target_rate as f64 / source_rate as f64,
                1.0,
                &parameters,
                maximum_input,
                1,
                FixedAsync::Input,
            )
            .map_err(|_| CodecError::Failed)?;
            // The pinned interpolator begins at -(taps-1), advances before
            // interpolation, and adds (delay-1) leading samples. Trim that
            // prefix once, as the original feed-mode filter does by waiting
            // for lookahead. Retain one extra sample to fence its fractional
            // floating-point phase; the public clock uses integer arithmetic.
            let skip = engine.output_delay() - 1;
            let holdback = skip + (2 * target_rate).div_ceil(source_rate) + 1;
            let samples = vec![0.0; engine.output_frames_max() + 16].into_boxed_slice();
            Some(Filtered {
                engine,
                samples,
                pending: 0,
                source: source_rate,
                target: target_rate,
                phase: 0,
                initial_skip: skip,
                skip,
                initial_holdback: holdback,
                holdback,
            })
        };
        let maximum_output = target_rate * 120 / 1000;
        Ok(Self {
            filter,
            maximum_input,
            maximum_output,
        })
    }

    pub fn maximum_input_samples(&self) -> usize {
        self.maximum_input
    }
    pub fn maximum_output_samples(&self) -> usize {
        self.maximum_output
    }
    pub fn startup_samples(&self) -> usize {
        self.filter.as_ref().map_or(0, |f| f.initial_holdback)
    }

    pub fn reset(&mut self) {
        if let Some(filter) = self.filter.as_mut() {
            filter.reset();
        }
    }

    /// Feed every input sample exactly once, returning the produced prefix.
    /// The unused output is cleared, including on errors. No processing or
    /// reset allocation occurs after construction. Invalid input is rejected
    /// before it can enter this generation's history.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<usize, CodecError> {
        output.fill(0.0);
        if input.is_empty()
            || input.len() > self.maximum_input
            || input.iter().any(|s| !s.is_finite())
        {
            return Err(CodecError::InvalidFrame);
        }
        let Some(filter) = self.filter.as_mut() else {
            if output.len() < input.len() {
                return Err(CodecError::InvalidFrame);
            }
            output[..input.len()].copy_from_slice(input);
            return Ok(input.len());
        };
        let tick = input.len() * filter.target + filter.phase;
        let nominal = tick / filter.source;
        let held = filter.holdback.min(nominal);
        let wanted = nominal - held;
        if output.len() < wanted {
            return Err(CodecError::InvalidFrame);
        }
        filter
            .engine
            .set_chunk_size(input.len())
            .map_err(|_| CodecError::InvalidFrame)?;
        let needed = filter.engine.output_frames_next();
        if needed > filter.samples.len() - filter.pending {
            filter.reset();
            return Err(CodecError::Failed);
        }
        let input_len = input.len();
        let input =
            InterleavedSlice::new(input, 1, input.len()).map_err(|_| CodecError::InvalidFrame)?;
        let output_len = filter.samples.len() - filter.pending;
        let mut adapter =
            InterleavedSlice::new_mut(&mut filter.samples[filter.pending..], 1, output_len)
                .map_err(|_| CodecError::InvalidFrame)?;
        match filter
            .engine
            .process_into_buffer(&input, &mut adapter, None)
        {
            Ok((consumed, produced)) if consumed == input_len && produced == needed => {
                let total = filter.pending + produced;
                let skip = filter.skip.min(total);
                let available = total - skip;
                if available >= wanted && filter.samples[..total].iter().all(|s| s.is_finite()) {
                    output[..wanted].copy_from_slice(&filter.samples[skip..skip + wanted]);
                    filter.samples.copy_within(skip + wanted..total, 0);
                    filter.pending = available - wanted;
                    filter.samples[filter.pending..].fill(0.0);
                    filter.skip -= skip;
                    filter.holdback -= held;
                    filter.phase = tick % filter.source;
                    return Ok(wanted);
                }
            }
            _ => {}
        }
        output.fill(0.0);
        filter.reset();
        Err(CodecError::Failed)
    }
}

impl Filtered {
    fn reset(&mut self) {
        self.engine.reset();
        self.samples.fill(0.0);
        self.pending = 0;
        self.phase = 0;
        self.skip = self.initial_skip;
        self.holdback = self.initial_holdback;
    }
}

impl Drop for PcmResampler {
    fn drop(&mut self) {
        self.reset();
    }
}
