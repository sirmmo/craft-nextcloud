//! `craft-nextcloud`: Nextcloud for the craft apps. `craft-nextcloud --manifest` prints its
//! `plugin.json`; otherwise it answers the app on standard input and output.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![forbid(unsafe_code)]

fn main() {
    if std::env::args().any(|a| a == "--manifest") {
        println!("{:#}", craft_nextcloud::manifest::manifest());
        return;
    }
    let stdin = std::io::stdin();
    craft_nextcloud::plugin::serve(stdin.lock(), std::io::stdout());
}
