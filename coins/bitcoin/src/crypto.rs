use lazy_static::lazy_static;

use sha2::{Digest, Sha256};

use k256::{
  elliptic_curve::{
    ops::Reduce,
    sec1::{Tag, ToEncodedPoint},
  },
  U256, Scalar, ProjectivePoint,
};

use bitcoin::XOnlyPublicKey;

use frost::{algorithm::Hram, curve::Secp256k1};

/// Get the x coordinate of a non-infinity, even point. Panics on invalid input.
pub fn x(key: &ProjectivePoint) -> [u8; 32] {
  let encoded = key.to_encoded_point(true);
  assert_eq!(encoded.tag(), Tag::CompressedEvenY);
  (*encoded.x().expect("point at infinity")).into()
}

/// Convert a non-infinite even point to a XOnlyPublicKey. Panics on invalid input.
pub fn x_only(key: &ProjectivePoint) -> XOnlyPublicKey {
  XOnlyPublicKey::from_slice(&x(key)).unwrap()
}

/// Make a point even, returning the even version and the offset required for it to be even.
pub fn make_even(mut key: ProjectivePoint) -> (ProjectivePoint, u64) {
  let mut c = 0;
  while key.to_encoded_point(true).tag() == Tag::CompressedOddY {
    key += ProjectivePoint::GENERATOR;
    c += 1;
  }
  (key, c)
}

/// A BIP-340 compatible HRAm for use with the modular-frost Schnorr Algorithm.
#[derive(Clone, Copy, Debug)]
pub struct BitcoinHram {}

lazy_static! {
  static ref TAG_HASH: [u8; 32] = Sha256::digest(b"BIP0340/challenge").into();
}

#[allow(non_snake_case)]
impl Hram<Secp256k1> for BitcoinHram {
  fn hram(R: &ProjectivePoint, A: &ProjectivePoint, m: &[u8]) -> Scalar {
    let (R, _) = make_even(*R);

    let mut data = Sha256::new();
    data.update(*TAG_HASH);
    data.update(*TAG_HASH);
    data.update(x(&R));
    data.update(x(A));
    data.update(m);

    Scalar::from_uint_reduced(U256::from_be_slice(&data.finalize()))
  }
}
