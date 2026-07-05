fn main() {
    if let Err(e) = svm::run() {
        svm::report_error(&e);
        std::process::exit(1);
    }
}
