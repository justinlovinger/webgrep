use clap::{command, Arg};
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::default::Default;

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
        // TODO: sleep between requests,
        // see `tokio::time::delay_for`.
        let dom = html5ever::parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .read_from(&mut reqwest::get(u).await?.text().await?.as_bytes())
            .unwrap();
        println!("{:?}", links(u, &dom));
    }

    Ok(())
}

fn links(origin: &str, dom: &RcDom) -> Vec<String> {
    fn walk(links: &mut Vec<String>, handle: &Handle) -> () {
        match handle.data {
            NodeData::Element {
                ref name,
                ref attrs,
                ..
            } => {
                if name.local.as_ref() == "a" {
                    attrs
                        .borrow()
                        .iter()
                        .filter(|x| x.name.local.as_ref() == "href")
                        .take(1) // An `a` tag shouldn't have more than one `href`
                        .for_each(|x| links.push(x.value.to_string()));
                }
            }
            _ => {}
        }
        for child in handle.children.borrow().iter() {
            walk(links, child);
        }
    }

    let mut xs = Vec::new();
    walk(&mut xs, &dom.document);
    xs
}
