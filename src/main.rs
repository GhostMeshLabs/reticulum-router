mod config;

use config::{Config, InterfaceConfig};
use rand::rngs::OsRng;
use reticulum_sdk::identity::PrivateIdentity;
use reticulum_sdk::iface::rnode::{RNodeConfig, RNodeInterface};
use reticulum_sdk::iface::tcp_client::TcpClient;
use reticulum_sdk::iface::tcp_server::TcpServer;
use reticulum_sdk::iface::modem73::Modem73Interface;
use reticulum_sdk::iface::udp::UdpInterface;
use reticulum_sdk::transport::{DiscoveryInterfaceConfig, Transport, TransportConfig};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tokio::signal;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const IDENTITY_FILE_NAME: &str = "identity";

fn split_host_port(input: &str) -> Result<(String, u16), String> {
    if input.is_empty() {
        return Err("empty address".into());
    }

    // Handles:
    // - 127.0.0.1:443
    // - [2001:db8::1]:443
    if let Ok(addr) = input.parse::<SocketAddr>() {
        return Ok((addr.ip().to_string(), addr.port()));
    }

    // Handles:
    // - google.com:443
    // - localhost:8080
    if let Some((host, port_str)) = input.rsplit_once(':') {
        if host.is_empty() {
            return Err("missing host".into());
        }

        let port = port_str
            .parse::<u16>()
            .map_err(|_| "invalid port".to_string())?;

        return Ok((host.to_string(), port));
    }

    Err("missing port".into())
}

struct Daemon {
    transport: Transport,
}

impl Daemon {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (config, config_path) = Config::load()?;

        env_logger::Builder::from_env(
            env_logger::Env::default().default_filter_or(config.log_filter()),
        )
        .init();

        log::info!("Reticulum daemon starting");
        log::info!("Configuration loaded from: {}", config_path.display());

        let identity = load_or_create_identity(&config_path)?;
        let transport = Transport::new({
            let mut cfg = TransportConfig::new(
                "reticulum-router",
                &identity,
                config.reticulum.enable_transport,
            );

            // RPC for local instance sharing
            cfg.set_share_instance(config.reticulum.share_instance);
            match config.reticulum.rpc_key {
                Some(key) => {
                    log::trace!("Loading RPC key securing shared instance.");
                    cfg.set_rpc_key_hex(&key)?;
                }
                None => {}
            }

            // Transport
            cfg.set_retransmit(config.reticulum.enable_transport);

            // Destinations
            cfg.set_respond_to_probes(config.reticulum.respond_to_probes);
            cfg
        });

        let iface_manager = transport.iface_manager();

        for iface in config.interfaces {
            let enabled = match &iface.config {
                InterfaceConfig::TCPServerInterface { enabled, .. } => *enabled,
                InterfaceConfig::TCPClientInterface { enabled, .. } => *enabled,
                InterfaceConfig::UDPInterface { enabled, .. } => *enabled,
                InterfaceConfig::AutoInterface { enabled, .. } => *enabled,
                InterfaceConfig::I2PInterface { enabled, .. } => *enabled,
                InterfaceConfig::RNodeInterface { enabled, .. } => *enabled,
                InterfaceConfig::BLEInterface { enabled, .. } => *enabled,
                InterfaceConfig::KISSInterface { enabled, .. } => *enabled,
                InterfaceConfig::AX25KISSInterface { enabled, .. } => *enabled,
                InterfaceConfig::Modem73Interface { enabled, .. } => *enabled,
                InterfaceConfig::Unsupported => false,
            };

            if !enabled {
                continue;
            }

            match iface.config {
                InterfaceConfig::TCPServerInterface {
                    bind_host,
                    bind_port,
                    ..
                } => {
                    let addr = format!("{}:{}", bind_host, bind_port);
                    log::info!(
                        "Enabling interface '{}': TCP Server on {}",
                        iface.name,
                        addr
                    );
                    let iface_addr = iface_manager.lock().await.spawn(
                        TcpServer::new(addr, iface_manager.clone()),
                        TcpServer::spawn,
                    );
                    if iface.discoverable {
                        // XXX: If reachable_on is None, we should check external IP somehow
                        let (reachable_host, reachable_port) = match iface.reachable_on {
                            Some(addr) => split_host_port(&addr)?,
                            None => (bind_host, bind_port),
                        };
                        let mut discovery_config = DiscoveryInterfaceConfig::tcp_server(
                            iface.name,
                            reachable_host,
                            reachable_port,
                        );
                        discovery_config.height = iface.height;
                        discovery_config.latitude = iface.latitude;
                        discovery_config.longitude = iface.longitude;
                        transport
                            .register_discoverable_interface(iface_addr, discovery_config)
                            .await;
                    }
                }
                InterfaceConfig::TCPClientInterface {
                    target_host,
                    target_port,
                    ..
                } => {
                    let addr = format!("{}:{}", target_host, target_port);
                    log::info!(
                        "Enabling interface '{}': TCP Client to {}",
                        iface.name,
                        addr
                    );
                    iface_manager
                        .lock()
                        .await
                        .spawn(TcpClient::new(addr), TcpClient::spawn);
                }
                InterfaceConfig::UDPInterface {
                    listen_ip,
                    listen_port,
                    forward_ip,
                    forward_port,
                    ..
                } => {
                    let bind_addr = format!("{}:{}", listen_ip, listen_port);
                    let forward_addr = format!("{}:{}", forward_ip, forward_port);
                    log::info!(
                        "Enabling interface '{}': UDP {}→{}",
                        iface.name,
                        bind_addr,
                        forward_addr
                    );
                    iface_manager.lock().await.spawn(
                        UdpInterface::new(bind_addr, Some(forward_addr)),
                        UdpInterface::spawn,
                    );
                }

                InterfaceConfig::RNodeInterface {
                    port,
                    frequency,
                    bandwidth,
                    txpower,
                    spreadingfactor,
                    codingrate,
                    flow_control,
                    ..
                } => {
                    log::info!(
                        "Enabling interface '{}': RNode {} ({},{},{},{})",
                        iface.name,
                        port,
                        frequency,
                        bandwidth,
                        spreadingfactor,
                        codingrate
                    );
                    let rnode_config = RNodeConfig::new(
                        port,
                        frequency,
                        bandwidth,
                        txpower,
                        spreadingfactor,
                        codingrate,
                    );
                    //rnode_config.with_flow_control(flow_control);
                    rnode_config.validate()?;
                    iface_manager
                        .lock()
                        .await
                        .spawn(RNodeInterface::new(rnode_config), RNodeInterface::spawn);
                }
                InterfaceConfig::Modem73Interface {
                    target_host,
                    target_port,
                    control_host,
                    control_port,
                    ..
                } => {
                    let target_addr = format!("{}:{}", target_host, target_port);
                    let control_addr = format!("{}:{}", control_host, control_port);
                    log::info!(
                        "Enabling interface '{}': Modem73 {}/{}",
                        iface.name,
                        target_addr,
                        control_addr
                    );
                    iface_manager.lock().await.spawn(
                        Modem73Interface::new(target_addr, control_addr),
                        Modem73Interface::spawn,
                    );
                }
                InterfaceConfig::AutoInterface { .. } => {
                    log::warn!(
                        "Interface '{}' type 'AutoInterface' is not yet supported",
                        iface.name
                    );
                }
                InterfaceConfig::I2PInterface { .. } => {
                    log::warn!(
                        "Interface '{}' type 'I2PInterface' is not yet supported",
                        iface.name
                    );
                }
                InterfaceConfig::BLEInterface { .. } => {
                    log::warn!(
                        "Interface '{}' type 'BLEInterface' is not yet supported",
                        iface.name
                    );
                }
                InterfaceConfig::KISSInterface { .. } => {
                    log::warn!(
                        "Interface '{}' type 'KISSInterface' is not yet supported",
                        iface.name
                    );
                }
                InterfaceConfig::AX25KISSInterface { .. } => {
                    log::warn!(
                        "Interface '{}' type 'AX25KISSInterface' is not yet supported",
                        iface.name
                    );
                }
                InterfaceConfig::Unsupported => {
                    log::warn!("Interface '{}' uses an unsupported type", iface.name);
                }
            }
        }

