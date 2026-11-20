use aegis_lockfile::{parse_file};
use aegis_net::DiskCache;
use std::collections::HashMap;

fn main() {
    let _c = DiskCache::new();
    let _m: HashMap<u8, u8> = HashMap::new();
    parse_file();
}
