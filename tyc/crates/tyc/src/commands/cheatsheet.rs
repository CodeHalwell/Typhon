//! `tyc cheatsheet` — print the 30-second Typhon cheat sheet to stdout.

use clap::Args;
use miette::Result;

#[derive(Args, Debug)]
pub struct CheatsheetArgs {}

const CHEATSHEET: &str = include_str!("../../../../../docs/cheatsheet.md");

pub fn run(_args: CheatsheetArgs) -> Result<()> {
    print!("{}", CHEATSHEET);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheatsheet_starts_with_typhon_header() {
        assert!(
            !CHEATSHEET.trim().is_empty(),
            "cheatsheet must not be empty"
        );
        assert!(
            CHEATSHEET.contains("Typhon"),
            "cheatsheet must mention Typhon"
        );
    }
}
