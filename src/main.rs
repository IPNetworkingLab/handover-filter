use clap::Parser;
use tracing::error;

use handover_filter::{Parameters, load, run};

fn main() -> Result<(), ()> {
    tracing_subscriber::fmt::init();

    let args = Parameters::parse();

    if !args.file.exists() {
        error!("Path does not exist.");
        return Err(());
    } else if !args.file.is_file() {
        error!("Path does not point towards a file.");
        return Err(());
    }
    let raw = load(&args)?;

    run(
        &raw,
        &args,
        args.file.parent().and_then(|path| Some(path.to_path_buf())),
    );
    Ok(())
}
