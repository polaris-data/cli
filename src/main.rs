use std::process::ExitCode;

use polaris::app;

#[tokio::main]
async fn main() -> ExitCode {
    app::main_entry().await
}
