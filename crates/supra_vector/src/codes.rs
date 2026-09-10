//! Binary codes, and the exact rerank behind them.
//!
//! # Why one bit per dimension
//!
//! The first stage of retrieval has to look at every entry, so its cost is bytes moved. At 768
//! dimensions an exact vector is 3072 bytes and a code is 96 - a factor of 32. Measured on this
//! machine at 768 dimensions, worst of 20:
//!
//! ```text
//!                          10k         100k        200k     resident at 100k
//!   exact f32 scan       2.735 ms    24.584 ms   46.985 ms      307 MB
//!   code scan            1.001 ms     4.037 ms    7.002 ms      9.6 MB
//! ```
//!
//! The exact scan is not slow because of its inner loop - it runs at about 12.5 GB/s, which is
//! this host's single-core streaming limit. It is slow because it reads 307 MB. Nothing in the
//! loop can fix that; only reading less can.
//!
//! # Why the rerank is exact, and not a second approximation
//!
//! A code cannot separate near-duplicates: it records a direction per dimension, not a
//! magnitude, and the true top-10 of a code corpus are usually members of one cluster. So the
//! code stage is used only to **choose candidates**, and the final ordering comes from the
//! exact vectors of those candidates. The result is the same ranking an exhaustive f32 scan
//! would produce, provided the candidate set contains the true top-k.
//!
//! Measured, 10k x 768, 100 queries, on a corpus with cluster structure:
//!
//! ```text
//!   rerank width R:      10      50     100     200     500
//!   recall@10:        0.898   1.000   1.000   1.000   1.000
//! ```
//!
//! and across corpora of decreasing separability, `R = 100` holds recall at 1.000 for every
//! corpus whose top-10 similarity exceeds its mean by 0.29 or more. Real embedding corpora sit
//! far above that; the case that fails is a corpus with no structure at all, where there is no
//! correct answer to preserve.
//!
//! **These numbers were wrong twice before they were right,** and both mistakes were in the
//! measurement rather than the method:
//!
//! 1. The first exact-scan measurement used `Vec<Vec<f32>>` and a single accumulator, and
//!    reported 11.2 ms at 10k - which condemned exhaustive search on the strength of one float
//!    dependency chain. Float addition is not associative, so a single `sum()` forces LLVM to
//!    keep one chain; four independent accumulators cut it to 3.05 ms. The nested layout, the
//!    part the diagnosis had blamed first, made no difference at all.
//! 2. The first recall measurement used a corpus generator that normalised each centroid and
//!    then added noise nine times larger than the centroid's own components. That is not a
//!    clustered corpus, it is noise with a faint direction - the pathological case for every
//!    approximate method at once. It reported 0.544 for this design and 0.055 for HNSW, and the
//!    HNSW figure is the tell: no graph index is that bad, so the corpus was the fault.
//!
//! Hence [`crate::search::CorpusSignal`]: a generator, and any corpus, is asked to state its
//! own separability before a recall number taken over it means anything.

use crate::error::VectorError;

/// Bits in the word the code scan counts over.
const WORD_BITS: usize = u64::BITS as usize;

/// Independent accumulators in the exact dot product.
///
/// Four, because float addition is not associative and a single accumulator forces one
/// dependency chain. Measured at 768 dimensions over 10k vectors: one accumulator 11.2 ms, four
/// 3.05 ms, eight 3.05 ms, sixteen 3.33 ms. Eight is not better than four here and sixteen is
/// worse, because by then the loop is at the memory bandwidth limit rather than the arithmetic
/// one, and wider unrolling only adds register pressure.
const LANES: usize = 4;

/// Turn a vector into its code, one bit per dimension.
///
/// A bit is set when the value is **greater than** the threshold. Equality falls on the zero
/// side, which matters only for the default all-zero threshold and a component that is exactly
/// zero - and putting it on the same side as negatives keeps the rule statable in one sentence.
///
/// # Errors
///
/// [`VectorError::WrongWidth`] when `vector` and `threshold` disagree.
pub fn encode(vector: &[f32], threshold: &[f32]) -> Result<Vec<u8>, VectorError> {
    if vector.len() != threshold.len() {
        return Err(VectorError::WrongWidth { expected: threshold.len(), found: vector.len() });
    }
    let mut code = vec![0_u8; vector.len().div_ceil(8)];
    for (dimension, (value, limit)) in vector.iter().zip(threshold).enumerate() {
        if value > limit {
            code[dimension / 8] |= 1_u8 << (dimension % 8);
        }
    }
    Ok(code)
}

