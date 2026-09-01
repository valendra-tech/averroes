pub struct McpClient {
    pub server_name: String,
    pub endpoint: String,
}

impl McpClient {
    pub fn new(server_name: String, endpoint: String) -> Self {
        Self {
            server_name,
            endpoint,
        }
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<McpToolDef>> {
        Ok(Vec::new())
    }

    pub async fn call_tool(
        &self,
        _name: &str,
        _params: serde_json::Value,
    ) -> anyhow::Result<String> {
        Ok(String::new())
    }
}

pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
