fn main() {
    println!("cargo:rerun-if-changed=ui/main.slint");
    println!("cargo:rerun-if-changed=ui/keyboard.slint");
    println!("cargo:rerun-if-changed=ui/key.slint");
    slint_build::compile("ui/main.slint").expect("failed to compile the Slint UI");
}
