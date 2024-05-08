use katana_rpc_api::ApiKind;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub port: u16,
    pub host: String,
    pub max_connections: u32,
    pub apis: Vec<ApiKind>,
    pub rpc_user: String,
    pub rpc_password: String,
}

impl ServerConfig {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
