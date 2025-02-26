use core::ops::Deref;
use std::collections::HashMap;

use rand_core::{RngCore, CryptoRng};

use group::ff::Field;

use ciphersuite::Ciphersuite;

use crate::{ThresholdCore, ThresholdKeys, lagrange};

/// FROST generation test.
pub mod frost;
use frost::frost_gen;

// Promotion test.
mod promote;
use promote::test_generator_promotion;

/// Constant amount of participants to use when testing.
pub const PARTICIPANTS: u16 = 5;
/// Constant threshold of participants to use when signing.
pub const THRESHOLD: u16 = ((PARTICIPANTS / 3) * 2) + 1;

/// Clone a map without a specific value.
pub fn clone_without<K: Clone + std::cmp::Eq + std::hash::Hash, V: Clone>(
  map: &HashMap<K, V>,
  without: &K,
) -> HashMap<K, V> {
  let mut res = map.clone();
  res.remove(without).unwrap();
  res
}

/// Recover the secret from a collection of keys.
pub fn recover_key<C: Ciphersuite>(keys: &HashMap<u16, ThresholdKeys<C>>) -> C::F {
  let first = keys.values().next().expect("no keys provided");
  assert!(keys.len() >= first.params().t().into(), "not enough keys provided");
  let included = keys.keys().cloned().collect::<Vec<_>>();

  let group_private = keys.iter().fold(C::F::zero(), |accum, (i, keys)| {
    accum + (lagrange::<C::F>(*i, &included) * keys.secret_share().deref())
  });
  assert_eq!(C::generator() * group_private, first.group_key(), "failed to recover keys");
  group_private
}

/// Generate threshold keys for tests.
pub fn key_gen<R: RngCore + CryptoRng, C: Ciphersuite>(
  rng: &mut R,
) -> HashMap<u16, ThresholdKeys<C>> {
  let res = frost_gen(rng)
    .drain()
    .map(|(i, core)| {
      assert_eq!(
        &ThresholdCore::<C>::deserialize::<&[u8]>(&mut core.serialize().as_ref()).unwrap(),
        &core
      );
      (i, ThresholdKeys::new(core))
    })
    .collect();
  assert_eq!(C::generator() * recover_key(&res), res[&1].group_key());
  res
}

/// Run the test suite on a ciphersuite.
pub fn test_ciphersuite<R: RngCore + CryptoRng, C: Ciphersuite>(rng: &mut R) {
  key_gen::<_, C>(rng);
  test_generator_promotion::<_, C>(rng);
}

#[test]
fn test_with_ristretto() {
  test_ciphersuite::<_, ciphersuite::Ristretto>(&mut rand_core::OsRng);
}
