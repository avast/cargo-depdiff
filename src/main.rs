use std::cmp;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Display, Formatter, Result as FmtResult};
use std::path::Path;

use anyhow::{Context, Error};
use either::Either;
use cargo_lock::Lockfile;
use cargo_lock::package::Package;
use cargo_lock::package::name::Name;
use itertools::{EitherOrBoth, Itertools};

type Packages = BTreeMap<Name, Vec<Package>>;

fn process_lockfile<P: AsRef<Path>>(p: P) -> Result<Packages, Error> {
    let mut f = Lockfile::load(&p)
        .with_context(|| format!("Couldn't load lockfile {}", p.as_ref().display()))?;
    f.packages.sort_unstable();

    let mut packages = Packages::new();

    for pkg in f.packages {
        packages
            .entry(pkg.name.clone())
            .or_default()
            .push(pkg);
    }

    Ok(packages)
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Sure, but Update will be much more common
enum Op {
    Add(Package),
    Remove(Package),
    Update(Package, Package),
}

impl Display for Op {
    fn fmt(&self, fmt: &mut Formatter) -> FmtResult {
        match self {
            Op::Add(pkg) => write!(fmt, "+++ {} {}", pkg.name, pkg.version),
            Op::Remove(pkg) => write!(fmt, "--- {} {}", pkg.name, pkg.version),
            Op::Update(old, new) => write!(fmt, "    {} {} -> {}", old.name, old.version, new.version),
        }
    }
}

fn wrap_op(op: fn(Package) -> Op, pkgs: Vec<Package>) -> impl Iterator<Item = Op> {
    pkgs.into_iter().map(op)
}

fn find_vers_diff(old: Vec<Package>, new: Vec<Package>) -> impl Iterator<Item = Op> {
    let mut old: BTreeSet<_> = old.into_iter().collect();
    let mut new: BTreeSet<_> = new.into_iter().collect();

    let unchanged = old
        .intersection(&new)
        .cloned()
        .collect::<Vec<_>>();

    for u in unchanged {
        old.remove(&u);
        new.remove(&u);
    }

    let mut old = old.into_iter().collect::<Vec<_>>();
    let mut new = new.into_iter().collect::<Vec<_>>();

    let common = cmp::min(old.len(), new.len());
    let removed = old.drain(common..).collect::<Vec<_>>();
    let added = new.drain(common..).collect::<Vec<_>>();

    let common = (old.into_iter().zip(new))
        .map(|(old, new)| Op::Update(old, new));
    let removed = removed.into_iter().map(Op::Remove);
    let added = added.into_iter().map(Op::Add);

    common.chain(removed).chain(added)
}

fn main() -> Result<(), Error> {
    let old = process_lockfile("testdata/old.lock")?;
    let new = process_lockfile("testdata/new.lock")?;

    old
        .into_iter()
        .merge_join_by(new, |l, r| {
            l.0.cmp(&r.0)
        })
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
