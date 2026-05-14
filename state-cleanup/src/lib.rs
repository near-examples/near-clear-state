use near_sdk::json_types::Base64VecU8;
use near_sdk::{env, near};

#[near(contract_state)]
#[derive(Default)]
pub struct Contract {}

#[near]
impl Contract {
    pub fn clean(keys: Vec<Base64VecU8>) {
        for key in keys.iter() {
            env::storage_remove(&key.0);
        }
    }
}
