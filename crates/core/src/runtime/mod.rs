pub mod governor;
pub mod pool;

use governor::ResourceGovernor;
use pool::ProviderConnectionPool;
use std::sync::Arc;
use tokio::runtime::Runtime as TokioRuntime;

pub struct RuntimeConfig {
    pub max_concurrent_calls: usize,
    pub token_budget_per_minute: u64,
    pub worker_threads: usize,
    pub enable_rayon: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_concurrent_calls: 10,
            token_budget_per_minute: 200_000,
            worker_threads: num_cpus::get(),
            enable_rayon: true,
        }
    }
}

pub struct Runtime {
    pub tokio: TokioRuntime,
    pub governor: Arc<ResourceGovernor>,
    pub pool: ProviderConnectionPool,
    config: RuntimeConfig,
}

impl Runtime {
    pub fn new(config: RuntimeConfig) -> Self {
        let tokio = TokioRuntime::new().expect("Failed to create Tokio runtime");
        let governor = Arc::new(ResourceGovernor::new(
            config.max_concurrent_calls,
            config.token_budget_per_minute,
        ));
        let pool = ProviderConnectionPool::new();
        Self { tokio, governor, pool, config }
    }

    pub fn config(&self) -> &RuntimeConfig { &self.config }
}
