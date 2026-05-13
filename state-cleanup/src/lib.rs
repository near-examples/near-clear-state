use base64::{Engine as _, engine::general_purpose::STANDARD};
use near_sdk::serde::Deserialize;
use near_sdk::{env, serde_json};

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct Args {
    pub keys: Vec<String>,
}

#[unsafe(no_mangle)]
pub extern "C" fn clean() {
    env::setup_panic_hook();
    let input = env::input().unwrap();
    let args: Args = serde_json::from_slice(&input).unwrap();
    for key in args.keys.iter() {
        env::storage_remove(&STANDARD.decode(key).unwrap());
    }
}
