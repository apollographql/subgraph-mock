use rand::{SeedableRng, rngs::StdRng};
use std::sync::{Arc, Mutex};

/// Source of randomness for a single request.
#[derive(Debug, Clone, Default)]
pub enum RngSource {
    #[default]
    Os,
    Seeded(Arc<Mutex<StdRng>>),
}

impl RngSource {
    pub fn seeded(seed: u64) -> Self {
        Self::Seeded(Arc::new(Mutex::new(StdRng::seed_from_u64(seed))))
    }

    /// Produce a fresh RNG for a single request.
    pub fn next(&self) -> StdRng {
        match self {
            Self::Os => StdRng::from_rng(&mut rand::rng()),
            Self::Seeded(master) => {
                let mut m = master.lock().expect("rng master mutex poisoned");
                StdRng::from_rng(&mut *m)
            }
        }
    }
}
