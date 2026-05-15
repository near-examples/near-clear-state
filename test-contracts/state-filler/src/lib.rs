use near_sdk::{near, store::LookupMap};

#[near(contract_state)]
pub struct Contract {
    entries: LookupMap<Vec<u8>, Vec<u8>>,
}

impl Default for Contract {
    fn default() -> Self {
        Self { entries: LookupMap::new(b"e") }
    }
}

#[near]
impl Contract {
    pub fn fill(&mut self, prefix: String, count: u32, value_size: u32) {
        let value = vec![0u8; value_size as usize];
        for i in 0..count {
            let key = format!("{prefix}{i}").into_bytes();
            self.entries.insert(key, value.clone());
        }
    }
}
