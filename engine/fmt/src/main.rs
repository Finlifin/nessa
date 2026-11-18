fn main() {
    let code = nessa_fmt::run_cli(std::env::args_os());
    std::process::exit(code);
}
