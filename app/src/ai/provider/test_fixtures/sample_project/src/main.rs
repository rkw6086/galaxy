use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct Config {
    name: String,
    port: u16,
    debug: bool,
}

impl Config {
    fn from_env() -> Self {
        Self {
            name: std::env::var("APP_NAME").unwrap_or_else(|_| "sample-app".into()),
            port: std::env::var("PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8080),
            debug: std::env::var("DEBUG").map(|v| v == "1").unwrap_or(false),
        }
    }
}

#[tokio::main]
async fn main() {
    let config = Config::from_env();
    println!("Starting {} on port {}", config.name, config.port);
    if config.debug {
        println!("Debug mode enabled");
    }
}
