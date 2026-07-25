use std::collections::HashMap;

pub struct Local;

impl Local {
    pub fn new() -> Self {
        Local
    }
}

pub fn run() -> u8 {
    // owner match: a `new` defined on `Client`
    let _c = Client::new();
    // module path: only the name identifies it
    let n = store::helper(1);
    // a type we don't define — must resolve to nothing, not to some other `new`
    let _m: HashMap<u8, u8> = HashMap::new();
    // our own type's `new` wins over the same-named one next door
    let _l = Local::new();
    n
}
