use reqwest::Client;
use std::sync::Arc;
use std::time::Duration;

pub struct ProviderConnectionPool {
    client: Client,
}

impl ProviderConnectionPool {
    pub fn new() -> Self {
        let client = Client::builder()
            .http2_prior_knowledge()
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(5)
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build HTTP client");

        Self { client }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn client_arc(&self) -> Arc<Client> {
        Arc::new(self.client.clone())
    }
}

impl Default for ProviderConnectionPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creates_client() {
        let pool = ProviderConnectionPool::new();
        let _client = pool.client();
    }
}
