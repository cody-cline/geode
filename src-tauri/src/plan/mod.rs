//! Versionless modpack plans.
//!
//! A plan is a list of mods with no Minecraft version and no loader attached.
//! That is the whole point: you collect what you want to play with first, and
//! the launcher works out afterwards which loader and game version can actually
//! run all of it. [`resolve`] is where that answer comes from.
//!
//! Plans live outside `instances/` because they are not instances — nothing is
//! downloaded, nothing is installed, and the same plan can be built more than
//! once at different versions.

pub mod build;
pub mod pack;
pub mod prefer;
pub mod resolve;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::paths::Paths;

/// One mod in a plan: an identity, and enough to draw a row for it.
///
/// Deliberately carries no version. A plan that pinned versions would be an
/// instance with extra steps, and would go stale the moment either the mod or
/// the plan's target moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanMod {
    /// `modrinth` or `curseforge`.
    pub provider: String,
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
}

impl PlanMod {
    /// Identity for de-duplication. The same project added twice — from a
    /// search and again from a pack — is one mod.
    pub fn key(&self) -> (String, String) {
        (self.provider.to_ascii_lowercase(), self.project_id.clone())
    }

    pub fn from_project(project: &crate::providers::Project) -> Self {
        Self {
            provider: project.provider.as_str().to_string(),
            project_id: project.id.clone(),
            name: project.name.clone(),
            slug: Some(project.slug.clone()),
            icon_url: project.icon_url.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub mods: Vec<PlanMod>,
}

impl Plan {
    pub fn new(name: String) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id: crate::instance::slugify(&name),
            name,
            notes: String::new(),
            created_at: now,
            updated_at: now,
            mods: Vec::new(),
        }
    }

    /// Drop repeats, keeping the first occurrence so the order the user built
    /// the list in survives.
    pub fn dedupe(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.mods.retain(|m| seen.insert(m.key()));
    }

    pub async fn save(&self, paths: &Paths) -> Result<()> {
        let dir = paths.plans();
        tokio::fs::create_dir_all(&dir).await?;
        tokio::fs::write(paths.plan(&self.id), serde_json::to_vec_pretty(self)?).await?;
        Ok(())
    }
}

pub async fn load(paths: &Paths, id: &str) -> Result<Plan> {
    let bytes = tokio::fs::read(paths.plan(id))
        .await
        .map_err(|_| Error::msg(format!("plan '{id}' was not found")))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::json("plan file", e))
}

pub async fn list(paths: &Paths) -> Result<Vec<Plan>> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(paths.plans()).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };

    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        // A file we cannot read is skipped rather than failing the listing.
        if let Ok(plan) = load(paths, id).await {
            out.push(plan);
        }
    }

    out.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

pub async fn create(paths: &Paths, name: String) -> Result<Plan> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(Error::msg("give the plan a name"));
    }

    let mut plan = Plan::new(name);
    plan.id = unique_id(paths, &plan.id).await;
    plan.save(paths).await?;
    Ok(plan)
}

/// Append a numeric suffix until the id is free on disk.
async fn unique_id(paths: &Paths, base: &str) -> String {
    let mut candidate = base.to_string();
    let mut n = 2;
    while tokio::fs::metadata(paths.plan(&candidate)).await.is_ok() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    candidate
}

pub async fn save(paths: &Paths, mut plan: Plan) -> Result<Plan> {
    plan.name = plan.name.trim().to_string();
    if plan.name.is_empty() {
        return Err(Error::msg("the plan needs a name"));
    }
    plan.dedupe();
    plan.updated_at = chrono::Utc::now().timestamp();
    plan.save(paths).await?;
    Ok(plan)
}

pub async fn delete(paths: &Paths, id: &str) -> Result<()> {
    let path = paths.plan(id);
    // Guard against an id that would reach outside the plans directory.
    if path.parent() != Some(paths.plans().as_path()) {
        return Err(Error::msg("refusing to delete an unexpected path"));
    }
    tokio::fs::remove_file(&path)
        .await
        .map_err(|_| Error::msg(format!("plan '{id}' was not found")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_mod(provider: &str, id: &str) -> PlanMod {
        PlanMod {
            provider: provider.into(),
            project_id: id.into(),
            name: id.into(),
            slug: None,
            icon_url: None,
        }
    }

    #[test]
    fn the_same_project_twice_is_one_mod() {
        let mut plan = Plan::new("Test".into());
        plan.mods = vec![
            plan_mod("modrinth", "AAA"),
            plan_mod("modrinth", "BBB"),
            plan_mod("modrinth", "AAA"),
        ];
        plan.dedupe();
        assert_eq!(plan.mods.len(), 2);
        // The first occurrence survives, so the list keeps its order.
        assert_eq!(plan.mods[0].project_id, "AAA");
        assert_eq!(plan.mods[1].project_id, "BBB");
    }

    #[test]
    fn the_same_id_on_two_platforms_is_two_mods() {
        let mut plan = Plan::new("Test".into());
        plan.mods = vec![plan_mod("modrinth", "123"), plan_mod("curseforge", "123")];
        plan.dedupe();
        assert_eq!(plan.mods.len(), 2);
    }

    #[test]
    fn provider_case_does_not_split_a_mod_in_two() {
        assert_eq!(
            plan_mod("Modrinth", "AAA").key(),
            plan_mod("modrinth", "AAA").key()
        );
    }
}
