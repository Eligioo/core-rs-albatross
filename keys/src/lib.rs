#[macro_use]
extern crate beserial_derive;
extern crate nimiq_hash as hash;
extern crate nimiq_macros as macros;
extern crate nimiq_utils as utils;

pub use utils::key_rng::{SecureGenerate, SecureRng};

pub use self::address::*;
pub use self::errors::*;
pub use self::key_pair::*;
pub use self::private_key::*;
pub use self::public_key::*;
pub use self::signature::*;

#[macro_export]
macro_rules! implement_simple_add_sum_traits {
    ($name: ident, $identity: expr) => {
        impl<'a, 'b> Add<&'b $name> for &'a $name {
            type Output = $name;
            fn add(self, other: &'b $name) -> $name {
                $name(self.0 + other.0)
            }
        }
        impl<'b> Add<&'b $name> for $name {
            type Output = $name;
            fn add(self, rhs: &'b $name) -> $name {
                &self + rhs
            }
        }

        impl<'a> Add<$name> for &'a $name {
            type Output = $name;
            fn add(self, rhs: $name) -> $name {
                self + &rhs
            }
        }

        impl Add<$name> for $name {
            type Output = $name;
            fn add(self, rhs: $name) -> $name {
                &self + &rhs
            }
        }

        impl<T> Sum<T> for $name
        where
            T: Borrow<$name>,
        {
            fn sum<I>(iter: I) -> Self
            where
                I: Iterator<Item = T>,
            {
                $name(iter.fold($identity, |acc, item| acc + item.borrow().0))
            }
        }
    };
}

pub mod multisig;

mod address;
mod errors;
mod key_pair;
mod private_key;
mod public_key;
mod signature;
