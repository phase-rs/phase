use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, Ipv4Addr},
    process::{Child, ChildStdin},
    time::{Duration, Instant},
};

use mdns_sd::{DaemonEvent, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;
use tauri::{AppHandle, WebviewWindow};

use crate::native_bridge::{authorize_lan_operation, LanOperation};
use crate::native_engine;
use crate::native_engine_contract::{NativeEngineError, NativeEngineIntent, NativeEngineKey};

const SERVICE_TYPE: &str = "_phase-rs._tcp.local.";
const SCAN_DURATION: Duration = Duration::from_secs(3);
const MAX_SERVERS: usize = 64;

#[derive(Clone, Debug, Default, Serialize)]
pub struct LanServerStatus {
    pub running: bool,
    pub key: Option<NativeEngineKey>,
    pub addresses: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LanCapabilities {
    supported: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiscoveredServer {
    name: String,
    url: String,
    channel: Option<String>,
    #[serde(skip)]
    fullname: String,
}

pub(crate) fn lan_error(error: impl std::fmt::Display) -> NativeEngineError {
    NativeEngineError::Internal {
        detail: error.to_string(),
    }
}

// The lifecycle mutex in native_engine owns this guard. Drop withdraws the
// advertisement before terminating the child, including every failed start.
pub(crate) struct RunningLan {
    pub key: NativeEngineKey,
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub addresses: Vec<String>,
    pub advertisement: Option<Advertisement>,
}

impl RunningLan {
    pub fn status(&self) -> LanServerStatus {
        LanServerStatus {
            running: true,
            key: Some(self.key.clone()),
            addresses: self.addresses.clone(),
        }
    }
}

impl Drop for RunningLan {
    fn drop(&mut self) {
        self.advertisement.take();
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_millis(250);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        if let Ok(receiver) = self.daemon.unregister(&self.fullname) {
            let _ = receiver.recv_timeout(Duration::from_secs(1));
        }
        shutdown_daemon(&self.daemon);
    }
}

fn shutdown_daemon(daemon: &ServiceDaemon) {
    if let Ok(receiver) = daemon.shutdown() {
        let _ = receiver.recv_timeout(Duration::from_secs(1));
    }
}

pub(crate) fn private_addresses() -> Result<Vec<Ipv4Addr>, NativeEngineError> {
    let addresses: BTreeSet<_> = if_addrs::get_if_addrs()
        .map_err(lan_error)?
        .into_iter()
        .filter(if_addrs::Interface::is_oper_up)
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(ip) if ip.is_private() => Some(ip),
            _ => None,
        })
        .collect();
    if addresses.is_empty() {
        return Err(lan_error("no private IPv4 network interface is available"));
    }
    Ok(addresses.into_iter().collect())
}

pub(crate) fn advertise(
    addresses: &[Ipv4Addr],
    port: u16,
    channel: &str,
) -> Result<Advertisement, NativeEngineError> {
    let name = format!("phase-{}-{port}", std::process::id());
    let ips: Vec<_> = addresses.iter().copied().map(IpAddr::V4).collect();
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &name,
        &format!("{name}.local."),
        ips.as_slice(),
        port,
        &[("path", "/ws"), ("channel", channel)][..],
    )
    .map_err(lan_error)?;
    let advertisement = Advertisement {
        daemon: ServiceDaemon::new().map_err(lan_error)?,
        fullname: service.get_fullname().to_owned(),
    };
    let monitor = advertisement.daemon.monitor().map_err(lan_error)?;
    advertisement.daemon.register(service).map_err(lan_error)?;
    let deadline = Instant::now() + SCAN_DURATION;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match monitor.recv_timeout(remaining) {
            Ok(DaemonEvent::Announce(_, _)) => return Ok(advertisement),
            Ok(DaemonEvent::Error(error)) => return Err(lan_error(error)),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Err(lan_error("LAN advertisement was not confirmed"))
}

#[tauri::command]
pub fn lan_capabilities() -> LanCapabilities {
    LanCapabilities { supported: true }
}

#[tauri::command]
pub async fn start_lan_server(
    app: AppHandle,
    window: WebviewWindow,
    key: NativeEngineKey,
    intent: NativeEngineIntent,
) -> Result<LanServerStatus, NativeEngineError> {
    let approval = authorize_lan_operation(&window, LanOperation::StartHosting)
        .await
        .map_err(|error| lan_error(format!("{error:?}")))?;
    tauri::async_runtime::spawn_blocking(move || {
        approval
            .require_current()
            .map_err(|error| lan_error(format!("{error:?}")))?;
        native_engine::start_lan_server_sync(&app, key, intent)
    })
    .await
    .map_err(lan_error)?
}

#[tauri::command]
pub async fn lan_server_status() -> Result<LanServerStatus, NativeEngineError> {
    tauri::async_runtime::spawn_blocking(native_engine::lan_server_status_sync)
        .await
        .map_err(lan_error)?
}

#[tauri::command]
pub async fn stop_lan_server(window: WebviewWindow) -> Result<(), NativeEngineError> {
    let approval = authorize_lan_operation(&window, LanOperation::StopHosting)
        .await
        .map_err(|error| lan_error(format!("{error:?}")))?;
    tauri::async_runtime::spawn_blocking(move || {
        approval
            .require_current()
            .map_err(|error| lan_error(format!("{error:?}")))?;
        native_engine::stop_lan_server_sync()
    })
    .await
    .map_err(lan_error)?
}

#[tauri::command]
pub async fn discover_lan_servers(
    window: WebviewWindow,
) -> Result<Vec<DiscoveredServer>, NativeEngineError> {
    let approval = authorize_lan_operation(&window, LanOperation::Discover)
        .await
        .map_err(|error| lan_error(format!("{error:?}")))?;
    tauri::async_runtime::spawn_blocking(move || {
        approval
            .require_current()
            .map_err(|error| lan_error(format!("{error:?}")))?;
        discover_sync()
    })
    .await
    .map_err(lan_error)?
}

fn collect_service(
    servers: &mut BTreeMap<String, DiscoveredServer>,
    service: mdns_sd::ResolvedService,
) {
    if service.get_port() == 0 || service.get_property_val_str("path") != Some("/ws") {
        return;
    }
    for ip in service
        .get_addresses_v4()
        .into_iter()
        .filter(Ipv4Addr::is_private)
    {
        let url = format!("ws://{ip}:{}/ws", service.get_port());
        if servers.len() >= MAX_SERVERS && !servers.contains_key(&url) {
            break;
        }
        servers.insert(
            url.clone(),
            DiscoveredServer {
                fullname: service.get_fullname().to_owned(),
                name: service
                    .get_fullname()
                    .strip_suffix(&format!(".{SERVICE_TYPE}"))
                    .unwrap_or(service.get_fullname())
                    .chars()
                    .take(128)
                    .collect(),
                url,
                channel: service
                    .get_property_val_str("channel")
                    .filter(|channel| matches!(*channel, "release" | "preview"))
                    .map(str::to_owned),
            },
        );
    }
}

fn discover_sync() -> Result<Vec<DiscoveredServer>, NativeEngineError> {
    let daemon = ServiceDaemon::new().map_err(lan_error)?;
    let result = (|| {
        let monitor = daemon.monitor().map_err(lan_error)?;
        let events = daemon.browse(SERVICE_TYPE).map_err(lan_error)?;
        let deadline = Instant::now() + SCAN_DURATION;
        let mut servers = BTreeMap::new();
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match events.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(ServiceEvent::ServiceResolved(service)) => {
                    collect_service(&mut servers, *service)
                }
                Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                    servers.retain(|_, server| server.fullname != fullname)
                }
                Ok(_) => {}
                Err(mdns_sd::RecvTimeoutError::Disconnected) => {
                    return Err(lan_error("LAN discovery daemon disconnected"))
                }
                Err(_) => {}
            }
            for event in monitor.try_iter() {
                if let DaemonEvent::Error(error) = event {
                    return Err(lan_error(error));
                }
            }
        }
        Ok(servers.into_values().collect())
    })();
    let _ = daemon.stop_browse(SERVICE_TYPE);
    shutdown_daemon(&daemon);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn running_child_guard_closes_stdin_and_reaps_child() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read line"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let stdin = child.stdin.take();
        drop(RunningLan {
            key: NativeEngineKey::Release {
                version: "1.0.0".into(),
            },
            child,
            stdin,
            addresses: vec![],
            advertisement: None,
        });
        assert!(!std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success());
    }

    #[test]
    #[ignore = "requires a multicast-enabled private IPv4 interface"]
    fn local_mdns_registration_discovery_and_withdrawal() {
        let ips = private_addresses().unwrap();
        let advertisement = advertise(&ips, 49321, "release").unwrap();
        let expected = format!("ws://{}:49321/ws", ips[0]);
        assert!(discover_sync()
            .unwrap()
            .iter()
            .any(|server| server.url == expected));
        drop(advertisement);
        assert!(!discover_sync()
            .unwrap()
            .iter()
            .any(|server| server.url == expected));
    }

    #[test]
    fn discovery_deduplicates_filters_addresses_and_ignores_untrusted_channel() {
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            "test",
            "test.local.",
            "192.168.1.3,8.8.8.8,127.0.0.1",
            1234,
            &[("path", "/ws"), ("channel", "compatible")][..],
        )
        .unwrap();
        let mut servers = BTreeMap::new();
        collect_service(&mut servers, service.clone().as_resolved_service());
        collect_service(&mut servers, service.as_resolved_service());
        assert_eq!(servers.len(), 1);
        let server = servers.values().next().unwrap();
        assert_eq!(server.url, "ws://192.168.1.3:1234/ws");
        assert!(server.channel.is_none());
    }

    #[test]
    fn discovery_rejects_path_override_and_caps_results() {
        let mut servers = BTreeMap::new();
        for port in 1..100 {
            let service = ServiceInfo::new(
                SERVICE_TYPE,
                "test",
                "test.local.",
                "10.1.2.3",
                port,
                &[("path", "/ws")][..],
            )
            .unwrap();
            collect_service(&mut servers, service.as_resolved_service());
        }
        assert_eq!(servers.len(), MAX_SERVERS);
        servers.clear();
        let service = ServiceInfo::new(
            SERVICE_TYPE,
            "test",
            "test.local.",
            "10.1.2.3",
            1234,
            &[("path", "/admin")][..],
        )
        .unwrap();
        collect_service(&mut servers, service.as_resolved_service());
        assert!(servers.is_empty());
    }
}
