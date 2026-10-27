//! Who owns a node shell pod and whether a sweep may delete it (#351).
//!
//! Several Kubach processes — another window, another user, another
//! machine — may use node shells in one cluster. Every pod therefore says
//! who created it: the installation id (a random UUID kept in the config
//! directory), the process id and the host, in annotations, plus a label
//! with a short hash of the installation id for a selector. While its tab
//! is open, the pod's [`HEARTBEAT_ANNOTATION`] is refreshed every
//! [`HEARTBEAT_INTERVAL`].
//!
//! [`decide`] is the whole sweep policy: a pod of this installation whose
//! process is gone is ours to delete, a pod of anyone else only when its
//! heartbeat stopped [`FOREIGN_SWEEP_AFTER`] ago; a live pod of someone
//! else is never deleted.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::jiff::{SignedDuration, Timestamp};
use sha2::{Digest as _, Sha256};

/// Installation id of the Kubach that created the pod.
pub const INSTALL_ANNOTATION: &str = "kubach.dev/owner-install";
/// Process id of the Kubach that created the pod.
pub const PID_ANNOTATION: &str = "kubach.dev/owner-pid";
/// Host of the Kubach that created the pod.
pub const HOST_ANNOTATION: &str = "kubach.dev/owner-host";
/// When the tab last said it is alive (RFC 3339).
pub const HEARTBEAT_ANNOTATION: &str = "kubach.dev/heartbeat";
/// Short hash of the installation id ([`install_hash`]).
pub const OWNER_LABEL: &str = "kubach.dev/owner";

/// How often an open tab refreshes its heartbeat.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
/// A heartbeat older than this is stale: its tab is most likely gone.
pub const STALE_AFTER: SignedDuration = SignedDuration::from_mins(5);
/// A pod of someone else is deleted without asking only when its
/// heartbeat is this old (ADR 0004): far beyond missed heartbeats, a
/// sleeping laptop's short nap and clock skew.
pub const FOREIGN_SWEEP_AFTER: SignedDuration = SignedDuration::from_mins(15);
/// A pod of this very process that is not (yet) registered as live is
/// left alone this long: it may be one being created right now.
pub const OWN_GRACE: SignedDuration = SignedDuration::from_mins(2);

/// File with the installation id in the config directory.
pub const INSTALL_ID_FILE: &str = "install-id";

/// The Kubach process creating node shell pods.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub install_id: String,
    pub pid: u32,
    pub host: String,
}

impl Owner {
    /// This process, with the installation id of the config directory.
    pub fn current() -> &'static Owner {
        static CURRENT: OnceLock<Owner> = OnceLock::new();
        CURRENT.get_or_init(|| Owner {
            install_id: install_id(),
            pid: std::process::id(),
            host: hostname(),
        })
    }

    /// Labels of a pod of this owner.
    pub fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([(OWNER_LABEL.to_owned(), install_hash(&self.install_id))])
    }

    /// Annotations of a pod of this owner whose heartbeat is `heartbeat`.
    pub fn annotations(&self, heartbeat: Timestamp) -> BTreeMap<String, String> {
        BTreeMap::from([
            (INSTALL_ANNOTATION.to_owned(), self.install_id.clone()),
            (PID_ANNOTATION.to_owned(), self.pid.to_string()),
            (HOST_ANNOTATION.to_owned(), self.host.clone()),
            (HEARTBEAT_ANNOTATION.to_owned(), heartbeat.to_string()),
        ])
    }

    /// Selects the node shell pods of this installation.
    pub fn selector(&self) -> String {
        format!(
            "{},{OWNER_LABEL}={}",
            super::label_selector(),
            install_hash(&self.install_id)
        )
    }
}

