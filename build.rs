//! Embeds the app icon in the exe as resource 1, where GPUI loads the window
//! and taskbar icon from. Dev builds get den's amber icon, so they are easy to
//! tell apart from the installed app.

fn main() {
    println!("cargo:rerun-if-changed=assets/icons");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = if std::env::var("PROFILE").as_deref() == Ok("debug") { "app-dev.ico" } else { "app.ico" };
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR").replace('\\', "/");
    let rc = out.join("app.rc");
    std::fs::write(&rc, format!("1 ICON \"{manifest}/assets/icons/{icon}\"\n")).expect("write app.rc");
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().expect("embed the app icon");
}
