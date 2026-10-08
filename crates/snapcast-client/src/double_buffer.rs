//! Size-limited circular buffer with median/mean/percentile.
//!
//! Port of the C++ `DoubleBuffer` template. Used by TimeProvider and Stream
//! for drift detection via median filtering.

use std::collections::VecDeque;

/// A fixed-capacity circular buffer that computes median over its contents.
#[derive(Debug, Clone)]
pub struct DoubleBuffer {
    buf: VecDeque<i64>,
    capacity: usize,
}

impl DoubleBuffer {
    /// Create a buffer with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Add a value. Drops the oldest if full.
    pub fn add(&mut self, value: i64) {
        if self.buf.len() >= self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(value);
    }

    /// Median value. If `mean_count > 1`, averages that many values around the median.
    pub fn median(&self, mean_count: usize) -> i64 {
        if self.buf.is_empty() {
            return 0;
        }
        let mut sorted: Vec<i64> = self.buf.iter().copied().collect();
        sorted.sort_unstable();

        if mean_count <= 1 || sorted.len() < mean_count {
            sorted[sorted.len() / 2]
        } else {
            let low = sorted.len() / 2 - mean_count / 2;
            let sum: i64 = sorted[low..low + mean_count].iter().sum();
            sum / mean_count as i64
        }
    }

    /// Simple median (mean_count=1).
    pub fn median_simple(&self) -> i64 {
        self.median(1)
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
        let db = DoubleBuffer::new(10);
        assert_eq!(db.median_simple(), 0);
    }

    #[test]
    fn single_element() {
        let mut db = DoubleBuffer::new(10);
        db.add(42);
        assert_eq!(db.median_simple(), 42);
    }

    #[test]
    fn odd_count_median() {
        let mut db = DoubleBuffer::new(10);
        for v in [3, 1, 4, 1, 5] {
            db.add(v);
        }
        // sorted: [1, 1, 3, 4, 5], median index 2 → 3
        assert_eq!(db.median_simple(), 3);
    }

    #[test]
    fn even_count_median() {
        let mut db = DoubleBuffer::new(10);
        for v in [10, 20, 30, 40] {
            db.add(v);
        }
        // sorted: [10, 20, 30, 40], index 2 → 30
        assert_eq!(db.median_simple(), 30);
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
        assert_eq!(db.median_simple(), 300);
    }

    #[test]
    fn median_with_mean() {
        let mut db = DoubleBuffer::new(10);
        for v in [1, 2, 3, 4, 5, 6, 7] {
            db.add(v);
        }
        // sorted: [1,2,3,4,5,6,7], mid=3, mean_count=3 → avg of [3,4,5] = 4
        assert_eq!(db.median(3), 4);
    }

    #[test]
    fn median_with_even_mean_count() {
        let mut db = DoubleBuffer::new(10);
        db.add(10);
        db.add(20);
        // Averages exactly two values (and doesn't index past the end).
        assert_eq!(db.median(2), 15);
        for v in [30, 40] {
            db.add(v);
        }
        // sorted: [10,20,30,40], low = 2 - 1 → avg of [20,30]
        assert_eq!(db.median(2), 25);
    }

    #[test]
    fn clear_resets() {
        let mut db = DoubleBuffer::new(10);
        db.add(1);
        db.add(2);
        db.clear();
        assert_eq!(db.len(), 0);
        assert_eq!(db.median_simple(), 0);
    }

    #[test]
    fn negative_values() {
        let mut db = DoubleBuffer::new(5);
        for v in [-100, -50, 0, 50, 100] {
            db.add(v);
        }
        assert_eq!(db.median_simple(), 0);
    }
}
