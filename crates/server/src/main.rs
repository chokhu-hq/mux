fn main() {
    if let Err(err) = mux_server::run() {
        eprintln!("mux: {err}");
        std::process::exit(1);
    }
}
