//! Embeds the app icon into `kubach.exe` on Windows.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/kubach.rc");
    println!("cargo:rerun-if-changed=assets/icon/kubach.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("assets/icon/kubach.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("failed to embed the app icon");
    }
}
