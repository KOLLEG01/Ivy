use super::MediaError;
use std::time::Duration;

/// Serialized by the owning route. Times are offsets from the same monotonic clock.
/// Session grants last until suspension/revocation; observation permits expire.
#[derive(Default)]
pub struct AudioPermit {
    observed_at: Duration,
    validity: Duration,
    granted: bool,
    revoked: bool,
    observed: bool,
    session_owned: bool,
    generation: u64,
}

impl AudioPermit {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn grant(&mut self) -> Result<(), MediaError> {
        if self.revoked {
            return Err(MediaError::Revoked);
        }
        if !self.granted {
            self.generation += 1;
        }
        self.session_owned = true;
        self.granted = true;
        Ok(())
    }

    pub fn renew(
        &mut self,
        observation: Duration,
        maximum_age: Duration,
        now: Duration,
    ) -> Result<(), MediaError> {
        if maximum_age.is_zero() || maximum_age > Duration::from_millis(500) {
            return Err(MediaError::InvalidPermitAge);
        }
        if self.revoked {
            return Err(MediaError::Revoked);
        }
        let age = now
            .checked_sub(observation)
            .ok_or(MediaError::StaleObservation)?;
        if age >= maximum_age || (self.observed && observation < self.observed_at) {
            return Err(MediaError::StaleObservation);
        }
        let previous_age = now.checked_sub(self.observed_at);
        if !self.granted || previous_age.is_none_or(|age| age >= self.validity) {
            self.generation += 1;
        }
        self.observed_at = observation;
        self.validity = maximum_age;
        self.granted = true;
        self.observed = true;
        self.session_owned = false;
        Ok(())
    }

    pub fn is_current(&mut self, now: Duration) -> bool {
        if !self.granted || self.revoked {
            return false;
        }
        if self.session_owned {
            return true;
        }
        if now
            .checked_sub(self.observed_at)
            .is_some_and(|age| age < self.validity)
        {
            return true;
        }
        self.granted = false;
        false
    }

    pub fn revoke(&mut self) {
        self.revoked = true;
        self.granted = false;
    }
    pub fn suspend(&mut self) {
        self.granted = false;
    }
}
