//! Opening the pack port on the admin's router, so they never do it by hand.
//!
//! UPnP-IGD through `igd-next`, which is what home routers overwhelmingly
//! speak. The mapping is leased, never permanent: renewed on a timer and
//! removed on a clean exit, so a crash leaves at most one lease that expires on
//! its own instead of a hole in somebody's firewall forever. A router that only
//! grants permanent mappings gets none at all, because nothing can promise the
//! removal will run — a crash, a kill or a logout skips it.
//!
//! This reports honestly rather than optimistically. A router that says yes
//! does not mean players can connect — carrier-grade NAT, a second router, or
//! an ISP that filters inbound traffic all make a granted mapping useless — so
//! the external address the router reports is classified, and the UI says
//! what that classification means.
//!
//! Known limits, from testing `igd-next` 0.17.1:
//! - no HTTP timeout of its own: every call is wrapped in one, or a hung
//!   router SOAP endpoint blocks forever;
//! - discovery sends one search with a three-second response window, so the
//!   search timeout must be at least five seconds, with one retry;
//! - the mapping description is not XML-escaped: plain ASCII only.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use igd_next::aio::tokio::{search_gateway, Tokio};
use igd_next::{
    AddPortError, GetExternalIpError, PortMappingProtocol, RequestError, SearchError, SearchOptions,
};
use serde::Serialize;
use tokio::sync::{oneshot, watch};
use tokio::time::timeout;

/// How long a mapping is leased for. Renewed at half this.
pub const LEASE: Duration = Duration::from_secs(60 * 60);

/// What the mapping is called in the router's table, which is where an admin
/// wondering what opened a port will look. Plain ASCII, and nothing XML would
/// take for markup: igd-next pastes it into the request unescaped.
const DESCRIPTION: &str = "Geode server packs";

type Gateway = igd_next::aio::Gateway<Tokio>;

/// How long the task gives each exchange with the router, and how often it
/// renews and retries. One set for the app; tests shrink them so a renewal or
/// a hung router takes a second rather than half an hour.
#[derive(Clone, Copy)]
struct Timings {
    lease: Duration,
    /// The wait after a failed attempt, doubled by each further failure in a
    /// row up to `retry_limit`, and back to this after a success.
    retry: Duration,
    retry_limit: Duration,
    /// Each of the two searches. The search asks devices to answer within
    /// three seconds and they may use all of it, so this is at least five.
    search: Duration,
    /// Each request to a router already found — on the way out as well.
    request: Duration,
}

const TIMINGS: Timings = Timings {
    lease: LEASE,
    retry: Duration::from_secs(10),
    retry_limit: Duration::from_secs(60),
    search: Duration::from_secs(5),
    request: Duration::from_secs(5),
};

/// What the router reported about where players would connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reachability {
    /// A public address. Players should be able to connect.
    Public,
    /// `100.64.0.0/10`: the ISP shares one address between customers, and no
    /// mapping on this router makes this machine reachable.
    Cgnat,
    /// A private address: there is another router in front of this one.
    DoubleNat,
    /// No usable IPv4 address at all (unspecified, DS-Lite range, empty).
    NoIpv4,
}

/// Why a mapping could not be made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "camelCase")]
pub enum Failure {
    /// Nothing answered a UPnP search. UPnP is off, or something on this
    /// machine (a VPN, a firewall, Wi-Fi client isolation) is in the way.
    NoGateway,
    /// A router answered but refused (UPnP error 606).
    Refused,
    /// Another device on the network already maps this port (error 718).
    PortInUse,
    /// The router only grants mappings that never expire (error 725), and
    /// Geode will not make one. Its removal runs only on a clean exit; after a
    /// crash, a kill or a logout the port would stay open to this machine for
    /// good, with no lease to close it and no record for a later launch to
    /// find. The router is left as it was.
    PermanentOnly,
    /// Anything else, with the router's own words.
    Other { detail: String },
}

/// The live state of the mapping, for the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Status {
    /// Mapping is set to manual; the router is left alone.
    Off,
    Searching,
    // An enum's `rename_all` names its variants, not their fields: without
    // this the UI would be handed `external_ip` where it reads `externalIp`.
    #[serde(rename_all = "camelCase")]
    Mapped {
        external_ip: IpAddr,
        port: u16,
        reachability: Reachability,
    },
    Failed(Failure),
}

/// A held mapping. Renews itself until [`PortMapper::stop`].
///
/// Await `stop` before starting another mapper on the same port. A mapper
/// that is merely dropped removes its mapping in the background, and that
/// removal can land after the new mapper's add and quietly close the port
/// again until the next renewal.
pub struct PortMapper {
    status: watch::Receiver<Status>,
    /// Tells the task to stop. Dropping it does too, so a mapper dropped
    /// rather than stopped still removes its mapping if the runtime outlives
    /// the removal — but nobody waits to find out.
    stop: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<()>,
}

impl PortMapper {
    /// Start mapping TCP `port` to this machine and keep it mapped.
    ///
    /// Returns immediately with the status at `Searching`; discovery, the
    /// mapping and every renewal happen on a spawned task that updates the
    /// status as it goes. Renewal re-adds the mapping and re-reads the
    /// external address each time, because a router reboot or a new WAN
    /// address wipes mappings without telling anyone.
    ///
    /// The internal client address is the one the OS routes toward the
    /// gateway — not the first interface found, which on a machine with VM
    /// or Docker bridges is the wrong one and makes the mapping silently
    /// useless.
    pub fn start(port: u16) -> Self {
        Self::spawn(port, Finder::lan(), TIMINGS)
    }

