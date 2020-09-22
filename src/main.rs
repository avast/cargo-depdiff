use std::cmp;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::fs;
use std::iter;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Error};
use cargo::util::config::Config;
use cargo_lock::package::{Name, Package, SourceId, Version};
use cargo_lock::Lockfile;
use either::Either;
use git2::{Object, ObjectType, Oid, Repository};
use itertools::{EitherOrBoth, Itertools};
use structopt::StructOpt;
use thiserror::Error;

use sources::Resolver;

mod sources;

/*
 * FIXME: What will happen if package moves from one source to another? When it gets renamed?
 */

/// Checking what changed about dependencies between versions.
#[derive(Debug, StructOpt)]
struct Opts {
    /// Git range specifying the changes to inspect.
    revspec: Option<String>,

    /// Path to the lock file inside the repository.
    ///
    /// Assumes it is the same in both versions.
    #[structopt(short = "p", long = "path", default_value = "Cargo.lock")]
    path: PathBuf,

    #[structopt(short = "g", long = "git-repo", default_value = ".")]
    repo: PathBuf,
}

#[derive(Clone, Debug, Ord, PartialOrd, Eq, PartialEq)]
struct Dep {
    name: Name,
    version: Version,
    source: Option<SourceId>,
}

impl From<Package> for Dep {
    fn from(pkg: Package) -> Self {
        Self {
            name: pkg.name,
            version: pkg.version,
            source: pkg.source,
        }
    }
}

#[derive(Debug, Error)]
#[error("Not a git blob")]
struct NotBlob;

fn snapshot_to_file_content(repo: &Repository, hash: Oid, path: &Path) -> Result<String, Error> {
    let commit = repo.find_commit(hash)?;
    let tree = commit.tree()?;
    let tree_entry = tree.get_path(path)?;
    let object = tree_entry.to_object(&repo)?;
    let blob = object.as_blob().ok_or(NotBlob)?;
    let content = blob.content();
    String::from_utf8(content.to_owned()).map_err(|_| anyhow!("cannot pase as UTF-8"))
}

type Deps = BTreeMap<Name, Vec<Dep>>;

fn packages_from_str(data: &str) -> Result<Deps, Error> {
    let mut lockfile: Lockfile = data.parse()?;
    lockfile.packages.sort_unstable();

    let mut packages = Deps::new();

    for pkg in lockfile.packages {
        packages
            .entry(pkg.name.clone())
            .or_default()
            .push(pkg.into());
    }

    Ok(packages)
}

fn packages_from_git(repo: &Repository, hash: Oid, path: &Path) -> Result<Deps, Error> {
    let data = snapshot_to_file_content(repo, hash, path)
        .with_context(|| format!("Couldn't find lock file {} in {}", path.display(), hash))?;

    let deps = packages_from_str(&data)
        .with_context(|| format!("{} in {} is not valid lock file", path.display(), hash))?;

    Ok(deps)
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Sure, but Update will be much more common
enum Op {
    Add(Dep),
    Remove(Dep),
    Update(Dep, Dep),
}

impl Op {
    fn print_root(&self, resolver: &Resolver) {
        match self {
            Op::Add(dep) | Op::Remove(dep) | Op::Update(_, dep) => {
                let dir = resolver.dir(dep).map(|d| d.display().to_string()).unwrap_or_default();
                println!("Root of {}: {}", dep.name, dir);
            }
        }
    }
}

impl Display for Op {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        match self {
            Op::Add(dep) => write!(fmt, "+++ {} {}", dep.name, dep.version),
            Op::Remove(dep) => write!(fmt, "--- {} {}", dep.name, dep.version),
            Op::Update(old, new) => {
                write!(fmt, "    {} {} -> {}", old.name, old.version, new.version)
            }
        }
    }
}

fn wrap_op(op: fn(Dep) -> Op, desp: Vec<Dep>) -> impl Iterator<Item = Op> {
    desp.into_iter().map(op)
}

