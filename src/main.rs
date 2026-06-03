fn main() {
    if let Err(error) = scam::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
