# Reticulum Router Daemon

<img src="docs/logo.png" width=256>

A pure, rust-based transport for the Reticulum network based largely on [reticulum-sdk](https://github.com/GhostMeshLabs/reticulum-sdk)

# Limitations

## LXMF

The original Python implementation of rnsd stuffs a bunch of add-on services behind rnsd including LXMF, and NomadNetwork.
[LXMF services](https://github.com/markqvist/lxmf) are not part of this project! This is strictly a Reticulum protocol transport.

Application messages such as LXMF and NomadNetwork will flow over the transport as expected.

## Implemented Transport Destinations

* ✅ rnstransport path.request
* ✅ rnstransport probe (aka respond_to_probes)
* ✅ rnstransport discovery (aka discoverable)
* ❌ rnstransport remote.management (aka enable_remote_management)
* ❌ info blackhole (aka publish_blackhole)

# Configuring

The Reticulum Router Daemon will automatically convert any existing non-standard Python rnsd configurations to standard toml config files.

> Not all interface types are supported yet! Just TCPServerInterface,TCPClientInterface,UDPInterface,RNodeInterface

## Differences from rnsd configuration

* discovery_name
  * Omitted. We just use interface name
* reachable_on
  * We *DO* optionally want a port number, because sometimes things are behind load balancers
  * Does *NOT* accept a local script to execute to get your IP
    * (in the future, we want to detect your external IP if reachable_on is omitted)

## Example Syntax

```toml
[reticulum]
enable_transport = true
share_instance = true
instance_name = "default"
respond_to_probes = true

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
discoverable = true
reachable_on = "cool.server.com:4242"

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
