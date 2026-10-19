use super::CodecError;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, Resizable, SincInterpolationParameters, WindowFunction,
};

/// One direction's fixed-rate, mono PCM stream. Packet boundaries never reset
/// the filter. The 48 kHz identity path has neither filter delay nor history.
pub struct PcmResampler {
    filter: Option<Async<f32>>,
    maximum_input: usize,
    maximum_output: usize,
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
            let parameters =
                SincInterpolationParameters::new(taps, WindowFunction::BlackmanHarris2)
                    .oversampling_factor(32);
            Some(
                Async::new_sinc(
                    target_rate as f64 / source_rate as f64,
                    1.0,
                    &parameters,
                    maximum_input,
                    1,
                    FixedAsync::Input,
                )
                .map_err(|_| CodecError::Failed)?,
            )
        };
        let maximum_output = filter
            .as_ref()
            .map_or(maximum_input, Resampler::output_frames_max);
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
    pub fn delay_samples(&self) -> usize {
        self.filter.as_ref().map_or(0, Resampler::output_delay)
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
        filter
            .set_chunk_size(input.len())
            .map_err(|_| CodecError::InvalidFrame)?;
        let needed = filter.output_frames_next();
        if output.len() < needed {
            return Err(CodecError::InvalidFrame);
        }
        let input =
            InterleavedSlice::new(input, 1, input.len()).map_err(|_| CodecError::InvalidFrame)?;
        let output_len = output.len();
        let mut adapter = InterleavedSlice::new_mut(output, 1, output_len)
            .map_err(|_| CodecError::InvalidFrame)?;
        match filter.process_into_buffer(&input, &mut adapter, None) {
            Ok((consumed, produced))
                if consumed == filter.input_frames_next() && produced == needed =>
            {
                if output[..produced].iter().all(|s| s.is_finite()) {
                    return Ok(produced);
                }
            }
            _ => {}
        }
        output.fill(0.0);
        filter.reset();
        Err(CodecError::Failed)
    }
}

impl Drop for PcmResampler {
    fn drop(&mut self) {
        self.reset();
    }
}
