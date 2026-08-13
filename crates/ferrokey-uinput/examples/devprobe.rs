// Temporary local repro: create+verify+emit through the real kernel path.
use ferrokey_core::PhysicalKey;
use ferrokey_uinput::{DeviceOptions, UinputDevice};

fn main() {
    let mut dev = match UinputDevice::create(DeviceOptions::default()) {
        Ok(d) => d,
        Err(e) => {
            println!("FAILED: {e}");
            return;
        }
    };
    println!("CREATE+VERIFY OK");
    // Emit a key down/up for A (code 30).
    dev.key_down(PhysicalKey::A).unwrap();
    dev.key_up(PhysicalKey::A).unwrap();
    println!("EMIT OK (held={})", dev.held_count());
    let errs = dev.release_all();
    println!("RELEASE_ALL errors: {}", errs.len());
    drop(dev);
    println!("DROPPED");
}
