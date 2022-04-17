use clap::{command, Arg};
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use reqwest::Url;
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

    for u_ in matches.values_of("URI").unwrap() {
        let u: Url = u_.parse().unwrap();
        // Making web requests
        // at the speed of a computer
        // can have negative repercussions,
        // like IP banning.
        // TODO: sleep based on time since last request to this domain.
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        let dom = html5ever::parse_document(RcDom::default(), Default::default())
            .from_utf8()
            .read_from(&mut reqwest::get(u.as_ref()).await?.text().await?.as_bytes())
            .unwrap();
        println!(
            "{:?}",
            links(&u, &dom)
                .iter()
                .map(|x| x.as_str())
                .collect::<Vec<_>>()
        );
    }

    Ok(())
}

// fn parse_page(...) -> (links, lines)

// TODO: deduplicate links,
// maybe return a set.
fn links(origin: &Url, dom: &RcDom) -> Vec<Url> {
    fn walk(origin: &Url, links: &mut Vec<Url>, handle: &Handle) {
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
                        .filter_map(|x| origin.join(&x.value).ok())
                        .for_each(|x| links.push(x));
                }
            }
            _ => {}
        }
        for child in handle.children.borrow().iter() {
            walk(origin, links, child);
        }
    }

    let mut xs = Vec::new();
    walk(origin, &mut xs, &dom.document);
    xs
}