        Ok(Self { transport })
    }

    async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("Reticulum instance running, interfaces initialized");

        signal::ctrl_c().await?;

        log::info!("Shutdown signal received, cleaning up");
        drop(self.transport);

        Ok(())
    }
}

fn load_or_create_identity(
    config_path: &Path,
) -> Result<PrivateIdentity, Box<dyn std::error::Error>> {
    let identity_path = identity_path(config_path);
    if identity_path.exists() {
        let identity_hex = fs::read_to_string(&identity_path)?;
        let identity =
            PrivateIdentity::new_from_hex_string(identity_hex.trim()).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "failed to parse identity at {}: {err:?}",
                        identity_path.display()
                    ),
                )
            })?;
        log::info!(
            "Loaded Reticulum identity {} from {}",
            identity.address_hash(),
            identity_path.display()
        );
        return Ok(identity);
    }

    let identity = PrivateIdentity::new_from_rand(OsRng);
    save_identity(&identity_path, &identity)?;
    log::info!(
        "Generated new Reticulum identity {} at {}",
        identity.address_hash(),
        identity_path.display()
    );

    Ok(identity)
}

fn identity_path(config_path: &Path) -> PathBuf {
    config_path.join(IDENTITY_FILE_NAME)
}

fn save_identity(path: &Path, identity: &PrivateIdentity) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let identity_hex = format!("{}\n", identity.to_hex_string());

    #[cfg(unix)]
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(identity_hex.as_bytes())?;
        file.sync_all()?;
    }

    #[cfg(not(unix))]
    {
        fs::write(path, identity_hex)?;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let daemon = Daemon::new().await?;
    daemon.run().await
}

#[cfg(test)]
mod tests {
    use super::{identity_path, load_or_create_identity};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn creates_and_reuses_identity_file() {
        let config_dir = unique_test_dir();
        fs::create_dir_all(&config_dir).expect("config dir");

        let first_identity = load_or_create_identity(&config_dir).expect("generated identity");
        let saved_identity =
            fs::read_to_string(identity_path(&config_dir)).expect("saved identity");

        assert_eq!(saved_identity.trim(), first_identity.to_hex_string());

        let second_identity = load_or_create_identity(&config_dir).expect("loaded identity");
        assert_eq!(
            second_identity.to_hex_string(),
            first_identity.to_hex_string()
        );

        fs::remove_dir_all(&config_dir).expect("cleanup");
    }

    fn unique_test_dir() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("valid timestamp")
            .as_nanos();

        std::env::temp_dir().join(format!(
            "reticulum-router-identity-test-{}-{}",
            std::process::id(),
            timestamp
        ))
    }
}