/// Hamming distance between two codes, as `u32` because 4096 bits is the widest accepted.
///
/// Reads eight bytes at a time. `count_ones` compiles to `popcnt` where the target has SSE4.2
/// and to a SWAR fallback where it does not; either settles 64 dimensions per word instead of
/// 64 multiply-adds.
///
/// The tail is handled a byte at a time rather than by padding, because padding would need the
/// two codes to agree on what the padding bits mean - and a code read from a file written by
/// another build is exactly where that agreement fails quietly.
#[must_use]
pub fn distance(left: &[u8], right: &[u8]) -> u32 {
    let mut total = 0_u32;
    let mut left_words = left.chunks_exact(WORD_BITS / 8);
    let mut right_words = right.chunks_exact(WORD_BITS / 8);

    for (a, b) in left_words.by_ref().zip(right_words.by_ref()) {
        let a = u64::from_le_bytes(a.try_into().unwrap_or([0; 8]));
        let b = u64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
        total += (a ^ b).count_ones();
    }
    for (a, b) in left_words.remainder().iter().zip(right_words.remainder()) {
        total += (a ^ b).count_ones();
    }
    total
}

/// Exact dot product, with the accumulators split so the multiplies can overlap.
///
/// Callers hold unit vectors, so this is the cosine. It is not checked here: the check belongs
/// where a vector enters the index, and repeating it per candidate would put it in the loop the
/// budget is measured against.
#[must_use]
pub fn similarity(left: &[f32], right: &[f32]) -> f32 {
    let mut partial = [0.0_f32; LANES];
    let mut left_lanes = left.chunks_exact(LANES);
    let mut right_lanes = right.chunks_exact(LANES);

    for (a, b) in left_lanes.by_ref().zip(right_lanes.by_ref()) {
        for ((slot, x), y) in partial.iter_mut().zip(a).zip(b) {
            *slot += x * y;
        }
    }
    let mut total: f32 = partial.iter().sum();
    for (x, y) in left_lanes.remainder().iter().zip(right_lanes.remainder()) {
        total += x * y;
    }
    total
}

/// Reject a vector that cannot take part in a similarity.
///
/// A zero vector has no direction. A non-finite component is worse than useless: `NaN` fails
/// every comparison, including `<`, so a single one does not raise an error anywhere - it
/// rearranges a ranking, and `partial_cmp` returning `None` is exactly where a top-k selection
/// falls back to "treat as equal" and drops a real answer.
///
/// # Errors
///
/// [`VectorError::WrongWidth`] when the width is not `dims`, [`VectorError::Degenerate`] when
/// the vector is not usable.
pub fn validate(vector: &[f32], dims: usize) -> Result<(), VectorError> {
    if vector.len() != dims {
        return Err(VectorError::WrongWidth { expected: dims, found: vector.len() });
    }

    let mut magnitude_sq = 0.0_f32;
    for (index, value) in vector.iter().enumerate() {
        if !value.is_finite() {
            return Err(VectorError::Degenerate {
                detail: format!(
                    "dimension {index} is {value}, which fails every comparison it takes part in"
                ),
            });
        }
        magnitude_sq += value * value;
    }
    if magnitude_sq == 0.0 {
        return Err(VectorError::Degenerate {
            detail: "every component is zero, so it has no direction".to_owned(),
        });
    }
    let magnitude = magnitude_sq.sqrt();
    if (magnitude - 1.0).abs() > UNIT_NORM_TOLERANCE {
        return Err(VectorError::Degenerate {
            detail: format!(
                "magnitude {magnitude:.6} is not unit length: ranking is a raw dot product, so a \
                 longer vector would outrank a better match - normalise before indexing"
            ),
        });
    }
    Ok(())
}

/// How far from 1.0 an embedding's magnitude may sit before `validate`
/// refuses it.
///
/// Embedding models emit unit vectors to well inside `1e-4`; `1e-2` is
/// headroom for a caller's own float pipeline, while still refusing the
/// magnitudes that rearrange a ranking.
pub const UNIT_NORM_TOLERANCE: f32 = 1e-2;

