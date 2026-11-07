//! The client's TUN device, and pointing the system's routes and DNS at it.
//!
//! Nearly all of this leans on the system's own tools (`ip`, `ifconfig`, `route`, `resolvectl`,
//! `scutil`), so `saki -v client` shows exactly what's being changed.

#[cfg(target_os = "linux")]
use crate::resolv_conf::{self, ResolvConf};
use anyhow::{Context, Result, bail};
use saki_core::proto::Welcome;
use std::net::Ipv4Addr;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Command;
use tracing::{debug, info, warn};
use tun::AbstractDevice;

/// IPv4 and IPv6 routes that together cover everything. They're more specific than a default
/// route, so they win without replacing it, and it's untouched when the device goes away.
const ROUTES_V4: [&str; 2] = ["0.0.0.0/1", "128.0.0.0/1"];
const ROUTES_V6: [&str; 2] = ["::/1", "8000::/1"];

pub struct Tunnel {
    name: String,
    dns_configured: bool,
    #[cfg(target_os = "linux")]
    resolv_conf: Option<ResolvConf>,
}

impl Tunnel {
    /// Creates the device, or attaches to one that's already there, and configures it.
    pub fn open(name: Option<&str>, welcome: &Welcome, routes: bool, dns: bool) -> Result<(Self, tun::AsyncDevice)> {
        let mut config = tun::Configuration::default();
        config
            .address(welcome.client_v4)
            .netmask(netmask(welcome.prefix_v4))
            .destination(welcome.gateway_v4)
            .mtu(welcome.mtu)
            .up();
        if let Some(name) = name.or(default_name()) {
            config.tun_name(name);
        }
        // Routes are set up below, the same way on each platform.
        #[cfg(target_os = "macos")]
        config.platform_config(|platform| {
            platform.enable_routing(false);
        });
        let device = tun::create_as_async(&config).map_err(|error| {
            anyhow::anyhow!("couldn't open the TUN device: {error}. `saki client` needs root, so try it with sudo")
        })?;
        let name = device.tun_name().context("couldn't get the TUN device's name")?;

        let mut tunnel = Tunnel {
            name,
            dns_configured: false,
            #[cfg(target_os = "linux")]
            resolv_conf: None,
        };
        tunnel.add_ipv6_address(welcome);
        if routes {
            tunnel.add_routes()?;
            info!(device = tunnel.name, "this machine's traffic now goes through the tunnel");
        }
        if dns {
            tunnel.dns_configured = tunnel.set_dns(welcome.gateway_v4);
        }
        Ok((tunnel, device))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    #[cfg(target_os = "linux")]
    fn add_ipv6_address(&self, welcome: &Welcome) {
        let address = format!("{}/{}", welcome.client_v6, welcome.prefix_v6);
        // Not every system has IPv6 enabled. IPv4 still works without it.
        if let Err(error) = run("ip", &["-6", "addr", "add", &address, "dev", &self.name]) {
            debug!("no IPv6 on the tunnel: {error:#}");
        }
    }

    #[cfg(target_os = "macos")]
    fn add_ipv6_address(&self, welcome: &Welcome) {
        let address = welcome.client_v6.to_string();
        let prefix = welcome.prefix_v6.to_string();
        if let Err(error) = run("ifconfig", &[&self.name, "inet6", &address, "prefixlen", &prefix]) {
            debug!("no IPv6 on the tunnel: {error:#}");
        }
    }

    #[cfg(target_os = "linux")]
    fn add_routes(&self) -> Result<()> {
        for route in ROUTES_V4 {
            run("ip", &["route", "replace", route, "dev", &self.name])?;
        }
        for route in ROUTES_V6 {
            // Without these, IPv6 would leak out of the normal connection on dual-stack networks.
            if let Err(error) = run("ip", &["-6", "route", "replace", route, "dev", &self.name]) {
                debug!("no IPv6 route: {error:#}");
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn add_routes(&self) -> Result<()> {
        for route in ROUTES_V4 {
            run("route", &["-q", "-n", "add", "-inet", route, "-interface", &self.name])?;
        }
        for route in ROUTES_V6 {
            if let Err(error) = run("route", &["-q", "-n", "add", "-inet6", route, "-interface", &self.name]) {
                debug!("no IPv6 route: {error:#}");
            }
        }
        Ok(())
    }

    /// Returns whether DNS was changed. Failing here isn't fatal: the tunnel still works, and
    /// the user can point their resolver at the address themselves.
    #[cfg(target_os = "linux")]
    fn set_dns(&mut self, resolver: Ipv4Addr) -> bool {
        let address = resolver.to_string();
        let result = run("resolvectl", &["dns", &self.name, &address])
            .and_then(|()| run("resolvectl", &["domain", &self.name, "~."]));
        let Err(error) = result else {
            info!(%resolver, "DNS now goes through the tunnel");
            return true;
        };
        // Most likely this system just doesn't run systemd-resolved.
        debug!("couldn't set DNS with resolvectl: {error:#}");
        match ResolvConf::install(Path::new(resolv_conf::PATH), resolver) {
            Ok(installed) => {
                self.resolv_conf = Some(installed);
                info!(%resolver, "DNS now goes through the tunnel, using {}", resolv_conf::PATH);
                true
            }
            Err(error) => {
                warn!("couldn't change {}: {error}", resolv_conf::PATH);
                warn!("to use the tunnel's resolver, set `nameserver {resolver}` there yourself");
                false
            }
        }
    }

    #[cfg(target_os = "macos")]
    fn set_dns(&mut self, resolver: Ipv4Addr) -> bool {
        let script = format!(
            "d.init\nd.add ServerAddresses * {resolver}\nd.add SupplementalMatchDomains * \"\"\nset State:/Network/Service/saki/DNS\n"
        );
        match scutil(&script) {
            Ok(()) => {
                info!(%resolver, "DNS now goes through the tunnel");
                true
            }
            Err(error) => {
                warn!("couldn't set DNS with scutil: {error:#}");
                false
            }
        }
    }
}

impl Drop for Tunnel {
    /// Routes vanish with the device, but these DNS settings would outlive it. With
    /// `systemd-resolved`, the setting belongs to the device and goes away too.
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if let Some(installed) = &self.resolv_conf
            && let Err(error) = installed.restore()
        {
            warn!("couldn't restore {}: {error}", resolv_conf::PATH);
        }
        #[cfg(target_os = "macos")]
        if self.dns_configured
            && let Err(error) = scutil("remove State:/Network/Service/saki/DNS\n")
        {
            warn!("couldn't restore DNS: {error:#}");
        }
        let _ = self.dns_configured;
    }
}

fn default_name() -> Option<&'static str> {
    // macOS numbers its utun devices itself.
    cfg!(target_os = "linux").then_some("saki0")
}

fn netmask(prefix: u8) -> Ipv4Addr {
    let bits = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
    Ipv4Addr::from(bits)
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    debug!("running {program} {}", args.join(" "));
    let output = Command::new(program).args(args).output().with_context(|| format!("couldn't run {program}"))?;
    if !output.status.success() {
        bail!("{program} {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn scutil(script: &str) -> Result<()> {
    use std::io::Write;
    use std::process::Stdio;

    debug!("running scutil with: {script}");
    let mut child =
        Command::new("scutil").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().context("couldn't run scutil")?;
    child.stdin.take().context("scutil has no stdin")?.write_all(script.as_bytes())?;
    if !child.wait()?.success() {
        bail!("scutil failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netmasks_come_out_right() {
        assert_eq!(netmask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(netmask(32), Ipv4Addr::new(255, 255, 255, 255));
        assert_eq!(netmask(0), Ipv4Addr::new(0, 0, 0, 0));
    }
}
