use std::collections::{HashMap, HashSet};
use std::fmt;

use itertools::Itertools;

use crate::io::ProjectIo;
use crate::unity_project::{
    package_resolution, pending_project_changes, AddPackageErr, LockedDependencyInfo,
    PendingProjectChanges,
};
use crate::version::DependencyRange;
use crate::{PackageCollection, UnityProject, VersionSelector};

#[derive(Debug)]
#[non_exhaustive]
pub enum ResolvePackageErr {
    DependencyNotFound { dependency_name: Box<str> },
}

impl fmt::Display for ResolvePackageErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolvePackageErr::DependencyNotFound { dependency_name } => write!(
                f,
                "Package {dependency_name} (maybe dependencies of the package) not found"
            ),
        }
    }
}

impl std::error::Error for ResolvePackageErr {}

impl From<AddPackageErr> for ResolvePackageErr {
    fn from(value: AddPackageErr) -> Self {
        match value {
            AddPackageErr::DependencyNotFound { dependency_name } => {
                Self::DependencyNotFound { dependency_name }
            }
            AddPackageErr::UpgradingNonLockedPackage { .. }
            | AddPackageErr::DowngradingNonLockedPackage { .. }
            | AddPackageErr::UpgradingWithDowngrade { .. } => {
                panic!("{value:?} should not be happened")
            }
        }
    }
}

impl<IO: ProjectIo> UnityProject<IO> {
    /// Returns whether the project should be resolved.
    ///
    /// The project will be resolved if: (not exhaustive)
    /// - some packages defined in `locked` section are missing
    /// - some packages defined in `dependencies` section are missing
    /// - some dependencies of unlocked packages are missing
    pub fn should_resolve(&self) -> bool {
        let mut installed_or_legacy = HashSet::<&str>::new();

        // check locked packages
        for locked in self.manifest.all_locked() {
            let Some(installed) = self.installed_packages.get(locked.name()) else {
                return true;
            };
            if installed.version() != locked.version() {
                return true;
            }
            installed_or_legacy.insert(locked.name());
            for legacy in installed.legacy_packages() {
                installed_or_legacy.insert(legacy.as_ref());
            }
        }

        // add legacy packages of unlocked packages to installed_or_legacy
        for (_, pkg) in self.unlocked_packages() {
            if let Some(pkg) = pkg {
                for legacy in pkg.legacy_packages() {
                    installed_or_legacy.insert(legacy.as_ref());
                }
            }
        }

        // check dependencies
        for (dependency, _) in self.manifest.dependencies() {
            if !installed_or_legacy.contains(dependency) {
                return true;
            }
        }

        // add unlocked packages to installed_or_legacy
        // we won't include unlocked package for checking packages in vpm-manifest,
        // so we add after checking locked and dependencies
        for (_, pkg) in self.unlocked_packages() {
            if let Some(pkg) = pkg {
                installed_or_legacy.insert(pkg.name());
            }
        }

        // check dependencies of unlocked packages
        for (_, pkg) in self.unlocked_packages() {
            if let Some(pkg) = pkg {
                for (dependency, _) in pkg.vpm_dependencies() {
                    if !installed_or_legacy.contains(dependency.as_ref()) {
                        return true;
                    }
                }
            }
        }

        // all packages are installed! no need to resolve
        false
    }

