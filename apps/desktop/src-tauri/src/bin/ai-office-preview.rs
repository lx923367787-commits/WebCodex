//! One bounded JSON request on stdin; never creates a Tauri window.
use std::io::{Read, Write};
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 || args[0] != "--data-dir" {
        eprintln!("Usage: ai-office-preview --data-dir ISOLATED_DIRECTORY");
        std::process::exit(2);
    }
    let path = PathBuf::from(&args[1]);
    let mut input = Vec::new();
    if std::io::stdin()
        .take(65537)
        .read_to_end(&mut input)
        .is_err()
        || input.len() > 65536
    {
        std::process::exit(2);
    }
    let request = match serde_json::from_slice(&input) {
        Ok(request) => request,
        Err(_) => {
            std::process::exit(2);
        }
    };
    let response = webcodex_desktop_lib::ai_office_preview_request(&path, request).await;
    let _ = std::io::stdout().write_all(serde_json::to_string(&response).unwrap().as_bytes());
}
