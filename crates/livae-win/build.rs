// Embeds the exe icon (livae.rc) when building for Windows, with windres (GNU) or rc.exe (MSVC).
fn main() {
    println!("cargo:rerun-if-changed=livae.rc");
    println!("cargo:rerun-if-changed=../../assets/livae.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("livae.rc", embed_resource::NONE).manifest_optional().expect("exe icon resource");
    }
}
