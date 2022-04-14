use clap::{command, Arg};

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
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

    let phrase = matches.value_of("PHRASE").unwrap();

    for u in matches.values_of("URI").unwrap() {
        let body = reqwest::get(u).await?.text().await?;
        println!("{}", body.contains(phrase));
    }

    Ok(())
}