/// The first 16 hex digits of the SHA-256 of `install_id`: a valid label
/// value whatever the id is.
pub fn install_hash(install_id: &str) -> String {
    Sha256::digest(install_id.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The installation id from [`INSTALL_ID_FILE`] in the config directory
/// (`KUBACH_CONFIG_DIR` overrides it), created on first use. Without a
/// usable config directory the id lives only as long as the process:
/// its pods then look foreign to the next run, and are swept by heartbeat.
fn install_id() -> String {
    crate::settings::config_dir()
        .and_then(|dir| load_or_create_id(&dir))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

/// Reads the id in `dir`, or writes a new one; `None` when neither works.
pub fn load_or_create_id(dir: &Path) -> Option<String> {
    let path = dir.join(INSTALL_ID_FILE);
    if let Ok(text) = fs::read_to_string(&path)
        && let Ok(id) = uuid::Uuid::parse_str(text.trim())
    {
        return Some(id.to_string());
    }
    let id = uuid::Uuid::new_v4().to_string();
    fs::create_dir_all(dir).ok()?;
    fs::write(&path, format!("{id}\n")).ok()?;
    Some(id)
}

/// This machine's name, `unknown` when the OS does not tell.
fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; the name is cut and
        // NUL-terminated by the OS (or not, which the search handles).
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            let name = String::from_utf8_lossy(&buf[..end]).trim().to_owned();
            if !name.is_empty() {
                return name;
            }
        }
    }
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|var| std::env::var(var).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Why a sweep leaves a pod alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Already being deleted.
    Deleting,
    /// A tab of this process uses it.
    LiveHere,
    /// Created by this process moments ago, not registered yet.
    JustCreated,
    /// Its heartbeat (or creation) is recent: someone uses it.
    Alive,
    /// Stale, but not for [`FOREIGN_SWEEP_AFTER`] yet.
    RecentlyStale,
    /// Neither heartbeat nor creation time: nothing to judge by.
    Unknown,
}

/// Why a sweep deletes a pod.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delete {
    /// Of this installation on this host, and its process is gone (or it
    /// is this process, which no longer uses it).
    OwnOrphan,
    /// Of someone else, heartbeat older than [`FOREIGN_SWEEP_AFTER`].
    ForeignStale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Keep(Keep),
    Delete(Delete),
}

/// Whether a sweep by `me` at `now` deletes the node shell `pod`.
/// `live_here`: a tab of this process uses it; `pid_alive`: whether a
/// local process id runs.
///
/// | Pod                                         | Verdict                    |
/// |---------------------------------------------|----------------------------|
/// | being deleted, or used by a tab here        | keep                       |
/// | this install and host, this pid             | delete after [`OWN_GRACE`] |
/// | this install and host, pid not running      | delete                     |
/// | anything else, heartbeat < 15 min           | keep                       |
/// | anything else, heartbeat ≥ 15 min           | delete                     |
///
/// "Anything else" is another installation, another host, a running pid
/// of this installation (a second process), or a pod without owner
/// annotations (a Kubach before #351); without a heartbeat its creation
/// time counts.
pub fn decide(
    pod: &Pod,
    me: &Owner,
    now: Timestamp,
    live_here: bool,
    pid_alive: impl Fn(u32) -> bool,
) -> Verdict {
    if pod.metadata.deletion_timestamp.is_some() {
        return Verdict::Keep(Keep::Deleting);
    }
    if live_here {
        return Verdict::Keep(Keep::LiveHere);
    }
    let annotations = pod.metadata.annotations.as_ref();
    let get = |key: &str| annotations.and_then(|a| a.get(key)).map(String::as_str);
    let heartbeat = get(HEARTBEAT_ANNOTATION)
        .and_then(|value| value.parse::<Timestamp>().ok())
        .or_else(|| pod.metadata.creation_timestamp.as_ref().map(|t| t.0));
    let age = heartbeat.map(|at| now.duration_since(at));

    let ours = get(INSTALL_ANNOTATION) == Some(me.install_id.as_str())
        && get(HOST_ANNOTATION) == Some(me.host.as_str());
    let pid = get(PID_ANNOTATION).and_then(|pid| pid.trim().parse::<u32>().ok());
    if ours && let Some(pid) = pid {
        if pid == me.pid {
            return match age {
                Some(age) if age < OWN_GRACE => Verdict::Keep(Keep::JustCreated),
                _ => Verdict::Delete(Delete::OwnOrphan),
            };
        }
        if !pid_alive(pid) {
            return Verdict::Delete(Delete::OwnOrphan);
        }
    }
    match age {
        None => Verdict::Keep(Keep::Unknown),
        Some(age) if age >= FOREIGN_SWEEP_AFTER => Verdict::Delete(Delete::ForeignStale),
        Some(age) if age >= STALE_AFTER => Verdict::Keep(Keep::RecentlyStale),
        Some(_) => Verdict::Keep(Keep::Alive),
    }
}