    /// [`PortMapper::start`] with the router and the timings chosen by the
    /// caller — the seam tests use to point it at a mock gateway, so that no
    /// test ever asks a real router for anything.
    fn spawn(port: u16, finder: Finder, timings: Timings) -> Self {
        let (publish, status) = watch::channel(Status::Searching);
        let (stop, stopped) = oneshot::channel();
        // Tauri's spawn, not Tokio's: this is a plain function that may be
        // called outside any Tokio runtime context, where `tokio::spawn` panics.
        let task = tauri::async_runtime::spawn(hold(port, finder, timings, publish, stopped));
        Self { status, stop, task }
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    /// Stop renewing and remove the mapping, giving up after a few seconds —
    /// this runs as the app exits, and an unresponsive router must not hold
    /// that up. The lease is the safety net if removal fails.
    ///
    /// An add already sent is seen through first, up to its own timeout, so
    /// the worst case is that plus the removal's: one request, or two when an
    /// add went unanswered. Whether the router made the
    /// mapping is only known from its answer; abandoning the add would leave a
    /// mapping that nothing removes.
    ///
    /// The status ends at `Off`, so whoever is still watching it is not left
    /// believing in a mapping that is gone.
    pub async fn stop(self) {
        // Both only fail if the task is already gone, which leaves nothing to
        // stop or wait for.
        let _ = self.stop.send(());
        let _ = self.task.await;
    }
}

/// Ask the router what its public address is, and change nothing.
///
/// For manual router setup: the admin forwards the port themselves, but Geode
/// can still learn the address to put in a link rather than make them look it
/// up. Discovery and one GetExternalIPAddress, each under a timeout.
///
/// With no router on the network it takes both searches running out to say
/// so — over ten seconds — so nothing the admin is waiting on should wait on
/// this.
pub async fn read_external_ip() -> std::result::Result<IpAddr, Failure> {
    read_external_ip_from(&Finder::lan(), TIMINGS).await
}

/// [`read_external_ip`] with the router and the timings chosen by the caller,
/// for the same reason as [`PortMapper::spawn`].
async fn read_external_ip_from(finder: &Finder, t: Timings) -> Result<IpAddr, Failure> {
    let gateway = finder.find(t.search).await?;
    // A fresh record: whether the router went missing only matters to a
    // task that will try it again.
    Held::default().external_ip(&gateway, t).await
}

/// What an external address means for whether players can connect.
///
/// Loopback and link-local count as no IPv4 as well. A router reports a
/// `169.254.x.x` WAN address when its ISP never gave it one, and loopback only
/// when its firmware is confused; either way there is no address anyone
/// outside could use.
pub fn classify(ip: IpAddr) -> Reachability {
    let IpAddr::V4(ip) = ip else {
        return Reachability::NoIpv4;
    };
    let [a, b, c, d] = ip.octets();
    if a == 100 && (64..128).contains(&b) {
        Reachability::Cgnat
    } else if ip.is_private() {
        Reachability::DoubleNat
    } else if ip.is_unspecified()
        // 192.0.0.0/29, where DS-Lite puts a home router's own IPv4 side.
        || ((a, b, c) == (192, 0, 0) && d < 8)
        || ip.is_loopback()
        || ip.is_link_local()
    {
        Reachability::NoIpv4
    } else {
        Reachability::Public
    }
}

/// Where the task looks for the router.
enum Finder {
    /// An SSDP search sent from `bind` to `target`: the LAN's multicast group
    /// in the app, a silent local socket in tests.
    Search {
        bind: SocketAddr,
        target: SocketAddr,
    },
    /// Gateways already known, so tests reach a mock without searching. Each
    /// search finds the next one, and the last is found from then on — which
    /// is how a test has a router move.
    #[cfg(test)]
    Known(std::sync::Mutex<Vec<Gateway>>),
}

impl Finder {
    /// The real router, wherever it is on the LAN.
    fn lan() -> Self {
        let lan = SearchOptions::default();
        Finder::Search {
            bind: lan.bind_addr,
            target: lan.broadcast_address,
        }
    }

    async fn find(&self, window: Duration) -> Result<Gateway, Failure> {
        match self {
            Finder::Search { bind, target } => discover(*bind, *target, window).await,
            #[cfg(test)]
            Finder::Known(gateways) => {
                let mut gateways = gateways.lock().unwrap();
                Ok(if gateways.len() > 1 {
                    gateways.remove(0)
                } else {
                    gateways[0].clone()
                })
            }
        }
    }
}

/// Search for the router, twice before concluding there is none: the search
/// is one UDP datagram, and a single one lost on busy Wi-Fi should not read as
/// UPnP being off.
async fn discover(
    bind: SocketAddr,
    target: SocketAddr,
    window: Duration,
) -> Result<Gateway, Failure> {
    for _ in 0..2 {
        let options = SearchOptions {
            bind_addr: bind,
            broadcast_address: target,
            timeout: Some(window),
            single_search_timeout: Some(window),
        };
        // igd-next bounds a search by `window` itself; this outer bound is the
        // rule for every call into it rather than a gap known in this one.
        match timeout(window + Duration::from_secs(1), search_gateway(options)).await {
            Ok(Ok(gateway)) => return Ok(gateway),
            Ok(Err(SearchError::NoResponseWithinTimeout)) | Err(_) => continue,
            Ok(Err(e)) => {
                return Err(Failure::Other {
                    detail: format!("Could not search for the router: {e}"),
                })
            }
        }
    }
    Err(Failure::NoGateway)
}

/// The address this machine uses to reach `gateway`, which is the one a
/// mapping must point at. Connecting a UDP socket sends nothing; it only asks
/// the OS which route, and so which source address, it would use.
fn local_toward(gateway: IpAddr) -> std::io::Result<IpAddr> {
    let any: IpAddr = match gateway {
        IpAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
        IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
    };
    let socket = std::net::UdpSocket::bind((any, 0))?;
    // The port is irrelevant: nothing is ever sent to it.
    socket.connect((gateway, 9))?;
    Ok(socket.local_addr()?.ip())
}

/// The mapping task: map, then renew at half the lease until told to stop,
/// then remove.
///
/// A failed attempt is retried rather than ending the task — the router that
/// did not answer at startup may well answer later, and the app should not
/// need restarting to notice — and retried within seconds, backing off to a
/// minute, not at the next renewal. Half a lease is half an hour: an admin
/// who turns UPnP on because the Share tab told them to would see nothing
/// change for that long, and a renewal that failed once would not be tried
/// again until its lease had already run out.
async fn hold(
    port: u16,
    finder: Finder,
    timings: Timings,
    publish: watch::Sender<Status>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut held = Held::default();
    let mut retry = timings.retry;
    loop {
        let now = match held.refresh(&finder, port, timings, &mut stop).await {
            Ok(external_ip) => Status::Mapped {
                external_ip,
                port,
                reachability: classify(external_ip),
            },
            Err(Halt::Failed(failure)) => Status::Failed(failure),
            Err(Halt::Stopped) => break,
        };
        let wait = if matches!(now, Status::Failed(_)) {
            let wait = retry;
            retry = (retry * 2).min(timings.retry_limit);
            wait
        } else {
            retry = timings.retry;
            timings.lease / 2
        };
        // A renewal that changes nothing tells nobody anything.
        publish.send_if_modified(|current| {
            let changed = *current != now;
            *current = now;
            changed
        });
        tokio::select! {
            _ = &mut stop => break,
            _ = tokio::time::sleep(wait) => {}
        }
    }
    held.remove(port, timings).await;
    publish.send_replace(Status::Off);
}

/// Why an attempt ended without a mapping.
enum Halt {
    /// Told to stop partway.
    Stopped,
    Failed(Failure),
}

impl From<Failure> for Halt {
    fn from(failure: Failure) -> Self {
        Halt::Failed(failure)
    }
}

/// `work`, unless the stop comes first, which drops `work` where it stands.
/// Only for work the router cannot act on.
async fn unless_stopped<T>(
    stop: &mut oneshot::Receiver<()>,
    work: impl std::future::Future<Output = T>,
) -> Result<T, Halt> {
    tokio::select! {
        _ = stop => Err(Halt::Stopped),
        done = work => Ok(done),
    }
}

/// What the task knows about the router between renewals.
#[derive(Default)]
struct Held {
    /// The router last talked to, and the one a removal goes to.
    gateway: Option<Gateway>,
    /// It stopped answering, so the next attempt searches again: a rebooted
    /// router can come back with its control endpoint on a different port.
    lost: bool,
    /// Whether the router maps this port to this machine, which is what
    /// removal goes by.
    ours: Ours,
}

/// Whether a mapping of the port on the router is this machine's.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Ours {
    /// Not as far as anyone knows: nothing has been granted, or the router
    /// has since said the port is another device's. Nothing is removed —
    /// removing after a 718 would delete that device's mapping.
    #[default]
    No,
    /// An add went out and no answer came back. The router may have made
    /// the mapping, or may have been saying the port is another device's.
    Unknown,
    /// The router granted it, and has not since said the port is another
    /// device's.
    Yes,
}

