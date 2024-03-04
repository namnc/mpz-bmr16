//! Types for working with binary encoded values.
//!
//! Binary garbled circuit protocols encode each bit of a value using a pair of labels. One label
//! to represent the bit value 0, and another to represent the bit value 1.
//!
//! Most computation one would like to perform does not operate on the bit level, rather it typically
//! involves representations such as integers and byte arrays. This module provides convenient types for
//! working with these representations of values in an encoded form.
//!
//! # Free-XOR
//!
//! The Free-XOR technique stipulates that a [global binary offset](Delta) is used such that the labels for bit
//! value 1 are generated by XORing the label for bit value 0 with the global offset, ie W_1 = W_0 ^ Delta.

mod encoder;
mod equality;
mod ops;
mod value;

mod crt;
mod utils;

use std::{
    ops::{BitXor, Deref, Index},
    sync::Arc,
};

use mpz_core::Block;
use rand::{CryptoRng, Rng};
use serde::{Deserialize, Deserializer, Serialize};

pub use crt::{
    add_label, cmul_label, get_delta_by_modulus, negate_label, state as crt_encoding_state,
    ChaChaCrtEncoder, CrtDecoding, CrtDelta, DecodeError, EncodedCrtValue, LabelModN,
    Labels as CrtLabels,
};
pub(crate) use crt::{tweak, tweak2};

pub use encoder::{ChaChaEncoder, Encoder};
pub use equality::EqualityCheck;
pub use value::{Decoding, Encode, EncodedValue, EncodingCommitment, ValueError};

/// Global binary offset used by the Free-XOR technique to create label
/// pairs where W_1 = W_0 ^ Delta.
///
/// In accordance with the (p&p) Point-and-Permute technique, the LSB of Delta is set to 1, so that
/// the pointer bit LSB(W_1) = LSB(W_0) ^ 1
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Delta(Block);

impl Delta {
    /// Creates new random Delta
    pub fn random<R: Rng + CryptoRng + ?Sized>(rng: &mut R) -> Self {
        let mut block = Block::random(rng);
        block.set_lsb();
        Self(block)
    }

    /// Returns the inner block
    #[inline]
    pub(crate) fn into_inner(self) -> Block {
        self.0
    }
}

impl Deref for Delta {
    type Target = Block;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Module containing the states of an encoded value.
pub mod state {
    use super::*;

    mod sealed {
        pub trait Sealed {}

        impl Sealed for super::Full {}
        impl Sealed for super::Active {}
    }

    /// Marker trait for label state
    pub trait LabelState: sealed::Sealed + Clone {}

    /// Full label state, ie contains both the low and high labels.
    #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
    pub struct Full {
        pub(super) delta: Delta,
    }

    impl LabelState for Full {}

    /// Active label state
    #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
    pub struct Active;

    impl LabelState for Active {}
}

use state::*;

fn deserialize_arc_array<'de, D, T, const N: usize>(deserialize: D) -> Result<Arc<[T; N]>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    serde_arrays::deserialize(deserialize).map(Arc::new)
}

/// A collection of labels.
///
/// This type uses an `Arc` reference to the underlying data to make it cheap to clone,
/// and thus more memory efficient when re-using labels between garbled circuit executions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Labels<const N: usize, S: LabelState> {
    state: S,
    #[serde(
        serialize_with = "serde_arrays::serialize",
        deserialize_with = "deserialize_arc_array"
    )]
    labels: Arc<[Label; N]>,
}

impl<const N: usize, S> Labels<N, S>
where
    S: LabelState,
{
    /// Returns number of labels
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// Returns an iterator over the labels.
    ///
    /// # Note
    ///
    /// When the labels are in the `Full` state, the iterator will return the low labels.
    ///
    /// When the labels are in the `Active` state, the iterator will return the active labels.
    pub fn iter(&self) -> impl Iterator<Item = &Label> {
        self.labels.iter()
    }
}

impl<const N: usize> Labels<N, state::Full> {
    pub(crate) fn new(delta: Delta, labels: [Label; N]) -> Self {
        Self {
            state: state::Full { delta },
            labels: Arc::new(labels),
        }
    }

    pub(crate) fn delta(&self) -> Delta {
        self.state.delta
    }

    pub(crate) fn verify(&self, active: &Labels<N, state::Active>) -> Result<(), ValueError> {
        for (low, active) in self.labels.iter().zip(active.labels.iter()) {
            let high = low ^ self.state.delta;
            if !(active == low || active == &high) {
                return Err(ValueError::InvalidActiveEncoding);
            }
        }

        Ok(())
    }

    pub(crate) fn iter_blocks(&self) -> impl Iterator<Item = [Block; 2]> + '_ {
        self.labels
            .iter()
            .map(|label| [label.0, label.0 ^ *self.delta()])
    }
}

impl<const N: usize> Labels<N, state::Active> {
    pub(crate) fn new(labels: [Label; N]) -> Self {
        Self {
            state: state::Active,
            labels: Arc::new(labels),
        }
    }
}

impl<const N: usize> BitXor for Labels<N, state::Full> {
    type Output = Labels<N, state::Full>;

