/// `1 - unique/uncompressed`. Zero when there are no uncompressed bytes (avoid div-by-zero).
pub fn dedup_ratio(bytes_unique_blobs: u64, bytes_uncompressed_entries: u64) -> f64 {
    if bytes_uncompressed_entries == 0 {
        0.0
    } else {
        1.0 - (bytes_unique_blobs as f64 / bytes_uncompressed_entries as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_ratio_zero_when_no_uncompressed_bytes() {
        // Guards inverted ratio or NaN from unique/0.
        assert_eq!(dedup_ratio(0, 0), 0.0);
        assert_eq!(dedup_ratio(10, 0), 0.0);
    }

    #[test]
    fn dedup_ratio_formula() {
        assert_eq!(dedup_ratio(25, 100), 0.75);
        assert_eq!(dedup_ratio(100, 100), 0.0);
        assert_eq!(dedup_ratio(0, 50), 1.0);
    }
}
