use std::sync::Arc;

use crate::config::Config;
use crate::router::Router;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub router: Arc<Router>,
    pub gateway_api_key: Arc<String>,
}

impl AppState {
    pub fn new(config: Arc<Config>, router: Arc<Router>, gateway_api_key: String) -> Self {
        Self {
            config,
            router,
            gateway_api_key: Arc::new(gateway_api_key),
        }
    }
}