#[cfg(test)]
mod tests {
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use serde_json::json;

    use super::*;

    fn me() -> Owner {
        Owner {
            install_id: "11111111-1111-4111-8111-111111111111".into(),
            pid: 100,
            host: "laptop".into(),
        }
    }

    fn other() -> Owner {
        Owner {
            install_id: "22222222-2222-4222-8222-222222222222".into(),
            ..me()
        }
    }

    fn now() -> Timestamp {
        "2026-10-25T12:00:00Z".parse().unwrap()
    }

    fn ago(mins: i64) -> Timestamp {
        now() - SignedDuration::from_mins(mins)
    }

    fn pod(owner: Option<&Owner>, heartbeat: Option<Timestamp>, created: Timestamp) -> Pod {
        let mut annotations = owner.map(|o| o.annotations(now())).unwrap_or_default();
        annotations.remove(HEARTBEAT_ANNOTATION);
        if let Some(at) = heartbeat {
            annotations.insert(HEARTBEAT_ANNOTATION.into(), at.to_string());
        }
        let mut pod: Pod = serde_json::from_value(json!({"metadata": {"name": "p"}})).unwrap();
        pod.metadata.annotations = Some(annotations);
        pod.metadata.creation_timestamp = Some(Time(created));
        pod
    }

    fn run(pod: &Pod, live_here: bool, alive: bool) -> Verdict {
        decide(pod, &me(), now(), live_here, |_| alive)
    }

    #[test]
    fn own_pods() {
        let own = |pid: u32, heartbeat: i64| {
            let owner = Owner { pid, ..me() };
            pod(Some(&owner), Some(ago(heartbeat)), ago(60))
        };
        // A tab here uses it: kept whatever else.
        assert_eq!(
            run(&own(100, 60), true, false),
            Verdict::Keep(Keep::LiveHere)
        );
        // This pid, no tab: an orphan once past the creation grace.
        assert_eq!(
            run(&own(100, 1), false, true),
            Verdict::Keep(Keep::JustCreated)
        );
        assert_eq!(
            run(&own(100, 3), false, true),
            Verdict::Delete(Delete::OwnOrphan)
        );
        // A previous run (pid gone): an orphan even with a fresh heartbeat.
        assert_eq!(
            run(&own(7, 0), false, false),
            Verdict::Delete(Delete::OwnOrphan)
        );
        // A second process of this installation that runs: like a foreign pod.
        assert_eq!(run(&own(7, 0), false, true), Verdict::Keep(Keep::Alive));
        assert_eq!(
            run(&own(7, 20), false, true),
            Verdict::Delete(Delete::ForeignStale)
        );
    }

    #[test]
    fn same_install_on_another_host_is_foreign() {
        let owner = Owner {
            host: "desktop".into(),
            pid: 7,
            ..me()
        };
        let fresh = pod(Some(&owner), Some(ago(1)), ago(60));
        assert_eq!(run(&fresh, false, false), Verdict::Keep(Keep::Alive));
    }

