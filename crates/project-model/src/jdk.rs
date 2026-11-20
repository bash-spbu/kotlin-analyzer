//! Where a JDK is (`docs/spec/jdk.md`): the one `JAVA_HOME` names, `java`
//! on `PATH`, or one downloaded to `~/.jdks`.

use std::path::{Path, PathBuf};

/// What the environment says about Java, read once: tests make their own.
#[derive(Clone, Debug, Default)]
pub struct JavaEnvironment {
    /// `JAVA_HOME`.
    pub java_home: Option<PathBuf>,
    /// `PATH`'s directories.
    pub path: Vec<PathBuf>,
    /// The user's home directory (`HOME`), which holds `.jdks`.
    pub home: Option<PathBuf>,
    /// On macOS, its `java` stub.
    pub macos_stub: Option<MacOsStub>,
}

/// macOS's `/usr/bin/java`: a stub that runs a JDK installed where
/// `installed_jdks` say, and fails when there is none.
#[derive(Clone, Debug)]
pub struct MacOsStub {
    pub java: PathBuf,
    /// The directories macOS installs JDKs into, each JDK a bundle.
    pub installed_jdks: Vec<PathBuf>,
}

/// Where the `java` a program the server starts runs on comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JavaRuntime {
    /// `JAVA_HOME`, holding `bin/java`.
    JavaHome(PathBuf),
    /// `java` on `PATH`: its home isn't known.
    OnPath,
    /// A JDK in `~/.jdks`: programs get `JAVA_HOME` set to it.
    Downloaded(PathBuf),
}

impl JavaRuntime {
    /// The JDK's home, when known.
    pub fn home(&self) -> Option<&Path> {
        match self {
            JavaRuntime::JavaHome(home) | JavaRuntime::Downloaded(home) => Some(home),
            JavaRuntime::OnPath => None,
        }
    }

    /// The `JAVA_HOME` a started program needs that the environment doesn't
    /// already give it.
    pub fn java_home_to_set(&self) -> Option<&Path> {
        match self {
            JavaRuntime::Downloaded(home) => Some(home),
            JavaRuntime::JavaHome(_) | JavaRuntime::OnPath => None,
        }
    }

    /// The `java` to run.
    pub fn java(&self) -> PathBuf {
        match self.home() {
            Some(home) => home.join("bin").join("java"),
            None => PathBuf::from("java"),
        }
    }
}

/// A JDK in `~/.jdks`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadedJdk {
    pub home: PathBuf,
    /// The feature release (`21`), from the `release` file.
    pub release: u32,
}

impl JavaEnvironment {
    /// The process's environment.
    pub fn current() -> JavaEnvironment {
        let path = std::env::var_os("PATH")
            .map(|path| std::env::split_paths(&path).collect())
            .unwrap_or_default();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let macos_stub = cfg!(target_os = "macos").then(|| MacOsStub {
            java: PathBuf::from("/usr/bin/java"),
            installed_jdks: [Some(PathBuf::from("/Library/Java/JavaVirtualMachines"))]
                .into_iter()
                .chain([home
                    .as_ref()
                    .map(|home| home.join("Library/Java/JavaVirtualMachines"))])
                .flatten()
                .collect(),
        });
        JavaEnvironment {
            java_home: std::env::var_os("JAVA_HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from),
            path,
            home,
            macos_stub,
        }
    }

    /// The `java` programs run on: `JAVA_HOME`'s, `java` on `PATH`, else
    /// the newest downloaded JDK; `None` when there's no JDK.
    pub fn runtime(&self) -> Option<JavaRuntime> {
        let java_home = self
            .java_home
            .as_ref()
            .filter(|home| home.join("bin").join("java").is_file());
        if let Some(home) = java_home {
            return Some(JavaRuntime::JavaHome(home.clone()));
        }
        if self.has_java_on_path() {
            return Some(JavaRuntime::OnPath);
        }
        let newest = self.downloaded_jdks().into_iter().next()?;
        Some(JavaRuntime::Downloaded(newest.home))
    }

    /// `~/.jdks`, where JDKs are downloaded to.
    pub fn jdks_directory(&self) -> Option<PathBuf> {
        Some(self.home.as_ref()?.join(".jdks"))
    }

    /// The JDKs in `~/.jdks`, the newest release first.
    pub fn downloaded_jdks(&self) -> Vec<DownloadedJdk> {
        let Some(directory) = self.jdks_directory() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Vec::new();
        };
        let mut jdks: Vec<DownloadedJdk> = entries
            .flatten()
            .filter_map(|entry| {
                let home = jdk_home_in(&entry.path())?;
                let release = feature_release(&home)?;
                Some(DownloadedJdk { home, release })
            })
            .collect();
        jdks.sort_by(|first, second| {
            second
                .release
                .cmp(&first.release)
                .then_with(|| first.home.cmp(&second.home))
        });
        jdks
    }

    fn has_java_on_path(&self) -> bool {
        let Some(java) = self
            .path
            .iter()
            .map(|directory| directory.join("java"))
            .find(|java| java.is_file())
        else {
            return false;
        };
        match &self.macos_stub {
            Some(stub) if stub.java == java => stub.runs_a_jdk(),
            _ => true,
        }
    }
}

