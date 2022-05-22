use clap::Parser;
use regex::{Regex, RegexBuilder};
use reqwest::Url;

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Regex pattern to search for
    #[clap(required = true, value_name = "PATTERN")]
    search_re: Regex,

    /// URIs to start search from
    #[clap(multiple_occurrences = true, required = true, value_name = "URL")]
    urls: Vec<Url>,

    /// Limit search depth to NUM links from starting URL
    #[clap(short = 'd', long, default_value_t = 1, value_name = "NUM")]
    max_depth: u64,

    /// Search case insensitively
    #[clap(short = 'i', long)]
    ignore_case: bool,

    /// Exclude URLs matching regex pattern
    #[clap(long, value_name = "PATTERN")]
    exclude_urls_re: Option<Regex>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    webgrep::run(
        std::io::stdout(),
        indicatif::MultiProgress::new(),
        webgrep::cache::FileCache::new("page-cache")
            .await
            .expect("Failed to initialize cache"),
        // Tokio uses number of CPU cores as default number of worker threads.
        // `tokio::runtime::Handle::current().metrics().num_workers()`
        // is only available in unstable Tokio.
        // A larger buffer isn't necessary faster.
        num_cpus::get(),
        args.exclude_urls_re,
        args.max_depth,
        RegexBuilder::new(args.search_re.as_str())
            .case_insensitive(args.ignore_case)
            .build()
            .unwrap(),
        args.urls,
    )
    .await
}
