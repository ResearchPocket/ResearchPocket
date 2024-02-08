pub struct Env {
    pub pocket_consumer_key: Option<String>,
    pub pocket_access_token: Option<String>,
}

impl Env {
    pub fn new() -> Self {
        use std::env::var;
        Self {
            pocket_consumer_key: var("POCKET_CONSUMER_KEY").ok(),
            pocket_access_token: var("POCKET_ACCESS_TOKEN").ok(),
        }
    }
}
