#![cfg_attr(feature = "target_vanadium_ledger", no_std, no_main)]

extern crate alloc;

use sdk::AppBuilder;

sdk::bootstrap!();

pub fn main() {
    AppBuilder::new(
        "Bitcoin",
        env!("CARGO_PKG_VERSION"),
        vnd_bitcoin::process_message,
    )
    .description("Bitcoin is ready")
    .developer("Salvatore Ingala")
    .run();
}
