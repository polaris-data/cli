use std::process::ExitCode;

use tick::app;

#[tokio::main]
async fn main() -> ExitCode {
    app::main_entry().await
}
