use std::process::ExitCode;

fn main() -> ExitCode {
    let code = nessa_lsp::run_cli(std::env::args_os().skip(1));
    ExitCode::from(code.clamp(0, 255) as u8)
}