impl Held {
    /// Add the mapping — or renew it, which is the identical request — and
    /// read the external address, returning that.
    ///
    /// A stop abandons the search and the address read, which change nothing
    /// on the router, but not an add already sent: the router acts on it
    /// whether or not anyone waits, and only its answer says whether there is
    /// a mapping to remove. The add has its own timeout, so seeing it through
    /// holds a stop up by at most that.
    async fn refresh(
        &mut self,
        finder: &Finder,
        port: u16,
        t: Timings,
        stop: &mut oneshot::Receiver<()>,
    ) -> Result<IpAddr, Halt> {
        let gateway = match &self.gateway {
            Some(gateway) if !self.lost => gateway.clone(),
            // A failed search keeps the old router, which is still the only
            // one that can take down a mapping it holds.
            _ => {
                let found = unless_stopped(stop, finder.find(t.search)).await??;
                self.gateway = Some(found.clone());
                self.lost = false;
                found
            }
        };
        self.add(&gateway, port, t).await?;
        Ok(unless_stopped(stop, self.external_ip(&gateway, t)).await??)
    }

    /// Ask `gateway` to map `port` to this machine for a lease, and keep
    /// `ours` true to what the answer says.
    async fn add(&mut self, gateway: &Gateway, port: u16, t: Timings) -> Result<(), Failure> {
        // Asked every time rather than remembered: the DHCP lease on this
        // machine can move it to a new address while the app runs.
        let local = local_toward(gateway.addr.ip()).map_err(|e| Failure::Other {
            detail: format!(
                "Could not tell which address this machine uses toward the router: {e}"
            ),
        })?;
        let client = SocketAddr::new(local, port);

        // Marked before sending: from here the router may make the mapping
        // whether or not its answer ever arrives.
        let before = self.ours;
        if before == Ours::No {
            self.ours = Ours::Unknown;
        }
        let lease = t.lease.as_secs() as u32;
        let added = timeout(
            t.request,
            gateway.add_port(PortMappingProtocol::TCP, port, client, lease, DESCRIPTION),
        )
        .await;
        let failure = match added {
            Ok(Ok(())) => {
                self.ours = Ours::Yes;
                return Ok(());
            }
            Ok(Err(AddPortError::PortInUse)) => {
                self.ours = Ours::No;
                return Err(Failure::PortInUse);
            }
            // No answer, or none that says what happened: the add may have
            // landed, so `ours` stays as marked.
            Err(_) => return Err(self.unanswered()),
            Ok(Err(AddPortError::RequestError(e))) if !matches!(e, RequestError::ErrorCode(..)) => {
                return Err(self.request_failed(e))
            }
            Ok(Err(AddPortError::ActionNotAuthorized)) => Failure::Refused,
            // Not asked again with the lease of 0 the router wants.
            Ok(Err(AddPortError::OnlyPermanentLeasesSupported)) => Failure::PermanentOnly,
            Ok(Err(AddPortError::RequestError(e))) => self.request_failed(e),
            Ok(Err(e)) => Failure::Other {
                detail: e.to_string(),
            },
        };
        // Refused with an error code, so this add made nothing, and whatever
        // was there before it still is.
        self.ours = before;
        Err(failure)
    }

    /// One read of the router's external address.
    async fn external_ip(&mut self, gateway: &Gateway, t: Timings) -> Result<IpAddr, Failure> {
        match timeout(t.request, gateway.get_external_ip()).await {
            Ok(Ok(ip)) => Ok(ip),
            Ok(Err(GetExternalIpError::ActionNotAuthorized)) => Err(Failure::Refused),
            Ok(Err(GetExternalIpError::RequestError(e))) => Err(self.request_failed(e)),
            Err(_) => Err(self.unanswered()),
        }
    }

