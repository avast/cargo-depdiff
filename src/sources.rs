use std::path::PathBuf;

use anyhow::Error;
use cargo::core::SourceId;
use cargo::core::source::SourceMap;
use cargo::core::package::PackageSet;
use cargo::core::package_id::PackageId;
use cargo::sources::config::SourceConfigMap;
use cargo::util::config::Config;
use semver::Version;

use super::Dep;

impl Dep {
    // TODO: Wheee!!!
    // Create some kind of batch resolver for everything and kitchen sink. This is likely very
    // inefficient. Also, preserve the config. And stuff. And sources maps.
    pub(crate) fn dir(&self) -> Result<PathBuf, Error> {
        let config = Config::default()?;
        // FIXME: A lot of ugly. Different version, different crates...
        let version: Version = self.version.to_string().parse().unwrap();
        // TODO: Fallback to something
        let source = self.source.as_ref().unwrap().to_string();
        let source = SourceId::from_url(&source).unwrap();
        let pkg_id = PackageId::new(self.name.as_str(), version, source)?;

        let sources = SourceConfigMap::new(&config)?;
        let source = sources.load(source, &Default::default())?;
        let mut map = SourceMap::new();
        map.insert(source);

        let package_set = PackageSet::new(&[pkg_id], map, &config)?;
        let pkg = package_set.get_one(pkg_id)?;
        Ok(pkg.root().to_owned())
    }
}
