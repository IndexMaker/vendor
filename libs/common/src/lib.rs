#![cfg_attr(all(feature = "stylus", not(feature = "stylus-test")), no_std)]

//#[macro_use]
extern crate alloc;

pub mod amount;
pub mod asset;


pub mod interfaces {
    pub mod banker;
    pub mod castle;
    pub mod abacus;
    pub mod constable;
    pub mod factor;
    pub mod clerk;
    pub mod guildmaster;
    pub mod scribe;
    pub mod treasury;
    pub mod worksman;
}

pub mod labels;
pub mod log;
pub mod math;
pub mod uint;
pub mod vector;