    /// A request that never reached the router, or never came back, means the
    /// router may have moved; one it answered with an error means it is still
    /// where it was.
    fn request_failed(&mut self, e: RequestError) -> Failure {
        if matches!(
            e,
            RequestError::IoError(_)
                | RequestError::HyperError(_)
                | RequestError::HyperClientError(_)
        ) {
            self.lost = true;
        }
        Failure::Other {
            detail: e.to_string(),
        }
    }

    fn unanswered(&mut self) -> Failure {
        self.lost = true;
        Failure::Other {
            detail: "The router stopped answering.".into(),
        }
    }

    /// Take the mapping down if it is this machine's.
    ///
    /// Each request gets the time any other request does. A router slow enough
    /// to need it for an add needs it here too, and a shorter budget for the
    /// way out left exactly that router's mapping in place — renewed, even, by
    /// an add whose answer came too late to be followed by the removal.
    ///
    /// When an add went unanswered, the router is asked again before anything
    /// is removed rather than having its mapping removed blind: the lost
    /// answer may have been a 718, and removing would delete another device's
    /// mapping. A grant settles that the port is this machine's — made just
    /// now, if the first add never landed — and is removed straight after.
    async fn remove(mut self, port: u16, t: Timings) {
        let Some(gateway) = self.gateway.clone() else {
            return;
        };
        // Nothing useful to do with a failure: this is the way out, and the
        // lease is what cleans up after a removal that did not land. `add` is
        // bounded by the request timeout on its own.
        if self.ours == Ours::Unknown {
            let _ = self.add(&gateway, port, t).await;
        }
        if self.ours == Ours::Yes {
            let _ = timeout(
                t.request,
                gateway.remove_port(PortMappingProtocol::TCP, port),
            )
            .await;
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    //! Every mapping test here talks to mock-igd on loopback, through
    //! [`PortMapper::spawn`] or [`read_external_ip_from`] with a gateway named
    //! outright. None calls [`PortMapper::start`], which would search the real
    //! LAN and change the real router's configuration, or
    //! [`read_external_ip`], which would search it too.

    use super::*;

    use std::collections::HashMap;
    use std::time::Instant;

    use mock_igd::matcher::{AddPortMappingRequest, DeletePortMappingRequest, SoapRequestBody};
    use mock_igd::{Action, MockIgdServer, Responder};
    use tokio::net::{TcpListener, TcpStream, UdpSocket};

    const PORT: u16 = 25590;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    /// Short enough that a test is over in a second or two, long enough that
    /// a loopback mock on a loaded machine still answers in time.
    ///
    /// A failure waits as long as a renewal here, so a test counting requests
    /// just after one is not racing the retry; the test of the retry sets its
    /// own.
    fn quick(lease: Duration) -> Timings {
        Timings {
            lease,
            retry: lease / 2,
            retry_limit: lease / 2,
            search: Duration::from_millis(300),
            request: Duration::from_secs(2),
        }
    }

    /// A gateway at `addr` with the control URL and argument lists mock-igd
    /// serves, built by hand so no search is ever sent.
    fn gateway_at(addr: SocketAddr) -> Gateway {
        let args = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        Gateway {
            addr,
            root_url: "/rootDesc.xml".into(),
            control_url: "/ctl/IPConn".into(),
            control_schema_url: "/WANIPCn.xml".into(),
            control_schema: HashMap::from([
                (
                    "AddPortMapping".to_string(),
                    args(&[
                        "NewRemoteHost",
                        "NewExternalPort",
                        "NewProtocol",
                        "NewInternalPort",
                        "NewInternalClient",
                        "NewEnabled",
                        "NewPortMappingDescription",
                        "NewLeaseDuration",
                    ]),
                ),
                (
                    "DeletePortMapping".to_string(),
                    args(&["NewRemoteHost", "NewExternalPort", "NewProtocol"]),
                ),
            ]),
            provider: Tokio,
        }
    }

    /// A mapper whose successive searches find a router at each of `addrs`.
    fn mapper_at(addrs: &[SocketAddr], timings: Timings) -> PortMapper {
        let gateways = addrs.iter().map(|a| gateway_at(*a)).collect();
        PortMapper::spawn(
            PORT,
            Finder::Known(std::sync::Mutex::new(gateways)),
            timings,
        )
    }

    async fn router() -> MockIgdServer {
        MockIgdServer::start().await.unwrap()
    }

    /// The first status that satisfies `done`, or a panic after ten seconds.
    async fn settle(mapper: &PortMapper, mut done: impl FnMut(&Status) -> bool) -> Status {
        let mut status = mapper.status();
        let seen = timeout(Duration::from_secs(10), status.wait_for(|s| done(s)))
            .await
            .expect("the status never got there")
            .expect("the mapping task ended");
        seen.clone()
    }

    async fn adds(router: &MockIgdServer) -> Vec<AddPortMappingRequest> {
        let requests = router.received_requests().await;
        requests
            .into_iter()
            .filter_map(|r| match r.body {
                SoapRequestBody::AddPortMapping(add) => Some(add),
                _ => None,
            })
            .collect()
    }

    async fn deletes(router: &MockIgdServer) -> Vec<DeletePortMappingRequest> {
        let requests = router.received_requests().await;
        requests
            .into_iter()
            .filter_map(|r| match r.body {
                SoapRequestBody::DeletePortMapping(delete) => Some(delete),
                _ => None,
            })
            .collect()
    }

    /// At least `n` AddPortMapping requests, or a panic after ten seconds.
    async fn at_least_adds(router: &MockIgdServer, n: usize) -> Vec<AddPortMappingRequest> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let seen = adds(router).await;
            if seen.len() >= n {
                return seen;
            }
            assert!(
                Instant::now() < deadline,
                "only {} of {n} adds arrived",
                seen.len()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A router that answers its first `answered` requests (by passing them
    /// to mock-igd at `to`) and then accepts every connection and never
    /// responds: one that hangs partway through. igd-next opens a fresh
    /// connection per request, so connections count requests.
    async fn hangs_after(answered: usize, to: SocketAddr) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            for n in 0.. {
                let (mut inbound, _) = listener.accept().await.unwrap();
                if n >= answered {
                    held.push(inbound);
                    continue;
                }
                tokio::spawn(async move {
                    let mut outbound = TcpStream::connect(to).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                });
            }
        });
        addr
    }

    /// A router that acts on each of its first `late` requests at once — they
    /// reach mock-igd at `to` straight away — but holds the answer back for
    /// `delay`: one that makes a change and is slow to say so. Later requests
    /// are answered as they come.
    async fn answers_late(late: usize, delay: Duration, to: SocketAddr) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for n in 0.. {
                let (inbound, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let outbound = TcpStream::connect(to).await.unwrap();
                    let (mut from_client, mut to_client) = inbound.into_split();
                    let (mut from_router, mut to_router) = outbound.into_split();
                    tokio::spawn(async move {
                        let _ = tokio::io::copy(&mut from_client, &mut to_router).await;
                    });
                    if n < late {
                        tokio::time::sleep(delay).await;
                    }
                    let _ = tokio::io::copy(&mut from_router, &mut to_client).await;
                });
            }
        });
        addr
    }

    /// A router that acts on every request at once but answers the first
    /// after `first` and each later one after `rest`.
    async fn answers_slowly(first: Duration, rest: Duration, to: SocketAddr) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            for n in 0.. {
                let (inbound, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let outbound = TcpStream::connect(to).await.unwrap();
                    let (mut from_client, mut to_client) = inbound.into_split();
                    let (mut from_router, mut to_router) = outbound.into_split();
                    tokio::spawn(async move {
                        let _ = tokio::io::copy(&mut from_client, &mut to_router).await;
                    });
                    tokio::time::sleep(if n == 0 { first } else { rest }).await;
                    let _ = tokio::io::copy(&mut from_router, &mut to_client).await;
                });
            }
        });
        addr
    }

    /// Where nothing is listening, as when a router has rebooted and its
    /// control endpoint came back on another port.
    async fn closed_port() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    }

    /// The usual router: grants the mapping, says where it is, and removes it.
    async fn granting(external_ip: &str) -> MockIgdServer {
        let router = router().await;
        router
            .mock(Action::add_port_mapping(), Responder::success())
            .await;
        router
            .mock(Action::delete_port_mapping(), Responder::success())
            .await;
        router
            .mock(
                Action::GetExternalIPAddress,
                Responder::success().with_external_ip(ip(external_ip)),
            )
            .await;
        router
    }

    #[test]
    fn every_kind_of_external_address_is_classified() {
        use Reachability::*;
        for (addr, expected) in [
            // Shared address space, RFC 6598, edges included.
            ("100.64.0.0", Cgnat),
            ("100.72.1.9", Cgnat),
            ("100.127.255.255", Cgnat),
            ("100.63.255.255", Public),
            ("100.128.0.0", Public),
            // RFC 1918, edges included.
            ("10.0.0.1", DoubleNat),
            ("10.255.255.255", DoubleNat),
            ("172.16.0.0", DoubleNat),
            ("172.31.255.255", DoubleNat),
            ("172.15.255.255", Public),
            ("172.32.0.0", Public),
            ("192.168.0.1", DoubleNat),
            ("192.168.255.255", DoubleNat),
            ("192.169.0.1", Public),
            // Nothing usable.
            ("0.0.0.0", NoIpv4),
            ("192.0.0.0", NoIpv4),
            ("192.0.0.2", NoIpv4),
            ("192.0.0.7", NoIpv4),
            ("127.0.0.1", NoIpv4),
            ("169.254.10.20", NoIpv4),
            ("::", NoIpv4),
            ("2001:db8::1", NoIpv4),
            // Public.
            ("81.2.69.160", Public),
            ("8.8.8.8", Public),
            ("11.0.0.1", Public),
        ] {
            assert_eq!(classify(ip(addr)), expected, "{addr}");
        }
    }

    /// The UI's `PortMappingStatus` type is the other half of this shape.
    #[test]
    fn status_is_serialised_the_way_the_ui_reads_it() {
        let json = |s: Status| serde_json::to_value(s).unwrap();
        assert_eq!(
            json(Status::Mapped {
                external_ip: ip("100.72.1.9"),
                port: PORT,
                reachability: Reachability::DoubleNat,
            }),
            serde_json::json!({
                "state": "mapped",
                "externalIp": "100.72.1.9",
                "port": PORT,
                "reachability": "doubleNat",
            })
        );
        assert_eq!(
            json(Status::Failed(Failure::NoGateway)),
            serde_json::json!({ "state": "failed", "reason": "noGateway" })
        );
        assert_eq!(
            json(Status::Failed(Failure::PermanentOnly)),
            serde_json::json!({ "state": "failed", "reason": "permanentOnly" })
        );
        assert_eq!(
            json(Status::Failed(Failure::Other {
                detail: "no".into()
            })),
            serde_json::json!({ "state": "failed", "reason": "other", "detail": "no" })
        );
        assert_eq!(json(Status::Off), serde_json::json!({ "state": "off" }));
    }

    #[tokio::test]
    async fn a_granted_mapping_reports_the_external_address_and_what_it_means() {
        let router = granting("100.72.1.9").await;

        let mapper = mapper_at(&[router.http_addr()], quick(LEASE));
        let status = settle(&mapper, |s| *s != Status::Searching).await;
        assert_eq!(
            status,
            Status::Mapped {
                external_ip: ip("100.72.1.9"),
                port: PORT,
                reachability: Reachability::Cgnat,
            }
        );

        let adds = adds(&router).await;
        assert_eq!(adds.len(), 1);
        let add = &adds[0];
        assert_eq!(add.external_port, PORT);
        assert_eq!(add.internal_port, PORT);
        assert_eq!(add.protocol, "TCP");
        // The gateway is on loopback, so the route toward it leaves from
        // loopback: the source address, not whichever interface comes first.
        assert_eq!(add.internal_client, "127.0.0.1");
        assert_eq!(add.lease_duration, 3600);
        assert_eq!(add.description, "Geode server packs");
        assert!(add.enabled);

        mapper.stop().await;
    }

    #[tokio::test]
    async fn each_renewal_re_adds_the_mapping_and_rereads_the_address() {
        let router = router().await;
        router
            .mock(Action::add_port_mapping(), Responder::success())
            .await;
        // The WAN address changes after the first read.
        router
            .mock_with_times(
                Action::GetExternalIPAddress,
                Responder::success().with_external_ip(ip("81.2.69.160")),
                1,
            )
            .await;
        router
            .mock(
                Action::GetExternalIPAddress,
                Responder::success().with_external_ip(ip("81.2.69.161")),
            )
            .await;

        let lease = Duration::from_secs(2);
        let mapper = mapper_at(&[router.http_addr()], quick(lease));
        let status = settle(&mapper, |s| {
            matches!(s, Status::Mapped { external_ip, .. } if *external_ip == ip("81.2.69.161"))
        })
        .await;
        assert_eq!(
            status,
            Status::Mapped {
                external_ip: ip("81.2.69.161"),
                port: PORT,
                reachability: Reachability::Public,
            }
        );

        let adds = at_least_adds(&router, 2).await;
        let fields = |a: &AddPortMappingRequest| {
            (
                a.external_port,
                a.protocol.clone(),
                a.internal_client.clone(),
                a.internal_port,
                a.lease_duration,
                a.description.clone(),
            )
        };
        assert_eq!(
            fields(&adds[0]),
            fields(&adds[1]),
            "a renewal is the same request again"
        );
        assert_eq!(adds[0].lease_duration, 2);

        // Renewed while the lease still had time to run, not once it had gone.
        let at: Vec<Duration> = router
            .received_requests()
            .await
            .into_iter()
            .filter(|r| matches!(r.body, SoapRequestBody::AddPortMapping(_)))
            .map(|r| r.timestamp)
            .collect();
        assert!(at[1] - at[0] < lease, "renewed {:?} apart", at[1] - at[0]);

        mapper.stop().await;
    }

    /// When each add after the first reached the router, measured from the
    /// one before it.
    async fn gaps_between_adds(router: &MockIgdServer) -> Vec<Duration> {
        let at: Vec<Duration> = router
            .received_requests()
            .await
            .into_iter()
            .filter(|r| matches!(r.body, SoapRequestBody::AddPortMapping(_)))
            .map(|r| r.timestamp)
            .collect();
        at.windows(2).map(|w| w[1] - w[0]).collect()
    }

    #[tokio::test]
    async fn a_failure_is_retried_soon_backing_off_and_a_success_renews_at_half_the_lease() {
        let router = router().await;
        let failing = || Responder::error(501, "Action Failed");
        // Three failures, a grant, one more failure, then grants.
        router
            .mock_with_times(Action::add_port_mapping(), failing(), 3)
            .await;
        router
            .mock_with_times(Action::add_port_mapping(), Responder::success(), 1)
            .await;
        router
            .mock_with_times(Action::add_port_mapping(), failing(), 1)
            .await;
        router
            .mock(Action::add_port_mapping(), Responder::success())
            .await;
        router
            .mock(Action::delete_port_mapping(), Responder::success())
            .await;
        router
            .mock(
                Action::GetExternalIPAddress,
                Responder::success().with_external_ip(ip("81.2.69.160")),
            )
            .await;

        let timings = Timings {
            retry: Duration::from_millis(300),
            retry_limit: Duration::from_millis(600),
            ..quick(Duration::from_secs(4))
        };
        let mapper = mapper_at(&[router.http_addr()], timings);
        at_least_adds(&router, 6).await;
        mapper.stop().await;

        let gaps = gaps_between_adds(&router).await;
        let within = |gap: Duration, from: u64, to: u64| {
            gap >= Duration::from_millis(from) && gap < Duration::from_millis(to)
        };
        // Each bound's far side is where the wrong wait would land.
        assert!(within(gaps[0], 300, 550), "first retry after {gaps:?}");
        assert!(within(gaps[1], 600, 1000), "not doubled: {gaps:?}");
        assert!(within(gaps[2], 600, 1000), "not capped: {gaps:?}");
        assert!(gaps[3] >= Duration::from_secs(2), "renewed early: {gaps:?}");
        assert!(within(gaps[4], 300, 550), "backoff not reset: {gaps:?}");
    }

    #[tokio::test]
    async fn stopping_removes_the_mapping_and_renews_no_more() {
        let router = granting("81.2.69.160").await;

        let lease = Duration::from_secs(2);
        let mapper = mapper_at(&[router.http_addr()], quick(lease));
        settle(&mapper, |s| matches!(s, Status::Mapped { .. })).await;
        let status = mapper.status();
        mapper.stop().await;

        let deletes = deletes(&router).await;
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].external_port, PORT);
        assert_eq!(deletes[0].protocol, "TCP");
        assert_eq!(*status.borrow(), Status::Off);

        let before = adds(&router).await.len();
        tokio::time::sleep(lease).await;
        assert_eq!(
            adds(&router).await.len(),
            before,
            "renewed after being stopped"
        );
    }

    /// Stopped once the router has the first add but before its answer is
    /// back. The router has made the mapping; dropping the add there would
    /// leave it with nobody to remove it.
    #[tokio::test]
    async fn stopping_while_the_first_add_is_unanswered_removes_the_mapping_once_answered() {
        let router = granting("81.2.69.160").await;
        let addr = answers_late(1, Duration::from_millis(700), router.http_addr()).await;

        let mapper = mapper_at(&[addr], quick(LEASE));
        at_least_adds(&router, 1).await;
        let status = mapper.status();
        assert_eq!(
            *status.borrow(),
            Status::Searching,
            "answered before the stop"
        );
        mapper.stop().await;

        assert_eq!(deletes(&router).await.len(), 1);
        assert_eq!(
            adds(&router).await.len(),
            1,
            "the answer settled it, so nothing needed asking again"
        );
        assert_eq!(*status.borrow(), Status::Off);
    }

    /// The add timed out, so whether the router made the mapping is unknown,
    /// and so is whether it was saying the port is another device's. Stopping
    /// asks again, and removes only on a grant.
    #[tokio::test]
    async fn stopping_after_an_unanswered_add_asks_again_and_removes_only_its_own() {
        for (answer, removed) in [
            (Responder::success().build(), 1),
            (Responder::error(718, "ConflictInMappingEntry"), 0),
        ] {
            let router = router().await;
            router.mock(Action::add_port_mapping(), answer).await;
            router
                .mock(Action::delete_port_mapping(), Responder::success())
                .await;
            let addr = answers_late(1, Duration::from_secs(1), router.http_addr()).await;

            let timings = Timings {
                request: Duration::from_millis(300),
                ..quick(LEASE)
            };
            let mapper = mapper_at(&[addr], timings);
            let status = settle(&mapper, |s| *s != Status::Searching).await;
            assert_eq!(
                status,
                Status::Failed(Failure::Other {
                    detail: "The router stopped answering.".into()
                })
            );
            mapper.stop().await;

            assert_eq!(adds(&router).await.len(), 2, "asked again");
            assert_eq!(
                deletes(&router).await.len(),
                removed,
                "removed {removed} after the second answer"
            );
        }
    }

    /// A router slow to answer everything, not only the first add: the add
    /// goes unanswered, and on the way out the second ask is answered late
    /// too — within what any request is given, but after what the way out
    /// used to allow. The mapping it confirms still comes down.
    #[tokio::test]
    async fn a_slow_router_still_has_its_mapping_removed_on_the_way_out() {
        let router = router().await;
        router
            .mock(Action::add_port_mapping(), Responder::success())
            .await;
        router
            .mock(Action::delete_port_mapping(), Responder::success())
            .await;
        let timings = Timings {
            request: Duration::from_millis(1500),
            ..quick(LEASE)
        };
        let addr = answers_slowly(
            Duration::from_secs(3),
            Duration::from_millis(900),
            router.http_addr(),
        )
        .await;

        let mapper = mapper_at(&[addr], timings);
        let status = settle(&mapper, |s| *s != Status::Searching).await;
        assert!(
            matches!(status, Status::Failed(Failure::Other { .. })),
            "{status:?}"
        );
        mapper.stop().await;

        assert_eq!(adds(&router).await.len(), 2, "asked again");
        assert_eq!(
            deletes(&router).await.len(),
            1,
            "the confirmed mapping was left in place"
        );
    }

    #[tokio::test]
    async fn a_router_that_refuses_is_reported_as_refused_and_left_alone() {
        let router = router().await;
        router
            .mock(
                Action::add_port_mapping(),
                Responder::error(606, "Action not authorized"),
            )
            .await;
        router
            .mock(Action::delete_port_mapping(), Responder::success())
            .await;

        let mapper = mapper_at(&[router.http_addr()], quick(LEASE));
        let status = settle(&mapper, |s| *s != Status::Searching).await;
        assert_eq!(status, Status::Failed(Failure::Refused));

        mapper.stop().await;
        assert!(deletes(&router).await.is_empty());
        // A refusal is a definite answer: nothing to ask again on the way out.
        assert_eq!(adds(&router).await.len(), 1);
    }

    /// The port was ours, a router reboot wiped it, and another device on the
    /// LAN took it before the renewal. Removing "our" mapping now would delete
    /// theirs.
    #[tokio::test]
    async fn a_port_another_device_has_taken_is_never_removed_on_its_behalf() {
        let router = router().await;
        router
            .mock_with_times(Action::add_port_mapping(), Responder::success(), 1)
            .await;
        router
            .mock(
                Action::add_port_mapping(),
                Responder::error(718, "ConflictInMappingEntry"),
            )
            .await;
        router
            .mock(Action::delete_port_mapping(), Responder::success())
            .await;
        router
            .mock(
                Action::GetExternalIPAddress,
                Responder::success().with_external_ip(ip("81.2.69.160")),
            )
            .await;

        let mapper = mapper_at(&[router.http_addr()], quick(Duration::from_secs(2)));
        let status = settle(&mapper, |s| matches!(s, Status::Failed(_))).await;
        assert_eq!(status, Status::Failed(Failure::PortInUse));
        assert!(adds(&router).await.len() >= 2, "the first add was granted");

        mapper.stop().await;
        assert!(deletes(&router).await.is_empty());
    }

    /// A permanent mapping outlives anything that skips the removal, so a
    /// router that turns down a lease is not offered the permanent mapping it
    /// asks for — even though, as here, it would take one.
    #[tokio::test]
    async fn a_router_that_only_takes_permanent_mappings_is_left_without_one() {
        let router = router().await;
        router
            .mock_with_times(
                Action::add_port_mapping(),
                Responder::error(725, "OnlyPermanentLeasesSupported"),
                1,
            )
            .await;
        // mock-igd cannot match on the lease, so this stands in for the
        // permanent add the router would grant.
        router
            .mock(Action::add_port_mapping(), Responder::success())
            .await;
        router
            .mock(Action::delete_port_mapping(), Responder::success())
            .await;
        router
            .mock(
                Action::GetExternalIPAddress,
                Responder::success().with_external_ip(ip("81.2.69.160")),
            )
            .await;

        let mapper = mapper_at(&[router.http_addr()], quick(LEASE));
        let status = settle(&mapper, |s| *s != Status::Searching).await;
        assert_eq!(status, Status::Failed(Failure::PermanentOnly));

        mapper.stop().await;
        let leases: Vec<u32> = adds(&router)
            .await
            .iter()
            .map(|a| a.lease_duration)
            .collect();
        assert_eq!(leases, [3600], "asked for a lease, and nothing else");
        assert!(deletes(&router).await.is_empty());
    }

    #[tokio::test]
    async fn any_other_refusal_carries_the_routers_own_words() {
        let router = router().await;
        router
            .mock(
                Action::add_port_mapping(),
                Responder::error(402, "Invalid Args"),
            )
            .await;

        let mapper = mapper_at(&[router.http_addr()], quick(LEASE));
        let status = settle(&mapper, |s| *s != Status::Searching).await;
        let Status::Failed(Failure::Other { detail }) = status else {
            panic!("expected Other, got {status:?}");
        };
        assert!(
            detail.contains("402") && detail.contains("Invalid Args"),
            "{detail}"
        );

        mapper.stop().await;
    }

    /// igd-next has no timeout of its own, so each request that can hang is
    /// made to: the add, and the address read after a granted add.
    #[tokio::test]
    async fn a_router_that_never_answers_fails_instead_of_hanging() {
        for answered in [0, 1] {
            let router = granting("81.2.69.160").await;
            let addr = hangs_after(answered, router.http_addr()).await;

            let timings = Timings {
                request: Duration::from_millis(300),
                ..quick(LEASE)
            };
            let mapper = mapper_at(&[addr], timings);
            let status = settle(&mapper, |s| *s != Status::Searching).await;
            assert_eq!(
                status,
                Status::Failed(Failure::Other {
                    detail: "The router stopped answering.".into()
                }),
                "after {answered} answered"
            );
            assert_eq!(adds(&router).await.len(), answered);

            timeout(Duration::from_secs(2), mapper.stop())
                .await
                .expect("stopping hung");
        }
    }

    /// A mapping made on a mock router on loopback that answers the add and
    /// the address read, then never answers again — so stopping the mapper
    /// waits out a request's timeout, which comes with it. For tests elsewhere
    /// of what must not wait on the router; keep the router alongside.
    pub(crate) async fn a_mapping_whose_removal_hangs() -> (PortMapper, MockIgdServer, Duration) {
        let router = granting("81.2.69.160").await;
        let addr = hangs_after(2, router.http_addr()).await;
        let timings = quick(LEASE);
        let mapper = mapper_at(&[addr], timings);
        settle(&mapper, |s| matches!(s, Status::Mapped { .. })).await;
        (mapper, router, timings.request)
    }

    #[tokio::test]
    async fn stopping_gives_up_on_a_router_that_hangs_on_removal() {
        let router = granting("81.2.69.160").await;
        // The add and the address read are answered; the removal is not.
        let addr = hangs_after(2, router.http_addr()).await;

        let timings = quick(LEASE);
        let mapper = mapper_at(&[addr], timings);
        settle(&mapper, |s| matches!(s, Status::Mapped { .. })).await;

        let started = Instant::now();
        timeout(Duration::from_secs(5), mapper.stop())
            .await
            .expect("stopping waited on the router");
        // Long enough to have tried the removal, not so long it waited it out.
        let took = started.elapsed();
        assert!(took >= timings.request, "gave up after only {took:?}");
        assert!(took < timings.request * 2, "took {took:?}");
    }

    /// A router that reboots can bring its control endpoint back on another
    /// port, or be slow to come back at all. Either way the next attempt
    /// searches again rather than knocking on the old door forever.
    #[tokio::test]
    async fn a_router_that_stops_answering_is_searched_for_again() {
        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let silent_addr = silent.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                held.push(silent.accept().await.unwrap());
            }
        });

        for gone in [closed_port().await, silent_addr] {
            let router = granting("81.2.69.160").await;
            let timings = Timings {
                request: Duration::from_millis(300),
                ..quick(Duration::from_secs(2))
            };
            let mapper = mapper_at(&[gone, router.http_addr()], timings);

            let status = settle(&mapper, |s| *s != Status::Searching).await;
            assert!(
                matches!(status, Status::Failed(Failure::Other { .. })),
                "{status:?}"
            );
            settle(&mapper, |s| matches!(s, Status::Mapped { .. })).await;
            mapper.stop().await;
        }
    }

    fn known(addr: SocketAddr) -> Finder {
        Finder::Known(std::sync::Mutex::new(vec![gateway_at(addr)]))
    }

    /// For manual router setup, where the admin was promised the router is
    /// left alone.
    #[tokio::test]
    async fn reading_the_external_address_asks_for_it_and_changes_nothing() {
        let router = granting("81.2.69.160").await;

        let read = read_external_ip_from(&known(router.http_addr()), quick(LEASE)).await;
        assert_eq!(read, Ok(ip("81.2.69.160")));

        let asked: Vec<String> = router
            .received_requests()
            .await
            .into_iter()
            .map(|r| r.action_name)
            .collect();
        assert_eq!(asked, ["GetExternalIPAddress"]);
    }

    #[tokio::test]
    async fn an_external_address_that_cannot_be_read_says_why_instead_of_hanging() {
        let refusing = router().await;
        refusing
            .mock(
                Action::GetExternalIPAddress,
                Responder::error(606, "Action not authorized"),
            )
            .await;
        let hung = hangs_after(0, refusing.http_addr()).await;
        let quiet = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        let timings = Timings {
            request: Duration::from_millis(300),
            ..quick(LEASE)
        };
        for (finder, expected) in [
            (known(refusing.http_addr()), Failure::Refused),
            (
                known(hung),
                Failure::Other {
                    detail: "The router stopped answering.".into(),
                },
            ),
            (
                Finder::Search {
                    bind: "127.0.0.1:0".parse().unwrap(),
                    target: quiet.local_addr().unwrap(),
                },
                Failure::NoGateway,
            ),
        ] {
            let read = timeout(
                Duration::from_secs(5),
                read_external_ip_from(&finder, timings),
            )
            .await
            .expect("the read hung");
            assert_eq!(read, Err(expected));
        }
    }

    #[tokio::test]
    async fn nothing_answering_the_search_is_no_gateway_after_one_retry() {
        // Stands where the multicast group would be, and never replies.
        let quiet = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let finder = Finder::Search {
            bind: "127.0.0.1:0".parse().unwrap(),
            target: quiet.local_addr().unwrap(),
        };

        let mapper = PortMapper::spawn(PORT, finder, quick(LEASE));
        let status = settle(&mapper, |s| *s != Status::Searching).await;
        assert_eq!(status, Status::Failed(Failure::NoGateway));

        let mut searches = 0;
        let mut buf = [0u8; 1500];
        while let Ok((n, _)) = quiet.try_recv_from(&mut buf) {
            assert!(buf[..n].starts_with(b"M-SEARCH"));
            searches += 1;
        }
        assert_eq!(searches, 2, "one search and one retry");

        mapper.stop().await;
    }
}
