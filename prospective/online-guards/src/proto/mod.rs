//! Protocol adapters.  Each adapter owns the settlement and recovery witnesses
//! for one wire contract; the generic ledger and pool never inspect bytes.

pub mod pg;
pub mod sky;
