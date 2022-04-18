use clap::{command, Arg};
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use reqwest::Url;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::default::Default;
use std::iter;
use std::rc::Rc;

struct Node<T> {
    parent: Option<Rc<Node<T>>>,
    value: T,
}

impl<T> Node<T> {
    pub fn new(parent: Option<Rc<Node<T>>>, value: T) -> Self {
        Node { parent, value }
    }

    pub fn path<'a>(&'a self) -> Box<dyn Iterator<Item = &'a T> + 'a> {
        match &self.parent {
            Some(p) => Box::new(p.path().chain(iter::once(&self.value))),
            None => Box::new(iter::empty()),
        }
    }
}

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

    // TODO: replace `VecDeque` with `Vec`,
    // push to the back,
    // and pop from the back.
    let mut xs: VecDeque<Node<Url>> = matches
        .values_of("URI")
        .unwrap()
        .map(|x| Node::new(None, x.parse().unwrap()))
        .collect();
    loop {
        match xs.pop_front() {
            Some(x) => {
                // Making web requests
                // at the speed of a computer
                // can have negative repercussions,
                // like IP banning.
                // TODO: sleep based on time since last request to this domain.
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let dom = html5ever::parse_document(RcDom::default(), Default::default())
                    .from_utf8()
                    .read_from(
                        &mut reqwest::get(x.value.as_ref())
                            .await?
                            .text()
                            .await?
                            .as_bytes(),
                    )
                    .unwrap();
                if inner_text(&dom).contains(phrase) {
                    println!("{}", x.value);
                }
                let rcx = Rc::new(x);
                // We don't need to know if a path cycles back on itself.
                // For us,
                // path cycles waste time and lead to infinite loops.
                let xpath: HashSet<_> = rcx.path().collect();
                links(&rcx.value, &dom)
                    .into_iter()
                    .filter(|u| !xpath.contains(&u))
                    .for_each(|u| xs.push_front(Node::new(Some(rcx.clone()), u)));
            }
            None => return Ok(()),
        }
    }
}

// fn parse_page(body: String) -> (links, lines)

fn inner_text(dom: &RcDom) -> String {
    let mut text = String::new();
    walk_dom(
        &mut |data| {
            match data {
                NodeData::Text { ref contents } => {
                    text.push_str(contents.borrow().to_string().as_str());
                }
                NodeData::Element { ref name, .. } => {
                    // The contents of script tags are invisible,
                    // and shouldn't be searched.
                    if name.local.as_ref() == "script" {
                        return false;
                    }
                }
                _ => {}
            }
            true
        },
        &dom.document,
    );
    text
}

// TODO: deduplicate links,
// maybe return a set.
fn links(origin: &Url, dom: &RcDom) -> Vec<Url> {
    let mut xs = Vec::new();
    walk_dom(
        &mut |data| {
            match data {
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
                            .for_each(|x| xs.push(x));
                    }
                }
                _ => {}
            }
            true
        },
        &dom.document,
    );
    xs
}

fn walk_dom<F>(f: &mut F, handle: &Handle)
where
    F: FnMut(&NodeData) -> bool,
{
    if f(&handle.data) {
        for child in handle.children.borrow().iter() {
            walk_dom(f, child);
        }
    }
}
