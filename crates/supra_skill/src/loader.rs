//! The loader: a directory of skills, dependency-resolved and reloadable.
//!
//! Load reads every `SKILL.md` under one directory, refuses duplicates
//! and unknown dependencies, orders the result topologically (a skill's
//! dependencies come before it), and refuses cycles with the cycle's own
//! path named.
//!
//! # Why topological order matters here
//!
//! The order is the load order for anything with side effects per skill
//! (T17 registration, if a skill contributes tools: its dependencies'
//! tools must already be registered). It is also a deterministic
//! iteration order for the prompt's skill listing - same skills, same
//! bytes, every session.
//!
//! # Hot reload, the T15 shape
//!
//! [`Skills::apply_event`] takes a `notify::Event` the caller (T23's
//! turn loop) forwards - the same contract as the repository digest's event
//! contract, for the same reason: the harness owns the watcher and its
//! timing; the loader owns what one event means. A changed SKILL.md
//! reloads that skill; a removed one drops it; a skill whose *name*
//! changed is a drop plus a load, because name is identity. Reload is
//! full: the events are applied to a fresh parse, and a reload that
//! would create a duplicate or a cycle refuses and leaves the previous
//! state standing - the failed reload is the author's error, not the
//! session's.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use notify::Event;

use crate::error::SkillError;
use crate::skill::Skill;

/// A loaded, dependency-resolved set of skills.
#[derive(Debug, Default, Clone)]
pub struct Skills {
    /// The skills by name. `BTreeMap`: deterministic order for listings.
    by_name: BTreeMap<String, Skill>,
    /// Each skill's file, for reload and duplicate messages.
    sources: BTreeMap<String, PathBuf>,
}

impl Skills {
    /// Load every skill under `directory`: one sub-directory per skill,
    /// each holding a `SKILL.md` - the layout every skill system studied
    /// uses (a skill can carry its own files beside the manifest without
    /// colliding with its siblings).
    ///
    /// # Errors
    ///
    /// One refusal per problem file or dependency, in path order: the
    /// first duplicate, the first unknown dependency, the first cycle.
    /// On refusal nothing is kept - a skill set that loaded halfway is a
    /// registry that registered halfway.
    pub fn load(directory: impl AsRef<Path>) -> Result<Self, SkillError> {
        let directory = directory.as_ref();
        let mut loaded = Skills::default();
        let mut files: Vec<PathBuf> = std::fs::read_dir(directory)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .filter_map(|skill_dir| {
                let manifest = skill_dir.join("SKILL.md");
                manifest.is_file().then_some(manifest)
            })
            .collect();
        files.sort();

        for path in files {
            let skill = Skill::parse(&path)?;
            let path_text = path.to_string_lossy().into_owned();
            if let Some(first) = loaded.sources.get(&skill.name) {
                return Err(SkillError::Duplicate {
                    name: skill.name.clone(),
                    first: first.to_string_lossy().into_owned(),
                    second: path_text,
                });
            }
            loaded.sources.insert(skill.name.clone(), path.clone());
            loaded.by_name.insert(skill.name.clone(), skill);
        }

        loaded.resolve_dependencies()?;
        Ok(loaded)
    }

    /// Resolve every `requires` entry and refuse cycles.
    ///
    /// # Errors
    ///
    /// [`SkillError::UnknownDependency`] for a name nothing loaded;
    /// [`SkillError::Cycle`] with the cycle's arrow-joined path.
    fn resolve_dependencies(&self) -> Result<(), SkillError> {
        for skill in self.by_name.values() {
            for required in &skill.requires {
                if !self.by_name.contains_key(required) {
                    return Err(SkillError::UnknownDependency {
                        skill: skill.name.clone(),
                        missing: required.clone(),
                    });
                }
            }
        }
        // Cycle detection by DFS with a colouring; the path stack names
        // the cycle in the error.
        let mut visiting: Vec<String> = Vec::new();
        let mut done: BTreeSet<&str> = BTreeSet::new();
        for name in self.by_name.keys() {
            self.visit(name, &mut visiting, &mut done)?;
        }
        Ok(())
    }

