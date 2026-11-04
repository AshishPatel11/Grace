//! Puts the app icon (and the name Task Manager shows) inside pulseterm.exe.
//! Without this, Explorer, the Start menu and the taskbar show Windows' default program icon.

fn main() {
    println!("cargo:rerun-if-changed=assets/pulseterm.rc");
    println!("cargo:rerun-if-changed=assets/pulseterm.ico");
    embed_resource::compile("assets/pulseterm.rc", embed_resource::NONE).manifest_optional().unwrap();
}
