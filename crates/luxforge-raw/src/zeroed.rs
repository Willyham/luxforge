//! Large buffers that start as zeros the kernel provides.
//!
//! `Vec::try_reserve_exact` followed by `Vec::resize` writes every value on the calling thread
//! before the pooled pass that fills the buffer touches it: 10.8 to 12.5 ms for 240 MB on the M4,
//! all of it serial. `alloc_zeroed` asks the allocator for pages it already knows are zero, which
//! for a large request are fresh pages from the kernel that no one has touched, so the first write
//! to each page, from whichever pooled worker writes it, is what faults it in (performance rule 2).
//! The content is the same zeros either way.

use crate::RawError;
use std::alloc::{Layout, alloc_zeroed};

mod sealed {
    pub trait Sealed {}
    impl Sealed for f32 {}
    impl Sealed for u16 {}
}

/// A sample type whose all-zero bit pattern is a valid, initialized value and which is not
/// zero-sized. Sealed: the two implementations below are the only ones, so [`zeroed_vec`]'s
/// `SAFETY` argument holds for every type it can be instantiated with.
pub(crate) trait Sample: sealed::Sealed + Copy {}
impl Sample for f32 {}
impl Sample for u16 {}

/// A vector of `len` zeros whose length and capacity are both `len`, or a
/// [`RawError::ResourceLimit`] naming `what` when `len` values cannot be laid out or the allocator
/// refuses them. A zero length allocates nothing.
pub(crate) fn zeroed_vec<T: Sample>(len: usize, what: &'static str) -> Result<Vec<T>, RawError> {
    if len == 0 {
        return Ok(Vec::new());
    }
    // Refused past `isize::MAX` bytes, and on overflow, which `Vec` would otherwise abort on.
    let layout = Layout::array::<T>(len).map_err(|_| RawError::ResourceLimit(what))?;
    // SAFETY: `layout` has a nonzero size: `len` is nonzero and every `Sample` is `f32` or `u16`,
    // so `T` is not zero-sized.
    let pointer = unsafe { alloc_zeroed(layout) }.cast::<T>();
    if pointer.is_null() {
        return Err(RawError::ResourceLimit(what));
    }
    // SAFETY: `pointer` came from the global allocator, which `Vec` also uses, with the layout of
    // `len` values of `T`, which is the layout `Vec` frees a capacity of `len` with, and it is
    // aligned for `T`. The allocation holds `len` values, each of which is zero bytes, a valid
    // initialized `T` for every `Sample`.
    Ok(unsafe { Vec::from_raw_parts(pointer, len, len) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_length_allocates_nothing() {
        let samples = zeroed_vec::<f32>(0, "test").expect("an empty vector");
        assert!(samples.is_empty());
        assert_eq!(samples.capacity(), 0);
        let samples = zeroed_vec::<u16>(0, "test").expect("an empty vector");
        assert!(samples.is_empty());
        assert_eq!(samples.capacity(), 0);
    }

    /// 64 MiB of each type is above the allocator's small-block pools, so the pages are the
    /// kernel's, and every value is zero, as the `resize` it replaces gave.
    #[test]
    fn a_large_length_is_all_zeros_with_that_exact_length_and_capacity() {
        let floats = zeroed_vec::<f32>(16 << 20, "test").expect("64 MiB of floats");
        assert_eq!((floats.len(), floats.capacity()), (16 << 20, 16 << 20));
        assert!(floats.iter().all(|value| value.to_bits() == 0));
        let samples = zeroed_vec::<u16>(32 << 20, "test").expect("64 MiB of samples");
        assert_eq!((samples.len(), samples.capacity()), (32 << 20, 32 << 20));
        assert!(samples.iter().all(|value| *value == 0));
    }

    /// A small length is zeros too, and the vector is an ordinary one: writable, growable and freed
    /// by `Vec`'s own deallocation.
    #[test]
    fn a_small_vector_behaves_as_an_ordinary_one() {
        let mut samples = zeroed_vec::<u16>(5, "test").expect("five samples");
        assert_eq!(samples, [0; 5]);
        samples.copy_from_slice(&[1, 2, 3, 4, 5]);
        samples.extend_from_slice(&[6, 7]);
        assert_eq!(samples, [1, 2, 3, 4, 5, 6, 7]);
        let mut floats = zeroed_vec::<f32>(3, "test").expect("three floats");
        floats.push(1.5);
        assert_eq!(floats, [0.0, 0.0, 0.0, 1.5]);
    }

    /// A length that cannot be laid out, and one that can but no allocator has the memory for, are
    /// the caller's error, with its message, and not an abort.
    #[test]
    fn an_impossible_length_is_a_resource_limit_error() {
        for len in [usize::MAX, usize::MAX / 2, isize::MAX as usize / 2] {
            assert_eq!(
                zeroed_vec::<f32>(len, "the sensor"),
                Err(RawError::ResourceLimit("the sensor")),
                "{len} floats"
            );
            assert_eq!(
                zeroed_vec::<u16>(len, "the sensor"),
                Err(RawError::ResourceLimit("the sensor")),
                "{len} samples"
            );
        }
        // Laid out in under `isize::MAX` bytes, so only the allocator can refuse it.
        let fits_a_layout = isize::MAX as usize / std::mem::size_of::<f32>();
        assert_eq!(
            zeroed_vec::<f32>(fits_a_layout, "the sensor"),
            Err(RawError::ResourceLimit("the sensor"))
        );
    }
}
