#![allow(dead_code)]

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

pub struct Sandbox {
    root: TempDir,
}

impl Sandbox {
    pub fn new() -> Self {
        let root = tempfile::tempdir().expect("create isolated CLI root");
        for name in ["home", "config", "cache"] {
            std::fs::create_dir(root.path().join(name)).expect("create isolated directory");
        }
        Self { root }
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_bupt"));
        cmd.env_clear();
        for name in ["SYSTEMROOT", "WINDIR"] {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        cmd.current_dir(self.root())
            .env("HOME", self.root().join("home"))
            .env("USERPROFILE", self.root().join("home"))
            .env("BUPT_CLI_CONFIG_DIR", self.root().join("config"))
            .env("BUPT_CLI_CACHE_DIR", self.root().join("cache"));
        cmd
    }

    pub fn run(&self, args: &[&str]) -> Invocation {
        Invocation(self.command().args(args).output().expect("run isolated CLI"))
    }
}

pub struct Invocation(pub Output);

impl Invocation {
    pub fn code(&self) -> i32 {
        self.0.status.code().expect("CLI exited with a status code")
    }

    pub fn stdout_json(&self) -> Value {
        self.parse("stdout", &self.0.stdout)
    }

    pub fn stderr_json(&self) -> Value {
        self.parse("stderr", &self.0.stderr)
    }

    fn parse(&self, stream: &str, bytes: &[u8]) -> Value {
        serde_json::from_slice(bytes).unwrap_or_else(|err| {
            panic!(
                "invalid {stream} JSON: {err}; exit={}; stdout={:?}; stderr={:?}",
                self.0.status,
                String::from_utf8_lossy(&self.0.stdout),
                String::from_utf8_lossy(&self.0.stderr)
            )
        })
    }
}
