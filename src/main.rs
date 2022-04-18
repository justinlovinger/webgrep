use clap::{command, Arg};
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use reqwest::Url;
use std::collections::HashSet;
use std::default::Default;
use std::io;
use std::io::Write;
use std::rc::Rc;

const CLEAR_CODE: &[u8] = b"\r\x1B[K";

pub struct Node<T> {
    parent: Option<Rc<Node<T>>>,
    value: T,
}

pub struct NodePathIterator<'a, T> {
    node: Option<&'a Rc<Node<T>>>,
}

impl<'a, T> Iterator for NodePathIterator<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let x = self.node?;
        self.node = x.parent.as_ref();
        Some(&x.value)
    }
}

impl<T> Node<T> {
    pub fn new(parent: Option<Rc<Node<T>>>, value: T) -> Self {
        Node { parent, value }
    }

    pub fn depth(&self) -> u64 {
        match &self.parent {
            Some(p) => p.depth() + 1,
            None => 0,
        }
    }

    pub fn path_from_root(&self) -> Vec<&T> {
        match &self.parent {
            Some(p) => {
                let mut xs = p.path_from_root();
                xs.push(&self.value);
                xs
            }
            None => Vec::from([&self.value]),
        }
    }
}

pub fn path_to_root<T>(x: &Rc<Node<T>>) -> NodePathIterator<T> {
    NodePathIterator { node: Some(x) }
}

#[tokio::main]
async fn main() -> Result<(), reqwest::Error> {
    let matches = command!()
        .about("Recursively search the web, starting from URI..., for PHRASE")
        .arg(
            Arg::new("phrase")
                .required(true)
                .value_name("PHRASE")
                .help("Phrase to search for"),
        )
        .arg(
            Arg::new("uri")
                .multiple_occurrences(true)
                .required(true)
                .value_name("URI")
                .help("URIs to start search from"),
        )
        .arg(
            Arg::new("depth")
                .short('d')
                .long("max-depth")
                .default_value("1")
                .value_name("NUM")
                .help("Limit search depth to NUM links from starting URI"),
        )
        .get_matches();

    let phrase = matches.value_of("phrase").unwrap();
    let max_depth = matches.value_of("depth").unwrap().parse().unwrap();

    let mut xs: Vec<Node<Url>> = matches
        .values_of("uri")
        .unwrap()
        .map(|x| Node::new(None, x.parse().unwrap()))
        .collect();
    let mut werr = io::BufWriter::new(io::stderr());
    loop {
        match xs.pop() {
            Some(x) => {
                // Search may spend a long time between matches,
                // but we don't want to clutter output
                // with every URL.
                let _ = werr.write_all(CLEAR_CODE);
                let _ = werr.write_all(x.value.as_str().as_bytes());
                let _ = werr.flush();

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
                    let _ = werr.write_all(CLEAR_CODE);
                    let _ = werr.flush();
                    // `map(...).intersperse(" > ")` would be better,
                    // but it is only available in nightly builds
                    // as of 2022-04-18.
                    println!(
                        "{}",
                        x.path_from_root()
                            .iter()
                            .map(|u| u.as_str())
                            .collect::<Vec<_>>()
                            .join(" > ")
                    );
                }

                if x.depth() < max_depth {
                    let rcx = Rc::new(x);
                    // We don't need to know if a path cycles back on itself.
                    // For us,
                    // path cycles waste time and lead to infinite loops.
                    let xpath: HashSet<_> = path_to_root(&rcx).collect();
                    links(&rcx.value, &dom)
                        .into_iter()
                        .filter(|u| !xpath.contains(&u))
                        .for_each(|u| xs.push(Node::new(Some(rcx.clone()), u)));
                }
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

// We only want unique links.
// `HashSet` takes care of this.
fn links(origin: &Url, dom: &RcDom) -> HashSet<Url> {
    let mut xs = HashSet::new();
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
                            .for_each(|x| {
                                xs.insert(x);
                                ()
                            });
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
