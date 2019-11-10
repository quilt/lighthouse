use clap::ArgMatches;
use network::NetworkConfig;
use serde_derive::{Deserialize, Serialize};
use slog::{info, o, Drain};
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::Mutex;

/// The number initial validators when starting the `Minimal`.
const TESTNET_SPEC_CONSTANTS: &str = "minimal";

/// Defines how the client should find the genesis `BeaconState`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientGenesis {
    /// Reads the genesis state and other persisted data from the `Store`.
    Resume,
    /// Creates a genesis state as per the 2019 Canada interop specifications.
    Interop {
        validator_count: usize,
        genesis_time: u64,
    },
    /// Connects to an eth1 node and waits until it can create the genesis state from the deposit
    /// contract.
    DepositContract,
    /// Loads the genesis state from a SSZ-encoded `BeaconState` file.
    SszFile { path: PathBuf },
    /// Connects to another Lighthouse instance and reads the genesis state and other data via the
    /// HTTP API.
    RemoteNode { server: String, port: Option<u16> },
}

impl Default for ClientGenesis {
    fn default() -> Self {
        Self::DepositContract
    }
}

/// The core configuration of a Lighthouse beacon node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub data_dir: PathBuf,
    pub db_type: String,
    db_name: String,
    pub log_file: PathBuf,
    pub spec_constants: String,
    /// If true, the node will use co-ordinated junk for eth1 values.
    ///
    /// This is the method used for the 2019 client interop in Canada.
    pub dummy_eth1_backend: bool,
    pub sync_eth1_chain: bool,
    #[serde(skip)]
    /// The `genesis` field is not serialized or deserialized by `serde` to ensure it is defined
    /// via the CLI at runtime, instead of from a configuration file saved to disk.
    pub genesis: ClientGenesis,
    pub network: network::NetworkConfig,
    pub rpc: rpc::Config,
    pub rest_api: rest_api::Config,
    pub websocket_server: websocket_server::Config,
    pub eth1: eth1::Config,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from(".lighthouse"),
            log_file: PathBuf::from(""),
            db_type: "disk".to_string(),
            db_name: "chain_db".to_string(),
            genesis: <_>::default(),
            network: NetworkConfig::new(),
            rpc: <_>::default(),
            rest_api: <_>::default(),
            websocket_server: <_>::default(),
            spec_constants: TESTNET_SPEC_CONSTANTS.into(),
            dummy_eth1_backend: false,
            sync_eth1_chain: false,
            eth1: <_>::default(),
        }
    }
}

impl Config {
    /// Returns the path to which the client may initialize an on-disk database.
    pub fn db_path(&self) -> Option<PathBuf> {
        self.data_dir()
            .and_then(|path| Some(path.join(&self.db_name)))
    }

    /// Returns the core path for the client.
    ///
    /// Creates the directory if it does not exist.
    pub fn data_dir(&self) -> Option<PathBuf> {
        let path = dirs::home_dir()?.join(&self.data_dir);
        fs::create_dir_all(&path).ok()?;
        Some(path)
    }

    // Update the logger to output in JSON to specified file
    fn update_logger(&mut self, log: &mut slog::Logger) -> Result<(), &'static str> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.log_file);

        if file.is_err() {
            return Err("Cannot open log file");
        }
        let file = file.unwrap();

        if let Some(file) = self.log_file.to_str() {
            info!(
                *log,
                "Log file specified, output will now be written to {} in json.", file
            );
        } else {
            info!(
                *log,
                "Log file specified output will now be written in json"
            );
        }

        let drain = Mutex::new(slog_json::Json::default(file)).fuse();
        let drain = slog_async::Async::new(drain).build().fuse();
        *log = slog::Logger::root(drain, o!());

        Ok(())
    }

    /// Apply the following arguments to `self`, replacing values if they are specified in `args`.
    ///
    /// Returns an error if arguments are obviously invalid. May succeed even if some values are
    /// invalid.
    pub fn apply_cli_args(
        &mut self,
        args: &ArgMatches,
        log: &mut slog::Logger,
    ) -> Result<(), String> {
        if let Some(dir) = args.value_of("datadir") {
            self.data_dir = PathBuf::from(dir);
        };

        if let Some(dir) = args.value_of("db") {
            self.db_type = dir.to_string();
        };

        self.network.apply_cli_args(args)?;
        self.rpc.apply_cli_args(args)?;
        self.rest_api.apply_cli_args(args)?;
        self.websocket_server.apply_cli_args(args)?;

        if let Some(log_file) = args.value_of("logfile") {
            self.log_file = PathBuf::from(log_file);
            self.update_logger(log)?;
        };

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml;

    #[test]
    fn serde_serialize() {
        let _ = toml::to_string(&Config::default()).expect("Should serde encode default config");
    }
}
