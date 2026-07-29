mod client;

pub mod config;
pub mod crypto;
#[cfg(feature = "desktop")]
pub mod desktop;
#[cfg(feature = "desktop")]
pub mod desktop_config;
#[cfg(feature = "desktop")]
pub mod desktop_controller;
pub mod fake_tls;
pub mod logging;
pub mod mtproto;
pub mod pool;
pub mod proxy;
#[cfg(feature = "desktop")]
pub mod single_instance;
pub mod stats;
pub mod websocket;

pub use config::ProxyConfig;
pub use proxy::Proxy;
