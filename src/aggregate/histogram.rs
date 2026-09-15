//! Log-bucketed duration histogram: fixed memory, quantiles within about 10%.

use std::time::Duration;

use super::DurationSummary;
use crate::num::round_sig_u64;

/// Four buckets per power of two over microseconds: covers every `u64` value.
const BUCKETS: usize = 256;

#[derive(Debug, Clone)]
pub(super) struct LogHistogram {
    counts: [u32; BUCKETS],
    count: u64,
    total_us: u64,
    max_us: u64,
}

impl Default for LogHistogram {
    fn default() -> Self {
        LogHistogram {
            counts: [0; BUCKETS],
            count: 0,
            total_us: 0,
            max_us: 0,
        }
    }
}

impl LogHistogram {
    pub(super) fn record(&mut self, duration: Duration) {
        let us = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        let bucket = &mut self.counts[bucket_of(us)];
        *bucket = bucket.saturating_add(1);
        self.count += 1;
        self.total_us = self.total_us.saturating_add(us);
        self.max_us = self.max_us.max(us);
    }

    pub(super) fn summary(&self) -> DurationSummary {
        let estimate = |q| Duration::from_micros(round_sig_u64(self.quantile(q), 2));
        DurationSummary {
            count: self.count,
            total: Duration::from_micros(self.total_us),
            p50: estimate(0.5),
            p95: estimate(0.95),
            max: Duration::from_micros(self.max_us),
        }
    }

    fn quantile(&self, q: f64) -> u64 {
        let rank = ((q * self.count as f64).ceil() as u64).max(1);
        let mut seen = 0;
        for (bucket, &count) in self.counts.iter().enumerate() {
            seen += u64::from(count);
            if seen >= rank {
                return midpoint(bucket).min(self.max_us);
            }
        }
        self.max_us
    }
}

fn bucket_of(us: u64) -> usize {
    if us < 4 {
        return us as usize;
    }
    let msb = 63 - us.leading_zeros();
    (msb * 4 + ((us >> (msb - 2)) & 3) as u32) as usize
}

fn midpoint(bucket: usize) -> u64 {
    if bucket < 4 {
        return bucket as u64;
    }
    let width = 1u64 << (bucket / 4 - 2);
    let lower = (4 + (bucket % 4) as u64) * width;
    lower + width / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_contain_their_values() {
        for us in [0, 3, 4, 7, 8, 1_000, 123_456_789, u64::MAX] {
            let bucket = bucket_of(us);
            assert!(bucket < BUCKETS, "{us}");
            if bucket >= 4 {
                let width = 1u64 << (bucket / 4 - 2);
                let lower = (4 + (bucket % 4) as u64) * width;
                assert!(lower <= us && us - lower < width, "{us} in bucket {bucket}");
            }
        }
    }
}
