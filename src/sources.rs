use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Error};
use cargo::core::package::{Package, PackageSet};
use cargo::core::package_id::PackageId;
use cargo::core::source::SourceMap;
use cargo::core::SourceId;
use cargo::sources::config::SourceConfigMap;
use cargo::util::config::Config;
use semver::Version;

use super::Dep;

impl Dep {
    fn pkg_id(&self, config: &Config) -> Result<PackageId, Error> {
        // FIXME: A lot of ugly. Different version, different crates...
        let version: Version = self.version.to_string().parse().unwrap();
        let source = match self.source.as_ref() {
            Some(source) => SourceId::from_url(&source.to_string())?,
            None => SourceId::crates_io(config)?,
        };
        let pkg_id = PackageId::new(self.name.as_str(), version, source)?;
        Ok(pkg_id)
    }
}

pub(crate) struct Resolver<'cfg> {
    config: &'cfg Config,
    pkgs: PackageSet<'cfg>,
}

impl<'cfg> Resolver<'cfg> {
    pub(crate) fn new<'i, I>(config: &'cfg Config, deps: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = &'i Dep>,
    {
        let pkgs = deps
            .into_iter()
            .map(|d| {
                d.pkg_id(&config)
                    .with_context(|| format!("Can't create pkg id for {}", d.name))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let sources = SourceConfigMap::new(config)?;
        let mut source_map = SourceMap::new();
        for pkg in &pkgs {
            let mut whitelist = HashSet::new();
            whitelist.insert(*pkg);
            let source = sources.load(pkg.source_id(), &whitelist)?;
            source_map.insert(source);
        }

        let package_set = PackageSet::new(&pkgs, source_map, config)?;

        Ok(Self {
            config,
            pkgs: package_set,
        })
    }

    pub(crate) fn pkg(&self, dep: &Dep) -> Result<&Package, Error> {
        let id = dep.pkg_id(self.config)?;
        let pkg = self.pkgs.get_one(id)?;
        Ok(pkg)
    }

    pub(crate) fn dir(&self, dep: &Dep) -> Result<&Path, Error> {
        let pkg = self.pkg(dep)?;
        Ok(pkg.root())
    }
}
