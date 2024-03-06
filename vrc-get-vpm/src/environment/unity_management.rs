use crate::io::EnvironmentIo;
use crate::version::UnityVersion;
use crate::{io, Environment, HttpClient};
use lazy_static::lazy_static;
use log::info;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::from_utf8;
use tokio::process::Command;
use vrc_get_litedb::UnityVersion as DbUnityVersion;

impl<T: HttpClient, IO: EnvironmentIo> Environment<T, IO> {
    pub fn get_unity_installations(&self) -> io::Result<Vec<UnityInstallation>> {
        Ok(self
            .get_db()?
            .get_unity_versions()?
            .into_vec()
            .into_iter()
            .map(UnityInstallation::new)
            .collect())
    }

    pub async fn add_unity_installation(&mut self, path: &str) -> io::Result<UnityVersion> {
        let db = self.get_db()?;

        // first, check for duplicates
        if db
            .get_unity_versions()?
            .into_vec()
            .into_iter()
            .any(|x| x.path() == path)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("unity installation at {} already exists", path),
            ));
        }

        let output = Command::new(path).arg("-version").output().await?;

        if !output.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid unity installation at {}", path),
            ));
        }

        let stdout = &output.stdout[..];
        let index = stdout
            .iter()
            .position(|&x| x == b' ')
            .unwrap_or(stdout.len());

        let version = from_utf8(&stdout[..index])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid version"))?
            .trim();

        let version = UnityVersion::parse(version).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid version: {version}"),
            )
        })?;

        let installation =
            DbUnityVersion::new(path.into(), version.to_string().into_boxed_str(), false);

        db.insert_unity_version(&installation)?;

        Ok(version)
    }

    pub async fn remove_unity_installation(&mut self, unity: &UnityInstallation) -> io::Result<()> {
        self.get_db()?.delete_unity_version(unity.inner.id())?;

        Ok(())
    }

    pub fn find_most_suitable_unity(
        &self,
        expected: UnityVersion,
    ) -> io::Result<Option<UnityInstallation>> {
        let mut revision_match = None;
        let mut minor_match = None;
        let mut major_match = None;

        for unity in self.get_unity_installations()? {
            if let Some(version) = unity.version() {
                if version == expected {
                    return Ok(Some(unity));
                }

                if version.major() == expected.major() {
                    if version.minor() == expected.minor() {
                        if version.revision() == expected.revision() {
                            revision_match = Some(unity);
                        } else {
                            minor_match = Some(unity);
                        }
                    } else {
                        major_match = Some(unity);
                    }
                } else {
                    continue;
                }
            }
        }

        Ok(revision_match.or(minor_match).or(major_match))
    }
}

/// UnityHub Operations
impl<T: HttpClient, IO: EnvironmentIo> Environment<T, IO> {
    fn default_unity_hub_path() -> &'static [&'static str] {
        // https://docs.unity3d.com/hub/manual/HubCLI.html
        if cfg!(windows) {
            &["C:\\Program Files\\Unity Hub\\Unity Hub.exe"]
        } else if cfg!(target_os = "macos") {
            &["/Applications/Unity Hub.app/Contents/MacOS/Unity Hub"]
        } else if cfg!(target_os = "linux") {
            // for linux,
            lazy_static! {
                static ref USER_INSTALLATION: String = {
                    let home = std::env::var("HOME").expect("HOME not set");
                    format!("{}/Applications/Unity Hub.AppImage", home)
                };
                static ref INSTALLATIONS: [&'static str; 2] =
                    [&USER_INSTALLATION, "/usr/bin/unity-hub"];
            }

            INSTALLATIONS.as_ref()
        } else {
            &[]
        }
    }

    pub async fn find_unity_hub(&mut self) -> io::Result<Option<String>> {
        let path = self.settings.unity_hub();
        if !path.is_empty() && self.io.is_file(path.as_ref()).await {
            // if configured one is valid path to file, return it
            return Ok(Some(path.to_string()));
        }

        // if not, try default paths

        for &path in Self::default_unity_hub_path() {
            if self.io.is_file(path.as_ref()).await {
                self.settings.set_unity_hub(path);
                return Ok(Some(path.to_string()));
            }
        }

        Ok(None)
    }

    pub async fn update_unity_from_unity_hub_and_fs(
        &mut self,
        paths_from_hub: impl IntoIterator<Item = PathBuf>,
    ) -> io::Result<()> {
        let paths_from_hub = paths_from_hub.into_iter().collect::<HashSet<_>>();

        let db = self.get_db()?;

        let mut installed = HashSet::new();

        for mut in_db in db.get_unity_versions()?.into_vec() {
            if !self.io.is_file(in_db.path().as_ref()).await {
                // if the unity editor not found, remove it from the db
                info!("Removed Unity that is not exists: {}", in_db.path());
                db.delete_unity_version(in_db.id())?;
                continue;
            }

            installed.insert(PathBuf::from(in_db.path()));

            let exists_in_hub = paths_from_hub.contains(Path::new(in_db.path()));

            if exists_in_hub != in_db.loaded_from_hub() {
                in_db.set_loaded_from_hub(exists_in_hub);
                db.update_unity_version(&in_db)?;
            }
        }

        for path in paths_from_hub {
            if !installed.contains(&path) {
                info!("Added Unity from Unity Hub: {}", path.display());
                self.add_unity_installation(&path.to_string_lossy()).await?;
            }
        }

        Ok(())
    }
}

#[allow(dead_code)]
pub struct UnityInstallation {
    inner: DbUnityVersion,
}

impl UnityInstallation {
    pub fn new(inner: DbUnityVersion) -> Self {
        Self { inner }
    }

    pub fn path(&self) -> &str {
        self.inner.path()
    }

    pub fn version(&self) -> Option<UnityVersion> {
        self.inner.version().and_then(UnityVersion::parse)
    }

    pub fn loaded_from_hub(&self) -> bool {
        self.inner.loaded_from_hub()
    }
}