    pub async fn resolve_request<'env>(
        &self,
        env: &'env impl PackageCollection,
    ) -> Result<PendingProjectChanges<'env>, AddPackageErr> {
        let mut changes = pending_project_changes::Builder::new();

        // first, process locked dependencies
        for dep in self.manifest.all_locked() {
            let pkg = env
                .find_package_by_name(dep.name(), VersionSelector::specific_version(dep.version()))
                .ok_or_else(|| AddPackageErr::DependencyNotFound {
                    dependency_name: dep.name().into(),
                })?;

            changes.install_already_locked(pkg);
        }

        // then, process packages in dependencies but not in locked.
        // This usually happens with template projects.
        self.add_just_dependency(env, &mut changes)?;

        // finally, process dependencies of unlocked packages.
        self.resolve_unlocked(env, &mut changes)?;

        Ok(changes.build_resolve(self).await)
    }

    fn add_just_dependency<'env>(
        &self,
        env: &'env impl PackageCollection,
        changes: &mut pending_project_changes::Builder<'env>,
    ) -> Result<(), AddPackageErr> {
        let mut to_install = vec![];
        let mut install_names = HashSet::new();

        for (name, range) in self.manifest.dependencies() {
            if self.manifest.get_locked(name).is_none() {
                to_install.push(
                    env.find_package_by_name(
                        name,
                        VersionSelector::range_for(
                            self.unity_version(),
                            &range.as_range(),
                            range.as_range().contains_pre(),
                        ),
                    )
                    .ok_or_else(|| AddPackageErr::DependencyNotFound {
                        dependency_name: name.into(),
                    })?,
                );
                install_names.insert(name);
            }
        }

        if to_install.is_empty() {
            return Ok(());
        }

        let allow_prerelease = to_install.iter().any(|x| !x.version().pre.is_empty());

        let result = package_resolution::collect_adding_packages(
            self.manifest.dependencies(),
            self.manifest.all_locked(),
            &self.unlocked_packages,
            |pkg| self.manifest.get_locked(pkg),
            self.unity_version(),
            env,
            to_install,
            allow_prerelease,
        )?;

        for x in result.new_packages {
            changes.install_to_locked(x);
            if install_names.contains(x.name()) {
                changes.add_to_dependencies(
                    x.name().into(),
                    DependencyRange::version(x.version().clone()),
                );
            }
        }

        for (package, conflicts_with) in result.conflicts {
            changes.conflict_multiple(package, conflicts_with);
        }

        Ok(())
    }

    fn resolve_unlocked<'env>(
        &self,
        env: &'env impl PackageCollection,
        changes: &mut pending_project_changes::Builder<'env>,
    ) -> Result<(), AddPackageErr> {
        if self.unlocked_packages().is_empty() {
            // if there are no unlocked packages, early return
            return Ok(());
        }

        // set of packages already installed as unlocked
        let unlocked_names: HashSet<_> = self
            .unlocked_packages()
            .iter()
            .filter_map(|(_, pkg)| pkg.as_ref())
            .map(|x| x.name())
            .collect();

        if unlocked_names.is_empty() {
            // if there are no unlocked packages, early return
            return Ok(());
        }

        // then, process dependencies of unlocked packages.
        let dependencies_of_unlocked_packages = self
            .unlocked_packages
            .iter()
            .filter_map(|(_, pkg)| pkg.as_ref())
            .flat_map(|pkg| {
                pkg.vpm_dependencies()
                    .into_iter()
                    .map(|(k, v)| (k, v, pkg.version().is_pre()))
            });

        let unlocked_dependencies_versions = dependencies_of_unlocked_packages
            .filter(|(k, _, _)| self.manifest.get_locked(k.as_ref()).is_none()) // skip if already installed to locked
            .filter(|(k, _, _)| changes.get_installing(k).is_none()) // skip if we're installing
            .filter(|(k, _, _)| !unlocked_names.contains(k.as_ref())) // skip if already installed as unlocked
            .map(|(k, r, pre)| (k, (r, pre)))
            .into_group_map();

        if unlocked_dependencies_versions.is_empty() {
            // if no dependencies are to be installed, early return
            return Ok(());
        }

        let mut virtual_locked_dependencies = self
            .manifest
            .all_locked()
            .map(|x| (x.name(), x))
            .collect::<HashMap<_, _>>();

        for x in changes.get_all_installing() {
            virtual_locked_dependencies.insert(
                x.name(),
                LockedDependencyInfo::new(x.name(), x.version(), Some(x.vpm_dependencies())),
            );
        }

        let unlocked_dependencies = unlocked_dependencies_versions
            .into_iter()
            .map(|(pkg_name, packages)| {
                let ranges = packages
                    .iter()
                    .map(|(range, _)| range)
                    .copied()
                    .collect::<Vec<_>>();
                let allow_prerelease = packages.iter().any(|(_, pre)| *pre);
                env.find_package_by_name(
                    pkg_name,
                    VersionSelector::ranges_for(self.unity_version, &ranges, allow_prerelease),
                )
                .ok_or_else(|| AddPackageErr::DependencyNotFound {
                    dependency_name: pkg_name.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let allow_prerelease = unlocked_dependencies
            .iter()
            .any(|x| !x.version().pre.is_empty());

        let result = package_resolution::collect_adding_packages(
            self.manifest.dependencies(),
            virtual_locked_dependencies.values().cloned(),
            &self.unlocked_packages,
            |pkg| virtual_locked_dependencies.get(pkg).cloned(),
            self.unity_version(),
            env,
            unlocked_dependencies,
            allow_prerelease,
        )?;

        for x in result.new_packages {
            changes.install_to_locked(x);
        }

        for (package, conflicts_with) in result.conflicts {
            changes.conflict_multiple(package, conflicts_with);
        }

        Ok(())
    }
}
