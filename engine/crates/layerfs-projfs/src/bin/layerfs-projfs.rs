//! Command line over the prototype's one entry point. Windows only.
#[cfg(windows)]
fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(layerfs_projfs::cli::run(&arguments));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("layerfs-projfs runs on Windows only: it mounts through ProjFS");
    std::process::exit(2);
}
