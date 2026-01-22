#![cfg_attr(all(feature = "stylus", not(feature = "stylus-test")), no_std)]

//#[macro_use]
extern crate alloc;

pub mod amount;


pub mod interfaces {
    pub mod banker;
    pub mod castle;
    pub mod constable;
    pub mod factor;
    pub mod clerk;
    pub mod guildmaster;
    pub mod pair_registry;
    pub mod scribe;
    pub mod steward;
    pub mod treasury;
    pub mod worksman;
    pub mod vault;
    pub mod vault_native;
    pub mod vault_native_claims;
    pub mod vault_native_orders;
    pub mod vault_requests;
}

pub mod event_cache;
pub mod labels;
pub mod uint;
pub mod vector;