    #[test]
    fn foreign_pods_go_only_when_long_stale() {
        let foreign = |heartbeat: i64| pod(Some(&other()), Some(ago(heartbeat)), ago(120));
        assert_eq!(run(&foreign(0), false, false), Verdict::Keep(Keep::Alive));
        assert_eq!(run(&foreign(4), false, false), Verdict::Keep(Keep::Alive));
        assert_eq!(
            run(&foreign(5), false, false),
            Verdict::Keep(Keep::RecentlyStale)
        );
        assert_eq!(
            run(&foreign(14), false, false),
            Verdict::Keep(Keep::RecentlyStale)
        );
        assert_eq!(
            run(&foreign(15), false, false),
            Verdict::Delete(Delete::ForeignStale)
        );
        // A heartbeat from the future (clock skew) is alive.
        let skewed = pod(
            Some(&other()),
            Some(now() + SignedDuration::from_mins(3)),
            ago(1),
        );
        assert_eq!(run(&skewed, false, false), Verdict::Keep(Keep::Alive));
    }

    #[test]
    fn pods_without_annotations_go_by_creation_time() {
        let old = |created: i64| pod(None, None, ago(created));
        assert_eq!(run(&old(3), false, false), Verdict::Keep(Keep::Alive));
        assert_eq!(
            run(&old(10), false, false),
            Verdict::Keep(Keep::RecentlyStale)
        );
        assert_eq!(
            run(&old(30), false, false),
            Verdict::Delete(Delete::ForeignStale)
        );
        // Garbage heartbeat: creation time counts.
        let mut garbage = pod(Some(&other()), None, ago(30));
        garbage
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(HEARTBEAT_ANNOTATION.into(), "yesterday".into());
        assert_eq!(
            run(&garbage, false, false),
            Verdict::Delete(Delete::ForeignStale)
        );
        // Nothing at all to judge by: kept.
        let mut bare = pod(None, None, ago(30));
        bare.metadata.creation_timestamp = None;
        assert_eq!(run(&bare, false, false), Verdict::Keep(Keep::Unknown));
        // A pid that is not a number: not ours to judge by pid.
        let mut odd = pod(Some(&me()), Some(ago(1)), ago(30));
        odd.metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert(PID_ANNOTATION.into(), "x".into());
        assert_eq!(run(&odd, false, false), Verdict::Keep(Keep::Alive));
    }

    #[test]
    fn pods_being_deleted_are_kept() {
        let mut gone = pod(Some(&other()), Some(ago(60)), ago(60));
        gone.metadata.deletion_timestamp = Some(Time(now()));
        assert_eq!(run(&gone, false, false), Verdict::Keep(Keep::Deleting));
    }

    #[test]
    fn owner_metadata_is_valid() {
        let owner = me();
        let hash = install_hash(&owner.install_id);
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(owner.labels()[OWNER_LABEL], hash);
        let annotations = owner.annotations(now());
        assert_eq!(annotations[PID_ANNOTATION], "100");
        assert_eq!(annotations[HEARTBEAT_ANNOTATION], "2026-10-25T12:00:00Z");
        assert!(
            owner
                .selector()
                .ends_with(&format!(",{OWNER_LABEL}={hash}"))
        );
    }

    #[test]
    fn install_id_is_stable_per_directory() {
        let dir = std::env::temp_dir().join(format!("351-install-id-{}", uuid::Uuid::new_v4()));
        let first = load_or_create_id(&dir).unwrap();
        assert_eq!(load_or_create_id(&dir), Some(first.clone()));
        // A broken file is replaced.
        fs::write(dir.join(INSTALL_ID_FILE), "nonsense").unwrap();
        let second = load_or_create_id(&dir).unwrap();
        assert_ne!(second, first);
        assert_eq!(load_or_create_id(&dir), Some(second));
        fs::remove_dir_all(&dir).ok();
        assert!(!hostname().is_empty());
    }
}
