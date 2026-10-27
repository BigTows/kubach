//! Which Prometheus a context uses (ADR 0009): the user's choice from the
//! settings ([`PrometheusSource`]) against the remembered search
//! ([`PrometheusDiscovery`] in the application state).
//!
//! The cluster is searched only for [`PrometheusSource::Auto`] without a
//! remembered search; after that the remembered result is used, also
//! across restarts. A remembered server that stops answering is not
//! replaced by a new search behind the user's back. A server Auto found
//! only outside the well-known namespaces is not used until the user
//! confirms it ([`Plan::Confirm`]).

use super::{Endpoint, PrometheusError};
use crate::settings::PrometheusSource;
use crate::state::PrometheusDiscovery;

/// What to do for a context's Prometheus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    /// Off: no search, no requests.
    Off,
    /// Auto and the cluster was never searched: search it.
    Discover,
    /// Auto, and the search found nothing.
    NotFound,
    /// Manual, but no address is typed.
    NoAddress,
    /// The chosen address cannot be parsed.
    Invalid(String),
    /// Auto, and the search found servers only outside
    /// [`WELL_KNOWN_NAMESPACES`](super::WELL_KNOWN_NAMESPACES): this, the most likely one, is not
    /// connected until the user confirms it (picks it as
    /// [`PrometheusSource::Found`]). Anyone who can create a Service there
    /// could otherwise feed made-up usage to the tables (#200).
    Confirm(Endpoint),
    /// Connect to this server.
    Use(Endpoint),
}

impl Plan {
    /// The plan for a context: `source` and the typed `manual` address from
    /// the settings, `discovery` from the application state.
    pub fn of(
        source: &PrometheusSource,
        manual: Option<&str>,
        discovery: Option<&PrometheusDiscovery>,
    ) -> Self {
        let address = match source {
            PrometheusSource::Off => return Self::Off,
            PrometheusSource::Auto => match discovery {
                None => return Self::Discover,
                Some(discovery) => return Self::auto(&discovery.found),
            },
            PrometheusSource::Found(address) => address.as_str(),
            PrometheusSource::Manual => match manual.map(str::trim).filter(|a| !a.is_empty()) {
                Some(address) => address,
                None => return Self::NoAddress,
            },
        };
        match Endpoint::parse(address) {
            Ok(endpoint) => Self::Use(endpoint),
            Err(_) => Self::Invalid(address.trim().to_owned()),
        }
    }

    /// Auto with a remembered search: the first server found in a
    /// well-known namespace (a search remembered before #200 may list
    /// others first), else the first one, to be confirmed.
    fn auto(found: &[String]) -> Self {
        let endpoints: Vec<Endpoint> = found
            .iter()
            .filter_map(|address| Endpoint::parse(address).ok())
            .collect();
        if let Some(usual) = endpoints.iter().find(|e| e.unusual_namespace().is_none()) {
            return Self::Use(usual.clone());
        }
        match endpoints.into_iter().next() {
            Some(unusual) => Self::Confirm(unusual),
            None => match found.first() {
                Some(address) => Self::Invalid(address.trim().to_owned()),
                None => Self::NotFound,
            },
        }
    }
}

/// Servers the user can pick for a context: the ones the last search found,
/// plus a picked one the last search no longer found, so the choice stays
/// visible.
pub fn choices(source: &PrometheusSource, discovery: Option<&PrometheusDiscovery>) -> Vec<String> {
    let mut choices: Vec<String> = discovery
        .map(|discovery| discovery.found.clone())
        .unwrap_or_default();
    if let PrometheusSource::Found(picked) = source
        && !choices.contains(picked)
    {
        choices.push(picked.clone());
    }
    choices
}