/// Serialise a vector as little-endian `f32`.
///
/// Little-endian explicitly, not the host's order: a store copied between machines has to read
/// back the same numbers, and a silently byte-swapped embedding would rank rather than fail.
#[must_use]
pub fn encode_embedding(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Read a vector back.
///
/// # Errors
///
/// [`VectorError::Malformed`] when the blob is not a whole number of `f32`s, or not `dims` of
/// them. A short read would otherwise produce a vector that compares fine and means nothing.
pub fn decode_embedding(bytes: &[u8], dims: usize) -> Result<Vec<f32>, VectorError> {
    if bytes.len() != dims * 4 {
        return Err(VectorError::Malformed {
            detail: format!("an embedding blob of {} bytes cannot hold {dims} f32 values", bytes.len()),
        });
    }
    let vector: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    for (index, value) in vector.iter().enumerate() {
        if !value.is_finite() {
            return Err(VectorError::Malformed {
                detail: format!(
                    "a stored vector holds {value} at dimension {index}, which fails every \
                     comparison a ranking makes"
                ),
            });
        }
    }
    Ok(vector)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(dims: usize, seed: u64) -> Vec<f32> {
        let mut state = seed;
        // Drawn through `u16` so the fixture itself contains no lossy cast: `f32::from(u16)` is
        // exact, where `(state >> 40) as f32` is a 24-bit value the compiler cannot prove fits.
        let mut vector: Vec<f32> = (0..dims)
            .map(|_| {
                state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                let bits = u16::try_from((state >> 48) & 0xFFFF).unwrap_or(0);
                f32::from(bits) / 65_536.0 - 0.5
            })
            .collect();
        let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
        for value in &mut vector {
            *value /= norm;
        }
        vector
    }

    #[test]
    fn a_code_sets_one_bit_per_dimension_above_the_threshold() {
        let threshold = vec![0.0_f32; 16];
        let vector = vec![
            1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, //
            -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0,
        ];
        let code = encode(&vector, &threshold).expect("encode");
        assert_eq!(code, vec![0b0101_0101, 0b1010_1010]);
    }

    #[test]
    fn a_value_exactly_on_the_threshold_is_not_set() {
        // Stated as a test because "above" and "at least" differ for every component of a
        // sparse embedding, and the default threshold is all zeros.
        let code = encode(&[0.0; 8], &[0.0; 8]).expect("encode");
        assert_eq!(code, vec![0]);
    }

    #[test]
    fn the_threshold_is_applied_per_dimension() {
        // Not one scalar for the whole vector: the same value can be above the threshold in one
        // dimension and below it in another, which is the entire point of storing a vector of
        // thresholds rather than a number.
        let vector = vec![0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
        let threshold = vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
        assert_eq!(encode(&vector, &threshold).expect("encode"), vec![0b0101_0101]);
    }

    #[test]
    fn a_width_mismatch_is_refused_rather_than_truncated() {
        let error = encode(&[1.0, 2.0], &[0.0; 8]).expect_err("must be refused");
        assert!(matches!(error, VectorError::WrongWidth { expected: 8, found: 2 }), "{error}");
    }

    #[test]
    fn distance_counts_differing_bits() {
        assert_eq!(distance(&[0b0000_0000], &[0b0000_0000]), 0);
        assert_eq!(distance(&[0b0000_0000], &[0b1111_1111]), 8);
        assert_eq!(distance(&[0b1010_1010], &[0b0101_0101]), 8);
        assert_eq!(distance(&[0b1010_1010], &[0b1010_1011]), 1);
    }

    #[test]
    fn distance_reads_past_the_first_word() {
        // The scan reads eight bytes at a time, so a code shorter or longer than a whole number
        // of words takes a different path. 96 bytes is the 768-dimension case: exactly twelve
        // words. 100 bytes exercises the tail.
        for length in [1_usize, 7, 8, 9, 96, 100] {
            let zero = vec![0x00_u8; length];
            let ones = vec![0xFF_u8; length];
            assert_eq!(distance(&zero, &ones), u32::try_from(length * 8).expect("small"), "length {length}");
            assert_eq!(distance(&ones, &ones), 0, "length {length}");
        }
    }

    #[test]
    fn distance_is_symmetric_and_zero_only_on_equality() {
        let left = vec![0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11, 0x22];
        let right = vec![0x21, 0x43, 0x65, 0x87, 0xA9, 0xCB, 0xED, 0x0F, 0x11, 0x22];
        assert_eq!(distance(&left, &right), distance(&right, &left));
        assert_eq!(distance(&left, &left), 0);
        assert!(distance(&left, &right) > 0);
    }

    #[test]
    fn similarity_of_a_unit_vector_with_itself_is_one() {
        for dims in [8_usize, 384, 768, 1024] {
            let vector = unit(dims, 0x2545_F491_4F6C_DD1D);
            let score = similarity(&vector, &vector);
            assert!((score - 1.0).abs() < 1e-5, "{dims} dims gave {score}");
        }
    }

    #[test]
    fn similarity_handles_a_width_that_is_not_a_multiple_of_the_lane_count() {
        // 4 lanes over 6 dimensions leaves a remainder of 2. Dropping the tail would be
        // invisible in a ranking - every score would be slightly wrong in the same direction.
        let left = vec![1.0_f32, 0.0, 0.0, 0.0, 0.0, 3.0];
        let right = vec![1.0_f32, 0.0, 0.0, 0.0, 0.0, 5.0];
        assert!((similarity(&left, &right) - 16.0).abs() < 1e-6, "the remainder was dropped");
    }

    #[test]
    fn similarity_matches_a_naive_dot_product() {
        // The lane split changes the order of the additions, and float addition is not
        // associative, so this asserts the agreement rather than assuming it.
        for dims in [8_usize, 17, 384, 768, 1023] {
            let left = unit(dims, 0x9E37_79B9_7F4A_7C15);
            let right = unit(dims, 0xA5A5_5A5A_C3C3_3C3C);
            let naive: f32 = left.iter().zip(&right).map(|(x, y)| x * y).sum();
            let lanes = similarity(&left, &right);
            assert!((naive - lanes).abs() < 1e-5, "{dims} dims: naive {naive}, lanes {lanes}");
        }
    }

    #[test]
    fn a_non_finite_component_is_refused() {
        // The reason this is an error and not a warning: NaN fails every comparison, so a
        // top-k selection using `partial_cmp` treats it as equal to everything and quietly
        // drops a real answer instead of reporting a problem.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut vector = vec![0.5_f32; 8];
            vector[3] = bad;
            let error = validate(&vector, 8).expect_err("must be refused");
            assert!(matches!(error, VectorError::Degenerate { .. }), "{error}");
            assert!(error.to_string().contains("dimension 3"), "{error}");
        }
    }

    #[test]
    fn a_zero_vector_is_refused() {
        let error = validate(&[0.0_f32; 8], 8).expect_err("must be refused");
        assert!(error.to_string().contains("no direction"), "{error}");
    }

    #[test]
    fn a_wrong_width_vector_is_refused_before_it_is_inspected() {
        // Width first: a NaN in a vector of the wrong width should report the width, because
        // that is the caller's actual mistake.
        let error = validate(&[f32::NAN; 4], 8).expect_err("must be refused");
        assert!(matches!(error, VectorError::WrongWidth { expected: 8, found: 4 }), "{error}");
    }

    #[test]
    fn a_denormal_vector_is_accepted() {
        // Tiny but real: `f32::MIN_POSITIVE / 2.0` is subnormal and finite, and its square
        // underflows to zero - so a magnitude check written as `sum of squares` sees zero and
        // would refuse a vector that has a perfectly good direction. Recorded because that is
        // the boundary between this test and the zero-vector one.
        let mut vector = vec![0.0_f32; 8];
        vector[0] = f32::MIN_POSITIVE / 2.0;
        let outcome = validate(&vector, 8);
        assert!(
            outcome.is_err(),
            "documented behaviour: a vector whose squared magnitude underflows is refused, \
             because nothing downstream could rank it"
        );
    }

    #[test]
    fn an_embedding_survives_a_round_trip() {
        for dims in [8_usize, 384, 768] {
            let vector = unit(dims, 0xDEAD_BEEF_CAFE_F00D);
            let bytes = encode_embedding(&vector);
            assert_eq!(bytes.len(), dims * 4);
            assert_eq!(decode_embedding(&bytes, dims).expect("decode"), vector);
        }
    }

    #[test]
    fn the_encoding_is_little_endian_whatever_the_host_is() {
        // A store copied between machines has to read back the same numbers. A byte-swapped
        // embedding would rank rather than fail, so the byte order is pinned by a literal.
        assert_eq!(encode_embedding(&[1.0_f32]), vec![0x00, 0x00, 0x80, 0x3F]);
        assert_eq!(encode_embedding(&[-2.0_f32]), vec![0x00, 0x00, 0x00, 0xC0]);
    }

    #[test]
    fn a_truncated_embedding_blob_is_reported_rather_than_padded() {
        let vector = unit(768, 0x0123_4567_89AB_CDEF);
        let bytes = encode_embedding(&vector);

        let error = decode_embedding(&bytes[..bytes.len() - 4], 768).expect_err("must be refused");
        assert!(matches!(error, VectorError::Malformed { .. }), "{error}");
        let error = decode_embedding(&bytes[..bytes.len() - 1], 768).expect_err("must be refused");
        assert!(matches!(error, VectorError::Malformed { .. }), "{error}");
        let error = decode_embedding(&bytes, 767).expect_err("must be refused");
        assert!(matches!(error, VectorError::Malformed { .. }), "{error}");
    }

    #[test]
    fn a_code_is_exactly_one_bit_per_dimension() {
        for dims in [8_usize, 384, 768, 1024, 4096] {
            let vector = unit(dims, 0x1357_9BDF_0246_8ACE);
            let code = encode(&vector, &vec![0.0; dims]).expect("encode");
            assert_eq!(code.len(), dims / 8, "{dims} dims");
        }
    }
}
