/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Settings with defaults on read ([model]), the JSON key/value file ([store]) and the keychain ([secrets]).

pub mod model;
pub mod secrets;
pub mod store;

pub use model::*;
pub use secrets::{SecretError, SecretStore};
pub use store::{Store, Value, Values};
