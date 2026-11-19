#![forbid(unsafe_code)]

fn main() {
    // Hooks and agents inherit their harness's I/O policy; an iCloud Vault
    // needs evicted files downloaded on read, not refused.
    agendar_materialize::allow_downloads();
    std::process::exit(agendar_cli::run());
}
