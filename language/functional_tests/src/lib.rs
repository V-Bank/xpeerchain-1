// Copyright (c) The XPeer Core Contributors
// SPDX-License-Identifier: Apache-2.0

#![feature(repeat_generic_slice)]
#![feature(slice_concat_ext)]

#[macro_use]
extern crate lazy_static;

pub mod checker;
pub mod config;
pub mod errors;
pub mod evaluator;
pub mod utils;

#[cfg(test)]
pub mod tests;
