fn main() {
    // `tauri_build::build()` embeds ../build's *current* contents at compile time but
    // doesn't watch that directory itself, so a `cargo build` after only a frontend change
    // silently reuses the stale embed unless we declare the dependency explicitly here.
    println!("cargo:rerun-if-changed=../build");
    tauri_build::build()
}
