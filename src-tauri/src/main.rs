#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(code) = codex_provider_switcher_desktop::credential_cli() {
        std::process::exit(code);
    }
    codex_provider_switcher_desktop::run();
}
