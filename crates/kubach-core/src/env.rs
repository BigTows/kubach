//! The environment Kubach works with: this process's environment with the
//! login shell's variables on top (ADR 0013).
//!
//! An app started from Finder, the Dock or a `.desktop` entry lacks the
//! shell's `PATH`, `KUBECONFIG` and the rest; [`crate::shell_env`] captures
//! them once at startup. They are not written into the process environment:
//! `setenv` while GPUI and tokio threads run races with C code calling
//! `getenv` (#307). Instead they are kept here ([`set_captured`]) and passed
//! explicitly: to every external program through [`crate::process`]
//! ([`apply`]), to the local terminal ([`vars_os`]), to kubeconfig exec
//! plugins ([`apply_to_exec`]), and to lookups such as [`var_os`],
//! [`path`] and [`home_dir`].

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use kube::config::{ExecConfig, Kubeconfig};

static CAPTURED: OnceLock<Vec<(OsString, OsString)>> = OnceLock::new();

/// Stores the login shell's variables; only the first call has an effect.
/// Returns whether these were stored.
pub fn set_captured(vars: Vec<(OsString, OsString)>) -> bool {
    CAPTURED.set(vars).is_ok()
}

/// The login shell's variables; empty before [`set_captured`] or when the
/// shell environment was not needed (started from a terminal, Windows).
pub fn captured() -> &'static [(OsString, OsString)] {
    CAPTURED.get().map_or(&[], Vec::as_slice)
}

/// `name` from the shell environment, else from this process.
pub fn var_os(name: &str) -> Option<OsString> {
    lookup(captured(), name).or_else(|| std::env::var_os(name))
}

/// [`var_os`] as text; `None` when unset or not valid Unicode.
pub fn var(name: &str) -> Option<String> {
    var_os(name)?.into_string().ok()
}

/// `PATH` ([`var_os`]); empty when unset.
pub fn path() -> OsString {
    var_os("PATH").unwrap_or_default()
}

