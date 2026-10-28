fn main() {
    if let Err(err) = mux_server::run() {
        log::error!("{err}");
        eprintln!("mux: {err}");
        std::process::exit(1);
    }
}
