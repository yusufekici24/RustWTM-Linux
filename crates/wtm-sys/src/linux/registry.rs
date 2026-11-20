//! There is no registry on Linux; reads find nothing.

pub enum Value {
    Text(String),
    Dword(u32),
}

pub fn value(_key: usize, _name: &str) -> Option<Value> {
    None
}
