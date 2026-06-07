//! The overlay engine shared by profiles, CLI overrides, `extends`
//! inheritance, and `leek` file references.
//!
//! The rule is uniform and cargo-profile-like: a present field in the overlay
//! wins, an absent one inherits; a non-empty `weapons`/`chips` vector replaces,
//! an empty one inherits. Entities are matched by `id`.

use anyhow::{anyhow, Result};

use crate::schema::{EntitySpec, Scenario, ScenarioPatch};

impl Scenario {
    /// Apply the named `[profiles.<name>]` block on top of this scenario.
    ///
    /// # Errors
    /// Returns an error if no profile by that name exists.
    pub fn apply_profile(&mut self, name: &str) -> Result<()> {
        let patch = self
            .profiles
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow!("no profile named '{name}'"))?;
        self.apply_patch(&patch);
        Ok(())
    }

    /// Overlay a sparse [`ScenarioPatch`] (profile or CLI overrides) onto this
    /// scenario.
    pub fn apply_patch(&mut self, patch: &ScenarioPatch) {
        if patch.seed.is_some() {
            self.seed = patch.seed;
        }
        if patch.max_turns.is_some() {
            self.max_turns = patch.max_turns;
        }
        if patch.version.is_some() {
            self.version = patch.version;
        }
        if patch.strict.is_some() {
            self.strict = patch.strict;
        }
        if patch.map.is_some() {
            self.map.clone_from(&patch.map);
        }
        merge_entities(&mut self.entities, &patch.entities);
    }
}

/// Overlay a whole `child` scenario on top of `base` (used by `extends`): the
/// child's present settings win, its entities merge by id, its profiles extend
/// the base's.
pub fn overlay_scenario(base: &mut Scenario, child: &Scenario) {
    if child.seed.is_some() {
        base.seed = child.seed;
    }
    if child.max_turns.is_some() {
        base.max_turns = child.max_turns;
    }
    if child.version.is_some() {
        base.version = child.version;
    }
    if child.strict.is_some() {
        base.strict = child.strict;
    }
    if child.map.is_some() {
        base.map.clone_from(&child.map);
    }
    for f in &child.farmers {
        if let Some(slot) = base.farmers.iter_mut().find(|b| b.id == f.id) {
            *slot = f.clone();
        } else {
            base.farmers.push(f.clone());
        }
    }
    if !child.teams.is_empty() {
        base.teams.clone_from(&child.teams);
    }
    merge_entities(&mut base.entities, &child.entities);
    for (name, patch) in &child.profiles {
        base.profiles.insert(name.clone(), patch.clone());
    }
    if child.testing.is_some() {
        base.testing.clone_from(&child.testing);
    }
}

/// Merge `patch` entities into `base` by id: an entity with a matching id is
/// overlaid in place; one with a new id (or none) is appended.
pub fn merge_entities(base: &mut Vec<EntitySpec>, patch: &[EntitySpec]) {
    for p in patch {
        match p.id.and_then(|id| base.iter_mut().find(|b| b.id == Some(id))) {
            Some(slot) => overlay_entity(slot, p),
            None => base.push(p.clone()),
        }
    }
}

/// Overlay `p`'s present fields onto `base`. Used both for entity patches and
/// for resolving a `leek` template (the referencing entity is `p`).
pub fn overlay_entity(base: &mut EntitySpec, p: &EntitySpec) {
    if p.id.is_some() {
        base.id = p.id;
    }
    if p.name.is_some() {
        base.name.clone_from(&p.name);
    }
    if p.cell.is_some() {
        base.cell = p.cell;
    }
    if p.team.is_some() {
        base.team = p.team;
    }
    if p.farmer.is_some() {
        base.farmer = p.farmer;
    }
    if p.ai.is_some() {
        base.ai.clone_from(&p.ai);
    }
    if p.leek.is_some() {
        base.leek.clone_from(&p.leek);
    }
    overlay_stat(&mut base.life, p.life);
    overlay_stat(&mut base.mp, p.mp);
    overlay_stat(&mut base.tp, p.tp);
    overlay_stat(&mut base.strength, p.strength);
    overlay_stat(&mut base.wisdom, p.wisdom);
    overlay_stat(&mut base.agility, p.agility);
    overlay_stat(&mut base.resistance, p.resistance);
    overlay_stat(&mut base.science, p.science);
    overlay_stat(&mut base.magic, p.magic);
    overlay_stat(&mut base.power, p.power);
    overlay_stat(&mut base.level, p.level);
    overlay_stat(&mut base.damage_return, p.damage_return);
    if !p.weapons.is_empty() {
        base.weapons.clone_from(&p.weapons);
    }
    if !p.chips.is_empty() {
        base.chips.clone_from(&p.chips);
    }
}

fn overlay_stat(base: &mut Option<i64>, p: Option<i64>) {
    if p.is_some() {
        *base = p;
    }
}