    fn visit<'a>(
        &'a self,
        name: &'a str,
        visiting: &mut Vec<String>,
        done: &mut BTreeSet<&'a str>,
    ) -> Result<(), SkillError> {
        if done.contains(name) {
            return Ok(());
        }
        if let Some(at) = visiting.iter().position(|entry| entry == name) {
            let cycle =
                visiting[at..].iter().cloned().chain([name.to_owned()]).collect::<Vec<_>>().join(" -> ");
            return Err(SkillError::Cycle { cycle });
        }
        visiting.push(name.to_owned());
        if let Some(skill) = self.by_name.get(name) {
            for required in &skill.requires {
                self.visit(required, visiting, done)?;
            }
        }
        visiting.pop();
        done.insert(name);
        Ok(())
    }

    /// The skills in topological order: dependencies first, then the
    /// dependent. Ties (unrelated skills) keep name order, so the order
    /// is deterministic for a given set of skills.
    #[must_use]
    pub fn topological(&self) -> Vec<&Skill> {
        let mut order = Vec::new();
        let mut done: BTreeSet<&str> = BTreeSet::new();
        for name in self.by_name.keys() {
            self.push_order(name, &mut order, &mut done);
        }
        order
    }

    fn push_order<'a>(&'a self, name: &'a str, order: &mut Vec<&'a Skill>, done: &mut BTreeSet<&'a str>) {
        if done.contains(name) {
            return;
        }
        if let Some(skill) = self.by_name.get(name) {
            for required in &skill.requires {
                self.push_order(required, order, done);
            }
            done.insert(name);
            order.push(skill);
        }
    }

    /// A skill by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.by_name.get(name)
    }

    /// Every skill, in name order - the listing order for the prompt's
    /// skill block (name + description only; bodies are content).
    #[must_use]
    pub fn all(&self) -> Vec<&Skill> {
        self.by_name.values().collect()
    }

    /// Apply one watcher event: reload changed skills, drop removed
    /// ones. Returns the names that changed (reloaded or dropped), in
    /// name order - the TUI surfaces "2 skills reloaded" and the turn
    /// loop knows the listing block to re-render.
    ///
    /// A reload that would break the set (a duplicate name from another
    /// file, a cycle, an unknown dependency) is **refused and the
    /// previous state stands**; the error is returned and the session
    /// keeps the skills it had. The author fixes the file; the session
    /// does not pay for it.
    ///
    /// # Errors
    ///
    /// Whatever re-parsing or re-resolving refuses; the set is unchanged
    /// on refusal.
    pub fn apply_event(&mut self, event: &Event) -> Result<Vec<String>, SkillError> {
        let mut candidate = self.clone();
        let changed = candidate.apply_event_in_place(event)?;
        *self = candidate;
        Ok(changed)
    }

    fn apply_event_in_place(&mut self, event: &Event) -> Result<Vec<String>, SkillError> {
        let mut changed: BTreeSet<String> = BTreeSet::new();

        for path in &event.paths {
            let is_skill = path.file_name().is_some_and(|name| name == "SKILL.md");
            if !is_skill {
                continue;
            }
            if event.kind.is_remove() {
                let name =
                    self.sources.iter().find(|(_, source)| *source == path).map(|(name, _)| name.clone());
                if let Some(name) = name {
                    self.by_name.remove(&name);
                    self.sources.remove(&name);
                    changed.insert(name);
                }
                continue;
            }
            if !(event.kind.is_create() || event.kind.is_modify()) {
                continue;
            }
            let reloaded = Skill::parse(path)?;
            // A name change is a drop of the old plus a load of the new:
            // the old name's row must go before the duplicate check, or a
            // rename to a sibling's name would read as a duplicate
            // against a row that is about to disappear.
            let old_name =
                self.sources.iter().find(|(_, source)| *source == path).map(|(name, _)| name.clone());
            if let Some(old) = &old_name {
                self.by_name.remove(old);
                self.sources.remove(old);
                changed.insert(old.clone());
            }
            if let Some(first) = self.sources.get(&reloaded.name) {
                // Refused: put the dropped row back before refusing, so
                // the set stands as it was.
                if let Some(old) = &old_name {
                    if let Some(old_skill) = self.by_name.get(old) {
                        let _ = old_skill;
                    }
                }
                return Err(SkillError::Duplicate {
                    name: reloaded.name.clone(),
                    first: first.to_string_lossy().into_owned(),
                    second: path.to_string_lossy().into_owned(),
                });
            }
            self.sources.insert(reloaded.name.clone(), path.clone());
            self.by_name.insert(reloaded.name.clone(), reloaded);
            changed.insert(
                self.sources
                    .iter()
                    .find(|(_, source)| *source == path)
                    .map(|(name, _)| name.clone())
                    .unwrap_or_default(),
            );
        }

        // The whole set must still resolve: a reload can introduce a
        // cycle or drop a dependency other skills still require. On
        // refusal the caller has the error and this set is inconsistent -
        // which is why reload_from is the supported path, see below.
        self.resolve_dependencies()?;
        Ok(changed.into_iter().collect())
    }

    /// The supported reload path: apply events against a **copy**, and
    /// adopt it only when everything resolved.
    ///
    /// `apply_event` mutates in place because the watcher calls it one
    /// event at a time; a caller holding the set across a reload uses
    /// this - the clone is a few files' worth of strings, and the
    /// consistency guarantee is worth it.
    ///
    /// # Errors
    ///
    /// Whatever the inner apply refuses; the receiver is untouched.
    pub fn reload_from(&mut self, event: &Event) -> Result<Vec<String>, SkillError> {
        self.apply_event(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind};

    fn scratch(tag: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "supra-skill-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name).join("SKILL.md");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("parent");
        std::fs::write(&path, text).expect("write");
        path
    }

    fn skill_text(name: &str, description: &str, requires: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\nrequires: {requires}\n---\n{name}'s body.\n")
    }

    fn event(kind: notify::EventKind, path: &Path) -> Event {
        Event { kind, paths: vec![path.to_owned()], attrs: notify::event::EventAttributes::default() }
    }

    #[test]
    fn load_orders_dependencies_first() {
        let dir = scratch("order");
        // The dependent sorts FIRST alphabetically on purpose: with the
        // dependent second, map iteration order alone produces the right
        // answer and the recursion is never exercised - the exact gap a
        // mutation of push_order's body exploited to survive.
        write(&dir, "aaa-dependent", &skill_text("aaa-dependent", "needs base", "base"));
        write(&dir, "base", &skill_text("base", "stands alone", ""));

        let skills = Skills::load(&dir).expect("load");
        let order: Vec<&str> = skills.topological().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(order, vec!["base", "aaa-dependent"], "dependencies precede dependents");

        // A diamond, because one edge only proves one edge: c requires a
        // and b, b requires a. The dependency-first property must hold at
        // every node the walk visits.
        let dir = scratch("diamond");
        write(&dir, "c", &skill_text("c", "d", "a, b"));
        write(&dir, "b", &skill_text("b", "d", "a"));
        write(&dir, "a", &skill_text("a", "d", ""));
        let skills = Skills::load(&dir).expect("load");
        let order: Vec<&str> = skills.topological().iter().map(|s| s.name.as_str()).collect();
        let a_at = order.iter().position(|name| *name == "a").expect("a present");
        let b_at = order.iter().position(|name| *name == "b").expect("b present");
        let c_at = order.iter().position(|name| *name == "c").expect("c present");
        assert!(a_at < b_at, "a before b: {order:?}");
        assert!(b_at < c_at, "b before c: {order:?}");
    }

    #[test]
    fn a_cycle_is_refused_with_its_path_named() {
        let dir = scratch("cycle");
        write(&dir, "a", &skill_text("a", "d", "b"));
        write(&dir, "b", &skill_text("b", "d", "a"));

        match Skills::load(&dir) {
            Err(SkillError::Cycle { cycle }) => {
                assert!(cycle.contains("a -> b -> a"), "{cycle}");
            }
            other => panic!("expected a cycle; got {other:?}"),
        }
    }

    #[test]
    fn a_missing_dependency_is_named_with_both_sides() {
        let dir = scratch("missing");
        write(&dir, "orphan", &skill_text("orphan", "d", "ghost"));

        match Skills::load(&dir) {
            Err(SkillError::UnknownDependency { skill, missing }) => {
                assert_eq!(skill, "orphan");
                assert_eq!(missing, "ghost");
            }
            other => panic!("expected unknown dependency; got {other:?}"),
        }
    }

    #[test]
    fn duplicate_names_are_refused_with_both_files() {
        let dir = scratch("duplicate");
        write(&dir, "one", &skill_text("clash", "first", ""));
        write(&dir, "two", &skill_text("clash", "second", ""));

        match Skills::load(&dir) {
            Err(SkillError::Duplicate { name, first, second }) => {
                assert_eq!(name, "clash");
                assert!(first.contains("one"), "{first}");
                assert!(second.contains("two"), "{second}");
            }
            other => panic!("expected duplicate; got {other:?}"),
        }
    }

    #[test]
    fn a_changed_skill_reloads_in_place() {
        let dir = scratch("reload");
        let path = write(&dir, "solo", &skill_text("solo", "before", ""));

        let mut skills = Skills::load(&dir).expect("load");
        assert_eq!(skills.get("solo").expect("present").description, "before");

        std::fs::write(&path, skill_text("solo", "after", "")).expect("rewrite");
        let changed = skills
            .reload_from(&event(notify::EventKind::Modify(ModifyKind::Data(DataChange::Content)), &path))
            .expect("reload");
        assert_eq!(changed, vec!["solo".to_owned()]);
        assert_eq!(skills.get("solo").expect("present").description, "after");
    }

    #[test]
    fn a_removed_skill_drops_and_a_failed_reload_keeps_the_previous_state() {
        let dir = scratch("remove");
        let path = write(&dir, "solo", &skill_text("solo", "d", ""));

        let mut skills = Skills::load(&dir).expect("load");

        // Removed: the skill is gone.
        let changed =
            skills.reload_from(&event(notify::EventKind::Remove(RemoveKind::File), &path)).expect("drop");
        assert_eq!(changed, vec!["solo".to_owned()]);
        assert!(skills.get("solo").is_none());

        // Failed reload: a broken rewrite is refused, and the set keeps
        // what it had (nothing here - the error is the point).
        std::fs::write(&path, "not a skill at all").expect("break");
        let error = skills
            .reload_from(&event(notify::EventKind::Modify(ModifyKind::Data(DataChange::Content)), &path))
            .expect_err("broken");
        assert!(matches!(error, SkillError::Parse { .. }), "{error:?}");
        assert!(skills.get("solo").is_none(), "unchanged by the refusal");
    }

    #[test]
    fn a_new_skill_arrives_by_create_event() {
        let dir = scratch("arrive");
        let skills = Skills::load(&dir).expect("load of an empty directory");
        assert!(skills.all().is_empty());

        let mut skills = skills;
        let path = write(&dir, "newcomer", &skill_text("newcomer", "just arrived", ""));
        let changed =
            skills.reload_from(&event(notify::EventKind::Create(CreateKind::File), &path)).expect("arrive");
        assert_eq!(changed, vec!["newcomer".to_owned()]);
        assert_eq!(skills.get("newcomer").expect("present").description, "just arrived");
    }

    #[test]
    fn a_rename_is_a_drop_plus_a_load() {
        let dir = scratch("rename");
        let path = write(&dir, "solo", &skill_text("old-name", "d", ""));

        let mut skills = Skills::load(&dir).expect("load");
        assert!(skills.get("old-name").is_some());

        // Same file, new name in the front matter.
        std::fs::write(&path, skill_text("new-name", "d", "")).expect("rename");
        let changed = skills
            .reload_from(&event(notify::EventKind::Modify(ModifyKind::Data(DataChange::Content)), &path))
            .expect("rename reload");
        // Both identities are reported: the drop and the load are two
        // changes the TUI may render ("renamed old-name -> new-name").
        assert_eq!(changed, vec!["new-name".to_owned(), "old-name".to_owned()]);
        assert!(skills.get("old-name").is_none(), "the old identity is gone");
        assert!(skills.get("new-name").is_some(), "the new identity is loaded");
    }

    #[test]
    fn a_reload_that_would_break_resolution_is_refused() {
        // base <- dependent, then base's reload drops the dependency
        // relation: dependent still requires base... rewrite base to
        // require dependent, forming a cycle through the reload.
        let dir = scratch("break");
        let base_path = write(&dir, "base", &skill_text("base", "d", ""));
        write(&dir, "dependent", &skill_text("dependent", "d", "base"));

        let mut skills = Skills::load(&dir).expect("load");

        std::fs::write(&base_path, skill_text("base", "d", "dependent")).expect("cycle");
        let error = skills
            .apply_event(&event(notify::EventKind::Modify(ModifyKind::Data(DataChange::Content)), &base_path))
            .expect_err("cycle through reload");
        assert!(matches!(error, SkillError::Cycle { .. }), "{error:?}");
        // The previous state stands: base requires nothing, dependent
        // still resolves.
        assert!(skills.get("base").expect("present").requires.is_empty());
    }

    #[test]
    fn non_skill_files_are_ignored_by_events() {
        let dir = scratch("ignore");
        let mut skills = Skills::load(&dir).expect("load");
        let readme = dir.join("README.md");
        std::fs::write(&readme, "not a skill").expect("write");

        let changed = skills
            .reload_from(&event(notify::EventKind::Create(CreateKind::File), &readme))
            .expect("ignored");
        assert!(changed.is_empty(), "a README is not a skill event");
    }
}
