// Copyright 2018-2020 the Deno authors. All rights reserved. MIT license.

use crate::colors;
use crate::fs_util::resolve_from_cwd;
use deno_core::error::custom_error;
use deno_core::error::uri_error;
use deno_core::error::AnyError;
use deno_core::url;
use deno_core::ModuleSpecifier;
use serde::Deserialize;
use std::collections::HashSet;
use std::env::current_dir;
use std::fmt;
use std::hash::Hash;
#[cfg(not(test))]
use std::io;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::Mutex;

const PERMISSION_EMOJI: &str = "⚠️";

/// Tri-state value for storing permission state
#[derive(PartialEq, Debug, Clone, Copy, Deserialize)]
pub enum PermissionState {
  Granted = 0,
  Prompt = 1,
  Denied = 2,
}

impl PermissionState {
  /// Check the permission state.
  fn check(self, msg: &str, flag_name: &str) -> Result<(), AnyError> {
    if self == PermissionState::Granted {
      log_perm_access(msg);
      return Ok(());
    }
    let message = format!("{}, run again with the {} flag", msg, flag_name);
    Err(custom_error("PermissionDenied", message))
  }
}

impl fmt::Display for PermissionState {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      PermissionState::Granted => f.pad("granted"),
      PermissionState::Prompt => f.pad("prompt"),
      PermissionState::Denied => f.pad("denied"),
    }
  }
}

