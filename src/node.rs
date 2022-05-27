use std::sync::Arc;

pub type NodeParent<T> = Option<Arc<Node<T>>>;

#[derive(Debug)]
pub struct Node<T> {
    depth: u64,
    parent: NodeParent<T>,
    value: T,
}

impl<T> Node<T> {
    pub fn new(parent: Option<Arc<Node<T>>>, value: T) -> Self {
        Node {
            depth: parent.as_ref().map_or(0, |p| p.depth + 1),
            parent,
            value,
        }
    }

    pub fn depth(&self) -> u64 {
        self.depth
    }

    pub fn value(&self) -> &T {
        &self.value
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

pub struct NodePathIterator<'a, T> {
    node: Option<&'a Arc<Node<T>>>,
}

impl<'a, T> Iterator for NodePathIterator<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let x = self.node?;
        self.node = x.parent.as_ref();
        Some(&x.value)
    }
}

pub fn path_to_root<T>(x: &Arc<Node<T>>) -> NodePathIterator<T> {
    NodePathIterator { node: Some(x) }
}
