//! The S-08 sidecar binary. Prints exactly one JSON line describing itself,
//! then exits with code 0. The app runs it through `run_sidecar` the same way
//! the product will run `marsad`.

fn main() {
    let info = serde_json::json!({
        "name": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    });
    println!("{info}");
}