/// The remembered form of a search's answer: the servers that answered, in
/// order, as `namespace/service:port` (what [`Endpoint::parse`] reads back).
/// Only "not found" is a result; a failed search (cluster unreachable) is
/// not remembered.
pub fn remembered(
    found: Result<&[Endpoint], &PrometheusError>,
    at: u64,
) -> Option<PrometheusDiscovery> {
    match found {
        Ok(endpoints) => Some(PrometheusDiscovery {
            found: endpoints.iter().map(Endpoint::describe).collect(),
            at,
        }),
        Err(PrometheusError::NotFound) => Some(PrometheusDiscovery {
            found: Vec::new(),
            at,
        }),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery(found: &[&str]) -> PrometheusDiscovery {
        PrometheusDiscovery {
            found: found.iter().map(|&s| s.to_owned()).collect(),
            at: 1,
        }
    }

    fn service(namespace: &str, name: &str, port: &str) -> Endpoint {
        Endpoint::Service {
            namespace: namespace.into(),
            name: name.into(),
            port: port.into(),
            https: false,
        }
    }

    #[test]
    fn auto_searches_once_then_uses_the_remembered_result() {
        let auto = PrometheusSource::Auto;
        assert_eq!(Plan::of(&auto, None, None), Plan::Discover);
        assert_eq!(Plan::of(&auto, None, Some(&discovery(&[]))), Plan::NotFound);
        let found = discovery(&["monitoring/prometheus-operated:web", "m/other:9090"]);
        assert_eq!(
            Plan::of(&auto, None, Some(&found)),
            Plan::Use(service("monitoring", "prometheus-operated", "web"))
        );
        // A typed address does not matter while the source is Auto.
        assert_eq!(
            Plan::of(&auto, Some("http://localhost:9090"), Some(&found)),
            Plan::Use(service("monitoring", "prometheus-operated", "web"))
        );
    }

    #[test]
    fn auto_asks_before_using_a_server_from_an_unusual_namespace() {
        let auto = PrometheusSource::Auto;
        let elsewhere = discovery(&["aaa-dev/prometheus-operated:web", "team/prometheus:9090"]);
        assert_eq!(
            Plan::of(&auto, None, Some(&elsewhere)),
            Plan::Confirm(service("aaa-dev", "prometheus-operated", "web"))
        );
        // Confirmed: picked as found, then used.
        let picked = PrometheusSource::Found("aaa-dev/prometheus-operated:web".into());
        assert_eq!(
            Plan::of(&picked, None, Some(&elsewhere)),
            Plan::Use(service("aaa-dev", "prometheus-operated", "web"))
        );
        // A search remembered before well-known namespaces ranked first.
        let old = discovery(&["aaa-dev/prometheus-operated:web", "monitoring/prom:web"]);
        assert_eq!(
            Plan::of(&auto, None, Some(&old)),
            Plan::Use(service("monitoring", "prom", "web"))
        );
    }

    #[test]
    fn off_neither_searches_nor_connects() {
        let found = discovery(&["m/p:web"]);
        for discovery in [None, Some(&found)] {
            assert_eq!(
                Plan::of(&PrometheusSource::Off, Some("m/p:web"), discovery),
                Plan::Off
            );
        }
    }

    #[test]
    fn manual_and_picked_servers_never_search() {
        let manual = PrometheusSource::Manual;
        assert_eq!(
            Plan::of(&manual, Some(" http://prom.local:9090/ "), None),
            Plan::Use(Endpoint::Url("http://prom.local:9090".into()))
        );
        assert_eq!(Plan::of(&manual, None, None), Plan::NoAddress);
        assert_eq!(Plan::of(&manual, Some("  "), None), Plan::NoAddress);
        assert_eq!(
            Plan::of(&manual, Some("not an address"), None),
            Plan::Invalid("not an address".into())
        );

        let picked = PrometheusSource::Found("m/other:9090".into());
        let found = discovery(&["m/p:web", "m/other:9090"]);
        assert_eq!(
            Plan::of(&picked, None, Some(&found)),
            Plan::Use(service("m", "other", "9090"))
        );
        // Still used when the last search did not find it, or never ran.
        assert_eq!(
            Plan::of(&picked, None, None),
            Plan::Use(service("m", "other", "9090"))
        );
    }

    #[test]
    fn choices_keep_a_picked_server_the_search_lost() {
        let found = discovery(&["m/p:web", "m/q:web"]);
        assert_eq!(
            choices(&PrometheusSource::Auto, Some(&found)),
            ["m/p:web", "m/q:web"]
        );
        assert_eq!(
            choices(&PrometheusSource::Found("m/q:web".into()), Some(&found)),
            ["m/p:web", "m/q:web"]
        );
        assert_eq!(
            choices(&PrometheusSource::Found("x/y:1".into()), Some(&found)),
            ["m/p:web", "m/q:web", "x/y:1"]
        );
        assert!(choices(&PrometheusSource::Manual, None).is_empty());
    }

    #[test]
    fn https_ports_read_back_as_https() {
        let https = Endpoint::Service {
            namespace: "m".into(),
            name: "p".into(),
            port: "https".into(),
            https: true,
        };
        assert_eq!(Endpoint::parse(&https.describe()).unwrap(), https);
    }

    #[test]
    fn only_finished_searches_are_remembered() {
        let found = [service("m", "p", "web"), service("m", "q", "9090")];
        assert_eq!(
            remembered(Ok(&found), 7),
            Some(PrometheusDiscovery {
                found: discovery(&["m/p:web", "m/q:9090"]).found,
                at: 7
            })
        );
        assert_eq!(
            remembered(Err(&PrometheusError::NotFound), 7),
            Some(PrometheusDiscovery {
                found: discovery(&[]).found,
                at: 7
            })
        );
        assert_eq!(
            remembered(Err(&PrometheusError::Request("refused".into())), 7),
            None
        );
    }
}
