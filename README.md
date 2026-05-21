# Reticulum Router Daemon

<img src="docs/logo.png" width=256>

A pure, rust-based router for the Reticulum network based largely on [Reticulum-rs](https://github.com/BeechatNetworkSystemsLtd/Reticulum-rs)

# Limitations

## LXMF

The original Python implementation of rnsd stuffs a bunch of add-on services behind rnsd including LXMF, and NomadNetwork.
[LXMF services](https://github.com/markqvist/lxmf) are not part of this project! This is strictly a Reticulum protocol transport.

Application messages such as LXMF and NomadNetwork will flow over the transport as expected.

## MTU

Resizable MTU's are a part of rnsd, however [not yet implemented](https://github.com/BeechatNetworkSystemsLtd/Reticulum-rs/issues/92) in Reticulum-rs resulting in errors.

We have expanded the maximum MTU supported to improve the situation and reduce communication failures until a
well-rounded adjustable MTU can be implemented in Reticulum-rs.

# Configuring

The Reticulum Router Daemon will automatically convert any existing non-standard Python rnsd configurations to standard toml config files.

> Not all interface types are supported yet! Just TCPServerInterface,TCPClientInterface,UDPInterface

## Example Syntax

```toml
[reticulum]
enable_transport = true
share_instance = true
instance_name = "default"
discover_interfaces = true

[logging]
loglevel = 5

[[interfaces]]
name = "Default Interface"
type = "AutoInterface"
enabled = false

[[interfaces]]
name = "Local"
type = "TCPServerInterface"
enabled = true
bind_host = "0.0.0.0"
bind_port = 4242

[[interfaces]]
name = "GhostMesh 👻 ATX (IPv4,IPv6,LoRA)"
type = "TCPClientInterface"
enabled = true
target_host = "rns.atx.ghostmesh.net"
target_port = 4242

[[interfaces]]
name = "GhostMesh 👻 ATL (IPv6)"
type = "TCPClientInterface"
enabled = true
target_host = "rns.atl.ghostmesh.net"
target_port = 4242
```
