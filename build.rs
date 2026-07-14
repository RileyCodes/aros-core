fn main() {
    // UniFFI with proc macros — no UDL file needed
    uniffi::generate_scaffolding("src/uniffi_interface.udl").unwrap();
}
