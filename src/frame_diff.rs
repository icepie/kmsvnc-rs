use std::sync::atomic::{AtomicU64, Ordering};

pub const TILE_SIZE: u32 = 64;

/// A dirty rectangle (coordinates only, no pixel data).
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Lock-free dirty tile accumulator shared between capture and VNC threads.
///
/// The capture thread sets bits for tiles that changed.
/// The VNC server drains (reads + clears) accumulated bits to get dirty rects.
pub struct DirtyTiles {
    bits: Vec<AtomicU64>,
    tiles_x: u32,
    tiles_y: u32,
    width: u32,
    height: u32,
}

impl DirtyTiles {
    pub fn new(width: u32, height: u32) -> Self {
        let tiles_x = width.div_ceil(TILE_SIZE);
        let tiles_y = height.div_ceil(TILE_SIZE);
        let total_tiles = (tiles_x * tiles_y) as usize;
        let word_count = total_tiles.div_ceil(64);
        Self {
            bits: std::iter::repeat_with(|| AtomicU64::new(0))
                .take(word_count)
                .collect(),
            tiles_x,
            tiles_y,
            width,
            height,
        }
    }

    /// Mark a tile as dirty (by tile index).
    #[inline]
    pub fn set(&self, tile_idx: usize) {
        let word = tile_idx / 64;
        let bit = tile_idx % 64;
        self.bits[word].fetch_or(1 << bit, Ordering::Relaxed);
    }

    /// Mark all tiles as dirty.
    pub fn set_all(&self) {
        let total = (self.tiles_x * self.tiles_y) as usize;
        for word in 0..(total / 64) {
            self.bits[word].store(u64::MAX, Ordering::Relaxed);
        }
        let remaining = total % 64;
        if remaining > 0 {
            let mask = (1u64 << remaining) - 1;
            self.bits[total / 64].fetch_or(mask, Ordering::Relaxed);
        }
    }

    /// Atomically drain all dirty bits and convert to DirtyRect list.
    pub fn drain_to_rects(&self) -> Vec<DirtyRect> {
        // Atomically swap all words to 0
        let mut words = Vec::with_capacity(self.bits.len());
        for bits in &self.bits {
            words.push(bits.swap(0, Ordering::Relaxed));
        }

        let mut rects = Vec::new();
        for ty in 0..self.tiles_y {
            for tx in 0..self.tiles_x {
                let idx = (ty * self.tiles_x + tx) as usize;
                let word = idx / 64;
                let bit = idx % 64;
                if words[word] & (1 << bit) != 0 {
                    let x0 = tx * TILE_SIZE;
                    let y0 = ty * TILE_SIZE;
                    rects.push(DirtyRect {
                        x: x0 as u16,
                        y: y0 as u16,
                        width: TILE_SIZE.min(self.width - x0) as u16,
                        height: TILE_SIZE.min(self.height - y0) as u16,
                    });
                }
            }
        }
        rects
    }
}