/// The home folder: `HOME` of the shell environment when set, else
/// [`std::env::home_dir`].
pub fn home_dir() -> Option<PathBuf> {
    lookup(captured(), "HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(std::env::home_dir)
}

/// This process's environment with the shell's variables on top: what a
/// child started without [`Command`] (the local terminal) gets.
pub fn vars_os() -> Vec<(OsString, OsString)> {
    overlay(std::env::vars_os(), captured())
}

/// Passes the shell's variables to `cmd`, keeping the ones the caller set
/// or removed. A `PATH` passed this way is also where the program is looked
/// up (`std::process::Command` searches the child's `PATH`).
pub fn apply(cmd: &mut Command) {
    apply_vars(cmd, captured());
}

/// Passes the shell's variables to every exec plugin of `kubeconfig` (see
/// [`apply_to_exec`]).
pub fn apply_to_kubeconfig(kubeconfig: &mut Kubeconfig) {
    kubeconfig_vars(kubeconfig, captured());
}

pub(crate) fn kubeconfig_vars(kubeconfig: &mut Kubeconfig, vars: &[(OsString, OsString)]) {
    if vars.is_empty() {
        return;
    }
    for named in &mut kubeconfig.auth_infos {
        if let Some(exec) = named.auth_info.as_mut().and_then(|auth| auth.exec.as_mut()) {
            exec_vars(exec, vars);
        }
    }
}

/// Adds the shell's variables to the `env` of an exec plugin (`aws`,
/// `gke-gcloud-auth-plugin`, `kubelogin`): kube-rs starts it with this
/// process's environment plus that list, and looks the command up in the
/// resulting `PATH`. Variables the kubeconfig sets or drops are kept as
/// they are.
pub fn apply_to_exec(exec: &mut ExecConfig) {
    exec_vars(exec, captured());
}

fn lookup(vars: &[(OsString, OsString)], name: &str) -> Option<OsString> {
    vars.iter()
        .rev()
        .find(|(key, _)| key == OsStr::new(name))
        .map(|(_, value)| value.clone())
}

fn overlay(
    base: impl IntoIterator<Item = (OsString, OsString)>,
    top: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = base.into_iter().collect();
    for (key, value) in top {
        env.retain(|(existing, _)| existing != key);
        env.push((key.clone(), value.clone()));
    }
    env
}

fn apply_vars(cmd: &mut Command, vars: &[(OsString, OsString)]) {
    let set: HashSet<OsString> = cmd.get_envs().map(|(key, _)| key.to_owned()).collect();
    for (key, value) in vars {
        if !set.contains(key) {
            cmd.env(key, value);
        }
    }
}

fn exec_vars(exec: &mut ExecConfig, vars: &[(OsString, OsString)]) {
    let env = exec.env.get_or_insert_with(Vec::new);
    let mut taken: HashSet<String> = env
        .iter()
        .filter_map(|entry| entry.get("name").cloned())
        .chain(exec.drop_env.iter().flatten().cloned())
        .collect();
    let mut added = Vec::new();
    for (key, value) in vars {
        let (Some(key), Some(value)) = (key.to_str(), value.to_str()) else {
            continue;
        };
        if taken.insert(key.to_owned()) {
            added.push(
                [("name", key), ("value", value)]
                    .into_iter()
                    .map(|(field, text)| (field.to_owned(), text.to_owned()))
                    .collect(),
            );
        }
    }
    // First, so that the kubeconfig's own entries still come last.
    added.append(env);
    *env = added;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect()
    }

    fn entry(name: &str, value: &str) -> HashMap<String, String> {
        HashMap::from([
            ("name".to_owned(), name.to_owned()),
            ("value".to_owned(), value.to_owned()),
        ])
    }

    #[test]
    fn overlay_puts_shell_vars_on_top() {
        let env = overlay(
            vars(&[("PATH", "/usr/bin"), ("KEEP", "1")]),
            &vars(&[("PATH", "/opt/bin:/usr/bin"), ("NEW", "2")]),
        );
        assert_eq!(lookup(&env, "PATH"), Some("/opt/bin:/usr/bin".into()));
        assert_eq!(lookup(&env, "KEEP"), Some("1".into()));
        assert_eq!(lookup(&env, "NEW"), Some("2".into()));
        assert_eq!(env.iter().filter(|(key, _)| key == "PATH").count(), 1);
    }

    #[test]
    fn command_gets_shell_vars_but_keeps_its_own() {
        let mut cmd = Command::new("kubach-test-307");
        cmd.env("KUBECONFIG", "/tmp/kubach-test-307/config")
            .env_remove("SECRET");
        apply_vars(
            &mut cmd,
            &vars(&[
                ("PATH", "/kubach-test-307/bin"),
                ("KUBECONFIG", "/shell/config"),
                ("SECRET", "x"),
            ]),
        );
        let envs: HashMap<&OsStr, Option<&OsStr>> = cmd.get_envs().collect();
        assert_eq!(
            envs[OsStr::new("PATH")],
            Some(OsStr::new("/kubach-test-307/bin"))
        );
        assert_eq!(
            envs[OsStr::new("KUBECONFIG")],
            Some(OsStr::new("/tmp/kubach-test-307/config"))
        );
        assert_eq!(envs[OsStr::new("SECRET")], None);
    }

    #[test]
    fn exec_config_gets_shell_vars_after_its_own_rules() {
        let mut exec = ExecConfig {
            command: Some("aws".into()),
            env: Some(vec![entry("AWS_PROFILE", "prod")]),
            drop_env: Some(vec!["AWS_SECRET".into()]),
            ..ExecConfig::default()
        };
        exec_vars(
            &mut exec,
            &vars(&[
                ("PATH", "/opt/homebrew/bin"),
                ("AWS_PROFILE", "dev"),
                ("AWS_SECRET", "x"),
            ]),
        );
        assert_eq!(
            exec.env.unwrap(),
            [
                entry("PATH", "/opt/homebrew/bin"),
                entry("AWS_PROFILE", "prod")
            ]
        );
    }

    /// A program found only through the `PATH` given to the command (the
    /// way an exec plugin is found after [`apply_to_exec`]) starts.
    #[cfg(unix)]
    #[test]
    fn program_is_looked_up_in_the_commands_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("kubach-test-307-tool");
        std::fs::write(&tool, "#!/bin/sh\necho found\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = Command::new("kubach-test-307-tool");
        apply_vars(&mut cmd, &vars(&[("PATH", dir.path().to_str().unwrap())]));
        let output = cmd.output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "found");
    }
}