    fn bitxor(self, rhs: Self) -> Labels<N, state::Full> {
        Labels {
            state: self.state,
            labels: Arc::new(std::array::from_fn(|i| self.labels[i] ^ rhs.labels[i])),
        }
    }
}

impl<const N: usize> BitXor for &Labels<N, state::Full> {
    type Output = Labels<N, state::Full>;

    fn bitxor(self, rhs: Self) -> Labels<N, state::Full> {
        Labels {
            state: self.state,
            labels: Arc::new(std::array::from_fn(|i| self.labels[i] ^ rhs.labels[i])),
        }
    }
}

impl<const N: usize> BitXor<&Self> for Labels<N, state::Full> {
    type Output = Labels<N, state::Full>;

    fn bitxor(self, rhs: &Self) -> Labels<N, state::Full> {
        Labels {
            state: self.state,
            labels: Arc::new(std::array::from_fn(|i| self.labels[i] ^ rhs.labels[i])),
        }
    }
}

impl<const N: usize> BitXor<Labels<N, state::Full>> for &Labels<N, state::Full> {
    type Output = Labels<N, state::Full>;

    fn bitxor(self, rhs: Labels<N, state::Full>) -> Labels<N, state::Full> {
        Labels {
            state: self.state,
            labels: Arc::new(std::array::from_fn(|i| self.labels[i] ^ rhs.labels[i])),
        }
    }
}

impl<const N: usize> BitXor for Labels<N, state::Active> {
    type Output = Labels<N, state::Active>;

    fn bitxor(self, rhs: Self) -> Labels<N, state::Active> {
        Labels {
            state: self.state,
            labels: Arc::new(std::array::from_fn(|i| self.labels[i] ^ rhs.labels[i])),
        }
    }
}

impl<const N: usize> BitXor for &Labels<N, state::Active> {
    type Output = Labels<N, state::Active>;

    fn bitxor(self, rhs: Self) -> Labels<N, state::Active> {
        Labels {
            state: self.state,
            labels: Arc::new(std::array::from_fn(|i| self.labels[i] ^ rhs.labels[i])),
        }
    }
}

impl<const N: usize> BitXor<&Self> for Labels<N, state::Active> {
    type Output = Labels<N, state::Active>;

    fn bitxor(self, rhs: &Self) -> Labels<N, state::Active> {
        Labels {
            state: self.state,
            labels: Arc::new(std::array::from_fn(|i| self.labels[i] ^ rhs.labels[i])),
        }
    }
}

impl<const N: usize> BitXor<Labels<N, state::Active>> for &Labels<N, state::Active> {
    type Output = Labels<N, state::Active>;

    fn bitxor(self, rhs: Labels<N, state::Active>) -> Labels<N, state::Active> {
        Labels {
            state: self.state,
            labels: Arc::new(std::array::from_fn(|i| self.labels[i] ^ rhs.labels[i])),
        }
    }
}

impl<const N: usize, S: LabelState> Index<usize> for Labels<N, S> {
    type Output = Label;

    fn index(&self, index: usize) -> &Self::Output {
        &self.labels[index]
    }
}

/// Encoded bit label.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Label(Block);

impl Label {
    /// Length of a label in bytes
    pub const LEN: usize = Block::LEN;

    /// Creates a new label
    #[inline]
    pub fn new(value: Block) -> Self {
        Self(value)
    }

    /// Returns inner block
    #[inline]
    pub(crate) fn to_inner(self) -> Block {
        self.0
    }

    /// Returns label pointer bit from the Point-and-Permute technique
    #[inline]
    pub(crate) fn pointer_bit(&self) -> bool {
        self.0.lsb() == 1
    }

    /// Creates a new random label
    #[cfg(test)]
    #[inline]
    pub(crate) fn random<R: Rng + CryptoRng + ?Sized>(rng: &mut R) -> Self {
        Self(Block::random(rng))
    }
}

impl BitXor<Label> for Label {
    type Output = Self;

    #[inline]
    fn bitxor(self, rhs: Label) -> Self::Output {
        Label(self.0 ^ rhs.0)
    }
}

impl BitXor<&Label> for Label {
    type Output = Label;

    #[inline]
    fn bitxor(self, rhs: &Label) -> Self::Output {
        Label(self.0 ^ rhs.0)
    }
}

impl BitXor<&Label> for &Label {
    type Output = Label;

    #[inline]
    fn bitxor(self, rhs: &Label) -> Self::Output {
        Label(self.0 ^ rhs.0)
    }
}

impl BitXor<Delta> for Label {
    type Output = Self;

    #[inline]
    fn bitxor(self, rhs: Delta) -> Self::Output {
        Self(self.0 ^ rhs.0)
    }
}

impl BitXor<Delta> for &Label {
    type Output = Label;

    #[inline]
    fn bitxor(self, rhs: Delta) -> Self::Output {
        Label(self.0 ^ rhs.0)
    }
}

impl AsRef<Block> for Label {
    fn as_ref(&self) -> &Block {
        &self.0
    }
}

impl From<Block> for Label {
    fn from(block: Block) -> Self {
        Self(block)
    }
}
