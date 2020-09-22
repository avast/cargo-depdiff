use std::cmp;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::path::Path;

use anyhow::{anyhow, Context, Error};
use cargo_lock::package::{Name, Package, SourceId, Version};
use cargo_lock::Lockfile;
use either::Either;
use git2::{Branch, Oid, Repository, Revspec};
use itertools::{EitherOrBoth, Itertools};

/*
 * FIXME: What will happen if package moves from one source to another? When it gets renamed?
 */

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

type Deps = BTreeMap<Name, Vec<Dep>>;

fn process_lockfile<P: AsRef<Path>>(p: P) -> Result<Deps, Error> {
    let mut f = Lockfile::load(&p)
        .with_context(|| format!("Couldn't load lockfile {}", p.as_ref().display()))?;
    f.packages.sort_unstable();

    let mut packages = Deps::new();

    for pkg in f.packages {
        packages
            .entry(pkg.name.clone())
            .or_default()
            .push(pkg.into());
    }

    Ok(packages)
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Sure, but Update will be much more common
enum Op {
    Add(Dep),
    Remove(Dep),
    Update(Dep, Dep),
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

    common.chain(removed).chain(added)
}

fn snapshot_to_file_content(repo: &Repository, hash: Oid, path: &Path) -> Result<String, Error> {
    let commit = repo.find_commit(hash)?;
    let tree = commit.tree()?;
    let tree_entry = tree.get_path(path)?;
    let object = tree_entry.to_object(&repo)?;
    let blob = object.as_blob().ok_or(anyhow!("not a blob"))?;
    let content = blob.content();
    String::from_utf8(content.to_owned()).map_err(|_| anyhow!("cannot pase as UTF-8"))
}

fn branch_to_file_content(repo: &Repository, branch: Branch, path: &Path) -> Result<String, Error> {
    let reference = branch.into_reference();
    let tree = reference.peel_to_tree()?;
    let tree_entry = tree.get_path(path)?;
    let object = tree_entry.to_object(&repo)?;
    let blob = object.as_blob().ok_or(anyhow!("not a blob"))?;
    let content = blob.content();
    String::from_utf8(content.to_owned()).map_err(|_| anyhow!("cannot pase as UTF-8"))
}

fn range_to_contents(
    repo: &Repository,
    revspec: Revspec,
    path: &Path,
) -> Result<(String, String), Error> {
    let old = revspec.from().ok_or(anyhow!("not a from spec"))?.id();
    let new = revspec.to().ok_or(anyhow!("not a to spec"))?.id();
    let old = snapshot_to_file_content(repo, old, path)?;
    let new = snapshot_to_file_content(repo, new, path)?;
    Ok((old, new))
}

fn main() -> Result<(), Error> {
    let old = process_lockfile("testdata/old.lock")?;
    let new = process_lockfile("testdata/new.lock")?;

    old.into_iter()
        .merge_join_by(new, |l, r| l.0.cmp(&r.0))
        .flat_map(|dep_group| match dep_group {
            EitherOrBoth::Left(remove) => Either::Left(wrap_op(Op::Remove, remove.1)),
            EitherOrBoth::Right(add) => Either::Left(wrap_op(Op::Add, add.1)),
            EitherOrBoth::Both(old, new) => Either::Right(find_vers_diff(old.1, new.1)),
        })
        .for_each(|op| {
            println!("{}", op);
        });

    Ok(())
}
