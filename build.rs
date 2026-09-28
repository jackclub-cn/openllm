use std::path::Path;

fn main() {
    let dist = Path::new("web/dist");
    println!("cargo:rerun-if-changed=web/dist");

    if std::env::var_os("PROFILE").as_deref() == Some(std::ffi::OsStr::new("release"))
        && !dist.join("index.html").exists()
    {
        panic!(
            "web/dist/index.html is missing. Run `npm --prefix web install && npm --prefix web run build` before a release build."
        );
    }
}
