use std::{env, fmt, str::FromStr};

use eyre::{Result, bail, eyre};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Libc {
    Glibc,
    Musl,
    Static,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Os {
    Linux(Libc),
    Macos,
    Windows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Target {
    pub os: Os,
    pub arch: Arch,
}

impl Target {
    #[must_use]
    pub fn exe_suffix(&self) -> &'static str {
        match self.os {
            Os::Windows => ".exe",
            Os::Linux(_) | Os::Macos => "",
        }
    }

    #[must_use]
    pub fn rust_target(&self) -> String {
        match (&self.arch, &self.os) {
            (arch, Os::Macos) => format!("{arch}-apple-darwin"),
            (arch, Os::Windows) => format!("{arch}-pc-windows-gnu"),
            (arch, Os::Linux(Libc::Glibc)) => format!("{arch}-unknown-linux-gnu"),
            (arch, Os::Linux(Libc::Musl | Libc::Static)) => format!("{arch}-unknown-linux-musl"),
        }
    }

    pub fn current() -> eyre::Result<Self> {
        let arch = match env::consts::ARCH {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            other => bail!("unsupported current architecture '{other}'"),
        };

        let os = match env::consts::OS {
            "macos" => Os::Macos,
            "windows" => Os::Windows,
            "linux" => Os::Linux(Libc::Static),
            other => bail!("unsupported current OS '{other}'"),
        };

        Ok(Self { os, arch })
    }
}

impl FromStr for Target {
    type Err = eyre::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (arch_str, rest) = s
            .split_once('-')
            .ok_or_else(|| eyre!("invalid target format '{s}': expected <arch>-<os>"))?;

        let arch = match arch_str {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            _ => bail!("unsupported architecture '{arch_str}': expected x86_64 or aarch64"),
        };

        let os = match rest {
            "macos" => Os::Macos,
            "windows" => Os::Windows,
            "linux" => bail!(
                "invalid target format '{s}': linux requires a libc variant (linux-glibc, linux-musl, or linux-static)"
            ),
            _ if rest.starts_with("linux-") => {
                let libc_str = &rest["linux-".len()..];
                let libc = match libc_str {
                    "glibc" => Libc::Glibc,
                    "musl" => Libc::Musl,
                    "static" => Libc::Static,
                    other => {
                        bail!("unsupported libc variant '{other}': expected glibc, musl, or static")
                    }
                };
                Os::Linux(libc)
            }
            _ => {
                let os_str = rest.split('-').next().unwrap_or(rest);
                bail!("unsupported OS '{os_str}': expected linux, macos, or windows")
            }
        };

        Ok(Self { os, arch })
    }
}

impl TryFrom<String> for Target {
    type Error = eyre::Report;

    fn try_from(s: String) -> Result<Self> {
        s.parse()
    }
}

impl From<Target> for String {
    fn from(target: Target) -> Self {
        target.to_string()
    }
}

impl fmt::Display for Libc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Libc::Glibc => write!(f, "glibc"),
            Libc::Musl => write!(f, "musl"),
            Libc::Static => write!(f, "static"),
        }
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Os::Linux(_) => write!(f, "linux"),
            Os::Macos => write!(f, "macos"),
            Os::Windows => write!(f, "windows"),
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Arch::X86_64 => write!(f, "x86_64"),
            Arch::Aarch64 => write!(f, "aarch64"),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.os {
            Os::Linux(libc) => write!(f, "{}-linux-{libc}", self.arch),
            _ => write!(f, "{}-{}", self.arch, self.os),
        }
    }
}

#[cfg(test)]
mod test {
    use quickcheck::{Arbitrary, Gen};
    use quickcheck_macros::quickcheck;

    use super::*;

    impl Arbitrary for Target {
        fn arbitrary(g: &mut Gen) -> Self {
            let targets = [
                Target {
                    os: Os::Linux(Libc::Glibc),
                    arch: Arch::X86_64,
                },
                Target {
                    os: Os::Linux(Libc::Glibc),
                    arch: Arch::Aarch64,
                },
                Target {
                    os: Os::Linux(Libc::Musl),
                    arch: Arch::X86_64,
                },
                Target {
                    os: Os::Linux(Libc::Musl),
                    arch: Arch::Aarch64,
                },
                Target {
                    os: Os::Linux(Libc::Static),
                    arch: Arch::X86_64,
                },
                Target {
                    os: Os::Linux(Libc::Static),
                    arch: Arch::Aarch64,
                },
                Target {
                    os: Os::Macos,
                    arch: Arch::X86_64,
                },
                Target {
                    os: Os::Macos,
                    arch: Arch::Aarch64,
                },
                Target {
                    os: Os::Windows,
                    arch: Arch::X86_64,
                },
                Target {
                    os: Os::Windows,
                    arch: Arch::Aarch64,
                },
            ];
            *g.choose(&targets).unwrap()
        }
    }

    #[quickcheck]
    fn test_display_parse_roundtrip(target: Target) {
        let s = target.to_string();
        let parsed: Target = s.parse().unwrap();
        assert_eq!(target, parsed);
    }

    #[test]
    fn test_invalid_os() {
        let err = "x86_64-bsd".parse::<Target>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "unsupported OS 'bsd': expected linux, macos, or windows"
        );
    }

    #[test]
    fn test_missing_separator() {
        let err = "linux".parse::<Target>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid target format 'linux': expected <arch>-<os>"
        );
    }

    #[test]
    fn test_linux_requires_libc_variant() {
        let err = "x86_64-linux".parse::<Target>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid target format 'x86_64-linux': linux requires a libc variant (linux-glibc, linux-musl, or linux-static)"
        );
    }

    #[test]
    fn test_invalid_libc_variant() {
        let err = "x86_64-linux-foobar".parse::<Target>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "unsupported libc variant 'foobar': expected glibc, musl, or static"
        );
    }

    #[test]
    fn test_aarch64_windows_parses() {
        let target = "aarch64-windows".parse::<Target>().unwrap();
        assert_eq!(target.arch, Arch::Aarch64);
        assert_eq!(target.os, Os::Windows);
    }

    #[test]
    fn test_exe_suffix() {
        assert_eq!(
            "x86_64-linux-static"
                .parse::<Target>()
                .unwrap()
                .exe_suffix(),
            ""
        );
        assert_eq!(
            "x86_64-windows".parse::<Target>().unwrap().exe_suffix(),
            ".exe"
        );
    }

    #[test]
    fn test_rust_target() {
        assert_eq!(
            "x86_64-linux-glibc"
                .parse::<Target>()
                .unwrap()
                .rust_target(),
            "x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            "x86_64-linux-static"
                .parse::<Target>()
                .unwrap()
                .rust_target(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            "aarch64-macos".parse::<Target>().unwrap().rust_target(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            "x86_64-windows".parse::<Target>().unwrap().rust_target(),
            "x86_64-pc-windows-gnu"
        );
    }
}