impl MacOsStub {
    /// Whether a JDK is installed for the stub to run.
    fn runs_a_jdk(&self) -> bool {
        self.installed_jdks
            .iter()
            .filter_map(|directory| std::fs::read_dir(directory).ok())
            .flat_map(|entries| entries.flatten())
            .any(|entry| jdk_home_in(&entry.path()).is_some())
    }
}

/// The JDK home in `directory`: itself, or its macOS bundle's
/// `Contents/Home`, whichever holds `bin/java`.
pub fn jdk_home_in(directory: &Path) -> Option<PathBuf> {
    [
        directory.to_path_buf(),
        directory.join("Contents").join("Home"),
    ]
    .into_iter()
    .find(|home| home.join("bin").join("java").is_file())
}

/// A JDK's feature release, from its `release` file's `JAVA_VERSION`
/// (`"21.0.5"` is 21, `"1.8.0_442"` is 8).
pub fn feature_release(home: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(home.join("release")).ok()?;
    let version = text.lines().find_map(|line| {
        let value = line.strip_prefix("JAVA_VERSION=")?;
        Some(value.trim().trim_matches('"').to_owned())
    })?;
    let mut parts = version.split(['.', '_', '+', '-']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        return parts.next()?.parse().ok();
    }
    Some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake JDK at `directory` (`bin/java`, a `release` file).
    fn fake_jdk(directory: &Path, version: &str) {
        std::fs::create_dir_all(directory.join("bin")).unwrap();
        std::fs::write(directory.join("bin/java"), "").unwrap();
        std::fs::write(
            directory.join("release"),
            format!("IMPLEMENTOR=\"Someone\"\nJAVA_VERSION=\"{version}\"\n"),
        )
        .unwrap();
    }

    fn scratch(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("kotlin-analyzer-jdk-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn java_home_comes_first_then_path_then_the_newest_downloaded_jdk() {
        let root = scratch("order");
        let home = root.join("user");
        fake_jdk(&home.join(".jdks/temurin-17.0.9"), "17.0.9");
        fake_jdk(&home.join(".jdks/zulu-21.0.5/Contents/Home"), "21.0.5");
        fake_jdk(&home.join(".jdks/corretto-1.8.0"), "1.8.0_442");
        // Not a JDK: no `bin/java`.
        std::fs::create_dir_all(home.join(".jdks/.kotlin-analyzer-download")).unwrap();
        fake_jdk(&root.join("java-home"), "11.0.2");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/java"), "").unwrap();

        let downloaded_only = JavaEnvironment {
            java_home: Some(root.join("no-such-jdk")),
            path: vec![root.join("empty")],
            home: Some(home.clone()),
            macos_stub: None,
        };
        assert_eq!(
            downloaded_only.runtime(),
            Some(JavaRuntime::Downloaded(
                home.join(".jdks/zulu-21.0.5/Contents/Home")
            ))
        );
        let releases: Vec<u32> = downloaded_only
            .downloaded_jdks()
            .iter()
            .map(|jdk| jdk.release)
            .collect();
        assert_eq!(releases, [21, 17, 8]);

        let on_path = JavaEnvironment {
            path: vec![root.join("empty"), root.join("bin")],
            ..downloaded_only.clone()
        };
        assert_eq!(on_path.runtime(), Some(JavaRuntime::OnPath));

        let java_home = JavaEnvironment {
            java_home: Some(root.join("java-home")),
            ..on_path.clone()
        };
        assert_eq!(
            java_home.runtime(),
            Some(JavaRuntime::JavaHome(root.join("java-home")))
        );
        assert_eq!(
            java_home.runtime().unwrap().java_home_to_set(),
            None,
            "a program inherits JAVA_HOME"
        );
        assert_eq!(
            downloaded_only.runtime().unwrap().java_home_to_set(),
            Some(home.join(".jdks/zulu-21.0.5/Contents/Home").as_path())
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn macos_java_stub_counts_only_with_an_installed_jdk() {
        let root = scratch("stub");
        std::fs::create_dir_all(root.join("usr/bin")).unwrap();
        std::fs::write(root.join("usr/bin/java"), "").unwrap();
        let stub_only = JavaEnvironment {
            java_home: None,
            path: vec![root.join("usr/bin")],
            home: Some(root.join("user")),
            macos_stub: Some(MacOsStub {
                java: root.join("usr/bin/java"),
                installed_jdks: vec![root.join("JavaVirtualMachines")],
            }),
        };
        assert_eq!(stub_only.runtime(), None);
        fake_jdk(
            &root.join("JavaVirtualMachines/temurin-21.jdk/Contents/Home"),
            "21.0.5",
        );
        assert_eq!(stub_only.runtime(), Some(JavaRuntime::OnPath));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn no_jdk_anywhere_is_none() {
        let root = scratch("none");
        let nothing = JavaEnvironment {
            java_home: None,
            path: vec![root.join("bin")],
            home: Some(root.join("user")),
            macos_stub: None,
        };
        assert_eq!(nothing.runtime(), None);
        assert_eq!(JavaEnvironment::default().runtime(), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