fn find_vers_diff(old: Vec<Dep>, new: Vec<Dep>) -> impl Iterator<Item = Op> {
    let mut old: BTreeSet<_> = old.into_iter().collect();
    let mut new: BTreeSet<_> = new.into_iter().collect();

    let unchanged = old.intersection(&new).cloned().collect::<Vec<_>>();

    for u in unchanged {
        old.remove(&u);
        new.remove(&u);
    }

    let mut old = old.into_iter().collect::<Vec<_>>();
    let mut new = new.into_iter().collect::<Vec<_>>();

    let common = cmp::min(old.len(), new.len());
    let removed = old.drain(common..).collect::<Vec<_>>();
    let added = new.drain(common..).collect::<Vec<_>>();

    let common = (old.into_iter().zip(new)).map(|(old, new)| Op::Update(old, new));
    let removed = removed.into_iter().map(Op::Remove);
    let added = added.into_iter().map(Op::Add);

    removed.chain(common).chain(added)
}

#[derive(Error, Debug)]
#[error("Not a git spec")]
struct NotSpec;

#[derive(Error, Debug)]
#[error("No parent to compare to")]
struct NoParent;

fn main() -> Result<(), Error> {
    let opts = Opts::from_args();
    let repo = Repository::open(&opts.repo)
        .with_context(|| format!("Can't open git repo at {}", opts.repo.display()))?;

    let (old, new) = if let Some(revspec) = opts.revspec.as_ref() {
        let spec = |spec: Option<&Object<'_>>| {
            let spec = spec.ok_or(NotSpec)?.id();
            packages_from_git(&repo, spec, &opts.path)
        };

        let revspec = repo.revparse(revspec)?;
        let parent;

        // FIXME: MERGE_BASE mode is not doing the right thing, probably
        let (old_id, new_id) = if revspec.mode().is_range() {
            // a..b mode
            (revspec.from(), revspec.to())
        } else {
            // single-commit
            //
            // Compare to its first parent.
            let commit_obj = revspec.from().ok_or(NotSpec).context("Single commit")?;
            let commit = commit_obj.as_commit().with_context(|| format!("{} is not a commit", commit_obj.id()))?;

            parent = commit.parent(0).map_err(|_| NoParent)?.into_object();
            (Some(&parent), Some(commit_obj))
        };

        (
            spec(old_id).context("Failed to decode old version")?,
            spec(new_id).context("Failed to decode new version")?,
        )
    } else {
        let head = repo.head().context("Failed to get current HEAD")?;
        let commit = head.peel(ObjectType::Commit).context("Can't resolve HEAD to commit")?.id();
        let old = packages_from_git(&repo, commit, &opts.path).context("Reading HEAD lock file")?;

        let path = opts.repo.join(opts.path);
        let current = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read current lock file at {}", path.display()))?;

        let new = packages_from_str(&current)
            .with_context(|| format!("{} is not valid lock file", path.display()))?;

        (old, new)
    };

    let ops = old.into_iter()
        .merge_join_by(new, |l, r| l.0.cmp(&r.0))
        .flat_map(|dep_group| match dep_group {
            EitherOrBoth::Left(remove) => Either::Left(wrap_op(Op::Remove, remove.1)),
            EitherOrBoth::Right(add) => Either::Left(wrap_op(Op::Add, add.1)),
            EitherOrBoth::Both(old, new) => Either::Right(find_vers_diff(old.1, new.1)),
        })
        .collect::<Vec<_>>();

    let all_deps = ops
        .iter()
        .flat_map(|op| match op {
            Op::Add(dep) | Op::Remove(dep) => Either::Left(iter::once(dep)),
            Op::Update(old, new) => Either::Right(iter::once(old).chain(iter::once(new))),
        });

    let config = Config::default()?;
    let resolver = Resolver::new(&config, all_deps)?;

    for op in &ops {
        op.print_root(&resolver);
        println!("{}", op);
    }

    Ok(())
}
