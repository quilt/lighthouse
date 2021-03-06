use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// HTTP REST API Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Enable the REST API server.
    pub enabled: bool,
    /// The IPv4 address the REST API HTTP server will listen on.
    pub listen_address: Ipv4Addr,
    /// The port the REST API HTTP server will listen on.
    pub port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            listen_address: Ipv4Addr::new(127, 0, 0, 1),
            port: 5052,
        }
    }
}
