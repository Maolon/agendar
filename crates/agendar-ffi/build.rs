#![forbid(unsafe_code)]

fn main() {
    uniffi::generate_scaffolding("src/agendar.udl")
        .expect("UniFFI scaffolding from src/agendar.udl");
}
