#![forbid(unsafe_code)]

//! The pinned UniFFI binding generator, run by `scripts/ios-build.sh`.

fn main() {
    uniffi::uniffi_bindgen_main()
}
