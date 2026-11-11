//! Turning the machine's network interfaces into download routes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;
use confluo_net::{Interface, Medium};

use crate::download::Route;
use crate::limit::Limiter;

/// Which interfaces a download may use.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Selection {
    /// Use every interface that has its own way out, unless a VPN is up.
    ///
    /// With a single such interface, or with a VPN tunnel carrying the default
    /// route, sockets are left unbound so the system's routing (and the VPN) is
    /// respected. Binding to the physical adapter would send traffic around the VPN.
    #[default]
    Auto,
    /// Leave routing to the operating system.
    System,
    /// Exactly these interfaces, by OS name or label. Listing a physical adapter
    /// here while a VPN is up sends that traffic outside the VPN; that is then
    /// the caller's explicit choice.
    Only(Vec<String>),
}

/// Limiters live as long as the process, so a per-network limit survives the
/// interface going away and coming back.
#[derive(Debug, Default)]
pub struct RouteBook {
    limiters: Mutex<HashMap<String, Arc<Limiter>>>,
}

impl RouteBook {
    pub fn limiter(&self, id: &str) -> Arc<Limiter> {
        self.limiters.lock().unwrap().entry(id.to_string()).or_default().clone()
    }

    fn route(&self, i: &Interface) -> Route {
        Route {
            id: i.name.clone(),
            label: i.label.clone(),
            bind: Some(i.bind_spec()),
            limiter: self.limiter(&i.name),
            uplink: i.gateways.first().map(ToString::to_string),
        }
    }

    fn system(&self) -> Route {
        Route { limiter: self.limiter("default"), ..Route::system() }
    }

    /// The routes to use right now, given the interfaces that are up.
    pub fn routes(&self, interfaces: &[Interface], selection: &Selection) -> Vec<Route> {
        match selection {
            Selection::System => vec![self.system()],
            Selection::Only(names) => interfaces
                .iter()
                .filter(|i| i.usable())
                .filter(|i| {
                    names.iter().any(|n| n.eq_ignore_ascii_case(&i.name) || n.eq_ignore_ascii_case(&i.label))
                })
                .map(|i| self.route(i))
                .collect(),
            Selection::Auto => {
                let outward: Vec<&Interface> =
                    interfaces.iter().filter(|i| i.usable() && i.has_gateway).collect();
                let vpn = outward.iter().any(|i| i.medium == Medium::Tunnel);
                if outward.len() < 2 || vpn {
                    return if interfaces.iter().any(Interface::usable) {
                        vec![self.system()]
                    } else {
                        Vec::new()
                    };
                }
                outward.into_iter().map(|i| self.route(i)).collect()
            }
        }
    }
}

/// A live list of routes that follows interfaces coming and going.
/// Must be called inside a Tokio runtime.
pub fn watch_routes(
    book: Arc<RouteBook>,
    selection: watch::Receiver<Selection>,
) -> watch::Receiver<Vec<Route>> {
    let mut interfaces = confluo_net::watch_changes();
    let mut selection = selection;
    let initial = book.routes(&interfaces.borrow(), &selection.borrow());
    let (tx, rx) = watch::channel(initial);
    tokio::spawn(async move {
        let mut selection_open = true;
        loop {
            tokio::select! {
                r = interfaces.changed() => if r.is_err() { break },
                r = selection.changed(), if selection_open => selection_open = r.is_ok(),
                () = tx.closed() => break,
            }
            let routes = book.routes(&interfaces.borrow_and_update(), &selection.borrow_and_update());
            if tx.send(routes).is_err() {
                break;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn iface(name: &str, medium: Medium, gw: Option<&str>) -> Interface {
        Interface {
            index: 1,
            name: name.into(),
            label: name.into(),
            description: String::new(),
            medium,
            up: true,
            has_gateway: gw.is_some(),
            gateways: gw.map(|g| g.parse::<IpAddr>().unwrap()).into_iter().collect(),
            link_bps: 0,
            addrs: vec!["10.0.0.2".parse().unwrap()],
            dns: Vec::new(),
        }
    }

    #[test]
    fn auto_selection() {
        let book = RouteBook::default();
        let eth = iface("eth", Medium::Ethernet, Some("10.0.0.1"));
        let wifi = iface("wifi", Medium::Wifi, Some("192.168.1.1"));
        let host_only = iface("vbox", Medium::Ethernet, None);
        let vpn = iface("wg0", Medium::Tunnel, Some("10.5.0.1"));

        // One way out: leave routing to the system.
        let r = book.routes(&[eth.clone(), host_only.clone()], &Selection::Auto);
        assert_eq!(r.len(), 1);
        assert!(r[0].bind.is_none());
        // Two ways out: bind to each; the host-only adapter is ignored.
        let r = book.routes(&[eth.clone(), wifi.clone(), host_only], &Selection::Auto);
        assert_eq!(r.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["eth", "wifi"]);
        assert!(r.iter().all(|r| r.bind.is_some()));
        // A VPN is up: never route around it on our own.
        let r = book.routes(&[eth.clone(), wifi.clone(), vpn], &Selection::Auto);
        assert!(r.len() == 1 && r[0].bind.is_none());
        // Nothing usable: no routes at all.
        assert!(book.routes(&[], &Selection::Auto).is_empty());
        // Explicit choice wins.
        let r = book.routes(&[eth, wifi], &Selection::Only(vec!["WIFI".into()]));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].uplink.as_deref(), Some("192.168.1.1"));
    }
}
