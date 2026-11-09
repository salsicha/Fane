#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

//! Fane: an unobtrusive, context-aware desktop assistant backed by a local model.

mod agent;
mod cli;
mod config;
mod connectors;
mod core;
mod discovery;
mod evals;
mod index;
mod ipc;
mod llm;
mod mail;
mod platform;
mod prefs;
mod runtime;
mod setup;
mod snapshot;
mod store;
mod suggester;
mod tools;
mod ui;

fn main() {
    #[cfg(windows)]
    if cli::wants_console(&std::env::args().collect::<Vec<_>>()) {
        // A GUI-subsystem exe has no console; borrow the terminal we were started from.
        // SAFETY: plain Win32 call with a constant argument.
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
    }
    // Several dependencies bring rustls; pick one crypto provider for the whole process.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    std::process::exit(cli::main());
}
