use core::hash::Hasher;

pub struct SimpleHasher {
    state: u64,
}

impl SimpleHasher {
    pub fn new() -> Self {
        SimpleHasher { state: 0 }
    }
}

impl Default for SimpleHasher {
    fn default() -> Self {
        SimpleHasher::new()
    }
}

impl Hasher for SimpleHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.state ^= byte as u64;
            self.state = self.state.rotate_left(1);
        }
    }

    fn finish(&self) -> u64 {
        self.state
    }
}
