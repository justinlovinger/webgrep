use std::collections::HashMap;

pub fn mk_static<T>(x: T) -> &'static T {
    Box::leak(Box::new(x))
}

pub fn line_occurences(buf: &[u8]) -> HashMap<&str, u32> {
    let mut map = HashMap::new();
    for line in std::str::from_utf8(&buf).unwrap().lines() {
        let counter = map.entry(line).or_insert(0);
        *counter += 1;
    }
    map
}
