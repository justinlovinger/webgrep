use clap::{command, Arg};
use std::io;

fn main() -> io::Result<()> {
    let matches = command!()
        .about("Recursively search the web, starting from URI..., for PHRASE")
        .arg(
            Arg::new("PHRASE")
                .required(true)
                .help("Phrase to search for"),
        )
        .arg(
            Arg::new("URI")
                .multiple_occurrences(true)
                .required(true)
                .help("URIs to start search from"),
        )
        .get_matches();

    println!("{}", matches.value_of("PHRASE").unwrap());
    println!(
        "{:?}",
        matches.values_of("URI").unwrap().collect::<Vec<_>>()
    );

    Ok(())
}
