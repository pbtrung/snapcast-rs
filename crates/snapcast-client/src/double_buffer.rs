//! Size-limited circular buffer with a median.
//!
//! Port of the C++ `DoubleBuffer` template. Used by Stream for drift
//! detection via median filtering.

use std::collections::VecDeque;

/// A fixed-capacity circular buffer that computes the median of its contents.
#[derive(Debug, Clone)]
pub struct DoubleBuffer {
    buf: VecDeque<i64>,
    capacity: usize,
    /// Scratch space for [`median`](Self::median), so that it doesn't
    /// allocate (it runs in the audio callback).
    scratch: Vec<i64>,
}

impl DoubleBuffer {
    /// Create a buffer with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
            scratch: Vec::with_capacity(capacity),
        }
    }

    /// Add a value. Drops the oldest if full.
    pub fn add(&mut self, value: i64) {
        if self.buf.len() >= self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(value);
    }

    /// Upper median (the middle value of an odd count, the larger of the
    /// two middle values of an even count), or 0 when empty.
    pub fn median(&mut self) -> i64 {
        if self.buf.is_empty() {
            return 0;
        }
        self.scratch.clear();
        self.scratch.extend(self.buf.iter().copied());
        let mid = self.scratch.len() / 2;
        *self.scratch.select_nth_unstable(mid).1
    }

    /// Remove all values.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Number of values currently stored.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Returns true if the buffer has reached capacity.
    pub fn full(&self) -> bool {
        self.buf.len() >= self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_median_is_zero() {
        let mut db = DoubleBuffer::new(10);
        assert_eq!(db.median(), 0);
    }

    #[test]
    fn single_element() {
        let mut db = DoubleBuffer::new(10);
        db.add(42);
        assert_eq!(db.median(), 42);
    }

    #[test]
    fn odd_count_median() {
        let mut db = DoubleBuffer::new(10);
        for v in [3, 1, 4, 1, 5] {
            db.add(v);
        }
        // sorted: [1, 1, 3, 4, 5], median index 2 → 3
        assert_eq!(db.median(), 3);
    }

    #[test]
    fn even_count_median() {
        let mut db = DoubleBuffer::new(10);
        for v in [10, 20, 30, 40] {
            db.add(v);
        }
        // sorted: [10, 20, 30, 40], index 2 → 30
        assert_eq!(db.median(), 30);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut db = DoubleBuffer::new(3);
        db.add(100);
        db.add(200);
        db.add(300);
        assert!(db.full());
        db.add(400); // evicts 100
        assert_eq!(db.len(), 3);
        // contents: [200, 300, 400], sorted: [200, 300, 400], median → 300
        assert_eq!(db.median(), 300);
    }

    #[test]
    fn clear_resets() {
        let mut db = DoubleBuffer::new(10);
        db.add(1);
        db.add(2);
        db.clear();
        assert_eq!(db.len(), 0);
        assert_eq!(db.median(), 0);
    }

    #[test]
    fn negative_values() {
        let mut db = DoubleBuffer::new(5);
        for v in [-100, -50, 0, 50, 100] {
            db.add(v);
        }
        assert_eq!(db.median(), 0);
    }
}