impl Default for PermissionState {
  fn default() -> Self {
    PermissionState::Prompt
  }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct UnaryPermission<T: Eq + Hash> {
  pub global_state: PermissionState,
  pub granted_list: HashSet<T>,
  pub denied_list: HashSet<T>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct Permissions {
  pub read: UnaryPermission<PathBuf>,
  pub write: UnaryPermission<PathBuf>,
  pub net: UnaryPermission<String>,
  pub env: PermissionState,
  pub run: PermissionState,
  pub plugin: PermissionState,
  pub hrtime: PermissionState,
}

fn resolve_fs_allowlist(allow: &Option<Vec<PathBuf>>) -> HashSet<PathBuf> {
  if let Some(v) = allow {
    v.iter()
      .map(|raw_path| resolve_from_cwd(Path::new(&raw_path)).unwrap())
      .collect()
  } else {
    HashSet::new()
  }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct PermissionsOptions {
  pub allow_env: bool,
  pub allow_hrtime: bool,
  pub allow_net: Option<Vec<String>>,
  pub allow_plugin: bool,
  pub allow_read: Option<Vec<PathBuf>>,
  pub allow_run: bool,
  pub allow_write: Option<Vec<PathBuf>>,
}

impl Permissions {
  pub fn from_options(opts: &PermissionsOptions) -> Self {
    fn global_state_from_flag_bool(flag: bool) -> PermissionState {
      if flag {
        PermissionState::Granted
      } else {
        PermissionState::Prompt
      }
    }
    fn global_state_from_option<T>(flag: &Option<Vec<T>>) -> PermissionState {
      if matches!(flag, Some(v) if v.is_empty()) {
        PermissionState::Granted
      } else {
        PermissionState::Prompt
      }
    }
    Self {
      read: UnaryPermission::<PathBuf> {
        global_state: global_state_from_option(&opts.allow_read),
        granted_list: resolve_fs_allowlist(&opts.allow_read),
        ..Default::default()
      },
      write: UnaryPermission::<PathBuf> {
        global_state: global_state_from_option(&opts.allow_write),
        granted_list: resolve_fs_allowlist(&opts.allow_write),
        ..Default::default()
      },
      net: UnaryPermission::<String> {
        global_state: global_state_from_option(&opts.allow_net),
        granted_list: opts
          .allow_net
          .as_ref()
          .map(|v| v.iter().cloned().collect())
          .unwrap_or_else(HashSet::new),
        ..Default::default()
      },
      env: global_state_from_flag_bool(opts.allow_env),
      run: global_state_from_flag_bool(opts.allow_run),
      plugin: global_state_from_flag_bool(opts.allow_plugin),
      hrtime: global_state_from_flag_bool(opts.allow_hrtime),
    }
  }

  /// Arbitrary helper. Resolves the path from CWD, and also gets a path that
  /// can be displayed without leaking the CWD when not allowed.
  fn resolved_and_display_path(&self, path: &Path) -> (PathBuf, PathBuf) {
    let resolved_path = resolve_from_cwd(path).unwrap();
    let display_path = if path.is_absolute() {
      path.to_path_buf()
    } else {
      match self
        .query_read(&Some(&current_dir().unwrap()))
        .check("", "")
      {
        Ok(_) => resolved_path.clone(),
        Err(_) => path.to_path_buf(),
      }
    };
    (resolved_path, display_path)
  }

  pub fn allow_all() -> Self {
    Self {
      read: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      write: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      net: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      env: PermissionState::Granted,
      run: PermissionState::Granted,
      plugin: PermissionState::Granted,
      hrtime: PermissionState::Granted,
    }
  }

  pub fn query_read(&self, path: &Option<&Path>) -> PermissionState {
    let path = path.map(|p| resolve_from_cwd(p).unwrap());
    if self.read.global_state == PermissionState::Denied
      && match path.as_ref() {
        None => true,
        Some(path) => check_path_blocklist(path, &self.read.denied_list),
      }
    {
      return PermissionState::Denied;
    }
    if self.read.global_state == PermissionState::Granted
      || match path.as_ref() {
        None => false,
        Some(path) => check_path_allowlist(path, &self.read.granted_list),
      }
    {
      return PermissionState::Granted;
    }
    PermissionState::Prompt
  }

  pub fn query_write(&self, path: &Option<&Path>) -> PermissionState {
    let path = path.map(|p| resolve_from_cwd(p).unwrap());
    if self.write.global_state == PermissionState::Denied
      && match path.as_ref() {
        None => true,
        Some(path) => check_path_blocklist(path, &self.write.denied_list),
      }
    {
      return PermissionState::Denied;
    }
    if self.write.global_state == PermissionState::Granted
      || match path.as_ref() {
        None => false,
        Some(path) => check_path_allowlist(path, &self.write.granted_list),
      }
    {
      return PermissionState::Granted;
    }
    PermissionState::Prompt
  }

  pub fn query_net<T: AsRef<str>>(
    &self,
    host: &Option<&(T, Option<u16>)>,
  ) -> PermissionState {
    if self.net.global_state == PermissionState::Denied
      && match host.as_ref() {
        None => true,
        Some(host) => check_host_blocklist(host, &self.net.denied_list),
      }
    {
      return PermissionState::Denied;
    }
    if self.net.global_state == PermissionState::Granted
      || match host.as_ref() {
        None => false,
        Some(host) => check_host_allowlist(host, &self.net.granted_list),
      }
    {
      return PermissionState::Granted;
    }
    PermissionState::Prompt
  }

  pub fn query_env(&self) -> PermissionState {
    self.env
  }

  pub fn query_run(&self) -> PermissionState {
    self.run
  }

  pub fn query_plugin(&self) -> PermissionState {
    self.plugin
  }

  pub fn query_hrtime(&self) -> PermissionState {
    self.hrtime
  }

  pub fn request_read(&mut self, path: &Option<&Path>) -> PermissionState {
    if let Some(path) = path {
      let (resolved_path, display_path) = self.resolved_and_display_path(path);
      let state = self.query_read(&Some(&resolved_path));
      if state == PermissionState::Prompt {
        if permission_prompt(&format!(
          "Deno requests read access to \"{}\"",
          display_path.display()
        )) {
          self
            .read
            .granted_list
            .retain(|path| !path.starts_with(&resolved_path));
          self.read.granted_list.insert(resolved_path);
          return PermissionState::Granted;
        } else {
          self
            .read
            .denied_list
            .retain(|path| !resolved_path.starts_with(path));
          self.read.denied_list.insert(resolved_path);
          self.read.global_state = PermissionState::Denied;
          return PermissionState::Denied;
        }
      }
      state
    } else {
      let state = self.query_read(&None);
      if state == PermissionState::Prompt {
        if permission_prompt("Deno requests read access") {
          self.read.granted_list.clear();
          self.read.global_state = PermissionState::Granted;
          return PermissionState::Granted;
        } else {
          self.read.global_state = PermissionState::Denied;
          return PermissionState::Denied;
        }
      }
      state
    }
  }

  pub fn request_write(&mut self, path: &Option<&Path>) -> PermissionState {
    if let Some(path) = path {
      let (resolved_path, display_path) = self.resolved_and_display_path(path);
      let state = self.query_write(&Some(&resolved_path));
      if state == PermissionState::Prompt {
        if permission_prompt(&format!(
          "Deno requests write access to \"{}\"",
          display_path.display()
        )) {
          self
            .write
            .granted_list
            .retain(|path| !path.starts_with(&resolved_path));
          self.write.granted_list.insert(resolved_path);
          return PermissionState::Granted;
        } else {
          self
            .write
            .denied_list
            .retain(|path| !resolved_path.starts_with(path));
          self.write.denied_list.insert(resolved_path);
          self.write.global_state = PermissionState::Denied;
          return PermissionState::Denied;
        }
      }
      state
    } else {
      let state = self.query_write(&None);
      if state == PermissionState::Prompt {
        if permission_prompt("Deno requests write access") {
          self.write.granted_list.clear();
          self.write.global_state = PermissionState::Granted;
          return PermissionState::Granted;
        } else {
          self.write.global_state = PermissionState::Denied;
          return PermissionState::Denied;
        }
      }
      state
    }
  }

  pub fn request_net<T: AsRef<str>>(
    &mut self,
    host: &Option<&(T, Option<u16>)>,
  ) -> PermissionState {
    if let Some(host) = host {
      let state = self.query_net(&Some(host));
      if state == PermissionState::Prompt {
        let host_string = format_host(host);
        if permission_prompt(&format!(
          "Deno requests network access to \"{}\"",
          host_string,
        )) {
          if host.1.is_none() {
            self
              .net
              .granted_list
              .retain(|h| !h.starts_with(&format!("{}:", host.0.as_ref())));
          }
          self.net.granted_list.insert(host_string);
          return PermissionState::Granted;
        } else {
          if host.1.is_some() {
            self.net.denied_list.remove(host.0.as_ref());
          }
          self.net.denied_list.insert(host_string);
          self.net.global_state = PermissionState::Denied;
          return PermissionState::Denied;
        }
      }
      state
    } else {
      let state = self.query_net::<&str>(&None);
      if state == PermissionState::Prompt {
        if permission_prompt("Deno requests network access") {
          self.net.granted_list.clear();
          self.net.global_state = PermissionState::Granted;
          return PermissionState::Granted;
        } else {
          self.net.global_state = PermissionState::Denied;
          return PermissionState::Denied;
        }
      }
      state
    }
  }

  pub fn request_env(&mut self) -> PermissionState {
    if self.env == PermissionState::Prompt {
      if permission_prompt("Deno requests access to environment variables") {
        self.env = PermissionState::Granted;
      } else {
        self.env = PermissionState::Denied;
      }
    }
    self.env
  }

  pub fn request_run(&mut self) -> PermissionState {
    if self.run == PermissionState::Prompt {
      if permission_prompt("Deno requests to access to run a subprocess") {
        self.run = PermissionState::Granted;
      } else {
        self.run = PermissionState::Denied;
      }
    }
    self.run
  }

  pub fn request_plugin(&mut self) -> PermissionState {
    if self.plugin == PermissionState::Prompt {
      if permission_prompt("Deno requests to open plugins") {
        self.plugin = PermissionState::Granted;
      } else {
        self.plugin = PermissionState::Denied;
      }
    }
    self.plugin
  }

  pub fn request_hrtime(&mut self) -> PermissionState {
    if self.hrtime == PermissionState::Prompt {
      if permission_prompt("Deno requests access to high precision time") {
        self.hrtime = PermissionState::Granted;
      } else {
        self.hrtime = PermissionState::Denied;
      }
    }
    self.hrtime
  }

  pub fn revoke_read(&mut self, path: &Option<&Path>) -> PermissionState {
    if let Some(path) = path {
      let path = resolve_from_cwd(path).unwrap();
      self
        .read
        .granted_list
        .retain(|path_| !path_.starts_with(&path));
    } else {
      self.read.granted_list.clear();
      if self.read.global_state == PermissionState::Granted {
        self.read.global_state = PermissionState::Prompt;
      }
    }
    self.query_read(path)
  }

  pub fn revoke_write(&mut self, path: &Option<&Path>) -> PermissionState {
    if let Some(path) = path {
      let path = resolve_from_cwd(path).unwrap();
      self
        .write
        .granted_list
        .retain(|path_| !path_.starts_with(&path));
    } else {
      self.write.granted_list.clear();
      if self.write.global_state == PermissionState::Granted {
        self.write.global_state = PermissionState::Prompt;
      }
    }
    self.query_write(path)
  }

  pub fn revoke_net<T: AsRef<str>>(
    &mut self,
    host: &Option<&(T, Option<u16>)>,
  ) -> PermissionState {
    if let Some(host) = host {
      self.net.granted_list.remove(&format_host(host));
      if host.1.is_none() {
        self
          .net
          .granted_list
          .retain(|h| !h.starts_with(&format!("{}:", host.0.as_ref())));
      }
    } else {
      self.net.granted_list.clear();
      if self.net.global_state == PermissionState::Granted {
        self.net.global_state = PermissionState::Prompt;
      }
    }
    self.query_net(host)
  }

  pub fn revoke_env(&mut self) -> PermissionState {
    if self.env == PermissionState::Granted {
      self.env = PermissionState::Prompt;
    }
    self.env
  }

  pub fn revoke_run(&mut self) -> PermissionState {
    if self.run == PermissionState::Granted {
      self.run = PermissionState::Prompt;
    }
    self.run
  }

  pub fn revoke_plugin(&mut self) -> PermissionState {
    if self.plugin == PermissionState::Granted {
      self.plugin = PermissionState::Prompt;
    }
    self.plugin
  }

  pub fn revoke_hrtime(&mut self) -> PermissionState {
    if self.hrtime == PermissionState::Granted {
      self.hrtime = PermissionState::Prompt;
    }
    self.hrtime
  }

  pub fn check_read(&self, path: &Path) -> Result<(), AnyError> {
    let (resolved_path, display_path) = self.resolved_and_display_path(path);
    self.query_read(&Some(&resolved_path)).check(
      &format!("read access to \"{}\"", display_path.display()),
      "--allow-read",
    )
  }

  /// As `check_read()`, but permission error messages will anonymize the path
  /// by replacing it with the given `display`.
  pub fn check_read_blind(
    &self,
    path: &Path,
    display: &str,
  ) -> Result<(), AnyError> {
    let resolved_path = resolve_from_cwd(path).unwrap();
    self
      .query_read(&Some(&resolved_path))
      .check(&format!("read access to <{}>", display), "--allow-read")
  }

  pub fn check_write(&self, path: &Path) -> Result<(), AnyError> {
    let (resolved_path, display_path) = self.resolved_and_display_path(path);
    self.query_write(&Some(&resolved_path)).check(
      &format!("write access to \"{}\"", display_path.display()),
      "--allow-write",
    )
  }

  pub fn check_net<T: AsRef<str>>(
    &self,
    host: &(T, Option<u16>),
  ) -> Result<(), AnyError> {
    self.query_net(&Some(host)).check(
      &format!("network access to \"{}\"", format_host(host)),
      "--allow-net",
    )
  }

  pub fn check_net_url(&self, url: &url::Url) -> Result<(), AnyError> {
    let hostname = url
      .host_str()
      .ok_or_else(|| uri_error("Missing host"))?
      .to_string();
    let display_host = match url.port() {
      None => hostname.clone(),
      Some(port) => format!("{}:{}", hostname, port),
    };
    self
      .query_net(&Some(&(hostname, url.port_or_known_default())))
      .check(
        &format!("network access to \"{}\"", display_host),
        "--allow-net",
      )
  }

  /// A helper function that determines if the module specifier is a local or
  /// remote, and performs a read or net check for the specifier.
  pub fn check_specifier(
    &self,
    specifier: &ModuleSpecifier,
  ) -> Result<(), AnyError> {
    let url = specifier.as_url();
    // TODO: Rely on file_fetcher's Scheme if appropriate
    match url.scheme() {
      "data" => Ok(()),
      "file" => {
        let path = url.to_file_path().unwrap();
        self.check_read(&path)
      }
      _ => self.check_net_url(url),
    }
  }

  pub fn check_env(&self) -> Result<(), AnyError> {
    self
      .env
      .check("access to environment variables", "--allow-env")
  }

  pub fn check_run(&self) -> Result<(), AnyError> {
    self.run.check("access to run a subprocess", "--allow-run")
  }

  pub fn check_plugin(&self, path: &Path) -> Result<(), AnyError> {
    let (_, display_path) = self.resolved_and_display_path(path);
    self.plugin.check(
      &format!("access to open a plugin: {}", display_path.display()),
      "--allow-plugin",
    )
  }

  pub fn check_hrtime(&self) -> Result<(), AnyError> {
    self
      .hrtime
      .check("access to high precision time", "--allow-hrtime")
  }
}

impl deno_fetch::FetchPermissions for Permissions {
  fn check_net_url(&self, url: &url::Url) -> Result<(), AnyError> {
    Permissions::check_net_url(self, url)
  }

  fn check_read(&self, p: &PathBuf) -> Result<(), AnyError> {
    Permissions::check_read(self, p)
  }
}

/// Shows the permission prompt and returns the answer according to the user input.
/// This loops until the user gives the proper input.
#[cfg(not(test))]
fn permission_prompt(message: &str) -> bool {
  if !atty::is(atty::Stream::Stdin) || !atty::is(atty::Stream::Stderr) {
    return false;
  };
  let msg = format!(
    "️{}  {}. Grant? [g/d (g = grant, d = deny)] ",
    PERMISSION_EMOJI, message
  );
  // print to stderr so that if deno is > to a file this is still displayed.
  eprint!("{}", colors::bold(&msg));
  loop {
    let mut input = String::new();
    let stdin = io::stdin();
    let result = stdin.read_line(&mut input);
    if result.is_err() {
      return false;
    };
    let ch = input.chars().next().unwrap();
    match ch.to_ascii_lowercase() {
      'g' => return true,
      'd' => return false,
      _ => {
        // If we don't get a recognized option try again.
        let msg_again =
          format!("Unrecognized option '{}' [g/d (g = grant, d = deny)] ", ch);
        eprint!("{}", colors::bold(&msg_again));
      }
    };
  }
}

#[cfg(test)]
lazy_static! {
  /// Lock this when you use `set_prompt_result` in a test case.
  static ref PERMISSION_PROMPT_GUARD: Mutex<()> = Mutex::new(());
}

#[cfg(test)]
static STUB_PROMPT_VALUE: AtomicBool = AtomicBool::new(true);

#[cfg(test)]
fn set_prompt_result(value: bool) {
  STUB_PROMPT_VALUE.store(value, Ordering::SeqCst);
}

// When testing, permission prompt returns the value of STUB_PROMPT_VALUE
// which we set from the test functions.
#[cfg(test)]
fn permission_prompt(_message: &str) -> bool {
  STUB_PROMPT_VALUE.load(Ordering::SeqCst)
}

fn log_perm_access(message: &str) {
  debug!(
    "{}",
    colors::bold(&format!("{}️  Granted {}", PERMISSION_EMOJI, message))
  );
}

fn check_path_allowlist(path: &Path, allowlist: &HashSet<PathBuf>) -> bool {
  for path_ in allowlist {
    if path.starts_with(path_) {
      return true;
    }
  }
  false
}

fn check_path_blocklist(path: &Path, blocklist: &HashSet<PathBuf>) -> bool {
  for path_ in blocklist {
    if path_.starts_with(path) {
      return true;
    }
  }
  false
}

fn check_host_allowlist<T: AsRef<str>>(
  host: &(T, Option<u16>),
  allowlist: &HashSet<String>,
) -> bool {
  let (hostname, port) = host;
  allowlist.contains(hostname.as_ref())
    || (port.is_some() && allowlist.contains(&format_host(host)))
}

fn check_host_blocklist<T: AsRef<str>>(
  host: &(T, Option<u16>),
  blocklist: &HashSet<String>,
) -> bool {
  let (hostname, port) = host;
  match port {
    None => blocklist.iter().any(|host| {
      host == hostname.as_ref()
        || host.starts_with(&format!("{}:", hostname.as_ref()))
    }),
    Some(_) => blocklist.contains(&format_host(host)),
  }
}

fn format_host<T: AsRef<str>>(host: &(T, Option<u16>)) -> String {
  let (hostname, port) = host;
  match port {
    None => hostname.as_ref().to_string(),
    Some(port) => format!("{}:{}", hostname.as_ref(), port),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use deno_core::serde_json;

  // Creates vector of strings, Vec<String>
  macro_rules! svec {
      ($($x:expr),*) => (vec![$($x.to_string()),*]);
  }

  #[test]
  fn check_paths() {
    let allowlist = vec![
      PathBuf::from("/a/specific/dir/name"),
      PathBuf::from("/a/specific"),
      PathBuf::from("/b/c"),
    ];

    let perms = Permissions::from_options(&PermissionsOptions {
      allow_read: Some(allowlist.clone()),
      allow_write: Some(allowlist),
      ..Default::default()
    });

    // Inside of /a/specific and /a/specific/dir/name
    assert!(perms.check_read(Path::new("/a/specific/dir/name")).is_ok());
    assert!(perms.check_write(Path::new("/a/specific/dir/name")).is_ok());

    // Inside of /a/specific but outside of /a/specific/dir/name
    assert!(perms.check_read(Path::new("/a/specific/dir")).is_ok());
    assert!(perms.check_write(Path::new("/a/specific/dir")).is_ok());

    // Inside of /a/specific and /a/specific/dir/name
    assert!(perms
      .check_read(Path::new("/a/specific/dir/name/inner"))
      .is_ok());
    assert!(perms
      .check_write(Path::new("/a/specific/dir/name/inner"))
      .is_ok());

    // Inside of /a/specific but outside of /a/specific/dir/name
    assert!(perms.check_read(Path::new("/a/specific/other/dir")).is_ok());
    assert!(perms
      .check_write(Path::new("/a/specific/other/dir"))
      .is_ok());

    // Exact match with /b/c
    assert!(perms.check_read(Path::new("/b/c")).is_ok());
    assert!(perms.check_write(Path::new("/b/c")).is_ok());

    // Sub path within /b/c
    assert!(perms.check_read(Path::new("/b/c/sub/path")).is_ok());
    assert!(perms.check_write(Path::new("/b/c/sub/path")).is_ok());

    // Sub path within /b/c, needs normalizing
    assert!(perms
      .check_read(Path::new("/b/c/sub/path/../path/."))
      .is_ok());
    assert!(perms
      .check_write(Path::new("/b/c/sub/path/../path/."))
      .is_ok());

    // Inside of /b but outside of /b/c
    assert!(perms.check_read(Path::new("/b/e")).is_err());
    assert!(perms.check_write(Path::new("/b/e")).is_err());

    // Inside of /a but outside of /a/specific
    assert!(perms.check_read(Path::new("/a/b")).is_err());
    assert!(perms.check_write(Path::new("/a/b")).is_err());
  }

  #[test]
  fn test_check_net() {
    let perms = Permissions::from_options(&PermissionsOptions {
      allow_net: Some(svec![
        "localhost",
        "deno.land",
        "github.com:3000",
        "127.0.0.1",
        "172.16.0.2:8000",
        "www.github.com:443"
      ]),
      ..Default::default()
    });

    let domain_tests = vec![
      ("localhost", 1234, true),
      ("deno.land", 0, true),
      ("deno.land", 3000, true),
      ("deno.lands", 0, false),
      ("deno.lands", 3000, false),
      ("github.com", 3000, true),
      ("github.com", 0, false),
      ("github.com", 2000, false),
      ("github.net", 3000, false),
      ("127.0.0.1", 0, true),
      ("127.0.0.1", 3000, true),
      ("127.0.0.2", 0, false),
      ("127.0.0.2", 3000, false),
      ("172.16.0.2", 8000, true),
      ("172.16.0.2", 0, false),
      ("172.16.0.2", 6000, false),
      ("172.16.0.1", 8000, false),
      // Just some random hosts that should err
      ("somedomain", 0, false),
      ("192.168.0.1", 0, false),
    ];

    let url_tests = vec![
      // Any protocol + port for localhost should be ok, since we don't specify
      ("http://localhost", true),
      ("https://localhost", true),
      ("https://localhost:4443", true),
      ("tcp://localhost:5000", true),
      ("udp://localhost:6000", true),
      // Correct domain + any port and protocol should be ok incorrect shouldn't
      ("https://deno.land/std/example/welcome.ts", true),
      ("https://deno.land:3000/std/example/welcome.ts", true),
      ("https://deno.lands/std/example/welcome.ts", false),
      ("https://deno.lands:3000/std/example/welcome.ts", false),
      // Correct domain + port should be ok all other combinations should err
      ("https://github.com:3000/denoland/deno", true),
      ("https://github.com/denoland/deno", false),
      ("https://github.com:2000/denoland/deno", false),
      ("https://github.net:3000/denoland/deno", false),
      // Correct ipv4 address + any port should be ok others should err
      ("tcp://127.0.0.1", true),
      ("https://127.0.0.1", true),
      ("tcp://127.0.0.1:3000", true),
      ("https://127.0.0.1:3000", true),
      ("tcp://127.0.0.2", false),
      ("https://127.0.0.2", false),
      ("tcp://127.0.0.2:3000", false),
      ("https://127.0.0.2:3000", false),
      // Correct address + port should be ok all other combinations should err
      ("tcp://172.16.0.2:8000", true),
      ("https://172.16.0.2:8000", true),
      ("tcp://172.16.0.2", false),
      ("https://172.16.0.2", false),
      ("tcp://172.16.0.2:6000", false),
      ("https://172.16.0.2:6000", false),
      ("tcp://172.16.0.1:8000", false),
      ("https://172.16.0.1:8000", false),
      // Testing issue #6531 (Network permissions check doesn't account for well-known default ports) so we dont regress
      ("https://www.github.com:443/robots.txt", true),
    ];

    for (url_str, is_ok) in url_tests.iter() {
      let u = url::Url::parse(url_str).unwrap();
      assert_eq!(*is_ok, perms.check_net_url(&u).is_ok());
    }

    for (hostname, port, is_ok) in domain_tests.iter() {
      assert_eq!(*is_ok, perms.check_net(&(hostname, Some(*port))).is_ok());
    }
  }

  #[test]
  fn check_specifiers() {
    let read_allowlist = if cfg!(target_os = "windows") {
      vec![PathBuf::from("C:\\a")]
    } else {
      vec![PathBuf::from("/a")]
    };
    let perms = Permissions::from_options(&PermissionsOptions {
      allow_read: Some(read_allowlist),
      allow_net: Some(svec!["localhost"]),
      ..Default::default()
    });

    let mut fixtures = vec![
      (
        ModuleSpecifier::resolve_url_or_path("http://localhost:4545/mod.ts")
          .unwrap(),
        true,
      ),
      (
        ModuleSpecifier::resolve_url_or_path("http://deno.land/x/mod.ts")
          .unwrap(),
        false,
      ),
    ];

    if cfg!(target_os = "windows") {
      fixtures.push((
        ModuleSpecifier::resolve_url_or_path("file:///C:/a/mod.ts").unwrap(),
        true,
      ));
      fixtures.push((
        ModuleSpecifier::resolve_url_or_path("file:///C:/b/mod.ts").unwrap(),
        false,
      ));
    } else {
      fixtures.push((
        ModuleSpecifier::resolve_url_or_path("file:///a/mod.ts").unwrap(),
        true,
      ));
      fixtures.push((
        ModuleSpecifier::resolve_url_or_path("file:///b/mod.ts").unwrap(),
        false,
      ));
    }

    for (specifier, expected) in fixtures {
      assert_eq!(perms.check_specifier(&specifier).is_ok(), expected);
    }
  }

  #[test]
  fn test_deserialize_perms() {
    let json_perms = r#"
    {
      "read": {
        "global_state": "Granted",
        "granted_list": [],
        "denied_list": []
      },
      "write": {
        "global_state": "Granted",
        "granted_list": [],
        "denied_list": []
      },
      "net": {
        "global_state": "Granted",
        "granted_list": [],
        "denied_list": []
      },
      "env": "Granted",
      "run": "Granted",
      "plugin": "Granted",
      "hrtime": "Granted"
    }
    "#;
    let perms0 = Permissions {
      read: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      write: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      net: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      env: PermissionState::Granted,
      run: PermissionState::Granted,
      hrtime: PermissionState::Granted,
      plugin: PermissionState::Granted,
    };
    let deserialized_perms: Permissions =
      serde_json::from_str(json_perms).unwrap();
    assert_eq!(perms0, deserialized_perms);
  }

  #[test]
  fn test_query() {
    let perms1 = Permissions {
      read: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      write: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      net: UnaryPermission {
        global_state: PermissionState::Granted,
        ..Default::default()
      },
      env: PermissionState::Granted,
      run: PermissionState::Granted,
      plugin: PermissionState::Granted,
      hrtime: PermissionState::Granted,
    };
    let perms2 = Permissions {
      read: UnaryPermission {
        global_state: PermissionState::Prompt,
        granted_list: resolve_fs_allowlist(&Some(vec![PathBuf::from("/foo")])),
        ..Default::default()
      },
      write: UnaryPermission {
        global_state: PermissionState::Prompt,
        granted_list: resolve_fs_allowlist(&Some(vec![PathBuf::from("/foo")])),
        ..Default::default()
      },
      net: UnaryPermission {
        global_state: PermissionState::Prompt,
        granted_list: ["127.0.0.1:8000".to_string()].iter().cloned().collect(),
        ..Default::default()
      },
      env: PermissionState::Prompt,
      run: PermissionState::Prompt,
      plugin: PermissionState::Prompt,
      hrtime: PermissionState::Prompt,
    };
    #[rustfmt::skip]
    {
      assert_eq!(perms1.query_read(&None), PermissionState::Granted);
      assert_eq!(perms1.query_read(&Some(&Path::new("/foo"))), PermissionState::Granted);
      assert_eq!(perms2.query_read(&None), PermissionState::Prompt);
      assert_eq!(perms2.query_read(&Some(&Path::new("/foo"))), PermissionState::Granted);
      assert_eq!(perms2.query_read(&Some(&Path::new("/foo/bar"))), PermissionState::Granted);
      assert_eq!(perms1.query_write(&None), PermissionState::Granted);
      assert_eq!(perms1.query_write(&Some(&Path::new("/foo"))), PermissionState::Granted);
      assert_eq!(perms2.query_write(&None), PermissionState::Prompt);
      assert_eq!(perms2.query_write(&Some(&Path::new("/foo"))), PermissionState::Granted);
      assert_eq!(perms2.query_write(&Some(&Path::new("/foo/bar"))), PermissionState::Granted);
      assert_eq!(perms1.query_net::<&str>(&None), PermissionState::Granted);
      assert_eq!(perms1.query_net(&Some(&("127.0.0.1", None))), PermissionState::Granted);
      assert_eq!(perms2.query_net::<&str>(&None), PermissionState::Prompt);
      assert_eq!(perms2.query_net(&Some(&("127.0.0.1", Some(8000)))), PermissionState::Granted);
      assert_eq!(perms1.query_env(), PermissionState::Granted);
      assert_eq!(perms2.query_env(), PermissionState::Prompt);
      assert_eq!(perms1.query_run(), PermissionState::Granted);
      assert_eq!(perms2.query_run(), PermissionState::Prompt);
      assert_eq!(perms1.query_plugin(), PermissionState::Granted);
      assert_eq!(perms2.query_plugin(), PermissionState::Prompt);
      assert_eq!(perms1.query_hrtime(), PermissionState::Granted);
      assert_eq!(perms2.query_hrtime(), PermissionState::Prompt);
    };
  }

  #[test]
  fn test_request() {
    let mut perms = Permissions {
      read: UnaryPermission {
        global_state: PermissionState::Prompt,
        ..Default::default()
      },
      write: UnaryPermission {
        global_state: PermissionState::Prompt,
        ..Default::default()
      },
      net: UnaryPermission {
        global_state: PermissionState::Prompt,
        ..Default::default()
      },
      env: PermissionState::Prompt,
      run: PermissionState::Prompt,
      plugin: PermissionState::Prompt,
      hrtime: PermissionState::Prompt,
    };
    #[rustfmt::skip]
    {
      let _guard = PERMISSION_PROMPT_GUARD.lock().unwrap();
      set_prompt_result(true);
      assert_eq!(perms.request_read(&Some(&Path::new("/foo"))), PermissionState::Granted);
      assert_eq!(perms.query_read(&None), PermissionState::Prompt);
      set_prompt_result(false);
      assert_eq!(perms.request_read(&Some(&Path::new("/foo/bar"))), PermissionState::Granted);
      set_prompt_result(false);
      assert_eq!(perms.request_write(&Some(&Path::new("/foo"))), PermissionState::Denied);
      assert_eq!(perms.query_write(&Some(&Path::new("/foo/bar"))), PermissionState::Prompt);
      set_prompt_result(true);
      assert_eq!(perms.request_write(&None), PermissionState::Denied);
      set_prompt_result(true);
      assert_eq!(perms.request_net(&Some(&("127.0.0.1", None))), PermissionState::Granted);
      set_prompt_result(false);
      assert_eq!(perms.request_net(&Some(&("127.0.0.1", Some(8000)))), PermissionState::Granted);
      set_prompt_result(true);
      assert_eq!(perms.request_env(), PermissionState::Granted);
      set_prompt_result(false);
      assert_eq!(perms.request_env(), PermissionState::Granted);
      set_prompt_result(false);
      assert_eq!(perms.request_run(), PermissionState::Denied);
      set_prompt_result(true);
      assert_eq!(perms.request_run(), PermissionState::Denied);
      set_prompt_result(true);
      assert_eq!(perms.request_plugin(), PermissionState::Granted);
      set_prompt_result(false);
      assert_eq!(perms.request_plugin(), PermissionState::Granted);
      set_prompt_result(false);
      assert_eq!(perms.request_hrtime(), PermissionState::Denied);
      set_prompt_result(true);
      assert_eq!(perms.request_hrtime(), PermissionState::Denied);
    };
  }

  #[test]
  fn test_revoke() {
    let mut perms = Permissions {
      read: UnaryPermission {
        global_state: PermissionState::Prompt,
        granted_list: resolve_fs_allowlist(&Some(vec![PathBuf::from("/foo")])),
        ..Default::default()
      },
      write: UnaryPermission {
        global_state: PermissionState::Prompt,
        granted_list: resolve_fs_allowlist(&Some(vec![PathBuf::from("/foo")])),
        ..Default::default()
      },
      net: UnaryPermission {
        global_state: PermissionState::Prompt,
        granted_list: svec!["127.0.0.1"].iter().cloned().collect(),
        ..Default::default()
      },
      env: PermissionState::Granted,
      run: PermissionState::Granted,
      plugin: PermissionState::Prompt,
      hrtime: PermissionState::Denied,
    };
    #[rustfmt::skip]
    {
      assert_eq!(perms.revoke_read(&Some(&Path::new("/foo/bar"))), PermissionState::Granted);
      assert_eq!(perms.revoke_read(&Some(&Path::new("/foo"))), PermissionState::Prompt);
      assert_eq!(perms.query_read(&Some(&Path::new("/foo/bar"))), PermissionState::Prompt);
      assert_eq!(perms.revoke_write(&Some(&Path::new("/foo/bar"))), PermissionState::Granted);
      assert_eq!(perms.revoke_write(&None), PermissionState::Prompt);
      assert_eq!(perms.query_write(&Some(&Path::new("/foo/bar"))), PermissionState::Prompt);
      assert_eq!(perms.revoke_net(&Some(&("127.0.0.1", Some(8000)))), PermissionState::Granted);
      assert_eq!(perms.revoke_net(&Some(&("127.0.0.1", None))), PermissionState::Prompt);
      assert_eq!(perms.revoke_env(), PermissionState::Prompt);
      assert_eq!(perms.revoke_run(), PermissionState::Prompt);
      assert_eq!(perms.revoke_plugin(), PermissionState::Prompt);
      assert_eq!(perms.revoke_hrtime(), PermissionState::Denied);
    };
  }
}
