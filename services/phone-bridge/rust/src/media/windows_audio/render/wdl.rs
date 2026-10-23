// This class based on the Resampler that is part of Cockos WDL
// originally written in C++ and ported to C# for NAudio by Mark Heath
// Used in NAudio with permission from Justin Frankel
// Original WDL License:
//     Copyright (C) 2005 and later Cockos Incorporated
//
//     Portions copyright other contributors, see each source file for more information
//
//     This software is provided 'as-is', without any express or implied
//     warranty.  In no event will the authors be held liable for any damages
//     arising from the use of this software.
//
//     Permission is granted to anyone to use this software for any purpose,
//     including commercial applications, and to alter it and redistribute it
//     freely, subject to the following restrictions:
//
//     1. The origin of this software must not be misrepresented; you must not
//        claim that you wrote the original software. If you use this software
//        in a product, an acknowledgment in the product documentation would be
//        appreciated but is not required.
//     2. Altered source versions must be plainly marked as such, and must not be
//        misrepresented as being the original software.
//     3. This notice may not be removed or altered from any source distribution.

// Altered Rust specialization of NAudio WdlResampler at commit
// 23922d2a87cf9508883a939af8b1495a0d35cb5a. Only the Windows render provider's
// 48k mono, output-driven, linear/two-IIR mode is implemented. Its queue source
// always returns the full requested span, so there is no EOF/flush branch.
use super::{SAMPLE_RATE, WindowsAudioBuffers};

pub(super) struct Resampler {
    ratio: f64,
    position: f64,
    input: Vec<f32>,
    retained: usize,
    filter: Iir,
}

impl Resampler {
    pub fn new(rate: u32) -> Self {
        let ratio = SAMPLE_RATE as f64 / f64::from(rate);
        let cutoff = if ratio > 1.0 { 1.0 / ratio } else { ratio };
        Self {
            ratio,
            position: 0.0,
            input: Vec::new(),
            retained: 0,
            filter: Iir::new(cutoff * f64::from(0.693_f32)),
        }
    }

    pub fn read(&mut self, owner: &mut WindowsAudioBuffers, output: &mut [f32]) -> usize {
        let requested =
            ((self.ratio * output.len() as f64) as usize + 4).saturating_sub(self.retained);
        self.input.resize(self.retained + requested, 0.0);
        // Even an empty source read observes the permit, as the original does.
        owner.render(&mut self.input[self.retained..]);
        if self.ratio > 1.0 && requested > 0 {
            self.filter.apply(&mut self.input[self.retained..]);
        }
        self.retained += requested;
        let mut position = self.position;
        let mut read = 0;
        for value in output.iter_mut() {
            let index = position as usize;
            if index + 1 >= self.retained {
                break;
            }
            let fraction = position - index as f64;
            *value = (f64::from(self.input[index]) * (1.0 - fraction)
                + f64::from(self.input[index + 1]) * fraction) as f32;
            position += self.ratio;
            read += 1;
        }
        if self.ratio < 1.0 && read > 0 {
            self.filter.apply(&mut output[..read]);
        }
        let consumed = (position as usize).min(self.retained);
        self.position = position - consumed as f64;
        self.input.copy_within(consumed..self.retained, 0);
        self.retained -= consumed;
        read
    }
}

struct Iir {
    b0: f64,
    b1: f64,
    a1: f64,
    a2: f64,
    history: [[f64; 4]; 2],
}

impl Iir {
    fn new(cutoff: f64) -> Self {
        let position = cutoff * std::f64::consts::PI;
        let cosine = position.cos();
        let sine = position.sin();
        let alpha = sine / (2.0 * f64::from(0.707_f32));
        let scale = 1.0 / (1.0 + alpha);
        let b1 = (1.0 - cosine) * scale;
        Self {
            b0: b1 * 0.5,
            b1,
            a1: -2.0 * cosine * scale,
            a2: (1.0 - alpha) * scale,
            history: [[0.0; 4]; 2],
        }
    }
    fn apply(&mut self, samples: &mut [f32]) {
        for history in &mut self.history {
            for value in samples.iter_mut() {
                let input = f64::from(*value);
                let output = input * self.b0 + history[0] * self.b1 + history[1] * self.b0
                    - history[2] * self.a1
                    - history[3] * self.a2;
                history[1] = history[0];
                history[0] = input;
                history[3] = history[2];
                // WDL's double denormal filter clears zero-exponent feedback.
                history[2] = if output.to_bits() & 0x7ff0_0000_0000_0000 == 0 {
                    0.0
                } else {
                    output
                };
                *value = history[2] as f32;
            }
        }
    }
}
