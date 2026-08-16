fn main() {
    println!("cargo:rerun-if-changed=ui/main.slint");
    println!("cargo:rerun-if-changed=ui/keyboard.slint");
    println!("cargo:rerun-if-changed=ui/key.slint");
    // The brand mark embedded into the UI via @image-url (key.slint).
    println!("cargo:rerun-if-changed=assets/ferrokey.png");
    slint_build::compile("ui/main.slint").expect("failed to compile the Slint UI");
}
