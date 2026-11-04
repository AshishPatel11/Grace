//! Puts the app icon (and the name Task Manager shows) inside grace.exe.
//! Without this, Explorer, the Start menu and the taskbar show Windows' default program icon.

fn main() {
    println!("cargo:rerun-if-changed=assets/grace.rc");
    println!("cargo:rerun-if-changed=assets/grace.ico");
    embed_resource::compile("assets/grace.rc", embed_resource::NONE).manifest_optional().unwrap();
}
