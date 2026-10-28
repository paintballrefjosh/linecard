use std::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

pub type DeviceId = String;

/// Hybrid logical clock timestamp.
///
/// Ordering is (wall_ms, counter, device), so two timestamps are only equal
/// if they came from the same device at the same logical instant — which a
/// [`Clock`] never produces twice. That makes an `Hlc` usable as an op id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u32,
    pub device: DeviceId,
}

impl Hlc {
    pub fn zero(device: impl Into<DeviceId>) -> Self {
        Hlc {
            wall_ms: 0,
            counter: 0,
            device: device.into(),
        }
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:04}@{}", self.wall_ms, self.counter, self.device)
    }
}

/// Per-device clock. Physical time is passed in so tests are deterministic.
#[derive(Debug, Clone)]
pub struct Clock {
    device: DeviceId,
    wall_ms: u64,
    counter: u32,
}

impl Clock {
    pub fn new(device: impl Into<DeviceId>) -> Self {
        Clock {
            device: device.into(),
            wall_ms: 0,
            counter: 0,
        }
    }

    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    /// Timestamp for a new local event.
    pub fn tick(&mut self, physical_ms: u64) -> Hlc {
        if physical_ms > self.wall_ms {
            self.wall_ms = physical_ms;
            self.counter = 0;
        } else {
            self.counter += 1;
        }
        self.current()
    }

    /// Fold in a timestamp received from another device so our next local
    /// event orders after it.
    pub fn observe(&mut self, remote: &Hlc, physical_ms: u64) {
        let max_wall = self.wall_ms.max(remote.wall_ms).max(physical_ms);
        self.counter = if max_wall == self.wall_ms && max_wall == remote.wall_ms {
            self.counter.max(remote.counter) + 1
        } else if max_wall == self.wall_ms {
            self.counter + 1
        } else if max_wall == remote.wall_ms {
            remote.counter + 1
        } else {
            0
        };
        self.wall_ms = max_wall;
    }

    fn current(&self) -> Hlc {
        Hlc {
            wall_ms: self.wall_ms,
            counter: self.counter,
            device: self.device.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_even_when_physical_time_stalls_or_goes_back() {
        let mut c = Clock::new("a");
        let t1 = c.tick(1000);
        let t2 = c.tick(1000);
        let t3 = c.tick(900); // clock went backwards
        assert!(t1 < t2 && t2 < t3);
        assert_eq!(t3.wall_ms, 1000);
    }

    #[test]
    fn observe_orders_after_remote() {
        let mut fast = Clock::new("fast");
        let mut slow = Clock::new("slow");
        let remote = fast.tick(50_000);
        slow.observe(&remote, 1_000);
        let local = slow.tick(1_001);
        assert!(local > remote, "{local} should order after {remote}");
    }

    #[test]
    fn device_breaks_ties() {
        let a = Hlc {
            wall_ms: 5,
            counter: 0,
            device: "a".into(),
        };
        let b = Hlc {
            wall_ms: 5,
            counter: 0,
            device: "b".into(),
        };
        assert!(a < b);
    }
}
