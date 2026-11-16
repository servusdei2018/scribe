//! Binary entry point for the `scribe` CLI.

fn main() -> miette::Result<()> {
    scribe_cli::run()
}
