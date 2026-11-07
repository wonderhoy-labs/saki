//! Points DNS at the tunnel on Linux systems without `systemd-resolved`, by editing `/etc/resolv.conf`.
//!
//! The previous contents stay in the file as comments, so nothing is lost even if this never gets to
//! put them back.

use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

pub const PATH: &str = "/etc/resolv.conf";

pub struct ResolvConf {
    path: PathBuf,
    previous: String,
    ours: String,
}

impl ResolvConf {
    pub fn install(path: &Path, resolver: Ipv4Addr) -> io::Result<Self> {
        let previous = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error),
        };
        let mut ours =
            format!("# Written by saki client, which puts this back when it exits.\nnameserver {resolver}\n");
        if !previous.is_empty() {
            ours.push_str("\n# What was here before:\n");
            for line in previous.lines() {
                ours.push_str("# ");
                ours.push_str(line);
                ours.push('\n');
            }
        }
        fs::write(path, &ours)?;
        Ok(Self { path: path.to_owned(), previous, ours })
    }

    /// Puts the file back. If something else has rewritten it since, like NetworkManager noticing a
    /// new network, that's newer than what this remembers, so it stays.
    pub fn restore(&self) -> io::Result<()> {
        if fs::read_to_string(&self.path)? == self.ours { fs::write(&self.path, &self.previous) } else { Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESOLVER: Ipv4Addr = Ipv4Addr::new(10, 233, 77, 1);

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("saki-{}-{name}", std::process::id()));
            let _ = fs::remove_file(&path);
            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn it_points_dns_at_the_tunnel_and_keeps_the_old_lines_as_comments() {
        let file = TempFile::new("install");
        fs::write(&file.0, "nameserver 192.168.0.1\nsearch lan\n").unwrap();
        let _installed = ResolvConf::install(&file.0, RESOLVER).unwrap();
        let written = fs::read_to_string(&file.0).unwrap();
        let active: Vec<_> = written.lines().filter(|line| !line.starts_with('#') && !line.is_empty()).collect();
        assert_eq!(active, ["nameserver 10.233.77.1"]);
        assert!(written.contains("# nameserver 192.168.0.1\n# search lan\n"));
    }

    #[test]
    fn restoring_puts_back_exactly_what_was_there() {
        let file = TempFile::new("restore");
        fs::write(&file.0, "nameserver 192.168.0.1\n\nsearch lan").unwrap();
        ResolvConf::install(&file.0, RESOLVER).unwrap().restore().unwrap();
        assert_eq!(fs::read_to_string(&file.0).unwrap(), "nameserver 192.168.0.1\n\nsearch lan");
    }

    #[test]
    fn a_missing_file_comes_back_empty() {
        let file = TempFile::new("missing");
        ResolvConf::install(&file.0, RESOLVER).unwrap().restore().unwrap();
        assert_eq!(fs::read_to_string(&file.0).unwrap(), "");
    }

    #[test]
    fn a_newer_version_from_something_else_stays() {
        let file = TempFile::new("newer");
        fs::write(&file.0, "nameserver 192.168.0.1\n").unwrap();
        let installed = ResolvConf::install(&file.0, RESOLVER).unwrap();
        fs::write(&file.0, "nameserver 172.16.0.1\n").unwrap();
        installed.restore().unwrap();
        assert_eq!(fs::read_to_string(&file.0).unwrap(), "nameserver 172.16.0.1\n");
    }
}
