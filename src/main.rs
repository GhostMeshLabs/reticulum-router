mod config;

use config::{Config, InterfaceConfig, MetricsConfig};
use rand::rngs::OsRng;
use reticulum_sdk::identity::PrivateIdentity;
use reticulum_sdk::iface::modem73::Modem73Interface;
use reticulum_sdk::iface::rnode::{RNodeConfig, RNodeInterface};
use reticulum_sdk::iface::tcp_client::TcpClient;
use reticulum_sdk::iface::tcp_server::TcpServer;
use reticulum_sdk::iface::udp::UdpInterface;
use reticulum_sdk::transport::{
    DiscoveryInterfaceConfig, Transport, TransportConfig, TransportMetrics,
};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::{interval, timeout};

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
    transport: Arc<Transport>,
    metrics_task: Option<JoinHandle<()>>,
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

        let transport = Arc::new(transport);
        let iface_manager = transport.iface_manager();

        for iface in config.interfaces {
            let enabled = match &iface.config {
                InterfaceConfig::TCPServerInterface { enabled, .. } => *enabled,
                InterfaceConfig::TCPClientInterface { enabled, .. } => *enabled,
                InterfaceConfig::BackboneInterface { enabled, .. } => *enabled,
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
                InterfaceConfig::BackboneInterface {
                    bind_host,
                    bind_port,
                    ..
                } => {
                    let addr = format!("{}:{}", bind_host, bind_port);
                    log::info!(
                        "Enabling interface '{}': Backbone on {}",
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
                        let mut discovery_config = DiscoveryInterfaceConfig::backbone(
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
                    flow_control: _,
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

        let metrics_task = if config.metrics.enabled {
            Some(spawn_metrics_server(config.metrics, transport.clone()).await?)
        } else {
            None
        };

        Ok(Self {
            transport,
            metrics_task,
        })
    }

    async fn run(mut self) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("Reticulum instance running, interfaces initialized");

        signal::ctrl_c().await?;

        log::info!("Shutdown signal received, cleaning up");
        if let Some(metrics_task) = self.metrics_task.take() {
            metrics_task.abort();
        }
        drop(self.transport);

        Ok(())
    }
}

async fn spawn_metrics_server(
    config: MetricsConfig,
    transport: Arc<Transport>,
) -> io::Result<JoinHandle<()>> {
    let addr = format!("{}:{}", config.bind_host, config.bind_port);
    let listener = TcpListener::bind(&addr).await.map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to bind Prometheus metrics endpoint on {addr}: {err}"),
        )
    })?;

    let collection_interval = Duration::from_secs(config.collection_interval_seconds.max(1));
    let collection_timeout = Duration::from_secs(config.collection_timeout_seconds.max(1));
    let request_timeout = Duration::from_secs(config.request_timeout_seconds.max(1));
    let cached_body = Arc::new(RwLock::new(render_prometheus_metrics(
        TransportMetrics::default(),
        None,
    )));

    Ok(tokio::spawn(async move {
        log::info!("Prometheus metrics endpoint listening on http://{addr}/metrics");
        log::info!(
            "Prometheus metrics collection interval: {collection_interval:?}, collection timeout: {collection_timeout:?}, request timeout: {request_timeout:?}"
        );
        let mut collection_tick = interval(collection_interval);

        loop {
            tokio::select! {
                _ = collection_tick.tick() => {
                    match timeout(collection_timeout, transport.metrics()).await {
                        Ok(metrics) => {
                            let mut body = cached_body.write().await;
                            *body = render_prometheus_metrics(metrics, unix_timestamp_seconds());
                        }
                        Err(_) => {
                            log::warn!(
                                "Prometheus metrics collection exceeded {collection_timeout:?}; serving previous snapshot"
                            );
                        }
                    }
                }
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(conn) => conn,
                        Err(err) => {
                            log::warn!("Failed to accept Prometheus metrics connection: {err}");
                            continue;
                        }
                    };

                    let cached_body = cached_body.clone();
                    tokio::spawn(async move {
                        match timeout(request_timeout, handle_metrics_connection(stream, cached_body)).await {
                            Ok(Ok(())) => {}
                            Ok(Err(err)) => {
                                log::debug!("Prometheus metrics connection from {peer} failed: {err}");
                            }
                            Err(_) => {
                                log::debug!("Prometheus metrics connection from {peer} exceeded {request_timeout:?}");
                            }
                        }
                    });
                }
            }
        }
    }))
}

async fn handle_metrics_connection(
    mut stream: TcpStream,
    cached_body: Arc<RwLock<String>>,
) -> io::Result<()> {
    let mut request = [0_u8; 1024];
    let bytes_read = stream.read(&mut request).await?;
    let request_line = std::str::from_utf8(&request[..bytes_read])
        .ok()
        .and_then(|request| request.lines().next())
        .unwrap_or("");

    if !request_line.starts_with("GET /metrics ") && !request_line.starts_with("GET /metrics?") {
        write_http_response(
            &mut stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found\n",
        )
        .await?;
        return Ok(());
    }

    let body = cached_body.read().await.clone();
    write_http_response(
        &mut stream,
        "200 OK",
        "text/plain; version=0.0.4; charset=utf-8",
        &body,
    )
    .await
}

async fn write_http_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}

fn render_prometheus_metrics(
    metrics: TransportMetrics,
    collected_at_seconds: Option<u64>,
) -> String {
    let mut output = String::new();

    output.push_str("# HELP reticulum_transport_path_table_entries Number of entries currently known in the Reticulum transport path table.\n");
    output.push_str("# TYPE reticulum_transport_path_table_entries gauge\n");
    output.push_str(&format!(
        "reticulum_transport_path_table_entries {}\n",
        metrics.path_table_entries
    ));

    output.push_str("# HELP reticulum_transport_interface_rx_queue_length Number of inbound packets queued from interface workers to transport.\n");
    output.push_str("# TYPE reticulum_transport_interface_rx_queue_length gauge\n");
    output.push_str(&format!(
        "reticulum_transport_interface_rx_queue_length {}\n",
        metrics.interface_queues.rx
    ));

    output.push_str("# HELP reticulum_transport_interface_tx_queue_length Number of outbound packets queued for an interface worker.\n");
    output.push_str("# TYPE reticulum_transport_interface_tx_queue_length gauge\n");
    output.push_str("# HELP reticulum_transport_interface_announce_queue_length Number of forwarded announces waiting in an interface announce pacer.\n");
    output.push_str("# TYPE reticulum_transport_interface_announce_queue_length gauge\n");

    for iface in metrics.interface_queues.interfaces {
        let interface = iface.address.to_hex_string();
        output.push_str(&format!(
            "reticulum_transport_interface_tx_queue_length{{interface=\"{}\"}} {}\n",
            interface, iface.tx
        ));
        output.push_str(&format!(
            "reticulum_transport_interface_announce_queue_length{{interface=\"{}\"}} {}\n",
            interface, iface.announce
        ));
    }

    output.push_str("# HELP reticulum_transport_metrics_last_collection_timestamp_seconds Unix timestamp of the last successful transport metrics collection.\n");
    output.push_str("# TYPE reticulum_transport_metrics_last_collection_timestamp_seconds gauge\n");
    if let Some(collected_at_seconds) = collected_at_seconds {
        output.push_str(&format!(
            "reticulum_transport_metrics_last_collection_timestamp_seconds {}\n",
            collected_at_seconds
        ));
    }

    output
}

fn unix_timestamp_seconds() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
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
