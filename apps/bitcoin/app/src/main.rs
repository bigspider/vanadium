#![cfg_attr(feature = "target_vanadium_ledger", no_std, no_main)]

extern crate alloc;

sdk::bootstrap!();

pub fn main() {
    vnd_bitcoin::app_builder().run();
}
